#![forbid(unsafe_code)]
#![doc = "Command orchestration for the `leptos_ui_theme` CLI."]

use clap::{Args, Parser, Subcommand};
use leptos_ui_theme_codegen::{
    ApplyCommand, BuildOptions as CodegenBuildOptions, Change as PlannedChange, ChangeOperation,
    ChangeScope, CodegenError, DependencyRecord, DependencyState, GeneratedArtifact, Ownership,
    apply_artifacts_for_with_wait, apply_with_wait, build_with_options, check,
    default_dependency_records, ensure_no_active_transaction, plan_artifacts,
    revalidate_build_result, seeded_controller, seeded_module, seeded_scope,
};
use leptos_ui_theme_core::{
    CONFIG_FILE, KitConfig, LogicalPath, Profile, ProjectConfig, SourceLoader, SourceRole,
    ThemeCompiler, ThemeError, discover_kit,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The immutable JSON command-envelope schema.
pub const DIAGNOSTIC_ENVELOPE_SCHEMA: &str =
    "https://triesap.github.io/leptos_ui_theme/schema/0.1.0/diagnostic-envelope.schema.json";
/// Packaged draft 2020-12 JSON command-envelope schema bytes.
pub const DIAGNOSTIC_ENVELOPE_SCHEMA_JSON: &str =
    include_str!("../schemas/diagnostic-envelope.schema.json");
/// The immutable runtime-qualification result schema.
pub const RUNTIME_QUALIFICATION_RESULT_SCHEMA: &str = "https://triesap.github.io/leptos_ui_theme/schema/0.1.0/runtime-qualification-result.schema.json";
/// Packaged draft 2020-12 runtime-qualification result schema bytes.
pub const RUNTIME_QUALIFICATION_RESULT_SCHEMA_JSON: &str =
    include_str!("../schemas/runtime-qualification-result.schema.json");

#[derive(Debug, Parser)]
#[command(
    name = "leptos_ui_theme",
    version,
    about = "Compile DTCG tokens into Leptos theme artifacts"
)]
pub struct Cli {
    #[arg(long, global = true, default_value = ".")]
    cwd: PathBuf,
    #[arg(long, global = true, conflicts_with_all = ["quiet", "verbose"])]
    json: bool,
    #[arg(long, global = true, conflicts_with_all = ["json", "verbose"])]
    quiet: bool,
    #[arg(long, global = true, conflicts_with_all = ["json", "quiet"])]
    verbose: bool,
    #[arg(
        long,
        global = true,
        default_value_t = 0,
        value_parser = clap::value_parser!(u64).range(0..=300_000)
    )]
    lock_wait_ms: u64,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init(HtmlWriteOptions),
    Build(BuildWriteOptions),
    Check,
    List,
    Explain {
        token_path: String,
        #[arg(long)]
        profile: String,
    },
    Add {
        id: String,
        #[arg(
            long,
            conflicts_with = "from_contract_defaults",
            required_unless_present = "from_contract_defaults"
        )]
        base: Option<String>,
        #[arg(long, conflicts_with = "base", required_unless_present = "base")]
        from_contract_defaults: bool,
        #[arg(long)]
        dry_run: bool,
    },
    Doctor {
        #[arg(long, required = true)]
        strict: bool,
    },
}

#[derive(Clone, Debug, Args)]
struct HtmlWriteOptions {
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    no_patch_index: bool,
}

#[derive(Clone, Debug, Args)]
struct BuildWriteOptions {
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    no_patch_index: bool,
    #[arg(long = "accept-generated")]
    accept_generated: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Core(#[from] ThemeError),
    #[error(transparent)]
    Codegen(#[from] leptos_ui_theme_codegen::CodegenError),
    #[error("cannot serialize output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid TOML in {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{0}")]
    Conflict(String),
    #[error("cannot write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl CliError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Conflict(_) => 4,
            Self::Core(ThemeError::Security(_)) => 5,
            Self::Core(ThemeError::Contract(_)) => 6,
            Self::Core(_) => 3,
            Self::Codegen(CodegenError::Conflict(_)) => 4,
            Self::Codegen(CodegenError::Core(ThemeError::Security(_))) => 5,
            Self::Codegen(CodegenError::Core(ThemeError::Contract(_))) => 6,
            Self::Codegen(CodegenError::Core(_)) => 3,
            Self::Toml { .. } => 3,
            Self::Codegen(_) | Self::Json(_) | Self::Io { .. } => 70,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Success,
    NoChange,
    Planned,
    Warning,
    Conflict,
    Error,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    schema_version: &'static str,
    command: Option<String>,
    status: Status,
    exit_code: i32,
    diagnostics: Vec<Diagnostic>,
    changes: Vec<Change>,
    data: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct Diagnostic {
    code: String,
    category: &'static str,
    severity: &'static str,
    message: String,
    locations: Vec<Location>,
    redirects: Vec<Redirect>,
    help: Option<String>,
}

#[derive(Debug, Serialize)]
struct Location {
    path: Option<String>,
    pointer: Option<String>,
    profile: Option<String>,
    label: Option<String>,
}

#[derive(Debug, Serialize)]
struct Redirect {
    from: String,
    to: String,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Change {
    path: String,
    scope: ChangeScope,
    action: ChangeOperation,
    ownership: Ownership,
    before_digest: Option<String>,
    after_digest: Option<String>,
    container_before_digest: Option<String>,
    container_after_digest: Option<String>,
    exterior_before_digest: Option<String>,
    exterior_after_digest: Option<String>,
    backup_path: Option<String>,
    accepted_generated_conflict: bool,
}

struct Outcome {
    command: &'static str,
    status: Status,
    exit_code: i32,
    changes: Vec<Change>,
    data: serde_json::Value,
}

struct DependencyPlan {
    state: DependencyState,
    records: Vec<DependencyRecord>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PriorThemeLock {
    html_integration: PriorHtmlIntegration,
    #[serde(default)]
    contract: Option<PriorContractIdentity>,
}

#[derive(Deserialize)]
struct PriorHtmlIntegration {
    mode: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PriorContractIdentity {
    canonical_digest: String,
    installed_bytes_digest: String,
}

pub fn run(cli: Cli) -> i32 {
    let json_mode = cli.json;
    let quiet = cli.quiet;
    match execute(&cli) {
        Ok(outcome) => {
            if json_mode {
                let envelope = Envelope {
                    schema_version: "1.0.0",
                    command: Some(outcome.command.into()),
                    status: outcome.status,
                    exit_code: outcome.exit_code,
                    diagnostics: Vec::new(),
                    changes: outcome.changes,
                    data: outcome.data,
                };
                match serde_json::to_string(&envelope) {
                    Ok(serialized) => println!("{serialized}"),
                    Err(error) => {
                        eprintln!("error: cannot serialize command output: {error}");
                        return 70;
                    }
                }
            } else if !quiet {
                print_human(&outcome);
            }
            outcome.exit_code
        }
        Err(error) => {
            let exit_code = error.exit_code();
            if json_mode {
                let envelope = Envelope {
                    schema_version: "1.0.0",
                    command: Some(command_name(&cli.command).into()),
                    status: if exit_code == 4 {
                        Status::Conflict
                    } else {
                        Status::Error
                    },
                    exit_code,
                    diagnostics: vec![Diagnostic {
                        code: format!("LUT{exit_code:04}"),
                        category: error_category(exit_code),
                        severity: "error",
                        message: error.to_string(),
                        locations: Vec::new(),
                        redirects: Vec::new(),
                        help: error_help(exit_code).map(str::to_owned),
                    }],
                    changes: Vec::new(),
                    data: serde_json::Value::Null,
                };
                match serde_json::to_string(&envelope) {
                    Ok(serialized) => println!("{serialized}"),
                    Err(serialization_error) => {
                        eprintln!("error: cannot serialize command failure: {serialization_error}");
                        return 70;
                    }
                }
            } else {
                eprintln!("error: {error}");
            }
            exit_code
        }
    }
}

pub fn run_from<I, T>(arguments: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
    let json_mode = arguments
        .iter()
        .filter_map(|argument| argument.to_str())
        .any(|argument| argument == "--json");
    match Cli::try_parse_from(arguments) {
        Ok(cli) => run(cli),
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            0
        }
        Err(error) if json_mode => {
            let envelope = Envelope {
                schema_version: "1.0.0",
                command: None,
                status: Status::Error,
                exit_code: 2,
                diagnostics: vec![Diagnostic {
                    code: "LUT2000".into(),
                    category: "usage",
                    severity: "error",
                    message: error.to_string(),
                    locations: Vec::new(),
                    redirects: Vec::new(),
                    help: Some("Run with --help to inspect the command syntax.".into()),
                }],
                changes: Vec::new(),
                data: serde_json::Value::Null,
            };
            match serde_json::to_string(&envelope) {
                Ok(value) => println!("{value}"),
                Err(_) => return 70,
            }
            2
        }
        Err(error) => {
            let _ = error.print();
            2
        }
    }
}

fn execute(cli: &Cli) -> Result<Outcome, CliError> {
    if let Command::Build(options) = &cli.command {
        let unique = options
            .accept_generated
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        if unique.len() != options.accept_generated.len() {
            return Err(CliError::Usage("duplicate --accept-generated path".into()));
        }
    }
    let cwd = std::fs::canonicalize(&cli.cwd).map_err(|source| CliError::Io {
        path: cli.cwd.clone(),
        source,
    })?;
    if !cwd.is_dir() {
        return Err(CliError::Usage(
            "--cwd must name an existing directory".into(),
        ));
    }
    let writes = match &cli.command {
        Command::Init(options) => !options.dry_run,
        Command::Build(options) => !options.dry_run,
        Command::Add { dry_run, .. } => !dry_run,
        Command::Check | Command::List | Command::Explain { .. } | Command::Doctor { .. } => false,
    };
    if cli.lock_wait_ms != 0 && !writes {
        return Err(CliError::Usage(
            "--lock-wait-ms is valid only for an applying write command".into(),
        ));
    }
    match &cli.command {
        Command::Init(options) => {
            let root = discover(&cwd, true)?;
            if options.dry_run {
                ensure_no_active_transaction(&root)?;
            }
            init(
                &root,
                options.dry_run,
                options.no_patch_index,
                cli.lock_wait_ms,
            )
        }
        Command::Build(options) => {
            let root = discover(&cwd, false)?;
            if options.dry_run {
                ensure_no_active_transaction(&root)?;
            }
            build_command(
                &root,
                options.dry_run,
                options.no_patch_index,
                cli.lock_wait_ms,
                &options.accept_generated,
            )
        }
        Command::Check => {
            let root = discover(&cwd, false)?;
            ensure_no_active_transaction(&root)?;
            check_command(&root)
        }
        Command::List => {
            let root = discover(&cwd, false)?;
            ensure_no_active_transaction(&root)?;
            list_command(&root)
        }
        Command::Explain {
            token_path,
            profile,
        } => {
            let root = discover(&cwd, false)?;
            ensure_no_active_transaction(&root)?;
            explain_command(&root, token_path, profile)
        }
        Command::Add {
            id,
            base,
            from_contract_defaults,
            dry_run,
        } => {
            let root = discover(&cwd, false)?;
            if *dry_run {
                ensure_no_active_transaction(&root)?;
            }
            add_command(
                &root,
                id,
                base.as_deref(),
                *from_contract_defaults,
                *dry_run,
                cli.lock_wait_ms,
            )
        }
        Command::Doctor { strict } => {
            debug_assert!(*strict);
            let root = discover(&cwd, false)?;
            ensure_no_active_transaction(&root)?;
            doctor_command(&root)
        }
    }
}

fn discover(start: &Path, init: bool) -> Result<PathBuf, CliError> {
    let mut matches = Vec::new();
    for ancestor in start.ancestors().take(256) {
        let config = ancestor.join(CONFIG_FILE);
        match std::fs::symlink_metadata(&config) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ThemeError::Security(format!(
                    "{CONFIG_FILE} cannot be a symbolic link"
                ))
                .into());
            }
            Ok(metadata) if metadata.is_file() => matches.push(ancestor.to_path_buf()),
            Ok(_) => {
                return Err(
                    ThemeError::Security(format!("{CONFIG_FILE} is not a regular file")).into(),
                );
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CliError::Io {
                    path: config,
                    source,
                });
            }
        }
        let git = ancestor.join(".git");
        match std::fs::symlink_metadata(&git) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ThemeError::Security(".git cannot be a symbolic link".into()).into());
            }
            Ok(metadata) if metadata.is_file() || metadata.is_dir() => break,
            Ok(_) => {
                return Err(
                    ThemeError::Security(".git has an unsupported file type".into()).into(),
                );
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(CliError::Io { path: git, source }),
        }
    }
    match matches.as_slice() {
        [root] => Ok(root.clone()),
        [] if init => Ok(start.to_path_buf()),
        [] => Err(ThemeError::Config(format!("no {CONFIG_FILE} found")).into()),
        _ => Err(ThemeError::Config(format!("multiple {CONFIG_FILE} files found")).into()),
    }
}

fn init(
    start: &Path,
    dry_run: bool,
    no_patch_index: bool,
    lock_wait_ms: u64,
) -> Result<Outcome, CliError> {
    let discovered = discover(start, true)?;
    let root = std::fs::canonicalize(&discovered).map_err(|source| CliError::Io {
        path: discovered,
        source,
    })?;
    if root.join(CONFIG_FILE).exists() {
        return Err(CliError::Conflict(format!("{CONFIG_FILE} already exists")));
    }
    let config = ProjectConfig::default();
    config.validate()?;
    let dependency_plan = dependency_plan(&root, &config, true)?;
    let resolver = starter_resolver();
    let starter_profiles = config
        .profiles
        .named
        .iter()
        .map(|profile| leptos_ui_theme_core::ResolvedProfile {
            id: profile.id.clone(),
            label: profile.label.clone(),
            color_scheme: profile.color_scheme,
            inputs: profile.inputs.clone(),
            values: Vec::new(),
            semantic_digest: String::new(),
        })
        .collect::<Vec<_>>();
    let files = [
        (CONFIG_FILE.into(), pretty_json(&config)?),
        ("tokens/theme.resolver.json".into(), pretty_json(&resolver)?),
        ("tokens/themes/light.tokens.json".into(), b"{}\n".to_vec()),
        ("tokens/themes/dark.tokens.json".into(), b"{}\n".to_vec()),
        (
            config.outputs.seeded.module.clone(),
            seeded_module().into_bytes(),
        ),
        (
            config.outputs.seeded.controller.clone(),
            seeded_controller(&config, &starter_profiles).into_bytes(),
        ),
        (
            config.outputs.seeded.scope.clone(),
            seeded_scope(&config).into_bytes(),
        ),
    ];
    for (path, _) in &files {
        if root.join(path).exists() {
            return Err(CliError::Conflict(format!("{path} already exists")));
        }
    }
    let mut artifacts = files
        .iter()
        .enumerate()
        .map(|(index, (path, bytes))| {
            if index < 4 {
                GeneratedArtifact::user_authored(path.clone(), bytes.clone())
            } else {
                GeneratedArtifact::seeded(path.clone(), bytes.clone())
            }
        })
        .collect::<Vec<_>>();
    let scratch = create_init_scratch()?;
    let generated = (|| -> Result<_, CliError> {
        for artifact in &artifacts {
            write_scratch_file(&scratch, &artifact.path, &artifact.bytes)?;
        }
        copy_init_inputs(&root, &scratch, &config)?;
        build_with_options(
            &scratch,
            CodegenBuildOptions {
                patch_index: !no_patch_index,
                dependency_state: dependency_plan.state,
                dependencies: dependency_plan.records.clone(),
                accept_generated: Vec::new(),
            },
        )
        .map_err(CliError::from)
    })();
    let cleanup = std::fs::remove_dir_all(&scratch);
    let generated = match (generated, cleanup) {
        (Err(error), _) => return Err(error),
        (Ok(_), Err(source)) => {
            return Err(CliError::Io {
                path: scratch,
                source,
            });
        }
        (Ok(generated), Ok(())) => generated,
    };
    for artifact in &generated.artifacts {
        let target = root.join(&artifact.path);
        if !target.exists() {
            continue;
        }
        let unchanged_user_input = artifact.ownership == Ownership::UserAuthored
            && std::fs::read(&target)
                .map(|bytes| bytes == artifact.bytes)
                .unwrap_or(false);
        if artifact.scope != ChangeScope::HtmlOwnedRegion && !unchanged_user_input {
            return Err(CliError::Conflict(format!(
                "{} already exists",
                artifact.path
            )));
        }
    }
    let html_snippet = no_patch_index.then(|| generated.bootstrap.html_snippet.clone());
    artifacts.extend(generated.artifacts);
    let plan = plan_artifacts(&root, &artifacts)?;
    let changed_paths = if dry_run {
        plan.changed_paths()
    } else {
        apply_artifacts_for_with_wait(
            &root,
            &artifacts,
            ApplyCommand::Init,
            Some(&config.outputs.lock),
            Duration::from_millis(lock_wait_ms),
        )?
    };
    let changes = plan
        .changes
        .iter()
        .filter(|change| changed_paths.contains(&change.path))
        .map(|change| change_from_plan(change, &BTreeMap::new()))
        .collect::<Vec<_>>();
    Ok(Outcome {
        command: "init",
        status: if dry_run {
            Status::Planned
        } else if dependency_plan.state == DependencyState::Pending {
            Status::Warning
        } else {
            Status::Success
        },
        exit_code: 0,
        changes,
        data: json!({
            "kind": "init",
            "planDigest": plan.digest,
            "dependencyPlan": dependency_plan.records,
            "cspSource": generated.bootstrap.csp_source,
            "htmlIntegrationMode": if no_patch_index { "manual" } else { "patched" },
            "htmlSnippet": html_snippet,
        }),
    })
}

fn create_init_scratch() -> Result<PathBuf, CliError> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for _ in 0..256 {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "leptos-ui-theme-init-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(CliError::Io { path, source }),
        }
    }
    Err(CliError::Conflict(
        "cannot allocate an initialization planning directory".into(),
    ))
}

fn write_scratch_file(root: &Path, relative: &str, bytes: &[u8]) -> Result<(), CliError> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CliError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(&path, bytes).map_err(|source| CliError::Io { path, source })
}

fn copy_init_inputs(root: &Path, scratch: &Path, config: &ProjectConfig) -> Result<(), CliError> {
    let verified = discover_kit(root, &config.kit, config.limits.clone())?;
    for source in [
        &verified.installation,
        &verified.contract_source,
        &verified.capability_source,
        &verified.stylesheet_source,
    ] {
        write_scratch_file(scratch, source.logical_path.as_str(), source.bytes.as_ref())?;
    }
    let capability_path = config
        .kit
        .lock_paths
        .iter()
        .find(|candidate| {
            let candidate_config = KitConfig {
                contract_path: config.kit.contract_path.clone(),
                lock_paths: vec![(*candidate).clone()],
            };
            discover_kit(root, &candidate_config, config.limits.clone()).is_ok()
        })
        .ok_or_else(|| ThemeError::Contract("valid kit capability path disappeared".into()))?;
    if verified.installation.logical_path.as_str() != capability_path {
        return Err(ThemeError::Contract(
            "selected installed kit capability path changed during initialization".into(),
        )
        .into());
    }
    let index_paths = config
        .html
        .index_path
        .iter()
        .chain(
            config
                .html
                .index_candidates
                .iter()
                .flat_map(|paths| paths.iter()),
        )
        .collect::<Vec<_>>();
    let loader = SourceLoader::new(root, config.limits.clone())?;
    for relative in index_paths {
        if root.join(relative).is_file() {
            let logical = LogicalPath::new(relative.clone())?;
            let source = loader.read_source(&logical, SourceRole::General)?;
            write_scratch_file(scratch, relative, source.bytes.as_ref())?;
        }
    }
    Ok(())
}

fn build_command(
    root: &Path,
    dry_run: bool,
    no_patch_index: bool,
    lock_wait_ms: u64,
    accept_generated: &[String],
) -> Result<Outcome, CliError> {
    let config: ProjectConfig = read_json(&root.join(CONFIG_FILE))?;
    let compiler = ThemeCompiler::load(root)?;
    let contract = contract_identity(&compiler)?;
    let dependency_plan = dependency_plan(root, &config, false)?;
    let result = build_with_options(
        root,
        CodegenBuildOptions {
            patch_index: !no_patch_index,
            dependency_state: dependency_plan.state,
            dependencies: dependency_plan.records.clone(),
            accept_generated: accept_generated.to_vec(),
        },
    )?;
    let stale = check(root, &result);
    let changed_paths = if dry_run {
        stale.clone()
    } else {
        apply_with_wait(root, &result, Duration::from_millis(lock_wait_ms))?
    };
    let changes = result
        .plan
        .changes
        .iter()
        .filter(|change| changed_paths.contains(&change.path))
        .filter(|change| {
            !result
                .accepted_generated
                .values()
                .any(|backup| backup == &change.path)
        })
        .map(|change| change_from_plan(change, &result.accepted_generated))
        .collect::<Vec<_>>();
    let status = if dry_run && !changes.is_empty() {
        Status::Planned
    } else if changes.is_empty() {
        Status::NoChange
    } else {
        Status::Success
    };
    let artifacts = artifact_summaries(&result);
    if dry_run {
        revalidate_build_result(root, &result)?;
    }
    Ok(Outcome {
        command: "build",
        status,
        exit_code: 0,
        changes,
        data: json!({
            "kind": "build",
            "planDigest": result.plan.digest,
            "contract": contract,
            "artifacts": artifacts,
            "cspSource": result.bootstrap.csp_source,
            "htmlIntegrationMode": if no_patch_index { "manual" } else { "patched" },
            "htmlSnippet": no_patch_index.then_some(result.bootstrap.html_snippet),
        }),
    })
}

fn check_command(root: &Path) -> Result<Outcome, CliError> {
    let config: ProjectConfig = read_json(&root.join(CONFIG_FILE))?;
    let compiler = ThemeCompiler::load(root)?;
    let contract = contract_identity(&compiler)?;
    let dependency_plan = dependency_plan(root, &config, true)?;
    let dependencies_resolved = dependency_plan.state == DependencyState::Resolved;
    let patch_index = prior_patch_index(root, &config.outputs.lock)?;
    let result = build_with_options(
        root,
        CodegenBuildOptions {
            patch_index,
            dependency_state: dependency_plan.state,
            dependencies: dependency_plan.records,
            accept_generated: Vec::new(),
        },
    )?;
    let stale = check(root, &result);
    let fresh = stale.is_empty() && dependencies_resolved;
    let artifacts = artifact_summaries(&result);
    revalidate_build_result(root, &result)?;
    Ok(Outcome {
        command: "check",
        status: if fresh {
            Status::Success
        } else {
            Status::Error
        },
        exit_code: if fresh { 0 } else { 7 },
        changes: Vec::new(),
        data: json!({
            "kind": "check",
            "planDigest": result.plan.digest,
            "contract": contract,
            "artifacts": artifacts,
            "cspSource": result.bootstrap.csp_source,
            "htmlIntegrationMode": if patch_index { "patched" } else { "manual" },
            "htmlSnippet": (!patch_index).then_some(result.bootstrap.html_snippet),
            "stale": !fresh,
        }),
    })
}

fn list_command(root: &Path) -> Result<Outcome, CliError> {
    let compiler = ThemeCompiler::load(root)?;
    let profiles = compiler.resolve()?;
    let contract = contract_identity(&compiler)?;
    let domains = compiler
        .contract
        .tokens
        .iter()
        .map(|mapping| mapping.domain)
        .collect::<std::collections::BTreeSet<_>>();
    let axes = compiler
        .config
        .axes
        .as_ref()
        .map(|axes| {
            [
                ("density", axes.density.as_ref()),
                ("motion", axes.motion.as_ref()),
                ("contrast", axes.contrast.as_ref()),
            ]
            .into_iter()
            .filter_map(|(name, axis)| {
                axis.map(|axis| {
                    json!({
                        "axis": name,
                        "attribute": axis.attribute,
                        "defaultContext": axis.default_context,
                        "contexts": axis.contexts,
                        "systemContext": axis.system.as_ref().map(|system| &system.context),
                    })
                })
            })
            .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let token_root = LogicalPath::new(compiler.config.token_root.clone())?;
    let mut sources = compiler
        .source_loader()
        .read_tree(&token_root, SourceRole::TokenResolver)?;
    let resolver = LogicalPath::new(compiler.config.resolver.clone())?;
    if !sources.iter().any(|source| source.logical_path == resolver) {
        sources.push(
            compiler
                .source_loader()
                .read_source(&resolver, SourceRole::TokenResolver)?,
        );
    }
    sources.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    let sources = sources
        .iter()
        .map(|source| {
            json!({
                "path": source.logical_path.as_str(),
                "kind": if source.logical_path == resolver { "resolver" } else { "token" },
                "status": "valid",
                "digest": source.bytes_digest,
            })
        })
        .collect::<Vec<_>>();
    revalidate_compiler_sources(&compiler)?;
    Ok(Outcome {
        command: "list",
        status: Status::Success,
        exit_code: 0,
        changes: Vec::new(),
        data: json!({
            "kind": "list",
            "contract": contract,
            "profiles": profiles.iter().map(|profile| {
                json!({
                    "id": profile.id,
                    "label": profile.label,
                    "colorScheme": profile.color_scheme,
                    "sourceStatus": "valid",
                })
            }).collect::<Vec<_>>(),
            "axes": axes,
            "domains": domains,
            "sources": sources,
        }),
    })
}

fn explain_command(root: &Path, token_path: &str, profile: &str) -> Result<Outcome, CliError> {
    let compiler = ThemeCompiler::load(root)?;
    let resolved = compiler.resolve_one(profile)?;
    let _ = compiler.config.profile(profile)?;
    let requested_mapping = compiler
        .contract
        .tokens
        .iter()
        .find(|mapping| mapping.path == token_path)
        .ok_or_else(|| ThemeError::Resolution(format!("unknown token `{token_path}`")))?;
    let terminal = compiler.contract.terminal_mapping(token_path)?;
    let token = resolved
        .values
        .iter()
        .find(|token| token.path == terminal.path)
        .ok_or_else(|| ThemeError::Resolution(format!("unknown token `{token_path}`")))?;
    let mut redirects = Vec::new();
    let mut current = requested_mapping;
    while let Some(deprecation) = &current.deprecation {
        redirects.push(json!({
            "from": current.path,
            "to": deprecation.replacement,
            "message": deprecation.message,
        }));
        current = compiler
            .contract
            .tokens
            .iter()
            .find(|mapping| mapping.path == deprecation.replacement)
            .ok_or_else(|| {
                ThemeError::Contract(format!(
                    "deprecated token `{}` has no replacement mapping",
                    current.path
                ))
            })?;
    }
    revalidate_compiler_sources(&compiler)?;
    Ok(Outcome {
        command: "explain",
        status: Status::Success,
        exit_code: 0,
        changes: Vec::new(),
        data: json!({
            "kind": "explain",
            "tokenPath": token_path,
            "terminalTokenPath": terminal.path,
            "profileId": profile,
            "status": "resolved",
            "type": token.token_type,
            "value": token.value,
            "absenceReason": null,
            "redirects": redirects,
            "provenance": token.provenance,
        }),
    })
}

fn doctor_command(root: &Path) -> Result<Outcome, CliError> {
    let config: ProjectConfig = read_json(&root.join(CONFIG_FILE))?;
    let compiler = ThemeCompiler::load(root)?;
    let contract = contract_identity(&compiler)?;
    let dependency_plan = dependency_plan(root, &config, true)?;
    let patch_index = prior_patch_index(root, &config.outputs.lock)?;
    let result = build_with_options(
        root,
        CodegenBuildOptions {
            patch_index,
            dependency_state: dependency_plan.state,
            dependencies: dependency_plan.records.clone(),
            accept_generated: Vec::new(),
        },
    )?;
    let stale = check(root, &result);
    let dependencies_resolved = dependency_plan.state == DependencyState::Resolved;
    let fresh = stale.is_empty();
    let runtime = inspect_runtime_evidence(root, &config, &compiler)?;
    let healthy = fresh && dependencies_resolved && (!runtime.required || runtime.qualified);
    let checks = vec![
        doctor_check(
            "app-shape",
            true,
            "pass",
            "project root and configuration are readable",
            None,
        ),
        doctor_check(
            "kit-identity",
            true,
            "pass",
            "installed kit identity is compatible",
            None,
        ),
        doctor_check(
            "kit-hashes",
            true,
            "pass",
            "installed kit artifacts match their declared hashes",
            None,
        ),
        doctor_check(
            "kit-stylesheet",
            true,
            "pass",
            "kit stylesheet and layer ABI are compatible",
            None,
        ),
        doctor_check(
            "config-schema",
            true,
            "pass",
            "project configuration matches the supported schema",
            None,
        ),
        doctor_check(
            "reference-security",
            true,
            "pass",
            "all token and resolver references remain contained",
            None,
        ),
        doctor_check(
            "resource-limits",
            true,
            "pass",
            "configured inputs and outputs are within resource limits",
            None,
        ),
        doctor_check(
            "output-freshness",
            true,
            if fresh { "pass" } else { "fail" },
            if fresh {
                "generated outputs are fresh"
            } else {
                "generated outputs are stale"
            },
            None,
        ),
        doctor_check(
            "lock-freshness",
            true,
            if fresh { "pass" } else { "fail" },
            if fresh {
                "theme lock matches current inputs"
            } else {
                "theme lock requires regeneration"
            },
            None,
        ),
        doctor_check(
            "html-integration",
            true,
            if fresh { "pass" } else { "fail" },
            if fresh {
                "HTML integration matches its ownership mode"
            } else {
                "HTML integration is missing or stale"
            },
            None,
        ),
        doctor_check(
            "dependency-plan",
            true,
            if dependencies_resolved {
                "pass"
            } else {
                "fail"
            },
            if dependencies_resolved {
                "generated runtime dependencies are declared and resolved"
            } else {
                "generated runtime dependencies are pending"
            },
            None,
        ),
        doctor_check(
            "ownership",
            true,
            "pass",
            "generated and user-authored ownership boundaries are valid",
            None,
        ),
        doctor_check(
            "accessibility",
            true,
            "pass",
            "configured contrast requirements pass",
            None,
        ),
        doctor_check(
            "presence-abi",
            true,
            "pass",
            "presence ABI is compatible",
            None,
        ),
        doctor_check(
            "portal-abi",
            true,
            "pass",
            "direct-body portal mount capability is compatible",
            None,
        ),
        doctor_check(
            "runtime-evidence",
            runtime.required,
            if runtime.qualified {
                "pass"
            } else if runtime.required {
                "fail"
            } else {
                "not-qualified"
            },
            if runtime.qualified {
                "bound browser and host runtime evidence passes"
            } else if runtime.required {
                "required runtime evidence is missing, stale, or failed"
            } else {
                "runtime evidence is not installed; runtime is not qualified"
            },
            runtime.digest,
        ),
    ];
    revalidate_build_result(root, &result)?;
    Ok(Outcome {
        command: "doctor",
        status: if healthy {
            Status::Success
        } else {
            Status::Error
        },
        exit_code: if healthy { 0 } else { 7 },
        changes: Vec::new(),
        data: json!({
            "kind": "doctor",
            "strict": true,
            "contract": contract,
            "checks": checks,
            "runtimeQualification": if runtime.qualified {
                "qualified"
            } else if runtime.required {
                "failed"
            } else {
                "not-qualified"
            },
        }),
    })
}

struct RuntimeEvidenceState {
    required: bool,
    qualified: bool,
    digest: Option<String>,
}

fn inspect_runtime_evidence(
    root: &Path,
    config: &ProjectConfig,
    compiler: &ThemeCompiler,
) -> Result<RuntimeEvidenceState, CliError> {
    let Some(evidence) = &config.runtime_evidence else {
        return Ok(RuntimeEvidenceState {
            required: false,
            qualified: false,
            digest: None,
        });
    };
    let path = root.join(&evidence.path);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RuntimeEvidenceState {
                required: evidence.required,
                qualified: false,
                digest: None,
            });
        }
        Err(source) => return Err(CliError::Io { path, source }),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ThemeError::Security(format!(
            "runtime evidence is not a regular file: {}",
            evidence.path
        ))
        .into());
    }
    let bytes = std::fs::read(&path).map_err(|source| CliError::Io { path, source })?;
    if bytes.len() as u64 > config.limits.file_bytes {
        return Err(ThemeError::Limit {
            resource: "fileBytes",
            limit: config.limits.file_bytes,
            observed: bytes.len() as u64,
        }
        .into());
    }
    let digest = Some(format!("sha256:{}", leptos_ui_theme_core::sha256(&bytes)));
    let value = leptos_ui_theme_core::parse_json_strict(&bytes, config.limits.json_depth);
    let qualified = value.is_ok_and(|value| {
        let browsers = value["browsers"].as_array();
        value["status"] == "pass"
            && value["contract"]["canonicalDigest"] == compiler.contract.canonical_digest
            && browsers.is_some_and(|browsers| {
                browsers.len() == 3 && browsers.iter().all(|browser| browser["result"] == "pass")
            })
            && value["hostImage"]["result"] == "pass"
    });
    Ok(RuntimeEvidenceState {
        required: evidence.required,
        qualified,
        digest,
    })
}

fn add_command(
    root: &Path,
    id: &str,
    base: Option<&str>,
    from_defaults: bool,
    dry_run: bool,
    lock_wait_ms: u64,
) -> Result<Outcome, CliError> {
    leptos_ui_theme_core::validate_theme_id(id)?;
    let config_path = root.join(CONFIG_FILE);
    let mut config: ProjectConfig = read_json(&config_path)?;
    if config.profiles.named.iter().any(|profile| profile.id == id) {
        return Err(CliError::Conflict(format!("profile `{id}` already exists")));
    }
    let resolver_path = root.join(&config.resolver);
    let resolver_bytes = std::fs::read(&resolver_path).map_err(|source| CliError::Io {
        path: resolver_path,
        source,
    })?;
    let mut resolver =
        leptos_ui_theme_core::parse_json_strict(&resolver_bytes, config.limits.json_depth)?;
    let source_path = format!("{}/themes/{id}.tokens.json", config.token_root);
    let source_target = root.join(&source_path);
    match std::fs::symlink_metadata(&source_target) {
        Ok(_) => return Err(CliError::Conflict(format!("{source_path} already exists"))),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(CliError::Io {
                path: source_target,
                source,
            });
        }
    }
    let source_parent = source_target
        .parent()
        .ok_or_else(|| ThemeError::Security(source_path.clone()))?;
    let parent_metadata =
        std::fs::symlink_metadata(source_parent).map_err(|source| CliError::Io {
            path: source_parent.to_path_buf(),
            source,
        })?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(ThemeError::Security(format!(
            "theme source parent is not a real directory: {}",
            source_parent.display()
        ))
        .into());
    }
    let (profile, sources) = if let Some(base) = base {
        let base_profile = config.profile(base)?.clone();
        let mut profile = base_profile;
        profile.id = id.into();
        profile.label = None;
        profile.inputs.insert("theme".into(), id.into());
        let sources = resolver["modifiers"]["theme"]["contexts"][base]
            .as_array()
            .cloned()
            .ok_or_else(|| ThemeError::Resolution(format!("base context `{base}` is missing")))?;
        (profile, sources)
    } else {
        debug_assert!(from_defaults);
        (
            Profile {
                id: id.into(),
                label: None,
                color_scheme: leptos_ui_theme_core::ColorScheme::Light,
                inputs: [("theme".into(), id.into())].into_iter().collect(),
            },
            Vec::new(),
        )
    };
    let resolver_dir = Path::new(&config.resolver)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let source_relative = Path::new(&source_path)
        .strip_prefix(resolver_dir)
        .map_err(|_| ThemeError::Security(source_path.clone()))?
        .to_string_lossy()
        .into_owned();
    let mut sources = sources;
    sources.push(json!({"$ref": source_relative}));
    resolver["modifiers"]["theme"]["contexts"][id] = serde_json::Value::Array(sources);
    config.profiles.named.push(profile);
    config.validate()?;
    let files = [
        (CONFIG_FILE.into(), pretty_json(&config)?),
        (config.resolver.clone(), pretty_json(&resolver)?),
        (source_path.clone(), b"{}\n".to_vec()),
    ];
    let artifacts = files
        .iter()
        .map(|(path, bytes)| GeneratedArtifact::user_authored(path.clone(), bytes.clone()))
        .collect::<Vec<_>>();
    let plan = plan_artifacts(root, &artifacts)?;
    let changes = plan
        .changes
        .iter()
        .map(|change| change_from_plan(change, &BTreeMap::new()))
        .collect();
    if !dry_run {
        apply_artifacts_for_with_wait(
            root,
            &artifacts,
            ApplyCommand::Add,
            None,
            Duration::from_millis(lock_wait_ms),
        )?;
    }
    Ok(Outcome {
        command: "add",
        status: if dry_run {
            Status::Planned
        } else {
            Status::Success
        },
        exit_code: 0,
        changes,
        data: json!({
            "kind": "add",
            "planDigest": plan.digest,
            "profileId": id,
            "sourcePath": source_path,
            "configPath": CONFIG_FILE,
            "resolverPath": config.resolver,
        }),
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CliError> {
    let bytes = std::fs::read(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(CliError::Json)
}

fn prior_patch_index(root: &Path, lock_path: &str) -> Result<bool, CliError> {
    let path = root.join(lock_path);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(source) => return Err(CliError::Io { path, source }),
    };
    let lock: PriorThemeLock = serde_json::from_slice(&bytes)?;
    match lock.html_integration.mode.as_str() {
        "patched" => Ok(true),
        "manual" => Ok(false),
        mode => Err(ThemeError::Config(format!(
            "theme lock has unsupported HTML integration mode `{mode}`"
        ))
        .into()),
    }
}

fn pretty_json(value: &impl Serialize) -> Result<Vec<u8>, CliError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn dependency_plan(
    root: &Path,
    config: &ProjectConfig,
    allow_pending: bool,
) -> Result<DependencyPlan, CliError> {
    let kit = discover_kit(root, &config.kit, config.limits.clone())?;
    let mut records = default_dependency_records();
    let manifest_path = root.join("Cargo.toml");
    let lock_path = root.join("Cargo.lock");
    let manifest = read_optional_toml(&manifest_path)?;
    let Some(manifest) = manifest else {
        if allow_pending {
            return Ok(DependencyPlan {
                state: DependencyState::Pending,
                records,
            });
        }
        return Err(ThemeError::Config(
            "Cargo.toml is required to validate generated runtime dependencies".into(),
        )
        .into());
    };
    let leptos_features =
        validate_dependency_declaration(&manifest, "leptos", "=0.9.0-alpha", &[])?;
    let primitives_features = validate_dependency_declaration(
        &manifest,
        "web_ui_primitives",
        ">=0.2.0,<0.3.0",
        &["core", "leptos"],
    )?;
    let (Some(leptos_features), Some(primitives_features)) = (leptos_features, primitives_features)
    else {
        if allow_pending {
            return Ok(DependencyPlan {
                state: DependencyState::Pending,
                records,
            });
        }
        return Err(ThemeError::Config(
            "Cargo.toml is missing generated runtime dependency declarations".into(),
        )
        .into());
    };
    let leptos_mode = selected_render_mode(&leptos_features);
    let primitives_mode = selected_render_mode(&primitives_features);
    if leptos_mode != primitives_mode {
        return Err(ThemeError::Config(
            "leptos and web_ui_primitives must select the same render mode".into(),
        )
        .into());
    }
    records[0].features = leptos_features;
    records[1].features = primitives_features;
    let Some(lock) = read_optional_toml(&lock_path)? else {
        if allow_pending {
            return Ok(DependencyPlan {
                state: DependencyState::Pending,
                records,
            });
        }
        return Err(ThemeError::Config(
            "Cargo.lock is required to validate generated runtime dependencies".into(),
        )
        .into());
    };
    let (leptos_version, leptos_checksum) =
        resolved_registry_package(&lock, "leptos", "0.9.0-alpha")?;
    let primitives_expected = &kit.capability.primitives;
    let (primitives_version, primitives_checksum) =
        resolved_registry_package(&lock, "web_ui_primitives", &primitives_expected.version)?;
    if primitives_checksum != primitives_expected.checksum {
        return Err(ThemeError::Contract(
            "resolved web_ui_primitives checksum differs from the kit capability".into(),
        )
        .into());
    }
    records[0].resolved_version = Some(leptos_version);
    records[0].checksum = Some(leptos_checksum);
    records[1].resolved_version = Some(primitives_version);
    records[1].checksum = Some(primitives_checksum);
    Ok(DependencyPlan {
        state: DependencyState::Resolved,
        records,
    })
}

fn read_optional_toml(path: &Path) -> Result<Option<toml::Value>, CliError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CliError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| ThemeError::Config(format!("{} is not UTF-8", path.display())))?;
    toml::from_str(text)
        .map(Some)
        .map_err(|source| CliError::Toml {
            path: path.to_path_buf(),
            source,
        })
}

fn validate_dependency_declaration(
    manifest: &toml::Value,
    package: &str,
    requirement: &str,
    expected_features: &[&str],
) -> Result<Option<Vec<String>>, CliError> {
    let Some(entry) = manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .and_then(|dependencies| dependencies.get(package))
    else {
        return Ok(None);
    };
    let Some(table) = entry.as_table() else {
        return Err(ThemeError::Config(format!(
            "dependency `{package}` must use the supported table form"
        ))
        .into());
    };
    let keys = table.keys().map(String::as_str).collect::<Vec<_>>();
    if table.len() != 3
        || !keys.contains(&"version")
        || !keys.contains(&"default-features")
        || !keys.contains(&"features")
        || table.get("version").and_then(toml::Value::as_str) != Some(requirement)
        || table.get("default-features").and_then(toml::Value::as_bool) != Some(false)
    {
        return Err(ThemeError::Config(format!(
            "dependency `{package}` differs from the generated runtime requirement"
        ))
        .into());
    }
    let features = table
        .get("features")
        .and_then(toml::Value::as_array)
        .expect("validated dependency features");
    let Some(features) = features
        .iter()
        .map(toml::Value::as_str)
        .collect::<Option<Vec<_>>>()
    else {
        return Err(
            ThemeError::Config(format!("dependency `{package}` features must be strings")).into(),
        );
    };
    let actual = features
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let required = expected_features
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let delivery = ["csr", "hydrate", "ssr"]
        .into_iter()
        .filter(|feature| actual.contains(feature))
        .collect::<Vec<_>>();
    if features.len() != actual.len()
        || !required.is_subset(&actual)
        || delivery.len() > 1
        || actual.len() != required.len() + delivery.len()
    {
        return Err(ThemeError::Config(format!(
            "dependency `{package}` differs from the generated runtime requirement"
        ))
        .into());
    }
    let mut features = actual.into_iter().map(str::to_owned).collect::<Vec<_>>();
    features.sort();
    Ok(Some(features))
}

fn selected_render_mode(features: &[String]) -> Option<&str> {
    features
        .iter()
        .find(|feature| matches!(feature.as_str(), "csr" | "hydrate" | "ssr"))
        .map(String::as_str)
}

fn resolved_registry_package(
    lock: &toml::Value,
    package: &str,
    expected_version: &str,
) -> Result<(String, String), CliError> {
    let matches = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
        .filter(|record| {
            record.get("name").and_then(toml::Value::as_str) == Some(package)
                && record.get("version").and_then(toml::Value::as_str) == Some(expected_version)
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(ThemeError::Config(format!(
            "Cargo.lock must contain exactly one `{package}` {expected_version} registry package"
        ))
        .into());
    };
    let source = record
        .get("source")
        .and_then(toml::Value::as_str)
        .filter(|source| source.starts_with("registry+"))
        .ok_or_else(|| {
            ThemeError::Config(format!(
                "resolved dependency `{package}` must come from a registry"
            ))
        })?;
    let _ = source;
    let checksum = record
        .get("checksum")
        .and_then(toml::Value::as_str)
        .filter(|checksum| !checksum.is_empty())
        .ok_or_else(|| {
            ThemeError::Config(format!(
                "resolved dependency `{package}` has no registry checksum"
            ))
        })?;
    Ok((expected_version.to_owned(), checksum.to_owned()))
}

fn contract_identity(compiler: &ThemeCompiler) -> Result<serde_json::Value, CliError> {
    let capability: leptos_ui_theme_core::KitCapability =
        serde_json::from_slice(&compiler.kit_capability.bytes)?;
    let previous = read_prior_theme_lock(
        &compiler.root,
        &compiler.config.outputs.lock,
        compiler.config.limits.file_bytes,
    )?;
    let compatibility = compiler.contract.compatibility_result(
        previous
            .as_ref()
            .and_then(|lock| lock.contract.as_ref())
            .map(|contract| contract.canonical_digest.as_str()),
        previous
            .as_ref()
            .and_then(|lock| lock.contract.as_ref())
            .map(|contract| contract.installed_bytes_digest.as_str()),
        &capability.contract.installed_bytes_digest,
    )?;
    Ok(json!({
        "contractId": compiler.contract.contract_id,
        "abiVersion": compiler.contract.abi_version,
        "revision": compiler.contract.revision,
        "layerAbiVersion": capability.layer_abi.version,
        "presenceAbiVersion": capability.primitives.presence_abi,
        "portalAbiVersion": capability.portal_abi.version,
        "canonicalDigest": compiler.contract.canonical_digest,
        "installedBytesDigest": capability.contract.installed_bytes_digest,
        "stylesheetBytesDigest": capability.stylesheet.installed_bytes_digest,
        "stylesheetPath": capability.stylesheet.path,
        "compatibility": compatibility,
    }))
}

fn read_prior_theme_lock(
    root: &Path,
    relative: &str,
    file_limit: u64,
) -> Result<Option<PriorThemeLock>, CliError> {
    let path = root.join(relative);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CliError::Io { path, source }),
    };
    if bytes.len() as u64 > file_limit {
        return Err(ThemeError::Limit {
            resource: "fileBytes",
            limit: file_limit,
            observed: bytes.len() as u64,
        }
        .into());
    }
    serde_json::from_slice(&bytes).map(Some).map_err(Into::into)
}

fn revalidate_compiler_sources(compiler: &ThemeCompiler) -> Result<(), CliError> {
    for expected in [
        &compiler.config_source,
        &compiler.kit_installation,
        &compiler.kit_capability,
        &compiler.contract_source,
        &compiler.kit_stylesheet,
    ] {
        let current = compiler
            .source_loader()
            .read_source(&expected.logical_path, SourceRole::General)?;
        if current.bytes_digest != expected.bytes_digest {
            return Err(CliError::Conflict(format!(
                "consumed input `{}` changed during read-only evaluation",
                expected.logical_path.as_str()
            )));
        }
    }
    let token_root = LogicalPath::new(compiler.config.token_root.clone())?;
    let _ = compiler
        .source_loader()
        .read_tree(&token_root, SourceRole::TokenResolver)?;
    Ok(())
}

fn artifact_summaries(result: &leptos_ui_theme_codegen::BuildResult) -> Vec<serde_json::Value> {
    let mut artifacts = result
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact.ownership == Ownership::GeneratedLockOwned
                && artifact.scope == ChangeScope::WholeFile
                && !artifact.path.starts_with(".leptos-ui-theme/backups/")
        })
        .map(|artifact| {
            json!({
                "path": artifact.path,
                "ownership": artifact.ownership,
                "digest": format!(
                    "sha256:{}",
                    leptos_ui_theme_core::sha256(&artifact.bytes)
                ),
            })
        })
        .collect::<Vec<_>>();
    artifacts.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap_or_default()
            .as_bytes()
            .cmp(right["path"].as_str().unwrap_or_default().as_bytes())
    });
    artifacts
}

fn doctor_check(
    id: &str,
    required: bool,
    status: &str,
    summary: &str,
    evidence_digest: Option<String>,
) -> serde_json::Value {
    json!({
        "id": id,
        "required": required,
        "status": status,
        "summary": summary,
        "evidenceDigest": evidence_digest,
    })
}

fn change_from_plan(
    change: &PlannedChange,
    accepted_generated: &BTreeMap<String, String>,
) -> Change {
    let backup_path = accepted_generated.get(&change.path).cloned();
    Change {
        path: change.path.clone(),
        scope: change.scope,
        action: change.operation,
        ownership: change.ownership,
        before_digest: change.before_digest.clone(),
        after_digest: change.after_digest.clone(),
        container_before_digest: change.container_before_digest.clone(),
        container_after_digest: change.container_after_digest.clone(),
        exterior_before_digest: change.exterior_before_digest.clone(),
        exterior_after_digest: change.exterior_after_digest.clone(),
        accepted_generated_conflict: backup_path.is_some(),
        backup_path,
    }
}

fn error_category(exit_code: i32) -> &'static str {
    match exit_code {
        2 => "usage",
        3 => "validation",
        4 => "conflict",
        5 => "security",
        6 => "contract",
        7 => "check",
        _ => "internal",
    }
}

fn error_help(exit_code: i32) -> Option<&'static str> {
    match exit_code {
        3 => Some("Correct the project configuration or token sources and retry."),
        4 => Some("Resolve local edits or the active transaction and retry."),
        5 => Some("Use regular files and paths contained by the project root."),
        6 => Some("Install a compatible leptos_ui_kit contract and lock."),
        7 => Some("Run build, then repeat the check."),
        _ => None,
    }
}

fn print_human(outcome: &Outcome) {
    match outcome.status {
        Status::NoChange => println!("{}: no changes", outcome.command),
        Status::Planned => println!(
            "{}: {} change(s) planned",
            outcome.command,
            outcome.changes.len()
        ),
        Status::Success => println!("{}: success", outcome.command),
        Status::Warning => println!("{}: completed with warnings", outcome.command),
        Status::Conflict => println!("{}: conflict", outcome.command),
        Status::Error => println!("{}: checks failed", outcome.command),
    }
    for change in &outcome.changes {
        println!("  {:?} {}", change.action, change.path);
    }
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Init(_) => "init",
        Command::Build(_) => "build",
        Command::Check => "check",
        Command::List => "list",
        Command::Explain { .. } => "explain",
        Command::Add { .. } => "add",
        Command::Doctor { .. } => "doctor",
    }
}

fn starter_resolver() -> serde_json::Value {
    json!({
        "$schema": "https://www.designtokens.org/schemas/2025.10/resolver.json",
        "name": "Application theme resolver",
        "version": "2025.10",
        "modifiers": {
            "theme": {
                "description": "Named visual theme",
                "contexts": {
                    "light": [{"$ref": "themes/light.tokens.json"}],
                    "dark": [{"$ref": "themes/dark.tokens.json"}]
                },
                "default": "light"
            }
        },
        "resolutionOrder": [{"$ref": "#/modifiers/theme"}]
    })
}

#[cfg(test)]
mod tests {
    use super::{
        add_command, build_command, check_command, doctor_command, explain_command, init,
        list_command, starter_resolver,
    };
    use std::path::PathBuf;

    #[test]
    fn cli_schema_assets_have_immutable_identities() {
        for (bytes, identity) in [
            (
                super::DIAGNOSTIC_ENVELOPE_SCHEMA_JSON,
                super::DIAGNOSTIC_ENVELOPE_SCHEMA,
            ),
            (
                super::RUNTIME_QUALIFICATION_RESULT_SCHEMA_JSON,
                super::RUNTIME_QUALIFICATION_RESULT_SCHEMA,
            ),
        ] {
            let schema: serde_json::Value = serde_json::from_str(bytes).unwrap();
            assert_eq!(schema["$id"], identity);
            assert_eq!(schema["additionalProperties"], false);
        }
    }

    #[test]
    fn starter_resolver_has_theme_modifier() {
        assert!(starter_resolver()["modifiers"]["theme"].is_object());
    }

    #[test]
    fn init_and_build_generate_a_complete_theme() {
        let root = temporary_root("init-build");
        if root.exists() {
            std::fs::remove_dir_all(&root).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "theme-app"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "=0.9.0-alpha", default-features = false, features = ["csr"] }
web_ui_primitives = { version = ">=0.2.0,<0.3.0", default-features = false, features = ["core", "csr", "leptos"] }
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("Cargo.lock"),
            r#"version = 4

[[package]]
name = "leptos"
version = "0.9.0-alpha"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "leptos-checksum"

[[package]]
name = "web_ui_primitives"
version = "0.2.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "sha256:test"
"#,
        )
        .unwrap();
        let contract_path = root.join("src/components/ui/_kit/token-contract.json");
        std::fs::create_dir_all(contract_path.parent().unwrap()).unwrap();
        let mut contract = serde_json::json!({
            "$schema": "https://triesap.github.io/leptos_ui_kit/schema/0.2.0/token-contract.schema.json",
            "schemaVersion": "1.0.0",
            "contractId": "leptos-ui-kit",
            "abiVersion": 1,
            "revision": 2,
            "dtcgVersion": "2025.10",
            "dtcgProfile": "format+color+resolver:2025.10",
            "canonicalDigest": "",
            "tokens": [{
                "path": "color.surface",
                "type": "color",
                "cssCustomProperty": "--kit-color-surface",
                "domain": "theme",
                "required": true,
                "order": 0,
                "themeOverride": true,
                "default": "#ffffff"
            }],
            "contrastChecks": []
        });
        let digest = leptos_ui_theme_core::canonical_contract_digest(&contract).unwrap();
        contract["canonicalDigest"] = format!("sha256:{digest}").into();
        let contract_bytes = serde_json::to_vec_pretty(&contract).unwrap();
        std::fs::write(&contract_path, &contract_bytes).unwrap();
        let stylesheet_path = root.join("styles/kit.css");
        std::fs::create_dir_all(stylesheet_path.parent().unwrap()).unwrap();
        let stylesheet_bytes =
            b"@layer leptos-ui-kit.tokens, leptos-ui-kit.themes, leptos-ui-kit.components;\n";
        std::fs::write(&stylesheet_path, stylesheet_bytes).unwrap();
        let contract_bytes_digest =
            format!("sha256:{}", leptos_ui_theme_core::sha256(&contract_bytes));
        let stylesheet_bytes_digest =
            format!("sha256:{}", leptos_ui_theme_core::sha256(stylesheet_bytes));
        let capability_path = root.join("src/components/ui/_kit/theme-integration.json");
        let capability = serde_json::json!({
            "$schema": "https://triesap.github.io/leptos_ui_kit/schema/0.2.0/theme-integration.schema.json",
            "schemaVersion": "1.0.0",
            "producer": {"package": "leptos_ui_kit_cli", "version": "0.2.0", "repository": "https://github.com/triesap/leptos_ui_kit", "checksum": null},
            "primitives": {"package": "web_ui_primitives", "requirement": ">=0.2.0,<0.3.0", "version": "0.2.0", "checksum": "sha256:test", "presenceAbi": 2},
            "contract": {"path": "token-contract.json", "contractId": "leptos-ui-kit", "abiVersion": 1, "revision": 2, "canonicalDigest": contract["canonicalDigest"], "installedBytesDigest": contract_bytes_digest},
            "stylesheet": {"path": "styles/kit.css", "installedBytesDigest": stylesheet_bytes_digest},
            "layerAbi": {"version": 1, "order": ["leptos-ui-kit.tokens", "leptos-ui-kit.themes", "leptos-ui-kit.components"]},
            "portalAbi": {"version": 1, "mountType": "web_ui_primitives::leptos::PortalMount", "bodyHost": true}
        });
        let capability_bytes = serde_json::to_vec_pretty(&capability).unwrap();
        std::fs::write(&capability_path, &capability_bytes).unwrap();
        let installed_capability = serde_json::json!({
            "schemaVersion": leptos_ui_theme_core::KIT_LOCK_SCHEMA_VERSION,
            "kitVersion": leptos_ui_theme_core::KIT_LOCK_SCHEMA_VERSION,
            "project": {
                "configHash": format!("sha256:{}", "0".repeat(64)),
                "crateRoot": ".",
                "kind": "single-crate-trunk-csr"
            },
            "items": {},
            "filesByPath": {},
            "styleBlocksById": {},
            "themeIntegration": {
            "producerPackage": "leptos_ui_kit_cli",
            "producerVersion": "0.2.0",
            "producerChecksum": null,
            "primitivesPackage": "web_ui_primitives",
            "primitivesRequirement": ">=0.2.0,<0.3.0",
            "primitivesVersion": "0.2.0",
            "primitivesChecksum": "sha256:test",
            "presenceAbiVersion": 2,
            "contractPath": "src/components/ui/_kit/token-contract.json",
            "contractId": "leptos-ui-kit",
            "contractAbiVersion": 1,
            "contractRevision": 2,
            "contractCanonicalDigest": contract["canonicalDigest"],
            "contractBytesDigest": contract_bytes_digest,
            "capabilityPath": "src/components/ui/_kit/theme-integration.json",
            "capabilityBytesDigest": format!("sha256:{}", leptos_ui_theme_core::sha256(&capability_bytes)),
            "stylesheetPath": "styles/kit.css",
            "stylesheetBytesDigest": stylesheet_bytes_digest,
            "layerAbiVersion": 1,
            "layerOrder": ["leptos-ui-kit.tokens", "leptos-ui-kit.themes", "leptos-ui-kit.components"],
            "portalAbiVersion": 1,
            "portalMountType": "web_ui_primitives::leptos::PortalMount",
            "portalBodyHost": true
        }});
        std::fs::write(
            root.join("src/components/ui/_kit/kit.lock.json"),
            serde_json::to_vec_pretty(&installed_capability).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("index.html"),
            "<!doctype html>\n<html>\n<head>\n<link data-trunk rel=\"css\" href=\"styles/kit.css\">\n<!-- leptos-ui-theme:anchor -->\n<link data-trunk rel=\"css\" href=\"styles/app.css\">\n</head>\n<body></body>\n</html>\n",
        )
        .unwrap();
        let outcome = init(&root, false, false, 0).unwrap();
        assert!(outcome.changes.len() >= 10);
        let html_change = outcome
            .changes
            .iter()
            .find(|change| change.path == "index.html")
            .unwrap();
        assert_eq!(
            html_change.scope,
            leptos_ui_theme_codegen::ChangeScope::HtmlOwnedRegion
        );
        assert!(html_change.before_digest.is_none());
        assert!(html_change.after_digest.is_some());
        assert!(html_change.container_before_digest.is_some());
        assert!(html_change.container_after_digest.is_some());
        assert_eq!(
            html_change.exterior_before_digest,
            html_change.exterior_after_digest
        );
        let listed = list_command(&root).unwrap();
        assert_eq!(listed.data["contract"]["contractId"], "leptos-ui-kit");
        assert_eq!(
            listed.data["contract"]["compatibility"],
            serde_json::json!({
                "semantic": "compatible",
                "revision": "exact",
                "canonicalDigest": "exact",
                "installedBytes": "exact"
            })
        );
        assert_eq!(listed.data["profiles"][0]["sourceStatus"], "valid");
        let explained = explain_command(&root, "color.surface", "light").unwrap();
        assert_eq!(explained.data["tokenPath"], "color.surface");
        assert_eq!(explained.data["terminalTokenPath"], "color.surface");
        assert_eq!(explained.data["status"], "resolved");
        let css = std::fs::read_to_string(root.join("styles/themes.css")).unwrap();
        assert!(css.contains("@layer leptos-ui-kit.themes"));
        assert!(css.contains("--kit-color-surface: #ffffff"));
        let lock: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("src/theme/theme.lock.json")).unwrap())
                .unwrap();
        assert_eq!(lock["tool"]["package"], "leptos_ui_theme_codegen");
        assert_eq!(lock["tool"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(lock["dtcgVersion"], "2025.10");
        assert_eq!(lock["contract"]["contractId"], "leptos-ui-kit");
        assert_eq!(lock["kit"]["installation"]["root"], "workspace");
        assert!(
            lock["kit"]["capabilityFingerprint"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert!(
            lock["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|input| input["path"] == "tokens/theme.resolver.json")
        );
        let index = std::fs::read_to_string(root.join("index.html")).unwrap();
        assert!(index.contains("<!-- leptos-ui-theme:start -->"));
        assert!(index.contains("href=\"styles/app.css\""));
        assert!(index.contains("href=\"styles/kit.css\""));
        let build = build_command(&root, false, false, 0, &[]).unwrap();
        assert!(build.changes.is_empty());
        std::fs::write(root.join("styles/themes.css"), "/* local edit */\n").unwrap();
        assert!(matches!(
            build_command(&root, false, false, 0, &[]),
            Err(super::CliError::Codegen(
                leptos_ui_theme_codegen::CodegenError::Conflict(_)
            ))
        ));
        let accepted =
            build_command(&root, false, false, 0, &["styles/themes.css".into()]).unwrap();
        let accepted_change = accepted
            .changes
            .iter()
            .find(|change| change.path == "styles/themes.css")
            .unwrap();
        assert!(accepted_change.accepted_generated_conflict);
        let backup_path = accepted_change.backup_path.as_ref().unwrap();
        assert_eq!(
            std::fs::read(root.join(backup_path)).unwrap(),
            b"/* local edit */\n"
        );
        assert!(
            std::fs::read_to_string(root.join("styles/themes.css"))
                .unwrap()
                .contains("@layer leptos-ui-kit.themes")
        );
        assert!(
            build_command(&root, false, false, 0, &[])
                .unwrap()
                .changes
                .is_empty()
        );
        assert!(matches!(
            build_command(&root, false, true, 0, &[]),
            Err(super::CliError::Codegen(
                leptos_ui_theme_codegen::CodegenError::Conflict(_)
            ))
        ));
        assert_eq!(check_command(&root).unwrap().exit_code, 0);
        let doctor = doctor_command(&root).unwrap();
        assert_eq!(doctor.exit_code, 0);
        assert_eq!(doctor.data["checks"].as_array().unwrap().len(), 16);
        assert_eq!(doctor.data["runtimeQualification"], "not-qualified");
        let config_before = std::fs::read(root.join(leptos_ui_theme_core::CONFIG_FILE)).unwrap();
        let resolver_before = std::fs::read(root.join("tokens/theme.resolver.json")).unwrap();
        let add_plan = add_command(&root, "ocean", Some("light"), false, true, 0).unwrap();
        assert_eq!(add_plan.data["kind"], "add");
        assert!(
            add_plan.data["planDigest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(
            std::fs::read(root.join(leptos_ui_theme_core::CONFIG_FILE)).unwrap(),
            config_before
        );
        assert_eq!(
            std::fs::read(root.join("tokens/theme.resolver.json")).unwrap(),
            resolver_before
        );
        assert!(!root.join("tokens/themes/ocean.tokens.json").exists());
        add_command(&root, "ocean", Some("light"), false, false, 0).unwrap();
        assert_eq!(
            std::fs::read(root.join("tokens/themes/ocean.tokens.json")).unwrap(),
            b"{}\n"
        );

        let config_path = root.join(leptos_ui_theme_core::CONFIG_FILE);
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        config["html"]["indexPath"] = serde_json::Value::String("nested/index.html".into());
        config["html"]["indexCandidates"] = serde_json::Value::Null;
        let mut config_bytes = serde_json::to_vec_pretty(&config).unwrap();
        config_bytes.push(b'\n');
        std::fs::write(&config_path, config_bytes).unwrap();
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(
            root.join("nested/index.html"),
            "<!doctype html>\n<html>\n<head>\n<link data-trunk rel=\"css\" href=\"../styles/kit.css\">\n<link data-trunk rel=\"css\" href=\"../styles/app.css\">\n</head>\n<body></body>\n</html>\n",
        )
        .unwrap();
        let migration = build_command(&root, false, false, 0, &[]).unwrap();
        let removed = migration
            .changes
            .iter()
            .find(|change| change.path == "index.html")
            .unwrap();
        let created = migration
            .changes
            .iter()
            .find(|change| change.path == "nested/index.html")
            .unwrap();
        assert_eq!(
            removed.action,
            leptos_ui_theme_codegen::ChangeOperation::Remove
        );
        assert_eq!(
            created.action,
            leptos_ui_theme_codegen::ChangeOperation::Create
        );
        assert!(
            !std::fs::read_to_string(root.join("index.html"))
                .unwrap()
                .contains("<!-- leptos-ui-theme:start -->")
        );
        assert!(
            std::fs::read_to_string(root.join("nested/index.html"))
                .unwrap()
                .contains("<!-- leptos-ui-theme:start -->")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn temporary_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("leptos-ui-theme-{label}-{}", std::process::id()))
    }
}

#![forbid(unsafe_code)]
#![doc = "Command orchestration for the `leptos_ui_theme` CLI."]

use clap::{Args, Parser, Subcommand};
use leptos_ui_theme_codegen::{
    ApplyCommand, BuildOptions as CodegenBuildOptions, Change as PlannedChange, ChangeOperation,
    ChangeScope, CodegenError, GeneratedArtifact, Ownership, apply, apply_artifacts,
    apply_artifacts_for, build, build_with_options, check, seeded_controller, seeded_module,
    seeded_scope,
};
use leptos_ui_theme_core::{CONFIG_FILE, Profile, ProjectConfig, ThemeCompiler, ThemeError};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init(HtmlWriteOptions),
    Build(HtmlWriteOptions),
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

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    Core(#[from] ThemeError),
    #[error(transparent)]
    Codegen(#[from] leptos_ui_theme_codegen::CodegenError),
    #[error("cannot serialize output: {0}")]
    Json(#[from] serde_json::Error),
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
            Self::Conflict(_) => 4,
            Self::Core(ThemeError::Security(_)) => 5,
            Self::Core(ThemeError::Contract(_)) => 6,
            Self::Core(_) => 3,
            Self::Codegen(CodegenError::Conflict(_)) => 4,
            Self::Codegen(CodegenError::Core(ThemeError::Security(_))) => 5,
            Self::Codegen(CodegenError::Core(ThemeError::Contract(_))) => 6,
            Self::Codegen(CodegenError::Core(_)) => 3,
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
                println!(
                    "{}",
                    serde_json::to_string(&envelope).expect("serialize envelope")
                );
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
                println!(
                    "{}",
                    serde_json::to_string(&envelope).expect("serialize envelope")
                );
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
    let cwd = std::fs::canonicalize(&cli.cwd).map_err(|source| CliError::Io {
        path: cli.cwd.clone(),
        source,
    })?;
    match &cli.command {
        Command::Init(options) => init(&cwd, options.dry_run, options.no_patch_index),
        Command::Build(options) => build_command(
            &discover(&cwd, false)?,
            options.dry_run,
            options.no_patch_index,
        ),
        Command::Check => check_command(&discover(&cwd, false)?),
        Command::List => list_command(&discover(&cwd, false)?),
        Command::Explain {
            token_path,
            profile,
        } => explain_command(&discover(&cwd, false)?, token_path, profile),
        Command::Add {
            id,
            base,
            from_contract_defaults,
            dry_run,
        } => add_command(
            &discover(&cwd, false)?,
            id,
            base.as_deref(),
            *from_contract_defaults,
            *dry_run,
        ),
        Command::Doctor { strict } => {
            debug_assert!(*strict);
            doctor_command(&discover(&cwd, false)?)
        }
    }
}

fn discover(start: &Path, init: bool) -> Result<PathBuf, CliError> {
    let mut matches = Vec::new();
    for ancestor in start.ancestors().take(256) {
        if ancestor.join(CONFIG_FILE).is_file() {
            matches.push(ancestor.to_path_buf());
        }
        if ancestor.join(".git").exists() {
            break;
        }
    }
    match matches.as_slice() {
        [root] => Ok(root.clone()),
        [] if init => Ok(start.to_path_buf()),
        [] => Err(ThemeError::Config(format!("no {CONFIG_FILE} found")).into()),
        _ => Err(ThemeError::Config(format!("multiple {CONFIG_FILE} files found")).into()),
    }
}

fn init(start: &Path, dry_run: bool, no_patch_index: bool) -> Result<Outcome, CliError> {
    let root = discover(start, true)?;
    if root.join(CONFIG_FILE).exists() {
        return Err(CliError::Conflict(format!("{CONFIG_FILE} already exists")));
    }
    let config = ProjectConfig::default();
    config.validate()?;
    let resolver = starter_resolver();
    let starter_profiles = config
        .profiles
        .named
        .iter()
        .map(|profile| leptos_ui_theme_core::ResolvedProfile {
            id: profile.id.clone(),
            label: profile.label.clone(),
            color_scheme: profile.color_scheme,
            values: Vec::new(),
        })
        .collect::<Vec<_>>();
    let files = vec![
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
    let mut changes = files
        .iter()
        .map(|(path, bytes)| create_change(path, bytes))
        .collect::<Vec<_>>();
    if !dry_run {
        for (path, _) in &files {
            if root.join(path).exists() {
                return Err(CliError::Conflict(format!("{path} already exists")));
            }
        }
        let artifacts = files
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
        apply_artifacts_for(&root, &artifacts, ApplyCommand::Init, None)?;
        let result = build_with_options(
            &root,
            CodegenBuildOptions {
                patch_index: !no_patch_index,
            },
        )?;
        let applied = apply(&root, &result)?;
        changes.extend(
            result
                .plan
                .changes
                .iter()
                .filter(|change| applied.contains(&change.path))
                .map(change_from_plan),
        );
    }
    Ok(Outcome {
        command: "init",
        status: if dry_run {
            Status::Planned
        } else {
            Status::Warning
        },
        exit_code: 0,
        changes,
        data: json!({
            "root": ".",
            "config": CONFIG_FILE,
            "htmlMode": if no_patch_index { "manual" } else { "patched" },
            "dependencies": dependency_requirements(),
        }),
    })
}

fn build_command(root: &Path, dry_run: bool, no_patch_index: bool) -> Result<Outcome, CliError> {
    let result = build_with_options(
        root,
        CodegenBuildOptions {
            patch_index: !no_patch_index,
        },
    )?;
    let stale = check(root, &result);
    let changed_paths = if dry_run {
        stale.clone()
    } else {
        apply(root, &result)?
    };
    let changes = result
        .plan
        .changes
        .iter()
        .filter(|change| changed_paths.contains(&change.path))
        .map(change_from_plan)
        .collect::<Vec<_>>();
    let status = if dry_run && !changes.is_empty() {
        Status::Planned
    } else if changes.is_empty() {
        Status::NoChange
    } else {
        Status::Success
    };
    Ok(Outcome {
        command: "build",
        status,
        exit_code: 0,
        changes,
        data: json!({
            "profiles": result.profiles.iter().map(|profile| &profile.id).collect::<Vec<_>>(),
            "bootstrap": {
                "mode": result.bootstrap.mode,
                "scriptDigest": result.bootstrap.script_digest,
                "cspSource": result.bootstrap.csp_source,
            },
            "htmlMode": if no_patch_index { "manual" } else { "patched" },
            "htmlSnippet": no_patch_index.then_some(result.bootstrap.html_snippet),
            "dependencies": dependency_requirements(),
        }),
    })
}

fn check_command(root: &Path) -> Result<Outcome, CliError> {
    let result = build(root)?;
    let stale = check(root, &result);
    Ok(Outcome {
        command: "check",
        status: if stale.is_empty() {
            Status::Success
        } else {
            Status::Error
        },
        exit_code: if stale.is_empty() { 0 } else { 7 },
        changes: Vec::new(),
        data: json!({"fresh": stale.is_empty(), "stale": stale}),
    })
}

fn list_command(root: &Path) -> Result<Outcome, CliError> {
    let compiler = ThemeCompiler::load(root)?;
    let profiles = compiler.resolve()?;
    Ok(Outcome {
        command: "list",
        status: Status::Success,
        exit_code: 0,
        changes: Vec::new(),
        data: json!({"profiles": profiles.iter().map(|profile| json!({"id": profile.id, "label": profile.label, "colorScheme": profile.color_scheme})).collect::<Vec<_>>() }),
    })
}

fn explain_command(root: &Path, token_path: &str, profile: &str) -> Result<Outcome, CliError> {
    let resolved = ThemeCompiler::load(root)?.resolve_one(profile)?;
    let token = resolved
        .values
        .iter()
        .find(|token| token.path == token_path)
        .ok_or_else(|| ThemeError::Resolution(format!("unknown token `{token_path}`")))?;
    Ok(Outcome {
        command: "explain",
        status: Status::Success,
        exit_code: 0,
        changes: Vec::new(),
        data: json!({"profile": profile, "path": token.path, "type": token.token_type, "value": token.value, "provenance": token.provenance, "aliasOf": token.alias_of}),
    })
}

fn doctor_command(root: &Path) -> Result<Outcome, CliError> {
    let result = build(root)?;
    let stale = check(root, &result);
    Ok(Outcome {
        command: "doctor",
        status: if stale.is_empty() {
            Status::Success
        } else {
            Status::Error
        },
        exit_code: if stale.is_empty() { 0 } else { 7 },
        changes: Vec::new(),
        data: json!({"configuration": "pass", "contract": "pass", "resolution": "pass", "outputsFresh": stale.is_empty()}),
    })
}

fn add_command(
    root: &Path,
    id: &str,
    base: Option<&str>,
    from_defaults: bool,
    dry_run: bool,
) -> Result<Outcome, CliError> {
    leptos_ui_theme_core::validate_theme_id(id)?;
    let config_path = root.join(CONFIG_FILE);
    let mut config: ProjectConfig = read_json(&config_path)?;
    if config.profiles.named.iter().any(|profile| profile.id == id) {
        return Err(CliError::Conflict(format!("profile `{id}` already exists")));
    }
    let mut resolver: serde_json::Value = read_json(&root.join(&config.resolver))?;
    let source_path = format!("{}/themes/{id}.tokens.json", config.token_root);
    if root.join(&source_path).exists() {
        return Err(CliError::Conflict(format!("{source_path} already exists")));
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
                inputs: BTreeMap::from([("theme".into(), id.into())]),
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
    let files = vec![
        (CONFIG_FILE.into(), pretty_json(&config)?),
        (config.resolver.clone(), pretty_json(&resolver)?),
        (source_path, b"{}\n".to_vec()),
    ];
    let changes = files
        .iter()
        .map(|(path, bytes)| {
            let before = std::fs::read(root.join(path)).ok();
            Change {
                path: path.clone(),
                scope: ChangeScope::WholeFile,
                action: if before.is_some() {
                    ChangeOperation::Replace
                } else {
                    ChangeOperation::Create
                },
                ownership: Ownership::UserAuthored,
                before_digest: before
                    .as_ref()
                    .map(|bytes| format!("sha256:{}", leptos_ui_theme_core::sha256(bytes))),
                after_digest: Some(format!("sha256:{}", leptos_ui_theme_core::sha256(bytes))),
                container_before_digest: None,
                container_after_digest: None,
                exterior_before_digest: None,
                exterior_after_digest: None,
                backup_path: None,
                accepted_generated_conflict: false,
            }
        })
        .collect();
    if !dry_run {
        let artifacts = files
            .iter()
            .map(|(path, bytes)| GeneratedArtifact::user_authored(path.clone(), bytes.clone()))
            .collect::<Vec<_>>();
        apply_artifacts(root, &artifacts)?;
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
        data: json!({"profile": id}),
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CliError> {
    let bytes = std::fs::read(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(CliError::Json)
}

fn pretty_json(value: &impl Serialize) -> Result<Vec<u8>, CliError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn dependency_requirements() -> serde_json::Value {
    json!([
        {
            "package": "leptos",
            "requirement": "=0.9.0-alpha",
            "features": ["csr"],
            "defaultFeatures": false,
            "resolvedVersion": null,
            "checksum": null
        },
        {
            "package": "web_ui_primitives",
            "requirement": ">=0.2.0,<0.3.0",
            "features": ["leptos"],
            "defaultFeatures": false,
            "resolvedVersion": null,
            "checksum": null
        }
    ])
}

fn create_change(path: &str, bytes: &[u8]) -> Change {
    Change {
        path: path.to_owned(),
        scope: ChangeScope::WholeFile,
        action: ChangeOperation::Create,
        ownership: if path.ends_with(".rs") {
            Ownership::SeededAppOwned
        } else {
            Ownership::UserAuthored
        },
        before_digest: None,
        after_digest: Some(format!("sha256:{}", leptos_ui_theme_core::sha256(bytes))),
        container_before_digest: None,
        container_after_digest: None,
        exterior_before_digest: None,
        exterior_after_digest: None,
        backup_path: None,
        accepted_generated_conflict: false,
    }
}

fn change_from_plan(change: &PlannedChange) -> Change {
    Change {
        path: change.path.clone(),
        scope: change.scope,
        action: change.operation,
        ownership: change.ownership,
        before_digest: change.before_digest.clone(),
        after_digest: change.after_digest.clone(),
        container_before_digest: None,
        container_after_digest: None,
        exterior_before_digest: None,
        exterior_after_digest: None,
        backup_path: None,
        accepted_generated_conflict: false,
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
    use super::{build_command, init, starter_resolver};
    use std::path::PathBuf;

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
        init(&root, false, false).unwrap();
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
        let lock = serde_json::json!({"themeIntegration": {
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
            serde_json::to_vec_pretty(&lock).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("index.html"),
            "<!doctype html>\n<html>\n<head>\n<link data-trunk rel=\"css\" href=\"styles/kit.css\">\n</head>\n<body></body>\n</html>\n",
        )
        .unwrap();
        let outcome = build_command(&root, false, false).unwrap();
        assert_eq!(outcome.changes.len(), 4);
        let css = std::fs::read_to_string(root.join("styles/themes.css")).unwrap();
        assert!(css.contains("@layer leptos-ui-kit.themes"));
        assert!(css.contains("--kit-color-surface: #ffffff"));
        let index = std::fs::read_to_string(root.join("index.html")).unwrap();
        assert!(index.contains("<!-- leptos-ui-theme:start -->"));
        let second = build_command(&root, false, false).unwrap();
        assert!(second.changes.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn temporary_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("leptos-ui-theme-{label}-{}", std::process::id()))
    }
}

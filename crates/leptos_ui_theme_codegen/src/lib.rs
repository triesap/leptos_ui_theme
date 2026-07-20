#![forbid(unsafe_code)]
#![doc = "Deterministic artifact generation for `leptos_ui_theme`."]

mod plan;
mod runtime;
mod transaction;

pub use plan::{
    ArtifactManifest, ArtifactManifestEntry, Change, ChangeOperation, ChangeScope,
    DesiredArtifactState, Ownership, PlanV1, Snapshot, plan_artifacts, plan_manifest,
};
pub use runtime::{seeded_controller, seeded_module, seeded_scope};
pub use transaction::{
    ApplyCommand, apply_transaction, apply_transaction_with_wait, ensure_no_active_transaction,
    recover,
};

/// The immutable theme-lock schema.
pub const LOCK_SCHEMA: &str =
    "https://triesap.github.io/leptos_ui_theme/schema/0.1.0/lock.schema.json";
/// Packaged draft 2020-12 theme-lock schema bytes.
pub const LOCK_SCHEMA_JSON: &str = include_str!("../schemas/lock.schema.json");

use html5ever::{
    ParseOpts, parse_document, tendril::TendrilSink, tokenizer::TokenizerOpts,
    tree_builder::TreeBuilderOpts,
};
use leptos_ui_theme_core::{
    BootstrapMode, COMPILED_LIMITS, CONFIG_FILE, ColorScheme, LogicalPath, OpenedSource,
    ProjectConfig, ResolvedProfile, ResolvedToken, SourceLoader, SourceRole, ThemeCompiler,
    ThemeError, TokenDomain, format_css_number, serialize_color_fallback, serialize_color_modern,
    sha256,
};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const HTML_INSERTION_ANCHOR: &str = "<!-- leptos-ui-theme:anchor -->";

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CodegenError {
    #[error(transparent)]
    Core(#[from] ThemeError),
    #[error("cannot serialize generated artifact: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot serialize token `{path}`: {reason}")]
    CssValue { path: String, reason: String },
    #[error("generated output exceeds the configured byte limit")]
    OutputLimit,
    #[error("generated output changed while applying: {0}")]
    Conflict(String),
}

#[derive(Clone, Debug)]
pub struct GeneratedArtifact {
    pub path: String,
    pub bytes: Vec<u8>,
    pub scope: ChangeScope,
    pub ownership: Ownership,
}

#[derive(Clone, Debug)]
pub struct BuildResult {
    pub artifacts: Vec<GeneratedArtifact>,
    pub manifest: ArtifactManifest,
    pub consumed_inputs: Vec<ConsumedInput>,
    pub workspace_root: PathBuf,
    pub profiles: Vec<ResolvedProfile>,
    pub plan: PlanV1,
    pub bootstrap: BootstrapMetadata,
    pub accepted_generated: BTreeMap<String, String>,
    pub manual_html_stale: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsumedInputRoot {
    AppConfig,
    Workspace,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConsumedInput {
    pub root: ConsumedInputRoot,
    pub path: String,
    pub digest: String,
}

#[derive(Clone, Debug)]
pub struct BootstrapMetadata {
    pub mode: BootstrapMode,
    pub script: Option<String>,
    pub script_digest: Option<String>,
    pub csp_source: Option<String>,
    pub html_snippet: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DependencyRecord {
    pub package: String,
    pub requirement: String,
    pub features: Vec<String>,
    pub default_features: bool,
    pub resolved_version: Option<String>,
    pub checksum: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyState {
    Pending,
    Resolved,
}

#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub patch_index: bool,
    pub dependency_state: DependencyState,
    pub dependencies: Vec<DependencyRecord>,
    pub accept_generated: Vec<String>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            patch_index: true,
            dependency_state: DependencyState::Pending,
            dependencies: default_dependency_records(),
            accept_generated: Vec::new(),
        }
    }
}

#[must_use]
pub fn default_dependency_records() -> Vec<DependencyRecord> {
    vec![
        DependencyRecord {
            package: "leptos".into(),
            requirement: "=0.9.0-alpha".into(),
            features: Vec::new(),
            default_features: false,
            resolved_version: None,
            checksum: None,
        },
        DependencyRecord {
            package: "web_ui_primitives".into(),
            requirement: ">=0.2.0,<0.3.0".into(),
            features: vec!["core".into(), "leptos".into()],
            default_features: false,
            resolved_version: None,
            checksum: None,
        },
    ]
}

fn validate_dependency_records(
    state: DependencyState,
    dependencies: &[DependencyRecord],
) -> Result<(), CodegenError> {
    if dependencies.len() != 2
        || dependencies[0].package != "leptos"
        || dependencies[0].requirement != "=0.9.0-alpha"
        || dependencies[0].default_features
        || dependencies[1].package != "web_ui_primitives"
        || dependencies[1].requirement != ">=0.2.0,<0.3.0"
        || dependencies[1].default_features
    {
        return Err(CodegenError::Core(ThemeError::Config(
            "dependency plan differs from the generated runtime contract".into(),
        )));
    }
    let leptos_mode = validate_render_features(&dependencies[0].features, &[])?;
    let primitives_mode = validate_render_features(&dependencies[1].features, &["core", "leptos"])?;
    if leptos_mode != primitives_mode {
        return Err(CodegenError::Core(ThemeError::Config(
            "generated runtime dependencies select different render modes".into(),
        )));
    }
    let all_resolved = dependencies
        .iter()
        .all(|dependency| dependency.resolved_version.is_some() && dependency.checksum.is_some());
    let all_pending = dependencies
        .iter()
        .all(|dependency| dependency.resolved_version.is_none() && dependency.checksum.is_none());
    if (state == DependencyState::Resolved && !all_resolved)
        || (state == DependencyState::Pending && !all_pending)
    {
        return Err(CodegenError::Core(ThemeError::Config(
            "dependency state and resolution records differ".into(),
        )));
    }
    Ok(())
}

fn validate_render_features(
    features: &[String],
    required: &[&str],
) -> Result<Option<&'static str>, CodegenError> {
    let mut delivery = None;
    for feature in features {
        if required.contains(&feature.as_str()) {
            continue;
        }
        let selected = match feature.as_str() {
            "csr" => "csr",
            "hydrate" => "hydrate",
            "ssr" => "ssr",
            _ => {
                return Err(CodegenError::Core(ThemeError::Config(
                    "dependency plan differs from the generated runtime contract".into(),
                )));
            }
        };
        if delivery.is_some() {
            return Err(CodegenError::Core(ThemeError::Config(
                "dependency plan differs from the generated runtime contract".into(),
            )));
        }
        delivery = Some(selected);
    }
    if required
        .iter()
        .any(|required| !features.iter().any(|feature| feature == required))
        || features.len() != required.len() + usize::from(delivery.is_some())
    {
        return Err(CodegenError::Core(ThemeError::Config(
            "dependency plan differs from the generated runtime contract".into(),
        )));
    }
    Ok(delivery)
}

impl GeneratedArtifact {
    #[must_use]
    pub fn generated(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            bytes,
            scope: ChangeScope::WholeFile,
            ownership: Ownership::GeneratedLockOwned,
        }
    }

    #[must_use]
    pub fn seeded(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            bytes,
            scope: ChangeScope::WholeFile,
            ownership: Ownership::SeededAppOwned,
        }
    }

    #[must_use]
    pub fn user_authored(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            bytes,
            scope: ChangeScope::WholeFile,
            ownership: Ownership::UserAuthored,
        }
    }

    #[must_use]
    pub fn html_region(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            bytes,
            scope: ChangeScope::HtmlOwnedRegion,
            ownership: Ownership::GeneratedLockOwned,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThemeLock {
    schema_version: &'static str,
    tool: ToolLock,
    dtcg_version: String,
    kit: KitProvenanceLock,
    contract: ContractLock,
    config: InputLock,
    inputs: Vec<InputLock>,
    profiles: Vec<ProfileLock>,
    dependency_state: DependencyState,
    dependencies: Vec<DependencyRecord>,
    bootstrap: BootstrapLock,
    html_integration: HtmlIntegrationLock,
    outputs: BTreeMap<String, OutputLock>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolLock {
    package: &'static str,
    version: &'static str,
    repository: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KitProvenanceLock {
    capability_fingerprint: String,
    installation: InputLock,
    capability: InputLock,
    stylesheet: InputLock,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviousThemeLock {
    schema_version: String,
    outputs: BTreeMap<String, PreviousOutputLock>,
    html_integration: Option<PreviousHtmlIntegration>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PreviousOutputLock {
    Digest(String),
    Record {
        digest: String,
        ownership: Ownership,
        scope: ChangeScope,
    },
}

impl PreviousOutputLock {
    fn digest(&self) -> &str {
        match self {
            Self::Digest(digest) | Self::Record { digest, .. } => digest,
        }
    }

    fn ownership(&self) -> Ownership {
        match self {
            Self::Digest(_) => Ownership::GeneratedLockOwned,
            Self::Record { ownership, .. } => *ownership,
        }
    }

    fn scope(&self) -> ChangeScope {
        match self {
            Self::Digest(_) => ChangeScope::WholeFile,
            Self::Record { scope, .. } => *scope,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviousHtmlIntegration {
    mode: String,
    #[serde(default)]
    selected_index_path: Option<String>,
    #[serde(default)]
    region_digest: Option<String>,
    #[serde(default)]
    container_digest: Option<String>,
    #[serde(default)]
    exterior_digest: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContractLock {
    path: String,
    contract_id: String,
    abi_version: u32,
    revision: u32,
    canonical_digest: String,
    installed_bytes_digest: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InputLock {
    root: InputRoot,
    path: String,
    bytes_digest: String,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum InputRoot {
    AppConfig,
    Workspace,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileLock {
    id: String,
    semantic_digest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputLock {
    digest: String,
    ownership: Ownership,
    scope: ChangeScope,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapLock {
    mode: BootstrapMode,
    script_digest: Option<String>,
    csp_source: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HtmlIntegrationLock {
    mode: &'static str,
    selected_index_path: String,
    snippet_digest: String,
    region_digest: Option<String>,
    container_digest: Option<String>,
    exterior_digest: Option<String>,
}

pub fn build(root: &Path) -> Result<BuildResult, CodegenError> {
    build_with_options(root, BuildOptions::default())
}

pub fn build_with_options(root: &Path, options: BuildOptions) -> Result<BuildResult, CodegenError> {
    build_with_workspace(root, root, options)
}

pub fn build_with_workspace(
    workspace_root: &Path,
    config_root: &Path,
    options: BuildOptions,
) -> Result<BuildResult, CodegenError> {
    validate_dependency_records(options.dependency_state, &options.dependencies)?;
    let compiler = ThemeCompiler::load_with_workspace(workspace_root, config_root)?;
    let root = compiler.root.clone();
    let profiles = compiler.resolve()?;
    let mut axes = Vec::new();
    if let Some(configured) = &compiler.config.axes {
        for (name, domain, axis) in [
            ("density", TokenDomain::Density, configured.density.as_ref()),
            ("motion", TokenDomain::Motion, configured.motion.as_ref()),
            (
                "contrast",
                TokenDomain::Contrast,
                configured.contrast.as_ref(),
            ),
        ] {
            if let Some(axis) = axis {
                axes.push(ResolvedAxis {
                    domain,
                    attribute: axis.attribute.clone(),
                    default_context: axis.default_context.clone(),
                    system: axis
                        .system
                        .as_ref()
                        .map(|system| (system.query.clone(), system.context.clone())),
                    contexts: compiler.resolve_axis(name, &axis.contexts)?,
                });
            }
        }
    }
    let metadata = generated_metadata(&compiler);
    let css = generate_css(&compiler.config, &profiles, &axes)?.replacen(
        "/* Generated by leptos_ui_theme. Do not edit. */",
        &format!("/* {metadata} */"),
        1,
    );
    let rust = generate_rust(&compiler.config, &profiles).replacen(
        "// Generated by leptos_ui_theme. Do not edit.",
        &format!("// {metadata}"),
        1,
    );
    let mut artifacts = vec![
        GeneratedArtifact::generated(compiler.config.outputs.css.clone(), css.into_bytes()),
        GeneratedArtifact::generated(compiler.config.outputs.rust.clone(), rust.into_bytes()),
    ];
    let selected_index = select_index(&root, &compiler.config)?;
    let index_relative = selected_index
        .strip_prefix(&root)
        .map_err(|_| {
            CodegenError::Core(ThemeError::Security(selected_index.display().to_string()))
        })?
        .to_string_lossy()
        .into_owned();
    let kit_href = relative_workspace_asset(
        &compiler.workspace_root,
        &root,
        &index_relative,
        compiler.kit_stylesheet.logical_path.as_str(),
    )?;
    let script = bootstrap_script(&compiler.config, &profiles)?;
    if compiler.config.bootstrap.mode == BootstrapMode::ExternalSync {
        let external = compiler.config.bootstrap.external.as_ref().ok_or_else(|| {
            CodegenError::Core(ThemeError::Config(
                "external bootstrap config is missing".into(),
            ))
        })?;
        let mut bytes = script.as_bytes().to_vec();
        bytes.push(b'\n');
        artifacts.push(GeneratedArtifact::generated(
            external.output_path.clone(),
            bytes,
        ));
    }
    let index_bytes = std::fs::read(&selected_index).map_err(|source| CodegenError::Io {
        path: selected_index.clone(),
        source,
    })?;
    let region = html_region(&compiler.config, &profiles, &index_relative, &script)?;
    let input_exterior_digest = html_exterior_digest_for_index(&index_bytes, &kit_href)?;
    let patched = patch_index(&index_bytes, &region, &kit_href)?;
    let patched_region_digest = format!(
        "sha256:{}",
        sha256(owned_html_region(&patched)?.ok_or_else(|| {
            CodegenError::Core(ThemeError::Config(
                "patched index has no owned theme region".into(),
            ))
        })?)
    );
    let patched_exterior_digest = html_exterior_digest(&patched)?;
    if patched_exterior_digest != input_exterior_digest {
        return Err(CodegenError::Core(ThemeError::Security(
            "HTML patch changed bytes outside the owned region".into(),
        )));
    }
    let script_digest = (compiler.config.bootstrap.mode != BootstrapMode::Disabled)
        .then(|| format!("sha256:{}", sha256(script.as_bytes())));
    let csp_source = (compiler.config.bootstrap.mode == BootstrapMode::InlineCspHash)
        .then(|| csp_source(script.as_bytes()));
    let bootstrap = BootstrapMetadata {
        mode: compiler.config.bootstrap.mode,
        script: (compiler.config.bootstrap.mode != BootstrapMode::Disabled).then(|| script.clone()),
        script_digest: script_digest.clone(),
        csp_source: csp_source.clone(),
        html_snippet: region.clone(),
    };
    let patched_digest = format!("sha256:{}", sha256(&patched));
    let previous = read_previous_theme_lock(
        &root,
        &compiler.config.outputs.lock,
        compiler.config.limits.file_bytes,
    )?;
    if !options.patch_index
        && previous
            .as_ref()
            .and_then(|lock| lock.html_integration.as_ref())
            .is_some_and(|html| html.mode == "patched")
    {
        return Err(CodegenError::Conflict(
            "--no-patch-index cannot release an owned HTML region".into(),
        ));
    }
    let migration_artifact = if options.patch_index {
        previous
            .as_ref()
            .and_then(|lock| lock.html_integration.as_ref())
            .filter(|html| html.mode == "patched")
            .and_then(|html| html.selected_index_path.as_deref())
            .filter(|old_path| *old_path != index_relative)
            .map(|old_path| {
                let old_logical = LogicalPath::new(old_path.to_owned())?;
                let old_bytes =
                    std::fs::read(root.join(old_logical.to_path_buf())).map_err(|source| {
                        CodegenError::Io {
                            path: PathBuf::from(old_path),
                            source,
                        }
                    })?;
                let old_html = previous
                    .as_ref()
                    .and_then(|lock| lock.html_integration.as_ref())
                    .ok_or_else(|| {
                        CodegenError::Conflict("previous HTML integration is missing".into())
                    })?;
                if old_html.container_digest.as_deref()
                    != Some(format!("sha256:{}", sha256(&old_bytes)).as_str())
                {
                    return Err(CodegenError::Conflict(format!(
                        "previous selected index `{old_path}` differs from its lock record"
                    )));
                }
                let old_region = owned_html_region(&old_bytes)?.ok_or_else(|| {
                    CodegenError::Conflict(format!(
                        "previous selected index `{old_path}` has no owned region"
                    ))
                })?;
                let old_exterior_digest = html_exterior_digest(&old_bytes)?;
                if old_html.region_digest.as_deref()
                    != Some(format!("sha256:{}", sha256(old_region)).as_str())
                    || old_html
                        .exterior_digest
                        .as_deref()
                        .is_some_and(|digest| digest != old_exterior_digest)
                {
                    return Err(CodegenError::Conflict(format!(
                        "previous selected index `{old_path}` has drifted ownership bytes"
                    )));
                }
                let old_kit_href = relative_workspace_asset(
                    &compiler.workspace_root,
                    &root,
                    old_path,
                    compiler.kit_stylesheet.logical_path.as_str(),
                )?;
                Ok(GeneratedArtifact::html_region(
                    old_path,
                    remove_owned_html_region(&old_bytes, &old_kit_href)?,
                ))
            })
            .transpose()?
    } else {
        None
    };
    let manual_html_stale = if options.patch_index {
        artifacts.push(GeneratedArtifact::html_region(
            index_relative.clone(),
            patched,
        ));
        None
    } else {
        let text = std::str::from_utf8(&index_bytes).map_err(|_| {
            CodegenError::Core(ThemeError::Config("index HTML must be UTF-8".into()))
        })?;
        let expected = patch_index(&index_bytes, &region, &kit_href)?;
        if text.contains("<!-- leptos-ui-theme:start -->") && index_bytes != expected {
            return Err(CodegenError::Core(ThemeError::Config(
                "manual HTML region is stale".into(),
            )));
        }
        artifacts.push(GeneratedArtifact::user_authored(
            index_relative.clone(),
            index_bytes.clone(),
        ));
        (index_bytes != expected).then_some(index_relative.clone())
    };
    let config_bytes = &compiler.config_source.bytes;
    let contract_bytes = &compiler.contract_source.bytes;
    let output_digests = artifacts
        .iter()
        .filter(|artifact| artifact.ownership == Ownership::GeneratedLockOwned)
        .map(|artifact| {
            let digest = if artifact.scope == ChangeScope::HtmlOwnedRegion {
                let region = owned_html_region(&artifact.bytes)?.ok_or_else(|| {
                    CodegenError::Core(ThemeError::Config("owned HTML output has no region".into()))
                })?;
                format!("sha256:{}", sha256(region))
            } else {
                format!("sha256:{}", sha256(&artifact.bytes))
            };
            Ok((
                artifact.path.clone(),
                OutputLock {
                    digest,
                    ownership: artifact.ownership,
                    scope: artifact.scope,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, CodegenError>>()?;
    if let Some(migration_artifact) = migration_artifact {
        artifacts.push(migration_artifact);
    }
    let input_digests = collect_input_digests(&compiler)?;
    let installation_input = input_lock(InputRoot::Workspace, &compiler.kit_installation);
    let capability_input = input_lock(InputRoot::Workspace, &compiler.kit_capability);
    let contract_input = input_lock(InputRoot::Workspace, &compiler.contract_source);
    let stylesheet_input = input_lock(InputRoot::Workspace, &compiler.kit_stylesheet);
    let profile_digests = profiles
        .iter()
        .map(|profile| ProfileLock {
            id: profile.id.clone(),
            semantic_digest: profile.semantic_digest.clone(),
        })
        .collect();
    let mut consumed_inputs = input_digests
        .iter()
        .map(|input| ConsumedInput {
            root: ConsumedInputRoot::AppConfig,
            path: input.path.clone(),
            digest: input.bytes_digest.clone(),
        })
        .collect::<Vec<_>>();
    consumed_inputs.push(ConsumedInput {
        root: ConsumedInputRoot::AppConfig,
        path: CONFIG_FILE.into(),
        digest: format!("sha256:{}", sha256(config_bytes)),
    });
    for input in [
        &installation_input,
        &capability_input,
        &contract_input,
        &stylesheet_input,
    ] {
        consumed_inputs.push(ConsumedInput {
            root: ConsumedInputRoot::Workspace,
            path: input.path.clone(),
            digest: input.bytes_digest.clone(),
        });
    }
    consumed_inputs.sort_by(|left, right| {
        consumed_root_order(left.root)
            .cmp(&consumed_root_order(right.root))
            .then_with(|| left.path.as_bytes().cmp(right.path.as_bytes()))
    });
    let lock = ThemeLock {
        schema_version: "1.0.0",
        tool: ToolLock {
            package: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
            repository: env!("CARGO_PKG_REPOSITORY"),
        },
        dtcg_version: compiler.config.dtcg_version.clone(),
        kit: KitProvenanceLock {
            capability_fingerprint: compiler.kit_capability_fingerprint.clone(),
            installation: installation_input,
            capability: capability_input,
            stylesheet: stylesheet_input,
        },
        contract: ContractLock {
            path: contract_input.path,
            contract_id: compiler.contract.contract_id.clone(),
            abi_version: compiler.contract.abi_version,
            revision: compiler.contract.revision,
            canonical_digest: compiler.contract.canonical_digest.clone(),
            installed_bytes_digest: format!("sha256:{}", sha256(contract_bytes)),
        },
        config: InputLock {
            root: InputRoot::AppConfig,
            path: CONFIG_FILE.into(),
            bytes_digest: format!("sha256:{}", sha256(config_bytes)),
        },
        inputs: input_digests.clone(),
        profiles: profile_digests,
        dependency_state: options.dependency_state,
        dependencies: options.dependencies.clone(),
        bootstrap: BootstrapLock {
            mode: compiler.config.bootstrap.mode,
            script_digest,
            csp_source,
        },
        html_integration: HtmlIntegrationLock {
            mode: if options.patch_index {
                "patched"
            } else {
                "manual"
            },
            selected_index_path: selected_index
                .strip_prefix(&root)
                .map_err(|_| {
                    CodegenError::Core(ThemeError::Security(selected_index.display().to_string()))
                })?
                .to_string_lossy()
                .into_owned(),
            snippet_digest: format!("sha256:{}", sha256(region.as_bytes())),
            region_digest: if options.patch_index {
                Some(patched_region_digest)
            } else {
                None
            },
            container_digest: if options.patch_index {
                Some(patched_digest)
            } else {
                None
            },
            exterior_digest: if options.patch_index {
                Some(patched_exterior_digest)
            } else {
                None
            },
        },
        outputs: output_digests,
    };
    let mut lock_bytes = serde_json::to_vec_pretty(&lock)?;
    lock_bytes.push(b'\n');
    protect_workspace_inputs(
        &root,
        &compiler.workspace_root,
        &artifacts,
        &[
            &compiler.kit_installation,
            &compiler.kit_capability,
            &compiler.contract_source,
            &compiler.kit_stylesheet,
        ],
    )?;
    let (backup_artifacts, accepted_generated) = protect_generated_ownership(
        &root,
        &artifacts,
        &compiler.config.outputs.lock,
        compiler.config.limits.file_bytes,
        &options.accept_generated,
    )?;
    let lock_artifact =
        GeneratedArtifact::generated(compiler.config.outputs.lock.clone(), lock_bytes);
    let total: usize = artifacts
        .iter()
        .map(|artifact| artifact.bytes.len())
        .sum::<usize>()
        + lock_artifact.bytes.len();
    if total as u64 > compiler.config.limits.generated_bytes
        || artifacts.iter().any(|artifact| {
            artifact.bytes.len() as u64 > compiler.config.limits.generated_artifact_bytes
        })
        || lock_artifact.bytes.len() as u64 > compiler.config.limits.generated_artifact_bytes
    {
        return Err(CodegenError::OutputLimit);
    }
    let mut backups = backup_artifacts
        .into_iter()
        .map(|artifact| (artifact.path.clone(), artifact))
        .collect::<BTreeMap<_, _>>();
    let mut ordered_artifacts = Vec::with_capacity(artifacts.len() + backups.len() + 1);
    for artifact in artifacts {
        if let Some(backup_path) = accepted_generated.get(&artifact.path) {
            ordered_artifacts.push(backups.remove(backup_path).ok_or_else(|| {
                CodegenError::Core(ThemeError::Config(format!(
                    "accepted output `{}` has no retained backup payload",
                    artifact.path
                )))
            })?);
        }
        ordered_artifacts.push(artifact);
    }
    if !backups.is_empty() {
        return Err(CodegenError::Core(ThemeError::Config(
            "retained backup has no accepted generated output".into(),
        )));
    }
    ordered_artifacts.push(lock_artifact);
    artifacts = ordered_artifacts;
    let manifest = desired_manifest(
        &root,
        &compiler.config.outputs.lock,
        compiler.config.limits.file_bytes,
        &input_digests
            .iter()
            .map(|input| input.path.as_str())
            .chain(std::iter::once(CONFIG_FILE))
            .collect::<std::collections::BTreeSet<_>>(),
        &artifacts,
    )?;
    let plan = plan_manifest(&root, &artifacts, &manifest)?;
    Ok(BuildResult {
        artifacts,
        manifest,
        consumed_inputs,
        workspace_root: compiler.workspace_root,
        profiles,
        plan,
        bootstrap,
        accepted_generated,
        manual_html_stale,
    })
}

fn consumed_root_order(root: ConsumedInputRoot) -> u8 {
    match root {
        ConsumedInputRoot::AppConfig => 0,
        ConsumedInputRoot::Workspace => 1,
    }
}

fn read_previous_theme_lock(
    root: &Path,
    lock_relative: &str,
    file_limit: u64,
) -> Result<Option<PreviousThemeLock>, CodegenError> {
    let path = root.join(lock_relative);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CodegenError::Io {
                path: PathBuf::from(lock_relative),
                source,
            });
        }
    };
    if bytes.len() as u64 > file_limit {
        return Err(CodegenError::Core(ThemeError::Limit {
            resource: "fileBytes",
            limit: file_limit,
            observed: bytes.len() as u64,
        }));
    }
    let lock = serde_json::from_slice::<PreviousThemeLock>(&bytes).map_err(|source| {
        CodegenError::Core(ThemeError::Json {
            path: PathBuf::from(lock_relative),
            source,
        })
    })?;
    if lock.schema_version != "1.0.0" {
        return Err(CodegenError::Core(ThemeError::Config(
            "existing theme lock has an unsupported schema".into(),
        )));
    }
    Ok(Some(lock))
}

fn desired_manifest(
    root: &Path,
    lock_relative: &str,
    file_limit: u64,
    protected_inputs: &std::collections::BTreeSet<&str>,
    artifacts: &[GeneratedArtifact],
) -> Result<ArtifactManifest, CodegenError> {
    let mut entries = artifacts
        .iter()
        .map(|artifact| ArtifactManifestEntry {
            path: artifact.path.clone(),
            scope: artifact.scope,
            ownership: artifact.ownership,
            state: DesiredArtifactState::Present,
            digest: Some(format!("sha256:{}", sha256(&artifact.bytes))),
        })
        .collect::<Vec<_>>();
    let present = artifacts
        .iter()
        .map(|artifact| artifact.path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let lock_path = root.join(lock_relative);
    let previous = match std::fs::read(&lock_path) {
        Ok(bytes) => {
            if bytes.len() as u64 > file_limit {
                return Err(CodegenError::Core(ThemeError::Config(
                    "existing theme lock exceeds the configured file limit".into(),
                )));
            }
            let previous =
                serde_json::from_slice::<PreviousThemeLock>(&bytes).map_err(|source| {
                    CodegenError::Core(ThemeError::Json {
                        path: PathBuf::from(lock_relative),
                        source,
                    })
                })?;
            if previous.schema_version != "1.0.0" {
                return Err(CodegenError::Core(ThemeError::Config(
                    "existing theme lock has an unsupported schema".into(),
                )));
            }
            Some(previous)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(CodegenError::Io {
                path: PathBuf::from(lock_relative),
                source,
            });
        }
    };
    if let Some(previous) = previous {
        for (path, output) in previous.outputs {
            if present.contains(path.as_str()) {
                continue;
            }
            if output.scope() == ChangeScope::HtmlOwnedRegion {
                continue;
            }
            if protected_inputs.contains(path.as_str()) {
                return Err(CodegenError::Core(ThemeError::Security(format!(
                    "stale generated output `{path}` is now a consumed input"
                ))));
            }
            if output.ownership() != Ownership::GeneratedLockOwned {
                return Err(CodegenError::Core(ThemeError::Config(format!(
                    "previous output `{path}` has invalid ownership"
                ))));
            }
            verify_stale_output(root, &path, output.digest(), file_limit)?;
            entries.push(ArtifactManifestEntry {
                path,
                scope: output.scope(),
                ownership: output.ownership(),
                state: DesiredArtifactState::Absent,
                digest: None,
            });
        }
    }
    ArtifactManifest::new(entries)
}

fn verify_stale_output(
    root: &Path,
    relative: &str,
    expected_digest: &str,
    file_limit: u64,
) -> Result<(), CodegenError> {
    let logical = LogicalPath::new(relative.to_owned())?;
    let path = root.join(logical.to_path_buf());
    let metadata = std::fs::symlink_metadata(&path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(relative),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CodegenError::Core(ThemeError::Security(format!(
            "stale generated output `{relative}` is not a regular file"
        ))));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(CodegenError::Core(ThemeError::Security(format!(
                "stale generated output `{relative}` has multiple hard links"
            ))));
        }
    }
    if metadata.len() > file_limit {
        return Err(CodegenError::Core(ThemeError::Limit {
            resource: "fileBytes",
            limit: file_limit,
            observed: metadata.len(),
        }));
    }
    let bytes = std::fs::read(&path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(relative),
        source,
    })?;
    let digest = format!("sha256:{}", sha256(&bytes));
    if digest != expected_digest {
        return Err(CodegenError::Conflict(format!(
            "stale generated output `{relative}` differs from its lock record"
        )));
    }
    Ok(())
}

fn protect_workspace_inputs(
    config_root: &Path,
    workspace_root: &Path,
    artifacts: &[GeneratedArtifact],
    inputs: &[&OpenedSource],
) -> Result<(), CodegenError> {
    for artifact in artifacts {
        let target = config_root.join(
            LogicalPath::new(artifact.path.clone())
                .map_err(CodegenError::Core)?
                .to_path_buf(),
        );
        if inputs
            .iter()
            .any(|input| target == workspace_root.join(input.logical_path.to_path_buf()))
        {
            return Err(CodegenError::Core(ThemeError::Security(format!(
                "generated output overlaps workspace input `{}`",
                artifact.path
            ))));
        }
    }
    Ok(())
}

fn protect_generated_ownership(
    root: &Path,
    artifacts: &[GeneratedArtifact],
    lock_relative: &str,
    file_limit: u64,
    accepted_paths: &[String],
) -> Result<(Vec<GeneratedArtifact>, BTreeMap<String, String>), CodegenError> {
    let mut requested = accepted_paths
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if requested.len() != accepted_paths.len() {
        return Err(CodegenError::Core(ThemeError::Config(
            "duplicate --accept-generated path".into(),
        )));
    }
    for path in &requested {
        LogicalPath::new(path.clone()).map_err(CodegenError::Core)?;
        if path == lock_relative {
            return Err(CodegenError::Core(ThemeError::Config(
                "the theme lock cannot be accepted as a generated conflict".into(),
            )));
        }
    }
    let lock_path = root.join(lock_relative);
    let previous = match std::fs::read(&lock_path) {
        Ok(bytes) => {
            if bytes.len() as u64 > file_limit {
                return Err(CodegenError::Core(ThemeError::Config(
                    "existing theme lock exceeds the configured file limit".into(),
                )));
            }
            let previous =
                serde_json::from_slice::<PreviousThemeLock>(&bytes).map_err(|source| {
                    CodegenError::Core(ThemeError::Json {
                        path: PathBuf::from(lock_relative),
                        source,
                    })
                })?;
            if previous.schema_version != "1.0.0" {
                return Err(CodegenError::Core(ThemeError::Config(
                    "existing theme lock has an unsupported schema".into(),
                )));
            }
            Some(previous)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(CodegenError::Io {
                path: PathBuf::from(lock_relative),
                source,
            });
        }
    };
    let mut backups = Vec::new();
    let mut accepted = BTreeMap::new();
    for artifact in artifacts {
        if artifact.ownership != Ownership::GeneratedLockOwned {
            continue;
        }
        let path = root.join(&artifact.path);
        let current = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(CodegenError::Io {
                    path: PathBuf::from(&artifact.path),
                    source,
                });
            }
        };
        let Some(current) = current else { continue };
        if current.len() as u64 > file_limit {
            return Err(CodegenError::Core(ThemeError::Config(format!(
                "existing generated output `{}` exceeds the configured file limit",
                artifact.path
            ))));
        }
        if artifact.scope == ChangeScope::HtmlOwnedRegion
            && previous
                .as_ref()
                .is_none_or(|lock| !lock.outputs.contains_key(&artifact.path))
            && !current
                .windows(b"<!-- leptos-ui-theme:start -->".len())
                .any(|window| window == b"<!-- leptos-ui-theme:start -->")
            && !current
                .windows(b"<!-- leptos-ui-theme:end -->".len())
                .any(|window| window == b"<!-- leptos-ui-theme:end -->")
        {
            continue;
        }
        let current_container_digest = format!("sha256:{}", sha256(&current));
        let current_digest = if artifact.scope == ChangeScope::HtmlOwnedRegion {
            owned_html_region(&current)?
                .map(|region| format!("sha256:{}", sha256(region)))
                .ok_or_else(|| {
                    CodegenError::Conflict(format!(
                        "owned HTML output `{}` has no theme region",
                        artifact.path
                    ))
                })?
        } else {
            current_container_digest.clone()
        };
        let expected_previous = previous
            .as_ref()
            .and_then(|lock| lock.outputs.get(&artifact.path))
            .map(PreviousOutputLock::digest);
        if expected_previous != Some(current_digest.as_str()) {
            if expected_previous.is_none() {
                return Err(CodegenError::Conflict(format!(
                    "generated output `{}` was not owned by the previous theme lock",
                    artifact.path
                )));
            }
            if !requested.remove(&artifact.path) {
                return Err(CodegenError::Conflict(format!(
                    "generated output `{}` contains unaccepted local edits",
                    artifact.path
                )));
            }
            let backup_path = format!(
                ".leptos-ui-theme/backups/{}-{}.bak",
                sha256(artifact.path.as_bytes()),
                current_container_digest.trim_start_matches("sha256:")
            );
            let physical_backup = root.join(&backup_path);
            match std::fs::read(&physical_backup) {
                Ok(existing) if existing == current => {
                    backups.push(GeneratedArtifact::generated(
                        backup_path.clone(),
                        current.clone(),
                    ));
                }
                Ok(_) => {
                    return Err(CodegenError::Conflict(format!(
                        "retained backup `{backup_path}` has unexpected bytes"
                    )));
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    backups.push(GeneratedArtifact::generated(
                        backup_path.clone(),
                        current.clone(),
                    ));
                }
                Err(source) => {
                    return Err(CodegenError::Io {
                        path: PathBuf::from(&backup_path),
                        source,
                    });
                }
            }
            accepted.insert(artifact.path.clone(), backup_path);
        }
    }
    if let Some(path) = requested.into_iter().next() {
        return Err(CodegenError::Core(ThemeError::Config(format!(
            "`--accept-generated {path}` does not name a changed generated output"
        ))));
    }
    Ok((backups, accepted))
}

fn collect_input_digests(compiler: &ThemeCompiler) -> Result<Vec<InputLock>, CodegenError> {
    let token_root = LogicalPath::new(compiler.config.token_root.clone())?;
    let mut inputs = compiler
        .source_loader()
        .read_tree(&token_root, SourceRole::TokenResolver)?;
    let resolver = LogicalPath::new(compiler.config.resolver.clone())?;
    if !inputs.iter().any(|source| source.logical_path == resolver) {
        inputs.push(
            compiler
                .source_loader()
                .read_source(&resolver, SourceRole::TokenResolver)?,
        );
    }
    inputs.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    Ok(inputs
        .into_iter()
        .map(|source| InputLock {
            root: InputRoot::AppConfig,
            path: source.logical_path.as_str().to_owned(),
            bytes_digest: source.bytes_digest,
        })
        .collect())
}

fn input_lock(kind: InputRoot, source: &OpenedSource) -> InputLock {
    InputLock {
        root: kind,
        path: source.logical_path.as_str().to_owned(),
        bytes_digest: source.bytes_digest.clone(),
    }
}

fn generated_metadata(compiler: &ThemeCompiler) -> String {
    format!(
        "leptos-ui-theme:v1 tool={} dtcg={} contract-id-b64url={} abi={} revision={} contract-digest={} config-path-b64url={}",
        env!("CARGO_PKG_VERSION"),
        compiler.config.dtcg_version,
        base64_url_no_pad(compiler.contract.contract_id.as_bytes()),
        compiler.contract.abi_version,
        compiler.contract.revision,
        compiler.contract.canonical_digest,
        base64_url_no_pad(CONFIG_FILE.as_bytes()),
    )
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0x3f) as usize] as char);
        }
    }
    output
}

pub fn apply(root: &Path, result: &BuildResult) -> Result<Vec<String>, CodegenError> {
    apply_with_wait(root, result, Duration::ZERO)
}

pub fn apply_with_wait(
    root: &Path,
    result: &BuildResult,
    lock_wait: Duration,
) -> Result<Vec<String>, CodegenError> {
    let lock_path = result
        .artifacts
        .last()
        .map(|artifact| artifact.path.as_str());
    transaction::apply_transaction_with_wait_checked(
        root,
        &result.artifacts,
        &result.plan,
        ApplyCommand::Build,
        lock_path,
        lock_wait,
        || verify_consumed_inputs(root, &result.workspace_root, &result.consumed_inputs),
    )
}

fn verify_consumed_inputs(
    config_root: &Path,
    workspace_root: &Path,
    inputs: &[ConsumedInput],
) -> Result<(), CodegenError> {
    let app_loader = SourceLoader::new(config_root, COMPILED_LIMITS)?;
    let workspace_loader = SourceLoader::new(workspace_root, COMPILED_LIMITS)?;
    for input in inputs {
        let logical = LogicalPath::new(input.path.clone())?;
        let source = match input.root {
            ConsumedInputRoot::AppConfig => {
                app_loader.read_source(&logical, SourceRole::General)?
            }
            ConsumedInputRoot::Workspace => {
                workspace_loader.read_source(&logical, SourceRole::General)?
            }
        };
        if source.bytes_digest != input.digest {
            return Err(CodegenError::Conflict(format!(
                "consumed input `{}` changed after planning",
                input.path
            )));
        }
    }
    Ok(())
}

pub fn apply_artifacts(
    root: &Path,
    artifacts: &[GeneratedArtifact],
) -> Result<Vec<String>, CodegenError> {
    apply_artifacts_for(root, artifacts, ApplyCommand::Add, None)
}

pub fn apply_artifacts_for(
    root: &Path,
    artifacts: &[GeneratedArtifact],
    command: ApplyCommand,
    theme_lock_path: Option<&str>,
) -> Result<Vec<String>, CodegenError> {
    apply_artifacts_for_with_wait(root, artifacts, command, theme_lock_path, Duration::ZERO)
}

pub fn apply_artifacts_for_with_wait(
    root: &Path,
    artifacts: &[GeneratedArtifact],
    command: ApplyCommand,
    theme_lock_path: Option<&str>,
    lock_wait: Duration,
) -> Result<Vec<String>, CodegenError> {
    let plan = plan_artifacts(root, artifacts)?;
    apply_transaction_with_wait(root, artifacts, &plan, command, theme_lock_path, lock_wait)
}

pub fn check(_root: &Path, result: &BuildResult) -> Vec<String> {
    let mut stale = result.plan.changed_paths();
    if let Some(path) = &result.manual_html_stale
        && !stale.contains(path)
    {
        stale.push(path.clone());
    }
    stale
}

pub fn revalidate_build_result(root: &Path, result: &BuildResult) -> Result<(), CodegenError> {
    result.plan.revalidate(root)?;
    verify_consumed_inputs(root, &result.workspace_root, &result.consumed_inputs)
}

pub fn generate_css(
    config: &ProjectConfig,
    profiles: &[ResolvedProfile],
    axes: &[ResolvedAxis],
) -> Result<String, CodegenError> {
    let fallback = generate_theme_blocks(config, profiles, axes, CssMode::Fallback, 2)?;
    let modern = generate_theme_blocks(config, profiles, axes, CssMode::Modern, 4)?;
    let mut css = String::from(
        "/* Generated by leptos_ui_theme. Do not edit. */\n@layer leptos-ui-kit.tokens, leptos-ui-kit.themes, leptos-ui-kit.components;\n\n@layer leptos-ui-kit.themes {\n",
    );
    css.push_str(&fallback.join("\n"));
    if !modern.is_empty() {
        css.push_str("\n  @supports (color: oklch(0 0 0)) {\n");
        css.push_str(&modern.join("\n"));
        css.push_str("  }\n");
    }
    css.push_str("}\n");
    validate_css_output(&css)?;
    Ok(css)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CssMode {
    Fallback,
    Modern,
}

fn generate_theme_blocks(
    config: &ProjectConfig,
    profiles: &[ResolvedProfile],
    axes: &[ResolvedAxis],
    mode: CssMode,
    indent: usize,
) -> Result<Vec<String>, CodegenError> {
    let default = profile(profiles, &config.profiles.default)?;
    let dark = profile(profiles, &config.profiles.system.dark)?;
    let mut blocks = Vec::new();
    let mut root_values = default
        .values
        .iter()
        .filter(|token| token.domain == TokenDomain::Theme)
        .collect::<Vec<_>>();
    for axis in axes {
        let default_axis = profile(&axis.contexts, &axis.default_context)?;
        root_values.extend(
            default_axis
                .values
                .iter()
                .filter(|token| token.domain == axis.domain),
        );
    }
    if let Some(block) = selector_block(
        ":root",
        Some(default.color_scheme),
        root_values.into_iter(),
        indent,
        mode,
    )? {
        blocks.push(block);
    }
    if let Some(dark_block) = selector_block(
        &format!(":root:not([{}])", config.selectors.theme),
        Some(dark.color_scheme),
        dark.values
            .iter()
            .filter(|token| token.domain == TokenDomain::Theme),
        indent + 2,
        mode,
    )? {
        let outer = " ".repeat(indent);
        blocks.push(format!(
            "{outer}@media (prefers-color-scheme: dark) {{\n{dark_block}{outer}}}\n"
        ));
    }
    for current in profiles {
        if let Some(block) = selector_block(
            &format!(":root[{}=\"{}\"]", config.selectors.theme, current.id),
            Some(current.color_scheme),
            current
                .values
                .iter()
                .filter(|token| token.domain == TokenDomain::Theme),
            indent,
            mode,
        )? {
            blocks.push(block);
        }
    }
    for axis in axes {
        if let Some((query, context)) = &axis.system {
            let current = profile(&axis.contexts, context)?;
            if let Some(system_block) = selector_block(
                &format!(":root:not([{}])", axis.attribute),
                None,
                current
                    .values
                    .iter()
                    .filter(|token| token.domain == axis.domain),
                indent + 2,
                mode,
            )? {
                let outer = " ".repeat(indent);
                blocks.push(format!(
                    "{outer}@media {query} {{\n{system_block}{outer}}}\n"
                ));
            }
        }
        for current in &axis.contexts {
            if let Some(block) = selector_block(
                &format!(":root[{}=\"{}\"]", axis.attribute, current.id),
                None,
                current
                    .values
                    .iter()
                    .filter(|token| token.domain == axis.domain),
                indent,
                mode,
            )? {
                blocks.push(block);
            }
        }
    }
    Ok(blocks)
}

#[derive(Clone, Debug)]
pub struct ResolvedAxis {
    pub domain: TokenDomain,
    pub attribute: String,
    pub default_context: String,
    pub system: Option<(String, String)>,
    pub contexts: Vec<ResolvedProfile>,
}

fn validate_css_output(css: &str) -> Result<(), CodegenError> {
    if css.contains('\r')
        || !css.ends_with('\n')
        || css.contains("!important")
        || css.contains("forced-color-adjust")
        || css.contains("@import")
        || css.matches("@supports (color: oklch(0 0 0))").count() > 1
    {
        return Err(CodegenError::Core(ThemeError::Security(
            "generated CSS violates the closed output grammar".into(),
        )));
    }
    Ok(())
}

fn selector_block<'a>(
    selector: &str,
    color_scheme: Option<ColorScheme>,
    values: impl Iterator<Item = &'a ResolvedToken>,
    indent: usize,
    mode: CssMode,
) -> Result<Option<String>, CodegenError> {
    let values: Vec<_> = values
        .filter(|token| mode == CssMode::Fallback || has_modern_color(token))
        .collect();
    let color_scheme = (mode == CssMode::Fallback)
        .then_some(color_scheme)
        .flatten();
    if values.is_empty() && color_scheme.is_none() {
        return Ok(None);
    }
    let outer = " ".repeat(indent);
    let inner = " ".repeat(indent + 2);
    let mut block = format!("{outer}{selector} {{\n");
    if let Some(scheme) = color_scheme {
        block.push_str(&format!(
            "{inner}color-scheme: {};\n",
            match scheme {
                ColorScheme::Light => "light",
                ColorScheme::Dark => "dark",
            }
        ));
    }
    for token in values {
        block.push_str(&format!(
            "{inner}{}: {};\n",
            token.css_custom_property,
            serialize_css(token, mode)?
        ));
    }
    block.push_str(&format!("{outer}}}\n"));
    Ok(Some(block))
}

fn has_modern_color(token: &ResolvedToken) -> bool {
    token.alias_of.is_none() && matches!(token.token_type.as_str(), "color" | "shadow")
}

fn serialize_css(token: &ResolvedToken, mode: CssMode) -> Result<String, CodegenError> {
    let fail = |reason: &str| CodegenError::CssValue {
        path: token.path.clone(),
        reason: reason.into(),
    };
    if token.alias_of.is_some() {
        return token
            .value
            .as_str()
            .filter(|value| {
                value.starts_with("var(--kit-")
                    && value.ends_with(')')
                    && !value.contains([';', '{', '}', '\n', '\r'])
            })
            .map(str::to_owned)
            .ok_or_else(|| fail("invalid deprecated property alias"));
    }
    match token.token_type.as_str() {
        "color" => match mode {
            CssMode::Fallback => serialize_color_fallback(&token.value),
            CssMode::Modern => serialize_color_modern(&token.value),
        }
        .map_err(CodegenError::Core),
        "dimension" => {
            serialize_unit(&token.value, &["px", "rem"], 1.0, false).map_err(|reason| fail(&reason))
        }
        "duration" => serialize_duration(&token.value).map_err(|reason| fail(&reason)),
        "number" => token
            .value
            .as_f64()
            .ok_or_else(|| fail("number must be numeric"))
            .and_then(|value| format_css_number(value).map_err(CodegenError::Core)),
        "cubicBezier" => serialize_cubic_bezier(&token.value).map_err(|reason| fail(&reason)),
        "fontFamily" => serialize_font_family(&token.value).map_err(|reason| fail(&reason)),
        "shadow" => serialize_shadow(&token.value, mode).map_err(CodegenError::Core),
        _ => Err(fail("unsupported ABI v1 serializer type")),
    }
}

fn serialize_unit(
    value: &serde_json::Value,
    units: &[&str],
    multiplier: f64,
    nonnegative: bool,
) -> Result<String, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "unit value must be an object".to_owned())?;
    if object.len() != 2 || !object.contains_key("value") || !object.contains_key("unit") {
        return Err("unit value has missing or unknown members".into());
    }
    let number = object["value"]
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| "unit value must be finite".to_owned())?;
    if nonnegative && number < 0.0 {
        return Err("unit value cannot be negative".into());
    }
    let unit = object["unit"]
        .as_str()
        .filter(|unit| units.contains(unit))
        .ok_or_else(|| "unit is unsupported".to_owned())?;
    format_css_number(number * multiplier)
        .map(|number| format!("{number}{unit}"))
        .map_err(|error| error.to_string())
}

fn serialize_duration(value: &serde_json::Value) -> Result<String, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "duration must be an object".to_owned())?;
    let unit = object
        .get("unit")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "duration unit is missing".to_owned())?;
    let multiplier = match unit {
        "ms" => 1.0,
        "s" => 1_000.0,
        _ => return Err("duration unit is unsupported".into()),
    };
    let mut rendered = serialize_unit(value, &["ms", "s"], multiplier, true)?;
    if unit == "s" {
        rendered.truncate(rendered.len() - 1);
        rendered.push_str("ms");
    }
    Ok(rendered)
}

fn serialize_cubic_bezier(value: &serde_json::Value) -> Result<String, String> {
    let values = value
        .as_array()
        .filter(|values| values.len() == 4)
        .ok_or_else(|| "cubicBezier must contain four values".to_owned())?;
    let values = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let value = value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| "cubicBezier value must be finite".to_owned())?;
            if matches!(index, 0 | 2) && !(0.0..=1.0).contains(&value) {
                return Err("cubicBezier x components must be within 0..1".into());
            }
            format_css_number(value).map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!(
        "cubic-bezier({}, {}, {}, {})",
        values[0], values[1], values[2], values[3]
    ))
}

fn serialize_font_family(value: &serde_json::Value) -> Result<String, String> {
    let families = if let Some(value) = value.as_str() {
        vec![value]
    } else {
        value
            .as_array()
            .ok_or_else(|| "fontFamily must be a string or array".to_owned())?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| "fontFamily entry must be a string".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    const GENERICS: &[&str] = &[
        "serif",
        "sans-serif",
        "monospace",
        "cursive",
        "fantasy",
        "system-ui",
        "ui-serif",
        "ui-sans-serif",
        "ui-monospace",
        "ui-rounded",
        "math",
        "fangsong",
    ];
    Ok(families
        .into_iter()
        .map(|family| {
            if GENERICS.contains(&family) {
                family.to_owned()
            } else {
                format!("\"{}\"", family.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(", "))
}

fn serialize_shadow(value: &serde_json::Value, mode: CssMode) -> Result<String, ThemeError> {
    let values = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(value));
    values
        .iter()
        .map(|value| serialize_shadow_entry(value, mode))
        .collect::<Result<Vec<_>, _>>()
        .map(|values| values.join(", "))
}

fn serialize_shadow_entry(value: &serde_json::Value, mode: CssMode) -> Result<String, ThemeError> {
    let object = value
        .as_object()
        .ok_or_else(|| ThemeError::Resolution("shadow entry must be an object".into()))?;
    let dimension = |name: &str| {
        object
            .get(name)
            .ok_or_else(|| ThemeError::Resolution(format!("shadow `{name}` is missing")))
            .and_then(|value| {
                serialize_unit(value, &["px", "rem"], 1.0, name == "blur")
                    .map_err(ThemeError::Resolution)
            })
    };
    let color_value = object
        .get("color")
        .ok_or_else(|| ThemeError::Resolution("shadow color is missing".into()))?;
    let color = match mode {
        CssMode::Fallback => serialize_color_fallback(color_value)?,
        CssMode::Modern => serialize_color_modern(color_value)?,
    };
    let prefix = if object
        .get("inset")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        "inset "
    } else {
        ""
    };
    Ok(format!(
        "{prefix}{} {} {} {} {color}",
        dimension("offsetX")?,
        dimension("offsetY")?,
        dimension("blur")?,
        dimension("spread")?
    ))
}

pub fn generate_rust(config: &ProjectConfig, profiles: &[ResolvedProfile]) -> String {
    let mut output = String::from(
        r#"// Generated by leptos_ui_theme. Do not edit.

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ThemeId(&'static str);

impl ThemeId {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemeColorScheme {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThemeMetadata {
    pub id: ThemeId,
    pub label: Option<&'static str>,
    pub color_scheme: ThemeColorScheme,
}

"#,
    );
    output.push_str(&format!(
        "pub const STORAGE_KEY: &str = {:?};\n",
        config.storage_key
    ));
    output.push_str(&format!(
        "pub const THEME_ATTRIBUTE: &str = {:?};\n",
        config.selectors.theme
    ));
    output.push_str("pub const BOOTSTRAP_ATTRIBUTE: &str = \"data-leptos-ui-theme-bootstrap\";\n");
    output.push_str(
        "pub const BOOTSTRAP_OUTCOME_PROPERTY: &str = \"__LEPTOS_UI_THEME_BOOTSTRAP_OUTCOME_V1__\";\n",
    );
    output.push_str(&format!(
        "pub const BOOTSTRAP_ENABLED: bool = {};\n\n",
        config.bootstrap.mode != BootstrapMode::Disabled
    ));
    for profile in profiles {
        output.push_str(&format!(
            "pub const {}: ThemeId = ThemeId({:?});\n",
            theme_constant(&profile.id),
            profile.id
        ));
    }
    output.push('\n');
    output.push_str("#[rustfmt::skip]\npub const THEME_IDS: &[ThemeId] = &[\n");
    for profile in profiles {
        output.push_str(&format!("    {},\n", theme_constant(&profile.id)));
    }
    output.push_str("];\n\n");
    output.push_str(
        "#[must_use]\npub fn parse_theme_id(value: &str) -> Option<ThemeId> {\n    match value {\n",
    );
    for profile in profiles {
        output.push_str(&format!(
            "        {:?} => Some({}),\n",
            profile.id,
            theme_constant(&profile.id)
        ));
    }
    output.push_str("        _ => None,\n    }\n}\n\n");
    output.push_str(&format!(
        "pub const DEFAULT_THEME: ThemeId = {};\n",
        theme_constant(&config.profiles.default)
    ));
    output.push_str(&format!(
        "pub const SYSTEM_LIGHT_THEME: ThemeId = {};\n",
        theme_constant(&config.profiles.system.light)
    ));
    output.push_str(&format!(
        "pub const SYSTEM_DARK_THEME: ThemeId = {};\n\n",
        theme_constant(&config.profiles.system.dark)
    ));
    output.push_str("pub const THEMES: &[ThemeMetadata] = &[\n");
    for profile in profiles {
        output.push_str(&format!(
            "    ThemeMetadata {{\n        id: {},\n        label: {},\n        color_scheme: ThemeColorScheme::{},\n    }},\n",
            theme_constant(&profile.id),
            profile
                .label
                .as_ref()
                .map_or("None".into(), |label| format!("Some({label:?})")),
            match profile.color_scheme {
                ColorScheme::Light => "Light",
                ColorScheme::Dark => "Dark",
            }
        ));
    }
    output.push_str("];\n");
    output
}

fn theme_constant(id: &str) -> String {
    format!("THEME_{}", id.replace('-', "_").to_ascii_uppercase())
}

pub fn bootstrap_script(
    config: &ProjectConfig,
    profiles: &[ResolvedProfile],
) -> Result<String, CodegenError> {
    let ids = profiles
        .iter()
        .map(|profile| script_json_string(&profile.id))
        .collect::<Vec<_>>()
        .join(",");
    let key = script_json_string(&config.storage_key);
    let attribute = script_json_string(&config.selectors.theme);
    let marker = script_json_string("data-leptos-ui-theme-bootstrap");
    let outcome = script_json_string("__LEPTOS_UI_THEME_BOOTSTRAP_OUTCOME_V1__");
    Ok(format!(
        "(()=>{{const a=[{ids}],k={key},n={attribute},m={marker},o={outcome},r=document.documentElement;let s=\"ok\",p=\"system\",t;try{{t=globalThis.localStorage;if(t===null||t===undefined)s=\"unavailable\"}}catch(_){{s=\"unavailable\"}}if(s===\"ok\"){{try{{const v=t.getItem(k);if(v!==null&&v!==\"system\"&&a.includes(v))p=v}}catch(_){{s=\"read-failed\"}}}}let d=\"ok\";try{{if(p===\"system\")r.removeAttribute(n);else r.setAttribute(n,p);r.setAttribute(m,\"v1:\"+p)}}catch(_){{d=\"apply-failed\"}}try{{Object.defineProperty(globalThis,o,{{value:\"v1:\"+s+\":\"+d,enumerable:false,writable:false,configurable:true}})}}catch(_){{}}}})();"
    ))
}

fn script_json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for scalar in value.chars() {
        match scalar {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{0009}' => output.push_str("\\t"),
            '\u{000a}' => output.push_str("\\n"),
            '\u{000c}' => output.push_str("\\f"),
            '\u{000d}' => output.push_str("\\r"),
            '\u{0000}'..='\u{001f}' => {
                output.push_str(&format!("\\u{:04x}", scalar as u32));
            }
            '<' => output.push_str("\\u003c"),
            '\u{2028}' => output.push_str("\\u2028"),
            '\u{2029}' => output.push_str("\\u2029"),
            _ => output.push(scalar),
        }
    }
    output.push('"');
    output
}

fn csp_source(script: &[u8]) -> String {
    let digest = Sha256::digest(script);
    format!("'sha256-{}'", base64_standard(&digest))
}

fn base64_standard(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn html_region(
    config: &ProjectConfig,
    profiles: &[ResolvedProfile],
    index_path: &str,
    script: &str,
) -> Result<String, CodegenError> {
    let theme_href = relative_asset(index_path, &config.outputs.css)?;
    let theme_href = html_escape(&theme_href);
    let mut lines = vec![
        "<!-- leptos-ui-theme:start -->".to_string(),
        "<meta name=\"color-scheme\" content=\"light dark\">".to_string(),
        format!("<link data-trunk rel=\"css\" href=\"{theme_href}\">"),
    ];
    match config.bootstrap.mode {
        BootstrapMode::InlineCspHash => lines.push(format!("<script>{script}</script>")),
        BootstrapMode::InlineCspNonceTemplate => lines.push(format!(
            "<script nonce=\"{{{{LEPTOS_UI_THEME_NONCE}}}}\">{script}</script>"
        )),
        BootstrapMode::ExternalSync => {
            let external = config.bootstrap.external.as_ref().ok_or_else(|| {
                CodegenError::Core(ThemeError::Config(
                    "external bootstrap config is missing".into(),
                ))
            })?;
            let copy_href = html_escape(&relative_asset(index_path, &external.output_path)?);
            let served_parent = Path::new(&external.served_path)
                .parent()
                .and_then(Path::to_str)
                .unwrap_or("");
            let target = if served_parent.is_empty() {
                String::new()
            } else {
                format!(" data-target-path=\"{}\"", html_escape(served_parent))
            };
            let public = external.public_path.clone().unwrap_or_else(|| {
                format!(
                    "{}{}",
                    config.html.public_base_path,
                    percent_encode_path(&external.served_path)
                )
            });
            lines.push(format!(
                "<link data-trunk rel=\"copy-file\" href=\"{copy_href}\"{target}>"
            ));
            lines.push(format!(
                "<script src=\"{}\"></script>",
                html_escape(&public)
            ));
        }
        BootstrapMode::Disabled => {}
    }
    let _ = profiles;
    lines.push("<!-- leptos-ui-theme:end -->".into());
    Ok(lines.join("\n") + "\n")
}

fn select_index(root: &Path, config: &ProjectConfig) -> Result<PathBuf, CodegenError> {
    let candidates: Vec<&str> = if let Some(path) = config.html.index_path.as_deref() {
        vec![path]
    } else {
        config
            .html
            .index_candidates
            .as_ref()
            .ok_or_else(|| {
                CodegenError::Core(ThemeError::Config("index candidates are missing".into()))
            })?
            .iter()
            .map(String::as_str)
            .collect()
    };
    let existing: Vec<PathBuf> = candidates
        .into_iter()
        .map(|path| root.join(path))
        .filter(|path| path.is_file() && !path.is_symlink())
        .collect();
    match existing.as_slice() {
        [path] => Ok(path.clone()),
        [] => Err(CodegenError::Core(ThemeError::Config(
            "no configured index candidate exists".into(),
        ))),
        _ => Err(CodegenError::Core(ThemeError::Config(
            "multiple configured index candidates exist".into(),
        ))),
    }
}

struct HtmlLayout {
    newline: &'static str,
    insertion_offset: usize,
    owned_region: Option<(usize, usize)>,
}

fn patch_index(
    index: &[u8],
    canonical_region: &str,
    kit_href: &str,
) -> Result<Vec<u8>, CodegenError> {
    let layout = inspect_html(index, kit_href)?;
    let region = canonical_region.replace('\n', layout.newline);
    let (start, end) = layout
        .owned_region
        .unwrap_or((layout.insertion_offset, layout.insertion_offset));
    let mut output = Vec::with_capacity(index.len() - (end - start) + region.len());
    output.extend_from_slice(&index[..start]);
    output.extend_from_slice(region.as_bytes());
    output.extend_from_slice(&index[end..]);
    Ok(output)
}

fn remove_owned_html_region(index: &[u8], kit_href: &str) -> Result<Vec<u8>, CodegenError> {
    let layout = inspect_html(index, kit_href)?;
    let (start, end) = layout.owned_region.ok_or_else(|| {
        CodegenError::Conflict("selected index has no owned theme region to remove".into())
    })?;
    let mut output = Vec::with_capacity(index.len() - (end - start));
    output.extend_from_slice(&index[..start]);
    output.extend_from_slice(&index[end..]);
    Ok(output)
}

fn inspect_html(index: &[u8], kit_href: &str) -> Result<HtmlLayout, CodegenError> {
    let text = std::str::from_utf8(index)
        .map_err(|_| CodegenError::Core(ThemeError::Config("index HTML must be UTF-8".into())))?;
    if text.contains('\0') || text.starts_with('\u{feff}') {
        return Err(CodegenError::Core(ThemeError::Config(
            "index HTML contains a forbidden scalar".into(),
        )));
    }
    let crlf = text.contains("\r\n");
    let without_crlf = text.replace("\r\n", "");
    if without_crlf.contains('\r') || (crlf && without_crlf.contains('\n')) {
        return Err(CodegenError::Core(ThemeError::Config(
            "index HTML uses mixed line endings".into(),
        )));
    }
    let newline = if crlf { "\r\n" } else { "\n" };
    validate_parsed_document(text)?;

    let escaped_kit_href = html_escape(kit_href);
    let mut offset = 0usize;
    let mut kit_lines = Vec::new();
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    for line in text.split_inclusive(newline) {
        let content = line.strip_suffix(newline).unwrap_or(line);
        if exact_kit_link_line(content, &escaped_kit_href) {
            if !line.ends_with(newline) {
                return Err(CodegenError::Core(ThemeError::Config(
                    "verified kit stylesheet link must end at a line boundary".into(),
                )));
            }
            kit_lines.push((offset, offset + line.len()));
        }
        if content == "<!-- leptos-ui-theme:start -->" {
            starts.push(offset);
        }
        if content == "<!-- leptos-ui-theme:end -->" {
            if !line.ends_with(newline) {
                return Err(CodegenError::Core(ThemeError::Config(
                    "theme end marker must include the selected line ending".into(),
                )));
            }
            ends.push(offset + line.len());
        }
        offset += line.len();
    }
    if kit_lines.len() != 1 {
        return Err(CodegenError::Core(ThemeError::Config(
            "index HTML must contain exactly one verified kit stylesheet line".into(),
        )));
    }
    validate_kit_node_and_order(text, kit_href)?;
    let insertion_offset = kit_lines[0].1;
    let owned_region = match (starts.as_slice(), ends.as_slice()) {
        ([], []) => None,
        ([start], [end]) if *start == insertion_offset && start < end => Some((*start, *end)),
        _ => {
            return Err(CodegenError::Core(ThemeError::Config(
                "index HTML has ambiguous or misplaced theme markers".into(),
            )));
        }
    };
    if text.matches("<!-- leptos-ui-theme:start -->").count() != starts.len()
        || text.matches("<!-- leptos-ui-theme:end -->").count() != ends.len()
    {
        return Err(CodegenError::Core(ThemeError::Config(
            "theme markers must be complete line contents".into(),
        )));
    }
    Ok(HtmlLayout {
        newline,
        insertion_offset,
        owned_region,
    })
}

fn validate_parsed_document(text: &str) -> Result<(), CodegenError> {
    let options = ParseOpts {
        tokenizer: TokenizerOpts {
            exact_errors: true,
            discard_bom: false,
            profile: false,
            initial_state: None,
            last_start_tag_name: None,
        },
        tree_builder: TreeBuilderOpts {
            exact_errors: true,
            scripting_enabled: true,
            iframe_srcdoc: false,
            drop_doctype: false,
            quirks_mode: html5ever::tree_builder::QuirksMode::NoQuirks,
        },
    };
    let dom = parse_document(RcDom::default(), options).one(text);
    if !dom.errors.borrow().is_empty() {
        return Err(CodegenError::Core(ThemeError::Config(
            "index HTML is outside the strict parser profile".into(),
        )));
    }
    let elements = collect_elements(&dom.document);
    for forbidden in ["template", "svg", "math"] {
        if elements
            .iter()
            .any(|node| element_name(node) == Some(forbidden))
        {
            return Err(CodegenError::Core(ThemeError::Config(format!(
                "index HTML cannot contain `{forbidden}`"
            ))));
        }
    }
    let unique = |name: &str| {
        elements
            .iter()
            .filter(|node| element_name(node) == Some(name))
            .cloned()
            .collect::<Vec<_>>()
    };
    let html = unique("html");
    let head = unique("head");
    let body = unique("body");
    if html.len() != 1
        || head.len() != 1
        || body.len() != 1
        || !is_parent(&head[0], &html[0])
        || !is_parent(&body[0], &html[0])
        || !has_explicit_tag_pair(text, "html")
        || !has_explicit_tag_pair(text, "head")
        || !has_explicit_tag_pair(text, "body")
    {
        return Err(CodegenError::Core(ThemeError::Config(
            "index HTML requires explicit unique html, head, and body elements".into(),
        )));
    }
    let child_elements = html[0]
        .children
        .borrow()
        .iter()
        .filter(|node| element_name(node).is_some())
        .cloned()
        .collect::<Vec<_>>();
    if child_elements.len() != 2
        || !std::rc::Rc::ptr_eq(&child_elements[0], &head[0])
        || !std::rc::Rc::ptr_eq(&child_elements[1], &body[0])
    {
        return Err(CodegenError::Core(ThemeError::Config(
            "head and body must be explicit ordered children of html".into(),
        )));
    }
    Ok(())
}

fn collect_elements(root: &Handle) -> Vec<Handle> {
    let mut output = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(node) = pending.pop() {
        if matches!(node.data, NodeData::Element { .. }) {
            output.push(node.clone());
        }
        let children = node.children.borrow();
        pending.extend(children.iter().rev().cloned());
    }
    output
}

fn element_name(node: &Handle) -> Option<&str> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.as_ref()),
        _ => None,
    }
}

fn is_parent(child: &Handle, parent: &Handle) -> bool {
    child
        .parent
        .take()
        .and_then(|weak| {
            child.parent.set(Some(weak.clone()));
            weak.upgrade()
        })
        .is_some_and(|actual| std::rc::Rc::ptr_eq(&actual, parent))
}

fn has_explicit_tag_pair(text: &str, name: &str) -> bool {
    source_tag_count(text, name, false) == 1 && source_tag_count(text, name, true) == 1
}

fn source_tag_count(text: &str, name: &str, end: bool) -> usize {
    let needle = if end {
        format!("</{name}")
    } else {
        format!("<{name}")
    };
    text.match_indices(&needle)
        .filter(|(offset, _)| {
            text.as_bytes()
                .get(offset + needle.len())
                .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>')
        })
        .count()
}

fn exact_kit_link_line(line: &str, escaped_href: &str) -> bool {
    let line = line.trim_matches([' ', '\t']);
    let attributes = [
        "data-trunk".to_owned(),
        "rel=\"css\"".to_owned(),
        format!("href=\"{escaped_href}\""),
    ];
    [
        [0usize, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ]
    .iter()
    .any(|order| {
        format!(
            "<link {} {} {}>",
            attributes[order[0]], attributes[order[1]], attributes[order[2]]
        ) == line
    })
}

fn validate_kit_node_and_order(text: &str, kit_href: &str) -> Result<(), CodegenError> {
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(text);
    let head = dom
        .document
        .children
        .borrow()
        .iter()
        .find_map(find_head)
        .ok_or_else(|| CodegenError::Core(ThemeError::Config("index head is missing".into())))?;
    let children = head.children.borrow();
    let mut kit_index = None;
    for (index, node) in children.iter().enumerate() {
        let NodeData::Element { name, attrs, .. } = &node.data else {
            continue;
        };
        if name.local.as_ref() != "link" {
            continue;
        }
        let attrs = attrs.borrow();
        let data_trunk = attribute(&attrs, "data-trunk");
        let rel = attribute(&attrs, "rel");
        let href = attribute(&attrs, "href");
        if attrs.len() == 3
            && data_trunk == Some("")
            && rel == Some("css")
            && href == Some(kit_href)
            && kit_index.replace(index).is_some()
        {
            return Err(CodegenError::Core(ThemeError::Config(
                "index HTML contains duplicate verified kit stylesheet links".into(),
            )));
        }
    }
    let kit_index = kit_index.ok_or_else(|| {
        CodegenError::Core(ThemeError::Config(
            "verified kit stylesheet is not a direct child of head".into(),
        ))
    })?;
    if children[..kit_index].iter().any(is_application_stylesheet) {
        return Err(CodegenError::Core(ThemeError::Config(
            "verified kit stylesheet must precede application stylesheets".into(),
        )));
    }
    Ok(())
}

fn find_head(node: &Handle) -> Option<Handle> {
    if element_name(node) == Some("head") {
        return Some(node.clone());
    }
    node.children.borrow().iter().find_map(find_head)
}

fn attribute<'a>(attrs: &'a [html5ever::Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|attribute| attribute.name.local.as_ref() == name)
        .map(|attribute| attribute.value.as_ref())
}

fn is_application_stylesheet(node: &Handle) -> bool {
    let NodeData::Element { name, attrs, .. } = &node.data else {
        return false;
    };
    if name.local.as_ref() == "style" {
        return true;
    }
    if name.local.as_ref() != "link" {
        return false;
    }
    let attrs = attrs.borrow();
    attribute(&attrs, "rel") == Some("stylesheet")
        || (attribute(&attrs, "rel") == Some("css") && attribute(&attrs, "data-trunk").is_some())
}

fn marker_offsets(index: &[u8]) -> Result<Option<(usize, usize)>, CodegenError> {
    let text = std::str::from_utf8(index)
        .map_err(|_| CodegenError::Core(ThemeError::Config("index HTML must be UTF-8".into())))?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let start = format!("<!-- leptos-ui-theme:start -->{newline}");
    let end = format!("<!-- leptos-ui-theme:end -->{newline}");
    let starts: Vec<_> = text.match_indices(&start).collect();
    let ends: Vec<_> = text.match_indices(&end).collect();
    match (starts.as_slice(), ends.as_slice()) {
        ([], []) => Ok(None),
        ([(start_offset, _)], [(end_offset, _)]) if start_offset < end_offset => {
            let end_offset = end_offset + end.len();
            Ok(Some((*start_offset, end_offset)))
        }
        _ => Err(CodegenError::Core(ThemeError::Config(
            "index HTML has ambiguous theme markers".into(),
        ))),
    }
}

fn owned_html_region(index: &[u8]) -> Result<Option<&[u8]>, CodegenError> {
    Ok(marker_offsets(index)?.map(|(start, end)| &index[start..end]))
}

fn html_exterior_digest(index: &[u8]) -> Result<String, CodegenError> {
    let (prefix, suffix) = marker_offsets(index)?.map_or((index, &[][..]), |(start, end)| {
        (&index[..start], &index[end..])
    });
    Ok(hash_html_exterior(prefix, suffix))
}

fn html_exterior_digest_for_index(index: &[u8], kit_href: &str) -> Result<String, CodegenError> {
    let layout = inspect_html(index, kit_href)?;
    let (start, end) = layout
        .owned_region
        .unwrap_or((layout.insertion_offset, layout.insertion_offset));
    Ok(hash_html_exterior(&index[..start], &index[end..]))
}

fn hash_html_exterior(prefix: &[u8], suffix: &[u8]) -> String {
    let mut domain = b"leptos-ui-theme/html-exterior/v1\0".to_vec();
    domain.extend_from_slice(&(prefix.len() as u64).to_be_bytes());
    domain.extend_from_slice(prefix);
    domain.extend_from_slice(suffix);
    format!("sha256:{}", sha256(&domain))
}

fn relative_asset(index_path: &str, target_path: &str) -> Result<String, CodegenError> {
    let index_parent: Vec<_> = Path::new(index_path)
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .collect();
    let target: Vec<_> = Path::new(target_path).components().collect();
    let common = index_parent
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = vec!["..".to_string(); index_parent.len() - common];
    for component in &target[common..] {
        parts.push(component.as_os_str().to_string_lossy().into_owned());
    }
    if parts.is_empty() {
        return Err(CodegenError::Core(ThemeError::Config(
            "asset path cannot equal index directory".into(),
        )));
    }
    Ok(parts.join("/"))
}

fn relative_workspace_asset(
    workspace_root: &Path,
    config_root: &Path,
    index_path: &str,
    target_path: &str,
) -> Result<String, CodegenError> {
    let config_relative = config_root.strip_prefix(workspace_root).map_err(|_| {
        CodegenError::Core(ThemeError::Security(
            "app config root is outside the security workspace root".into(),
        ))
    })?;
    let index_logical = LogicalPath::new(index_path.to_owned())?;
    let workspace_index = config_relative.join(index_logical.to_path_buf());
    relative_asset(&workspace_index.to_string_lossy(), target_path)
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn percent_encode_path(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte == b'/' || byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
        {
            output.push(byte as char);
        } else {
            output.push('%');
            output.push(char::from(b"0123456789ABCDEF"[(byte >> 4) as usize]));
            output.push(char::from(b"0123456789ABCDEF"[(byte & 0x0f) as usize]));
        }
    }
    output
}

fn profile<'a>(
    profiles: &'a [ResolvedProfile],
    id: &str,
) -> Result<&'a ResolvedProfile, CodegenError> {
    profiles
        .iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| CodegenError::Core(ThemeError::Config(format!("unknown profile `{id}`"))))
}

#[cfg(test)]
mod tests {
    use super::{
        ArtifactManifest, ArtifactManifestEntry, ChangeOperation, ChangeScope, ConsumedInput,
        ConsumedInputRoot, CssMode, DependencyState, DesiredArtifactState, GeneratedArtifact,
        Ownership, ResolvedAxis, apply_artifacts, apply_transaction, bootstrap_script, csp_source,
        default_dependency_records, generate_css, generate_rust, html_exterior_digest,
        html_exterior_digest_for_index, owned_html_region, patch_index, plan_manifest,
        relative_workspace_asset, remove_owned_html_region, serialize_css,
        validate_dependency_records, verify_consumed_inputs,
    };
    use crate::ApplyCommand;
    use leptos_ui_theme_core::{
        ColorScheme, ProjectConfig, ResolvedProfile, ResolvedToken, TokenDomain, format_css_number,
    };
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn temporary_directory() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "leptos-ui-theme-codegen-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create temporary directory");
        path
    }

    #[test]
    fn numbers_are_stable() {
        assert_eq!(format_css_number(-0.0).unwrap(), "0");
        assert_eq!(format_css_number(1.25).unwrap(), "1.25");
    }

    #[test]
    fn lock_schema_has_the_immutable_identity() {
        let schema: serde_json::Value = serde_json::from_str(super::LOCK_SCHEMA_JSON).unwrap();
        assert_eq!(schema["$id"], super::LOCK_SCHEMA);
        assert_eq!(
            schema["$defs"]["output"]["properties"]["ownership"]["enum"][3],
            "external kit-owned"
        );
    }

    #[test]
    fn oklch_uses_valid_css_function_syntax() {
        let token = oklch_token();
        assert_eq!(
            serialize_css(&token, CssMode::Modern).expect("serialize color"),
            "oklch(0.62 0.2 260 / 0.8)"
        );
        assert!(
            serialize_css(&token, CssMode::Fallback)
                .expect("serialize fallback")
                .starts_with('#')
        );
    }

    fn oklch_token() -> ResolvedToken {
        ResolvedToken {
            path: "color.primary".into(),
            token_type: "color".into(),
            css_custom_property: "--kit-color-primary".into(),
            domain: TokenDomain::Theme,
            value: serde_json::json!({
                "colorSpace": "oklch",
                "components": [0.62, 0.2, 260.0],
                "alpha": 0.8
            }),
            provenance: Vec::new(),
            alias_of: None,
        }
    }

    #[test]
    fn modern_colors_are_isolated_in_the_final_supports_block() {
        let config = ProjectConfig::default();
        let profiles = [
            ResolvedProfile {
                id: "light".into(),
                label: None,
                color_scheme: ColorScheme::Light,
                inputs: Default::default(),
                values: vec![oklch_token()],
                semantic_digest: String::new(),
            },
            ResolvedProfile {
                id: "dark".into(),
                label: None,
                color_scheme: ColorScheme::Dark,
                inputs: Default::default(),
                values: vec![oklch_token()],
                semantic_digest: String::new(),
            },
        ];
        let css = generate_css(&config, &profiles, &[]).expect("generate CSS");
        let supports = css
            .find("@supports (color: oklch(0 0 0))")
            .expect("modern supports block");
        assert!(css[..supports].contains("--kit-color-primary: #"));
        assert!(!css[..supports].contains("--kit-color-primary: oklch("));
        assert!(css[supports..].contains("--kit-color-primary: oklch("));
    }

    #[test]
    fn generated_theme_inventory_is_rustfmt_stable_for_consumers() {
        let config = ProjectConfig::default();
        let profiles = config
            .profiles
            .named
            .iter()
            .map(|profile| ResolvedProfile {
                id: profile.id.clone(),
                label: profile.label.clone(),
                color_scheme: profile.color_scheme,
                inputs: profile.inputs.clone(),
                values: Vec::new(),
                semantic_digest: String::new(),
            })
            .collect::<Vec<_>>();

        assert!(
            generate_rust(&config, &profiles)
                .contains("#[rustfmt::skip]\npub const THEME_IDS: &[ThemeId] = &[")
        );
    }

    #[test]
    fn css_emits_root_axis_defaults_and_root_scoped_explicit_selectors() {
        let config = ProjectConfig::default();
        let profiles = [
            ResolvedProfile {
                id: "light".into(),
                label: None,
                color_scheme: ColorScheme::Light,
                inputs: Default::default(),
                values: vec![oklch_token()],
                semantic_digest: String::new(),
            },
            ResolvedProfile {
                id: "dark".into(),
                label: None,
                color_scheme: ColorScheme::Dark,
                inputs: Default::default(),
                values: vec![oklch_token()],
                semantic_digest: String::new(),
            },
        ];
        let axis_token = |id: &str, value: f64| ResolvedProfile {
            id: id.into(),
            label: None,
            color_scheme: ColorScheme::Light,
            inputs: Default::default(),
            values: vec![ResolvedToken {
                path: "density.scale".into(),
                token_type: "number".into(),
                css_custom_property: "--kit-density-scale".into(),
                domain: TokenDomain::Density,
                value: value.into(),
                provenance: Vec::new(),
                alias_of: None,
            }],
            semantic_digest: String::new(),
        };
        let axes = [ResolvedAxis {
            domain: TokenDomain::Density,
            attribute: config.selectors.density.clone(),
            default_context: "comfortable".into(),
            system: None,
            contexts: vec![axis_token("compact", 0.8), axis_token("comfortable", 1.0)],
        }];
        let css = generate_css(&config, &profiles, &axes).unwrap();
        let root_end = css.find("  }\n\n").unwrap();
        assert!(css[..root_end].contains("--kit-density-scale: 1;"));
        assert!(css.contains(&format!(":root[{}=\"light\"]", config.selectors.theme)));
        assert!(css.contains(&format!(":root[{}=\"compact\"]", config.selectors.density)));
        assert!(!css.contains(&format!("\n  [{}=", config.selectors.theme)));
    }

    #[test]
    fn desired_manifest_plans_stale_generated_removal() {
        let root = temporary_directory();
        std::fs::write(root.join("stale.css"), b"old\n").unwrap();
        let manifest = ArtifactManifest::new(vec![ArtifactManifestEntry {
            path: "stale.css".into(),
            scope: ChangeScope::WholeFile,
            ownership: Ownership::GeneratedLockOwned,
            state: DesiredArtifactState::Absent,
            digest: None,
        }])
        .unwrap();
        let plan = plan_manifest(&root, &[], &manifest).unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].operation, ChangeOperation::Remove);
        assert!(plan.changes[0].after_digest.is_none());
        assert_eq!(
            apply_transaction(&root, &[], &plan, ApplyCommand::Add, None).unwrap(),
            ["stale.css"]
        );
        assert!(!root.join("stale.css").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn consumed_inputs_are_revalidated_from_secure_handles() {
        let root = temporary_directory();
        std::fs::write(root.join("input.json"), b"{}\n").unwrap();
        let input = ConsumedInput {
            root: ConsumedInputRoot::AppConfig,
            path: "input.json".into(),
            digest: format!("sha256:{}", leptos_ui_theme_core::sha256(b"{}\n")),
        };
        verify_consumed_inputs(&root, &root, std::slice::from_ref(&input)).unwrap();
        std::fs::write(root.join("input.json"), b"{\"changed\":true}\n").unwrap();
        assert!(verify_consumed_inputs(&root, &root, &[input]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn artifact_application_is_idempotent() {
        let root = temporary_directory();
        let artifacts = vec![
            GeneratedArtifact::generated("generated/theme.css", b"theme\n".to_vec()),
            GeneratedArtifact::generated("theme.lock.json", b"lock\n".to_vec()),
        ];
        assert_eq!(
            apply_artifacts(&root, &artifacts)
                .expect("first apply")
                .len(),
            2
        );
        assert!(
            apply_artifacts(&root, &artifacts)
                .expect("second apply")
                .is_empty()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("generated/theme.css"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o644
            );
        }
        std::fs::remove_dir_all(root).expect("remove temporary directory");
    }

    #[test]
    fn artifact_planning_is_byte_deterministic() {
        let root = temporary_directory();
        let artifacts = vec![
            GeneratedArtifact::generated("generated/theme.css", b"theme\n".to_vec()),
            GeneratedArtifact::generated("theme.lock.json", b"lock\n".to_vec()),
        ];
        let first = super::plan_artifacts(&root, &artifacts).expect("first plan");
        let second = super::plan_artifacts(&root, &artifacts).expect("second plan");

        assert_eq!(first, second);
        std::fs::remove_dir_all(root).expect("remove temporary directory");
    }

    #[cfg(unix)]
    #[test]
    fn artifact_planning_rejects_hardlinked_targets() {
        let root = temporary_directory();
        std::fs::write(root.join("outside.txt"), b"shared\n").expect("write shared inode");
        std::fs::hard_link(root.join("outside.txt"), root.join("generated.txt"))
            .expect("create hard link");
        let artifacts = [GeneratedArtifact::generated(
            "generated.txt",
            b"replacement\n".to_vec(),
        )];

        assert!(super::plan_artifacts(&root, &artifacts).is_err());
        assert_eq!(
            std::fs::read(root.join("outside.txt")).expect("read shared inode"),
            b"shared\n"
        );
        std::fs::remove_dir_all(root).expect("remove temporary directory");
    }

    #[cfg(unix)]
    #[test]
    fn generated_modes_converge_and_seeded_replacements_are_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let root = temporary_directory();
        std::fs::write(root.join("generated.txt"), b"same").unwrap();
        std::fs::set_permissions(
            root.join("generated.txt"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::write(root.join("seeded.txt"), b"before").unwrap();
        std::fs::set_permissions(
            root.join("seeded.txt"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let artifacts = vec![GeneratedArtifact::generated(
            "generated.txt",
            b"same".to_vec(),
        )];
        assert_eq!(
            apply_artifacts(&root, &artifacts).unwrap(),
            ["generated.txt"]
        );
        assert_eq!(
            std::fs::metadata(root.join("generated.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        let seeded = [GeneratedArtifact::seeded("seeded.txt", b"after".to_vec())];
        assert!(apply_artifacts(&root, &seeded).is_err());
        assert_eq!(std::fs::read(root.join("seeded.txt")).unwrap(), b"before");
        assert_eq!(
            std::fs::metadata(root.join("seeded.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn restrictive_umask_child() {
        use std::os::unix::fs::PermissionsExt;

        let Ok(root) = std::env::var("LEPTOS_UI_THEME_UMASK_CHILD_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let artifacts = vec![GeneratedArtifact::generated(
            "generated/theme.css",
            b"theme\n".to_vec(),
        )];
        apply_artifacts(&root, &artifacts).unwrap();
        assert_eq!(
            std::fs::metadata(root.join("generated/theme.css"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    #[cfg(unix)]
    #[test]
    fn publication_mode_is_independent_of_process_umask() {
        let root = temporary_directory();
        let current = std::env::current_exe().unwrap();
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("umask 077; exec \"$1\" --exact tests::restrictive_umask_child --nocapture")
            .arg("leptos-ui-theme-umask")
            .arg(current)
            .env("LEPTOS_UI_THEME_UMASK_CHILD_ROOT", &root)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dependency_records_allow_neutral_or_one_coherent_delivery_mode() {
        let neutral = default_dependency_records();
        validate_dependency_records(DependencyState::Pending, &neutral).unwrap();

        for mode in ["csr", "hydrate", "ssr"] {
            let mut selected = neutral.clone();
            selected[0].features.push(mode.to_owned());
            selected[1].features.push(mode.to_owned());
            validate_dependency_records(DependencyState::Pending, &selected).unwrap();
        }

        let mut mismatched = neutral.clone();
        mismatched[0].features.push("ssr".to_owned());
        mismatched[1].features.push("hydrate".to_owned());
        assert!(validate_dependency_records(DependencyState::Pending, &mismatched).is_err());

        let mut conflicting = neutral;
        conflicting[0]
            .features
            .extend(["csr".to_owned(), "ssr".to_owned()]);
        assert!(validate_dependency_records(DependencyState::Pending, &conflicting).is_err());
    }

    #[test]
    fn bootstrap_csp_source_is_scanner_compatible_and_stable() {
        let config = ProjectConfig::default();
        let profiles = config
            .profiles
            .named
            .iter()
            .map(|profile| ResolvedProfile {
                id: profile.id.clone(),
                label: profile.label.clone(),
                color_scheme: profile.color_scheme,
                inputs: profile.inputs.clone(),
                values: Vec::new(),
                semantic_digest: String::new(),
            })
            .collect::<Vec<_>>();
        let script = bootstrap_script(&config, &profiles).unwrap();
        let source = csp_source(script.as_bytes());
        assert!(source.starts_with("'sha256-"));
        assert!(source.ends_with('\''));
        assert_eq!(source.len(), "'sha256-'".len() + 44);
    }

    #[test]
    fn html_region_is_inserted_after_kit_and_preserves_the_exterior() {
        let index = b"<!doctype html>\n<html>\n<head>\n<link href=\"styles/kit.css\" data-trunk rel=\"css\">\n<link data-trunk rel=\"css\" href=\"styles/app.css\">\n</head>\n<body></body>\n</html>\n";
        let region = "<!-- leptos-ui-theme:start -->\n<meta name=\"color-scheme\" content=\"light dark\">\n<link data-trunk rel=\"css\" href=\"styles/themes.css\">\n<!-- leptos-ui-theme:end -->\n";
        let before_exterior = html_exterior_digest_for_index(index, "styles/kit.css").unwrap();
        let patched = patch_index(index, region, "styles/kit.css").unwrap();
        let text = std::str::from_utf8(&patched).unwrap();

        assert!(
            text.find("styles/kit.css").unwrap()
                < text.find("<!-- leptos-ui-theme:start -->").unwrap()
        );
        assert!(
            text.find("<!-- leptos-ui-theme:end -->").unwrap()
                < text.find("styles/app.css").unwrap()
        );
        assert_eq!(
            owned_html_region(&patched).unwrap().unwrap(),
            region.as_bytes()
        );
        assert_eq!(html_exterior_digest(&patched).unwrap(), before_exterior);
        assert_eq!(
            remove_owned_html_region(&patched, "styles/kit.css").unwrap(),
            index
        );
    }

    #[test]
    fn html_validation_rejects_application_css_before_the_kit() {
        let index = b"<!doctype html>\n<html>\n<head>\n<link data-trunk rel=\"css\" href=\"styles/app.css\">\n<link data-trunk rel=\"css\" href=\"styles/kit.css\">\n</head>\n<body></body>\n</html>\n";
        let region = "<!-- leptos-ui-theme:start -->\n<!-- leptos-ui-theme:end -->\n";

        assert!(patch_index(index, region, "styles/kit.css").is_err());
    }

    #[test]
    fn html_patching_preserves_uniform_crlf() {
        let index = b"<!doctype html>\r\n<html>\r\n<head>\r\n<link data-trunk rel=\"css\" href=\"styles/kit.css\">\r\n</head>\r\n<body></body>\r\n</html>\r\n";
        let region = "<!-- leptos-ui-theme:start -->\n<!-- leptos-ui-theme:end -->\n";
        let patched = patch_index(index, region, "styles/kit.css").unwrap();

        assert!(!patched.windows(2).any(|window| window == b"\n\n"));
        assert!(
            std::str::from_utf8(&patched)
                .unwrap()
                .contains("<!-- leptos-ui-theme:start -->\r\n")
        );
    }

    #[test]
    fn nested_app_kit_href_uses_the_workspace_path_space() {
        let workspace = Path::new("/workspace");
        let app = workspace.join("enterprise/apps/web");

        assert_eq!(
            relative_workspace_asset(
                workspace,
                &app,
                "index.html",
                "enterprise/apps/web/styles/kit.css",
            )
            .unwrap(),
            "styles/kit.css"
        );
        assert_eq!(
            relative_workspace_asset(workspace, workspace, "index.html", "styles/kit.css",)
                .unwrap(),
            "styles/kit.css"
        );
    }

    #[test]
    fn preflight_failure_does_not_write_an_earlier_artifact() {
        let root = temporary_directory();
        std::fs::write(root.join("blocked"), b"not a directory").expect("create blocking file");
        let artifacts = vec![
            GeneratedArtifact::generated("first.txt", b"first".to_vec()),
            GeneratedArtifact::generated("blocked/second.txt", b"second".to_vec()),
        ];
        assert!(apply_artifacts(&root, &artifacts).is_err());
        assert!(!root.join("first.txt").exists());
        std::fs::remove_dir_all(root).expect("remove temporary directory");
    }
}

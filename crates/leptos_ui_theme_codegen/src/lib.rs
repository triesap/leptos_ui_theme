#![forbid(unsafe_code)]
#![doc = "Deterministic artifact generation for `leptos_ui_theme`."]

mod plan;

pub use plan::{Change, ChangeOperation, ChangeScope, Ownership, PlanV1, Snapshot, plan_artifacts};

use leptos_ui_theme_core::{
    BootstrapMode, ColorScheme, ProjectConfig, ResolvedProfile, ResolvedToken, ThemeCompiler,
    ThemeError, TokenDomain, format_css_number, serialize_color_fallback, serialize_color_modern,
    sha256,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
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
    pub profiles: Vec<ResolvedProfile>,
    pub plan: PlanV1,
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

struct PendingArtifact {
    relative: String,
    path: PathBuf,
    stage: PathBuf,
    backup: PathBuf,
    previous: Option<Vec<u8>>,
    bytes: Vec<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThemeLock<'a> {
    schema_version: &'static str,
    config_digest: String,
    contract_digest: String,
    profiles: Vec<&'a str>,
    outputs: BTreeMap<&'a str, String>,
}

pub fn build(root: &Path) -> Result<BuildResult, CodegenError> {
    let compiler = ThemeCompiler::load(root)?;
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
                    system: axis
                        .system
                        .as_ref()
                        .map(|system| (system.query.clone(), system.context.clone())),
                    contexts: compiler.resolve_axis(name, &axis.contexts)?,
                });
            }
        }
    }
    let css = generate_css(&compiler.config, &profiles, &axes)?;
    let rust = generate_rust(&compiler.config, &profiles);
    let mut artifacts = vec![
        GeneratedArtifact::generated(compiler.config.outputs.css.clone(), css.into_bytes()),
        GeneratedArtifact::generated(compiler.config.outputs.rust.clone(), rust.into_bytes()),
    ];
    let selected_index = select_index(root, &compiler.config)?;
    let index_relative = selected_index
        .strip_prefix(root)
        .map_err(|_| {
            CodegenError::Core(ThemeError::Security(selected_index.display().to_string()))
        })?
        .to_string_lossy()
        .into_owned();
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
    let patched = patch_index(&index_bytes, &region)?;
    artifacts.push(GeneratedArtifact::html_region(index_relative, patched));
    let config_bytes = std::fs::read(&compiler.config_path).map_err(|source| CodegenError::Io {
        path: compiler.config_path.clone(),
        source,
    })?;
    let contract_bytes =
        std::fs::read(&compiler.contract_path).map_err(|source| CodegenError::Io {
            path: compiler.contract_path.clone(),
            source,
        })?;
    let output_digests = artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.path.as_str(),
                format!("sha256:{}", sha256(&artifact.bytes)),
            )
        })
        .collect();
    let lock = ThemeLock {
        schema_version: "1.0.0",
        config_digest: format!("sha256:{}", sha256(&config_bytes)),
        contract_digest: format!("sha256:{}", sha256(&contract_bytes)),
        profiles: profiles.iter().map(|profile| profile.id.as_str()).collect(),
        outputs: output_digests,
    };
    let mut lock_bytes = serde_json::to_vec_pretty(&lock)?;
    lock_bytes.push(b'\n');
    artifacts.push(GeneratedArtifact::generated(
        compiler.config.outputs.lock.clone(),
        lock_bytes,
    ));
    let total: usize = artifacts.iter().map(|artifact| artifact.bytes.len()).sum();
    if total as u64 > compiler.config.limits.generated_bytes
        || artifacts.iter().any(|artifact| {
            artifact.bytes.len() as u64 > compiler.config.limits.generated_artifact_bytes
        })
    {
        return Err(CodegenError::OutputLimit);
    }
    let plan = plan_artifacts(root, &artifacts)?;
    Ok(BuildResult {
        artifacts,
        profiles,
        plan,
    })
}

pub fn apply(root: &Path, result: &BuildResult) -> Result<Vec<String>, CodegenError> {
    result.plan.revalidate(root)?;
    apply_artifacts(root, &result.artifacts)
}

pub fn apply_artifacts(
    root: &Path,
    artifacts: &[GeneratedArtifact],
) -> Result<Vec<String>, CodegenError> {
    let mut paths = BTreeSet::new();
    for artifact in artifacts {
        let relative = Path::new(&artifact.path);
        if relative.is_absolute()
            || artifact.path.is_empty()
            || !relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
            || !paths.insert(&artifact.path)
        {
            return Err(CodegenError::Core(ThemeError::Security(
                artifact.path.clone(),
            )));
        }
    }
    let canonical_root = std::fs::canonicalize(root).map_err(|source| CodegenError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let mut pending = Vec::new();
    for (ordinal, artifact) in artifacts.iter().enumerate() {
        let path = root.join(&artifact.path);
        if path.is_symlink() {
            return Err(CodegenError::Core(ThemeError::Security(
                artifact.path.clone(),
            )));
        }
        let previous = read_optional(&path)?;
        if previous.as_deref() == Some(&artifact.bytes) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CodegenError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
            let canonical_parent =
                std::fs::canonicalize(parent).map_err(|source| CodegenError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            if !canonical_parent.starts_with(&canonical_root) {
                return Err(CodegenError::Core(ThemeError::Security(
                    artifact.path.clone(),
                )));
            }
        }
        let transaction = format!("{}-{ordinal:06}", std::process::id());
        let stage = sibling(&path, &format!(".leptos-ui-theme-{transaction}.stage"));
        let backup = sibling(&path, &format!(".leptos-ui-theme-{transaction}.backup"));
        if stage.exists() || backup.exists() {
            return Err(CodegenError::Conflict(artifact.path.clone()));
        }
        pending.push(PendingArtifact {
            relative: artifact.path.clone(),
            path,
            stage,
            backup,
            previous,
            bytes: artifact.bytes.clone(),
        });
    }

    for (index, item) in pending.iter().enumerate() {
        let write = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&item.stage)?;
            file.write_all(&item.bytes)?;
            file.sync_all()
        })();
        if let Err(source) = write {
            cleanup_pending(&pending[..index]);
            return Err(CodegenError::Io {
                path: item.stage.clone(),
                source,
            });
        }
    }

    for item in &pending {
        let current = match read_optional(&item.path) {
            Ok(current) => current,
            Err(error) => {
                cleanup_pending(&pending);
                return Err(error);
            }
        };
        if item.path.is_symlink() || current != item.previous {
            cleanup_pending(&pending);
            return Err(CodegenError::Conflict(item.relative.clone()));
        }
    }

    for (installed, item) in pending.iter().enumerate() {
        if item.previous.is_some()
            && let Err(source) = std::fs::rename(&item.path, &item.backup)
        {
            rollback(&pending[..installed]);
            cleanup_pending(&pending);
            return Err(CodegenError::Io {
                path: item.path.clone(),
                source,
            });
        }
        if let Err(source) = std::fs::rename(&item.stage, &item.path) {
            if item.previous.is_some() {
                let _ = std::fs::rename(&item.backup, &item.path);
            }
            rollback(&pending[..installed]);
            cleanup_pending(&pending);
            return Err(CodegenError::Io {
                path: item.path.clone(),
                source,
            });
        }
    }
    for item in &pending {
        if item.backup.exists() {
            std::fs::remove_file(&item.backup).map_err(|source| CodegenError::Io {
                path: item.backup.clone(),
                source,
            })?;
        }
    }
    Ok(pending.into_iter().map(|item| item.relative).collect())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, CodegenError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CodegenError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn cleanup_pending(pending: &[PendingArtifact]) {
    for item in pending {
        let _ = std::fs::remove_file(&item.stage);
        let _ = std::fs::remove_file(&item.backup);
    }
}

fn rollback(installed: &[PendingArtifact]) {
    for item in installed.iter().rev() {
        let _ = std::fs::remove_file(&item.path);
        if item.backup.exists() {
            let _ = std::fs::rename(&item.backup, &item.path);
        }
    }
}

pub fn check(_root: &Path, result: &BuildResult) -> Vec<String> {
    result.plan.changed_paths()
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
    if let Some(block) = selector_block(
        ":root",
        Some(ColorScheme::Light),
        default
            .values
            .iter()
            .filter(|token| emitted_root_domain(token.domain)),
        indent,
        mode,
    )? {
        blocks.push(block);
    }
    if let Some(dark_block) = selector_block(
        &format!(":root:not([{}])", config.selectors.theme),
        Some(ColorScheme::Dark),
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
            &format!("[{}=\"{}\"]", config.selectors.theme, current.id),
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
    pub system: Option<(String, String)>,
    pub contexts: Vec<ResolvedProfile>,
}

fn emitted_root_domain(domain: TokenDomain) -> bool {
    matches!(
        domain,
        TokenDomain::Theme | TokenDomain::Density | TokenDomain::Motion | TokenDomain::Contrast
    )
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
        .map(|value| {
            value
                .as_f64()
                .ok_or_else(|| "cubicBezier value must be numeric".to_owned())
                .and_then(|value| format_css_number(value).map_err(|error| error.to_string()))
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
                serialize_unit(value, &["px", "rem"], 1.0, false).map_err(ThemeError::Resolution)
            })
    };
    let color_value = object
        .get("color")
        .ok_or_else(|| ThemeError::Resolution("shadow color is missing".into()))?;
    let color = match mode {
        CssMode::Fallback => serialize_color_fallback(color_value)?,
        CssMode::Modern => serialize_color_modern(color_value)?,
    };
    let prefix = object
        .get("inset")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        .then_some("inset ")
        .unwrap_or("");
    Ok(format!(
        "{prefix}{} {} {} {} {color}",
        dimension("offsetX")?,
        dimension("offsetY")?,
        dimension("blur")?,
        dimension("spread")?
    ))
}

fn generate_rust(config: &ProjectConfig, profiles: &[ResolvedProfile]) -> String {
    let mut output = String::from(
        "// Generated by leptos_ui_theme. Do not edit.\n\n#[derive(Clone, Copy, Debug, Eq, PartialEq)]\npub struct ThemeMetadata {\n    pub id: &'static str,\n    pub label: Option<&'static str>,\n    pub color_scheme: &'static str,\n}\n\n",
    );
    output.push_str(&format!(
        "pub const STORAGE_KEY: &str = {:?};\n",
        config.storage_key
    ));
    output.push_str(&format!(
        "pub const THEME_ATTRIBUTE: &str = {:?};\n\n",
        config.selectors.theme
    ));
    output.push_str("pub const THEMES: &[ThemeMetadata] = &[\n");
    for profile in profiles {
        output.push_str(&format!(
            "    ThemeMetadata {{ id: {:?}, label: {}, color_scheme: {:?} }},\n",
            profile.id,
            profile
                .label
                .as_ref()
                .map_or("None".into(), |label| format!("Some({label:?})")),
            match profile.color_scheme {
                ColorScheme::Light => "light",
                ColorScheme::Dark => "dark",
            }
        ));
    }
    output.push_str("];\n");
    output
}

fn bootstrap_script(
    config: &ProjectConfig,
    profiles: &[ResolvedProfile],
) -> Result<String, CodegenError> {
    let ids: Vec<&str> = profiles.iter().map(|profile| profile.id.as_str()).collect();
    let ids = serde_json::to_string(&ids)?;
    let key = serde_json::to_string(&config.storage_key)?;
    let attribute = serde_json::to_string(&config.selectors.theme)?;
    Ok(format!(
        "(()=>{{const a={ids},k={key},n={attribute},r=document.documentElement;let v=null;try{{v=localStorage.getItem(k)}}catch(_){{}}if(v!==\"system\"&&a.includes(v)){{r.setAttribute(n,v)}}else{{r.removeAttribute(n)}}window.__LEPTOS_UI_THEME_BOOTSTRAP__={{preference:v,effective:r.getAttribute(n),adopted:false}}}})();"
    ))
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
                format!("{}{}", config.html.public_base_path, external.served_path)
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

fn patch_index(index: &[u8], canonical_region: &str) -> Result<Vec<u8>, CodegenError> {
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
    let region = canonical_region.replace('\n', newline);
    let start = format!("<!-- leptos-ui-theme:start -->{newline}");
    let end = format!("<!-- leptos-ui-theme:end -->{newline}");
    let starts: Vec<_> = text.match_indices(&start).collect();
    let ends: Vec<_> = text.match_indices(&end).collect();
    if starts.len() == 1 && ends.len() == 1 && starts[0].0 < ends[0].0 {
        let end_offset = ends[0].0 + end.len();
        let mut output = Vec::with_capacity(index.len() + region.len());
        output.extend_from_slice(&index[..starts[0].0]);
        output.extend_from_slice(region.as_bytes());
        output.extend_from_slice(&index[end_offset..]);
        return Ok(output);
    }
    if !starts.is_empty() || !ends.is_empty() {
        return Err(CodegenError::Core(ThemeError::Config(
            "index HTML has ambiguous theme markers".into(),
        )));
    }
    let kit_lines: Vec<_> = text
        .split_inclusive(newline)
        .scan(0usize, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line))
        })
        .filter(|(_, line)| {
            line.contains("<link")
                && line.contains("data-trunk")
                && line.contains("rel=\"css\"")
                && !line.contains("leptos-ui-theme")
        })
        .collect();
    if kit_lines.len() != 1 || !kit_lines[0].1.ends_with(newline) {
        return Err(CodegenError::Core(ThemeError::Config(
            "index HTML must contain one line-bounded kit stylesheet link".into(),
        )));
    }
    let insertion = kit_lines[0].0 + kit_lines[0].1.len();
    let mut output = Vec::with_capacity(index.len() + region.len());
    output.extend_from_slice(&index[..insertion]);
    output.extend_from_slice(region.as_bytes());
    output.extend_from_slice(&index[insertion..]);
    Ok(output)
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

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
    use super::{CssMode, GeneratedArtifact, apply_artifacts, generate_css, serialize_css};
    use leptos_ui_theme_core::{
        ColorScheme, ProjectConfig, ResolvedProfile, ResolvedToken, TokenDomain, format_css_number,
    };
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
            provenance: "test".into(),
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
                values: vec![oklch_token()],
            },
            ResolvedProfile {
                id: "dark".into(),
                label: None,
                color_scheme: ColorScheme::Dark,
                values: vec![oklch_token()],
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
        std::fs::remove_dir_all(root).expect("remove temporary directory");
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

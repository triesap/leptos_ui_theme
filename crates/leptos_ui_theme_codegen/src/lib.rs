#![forbid(unsafe_code)]
#![doc = "Deterministic artifact generation for `leptos_ui_theme`."]

use leptos_ui_theme_core::{
    ColorScheme, ProjectConfig, ResolvedProfile, ResolvedToken, ThemeCompiler, ThemeError,
    TokenDomain, sha256,
};
use serde::Serialize;
use std::collections::BTreeMap;
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
}

#[derive(Clone, Debug)]
pub struct GeneratedArtifact {
    pub path: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct BuildResult {
    pub artifacts: Vec<GeneratedArtifact>,
    pub profiles: Vec<ResolvedProfile>,
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
    let css = generate_css(&compiler.config, &profiles)?;
    let rust = generate_rust(&compiler.config, &profiles);
    let mut artifacts = vec![
        GeneratedArtifact {
            path: compiler.config.outputs.css.clone(),
            bytes: css.into_bytes(),
        },
        GeneratedArtifact {
            path: compiler.config.outputs.rust.clone(),
            bytes: rust.into_bytes(),
        },
    ];
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
    artifacts.push(GeneratedArtifact {
        path: compiler.config.outputs.lock.clone(),
        bytes: lock_bytes,
    });
    let total: usize = artifacts.iter().map(|artifact| artifact.bytes.len()).sum();
    if total as u64 > compiler.config.limits.max_output_bytes {
        return Err(CodegenError::OutputLimit);
    }
    Ok(BuildResult {
        artifacts,
        profiles,
    })
}

pub fn apply(root: &Path, result: &BuildResult) -> Result<Vec<String>, CodegenError> {
    let mut changed = Vec::new();
    for artifact in &result.artifacts {
        let path = root.join(&artifact.path);
        if path.is_symlink() {
            return Err(CodegenError::Core(ThemeError::Security(
                artifact.path.clone(),
            )));
        }
        if std::fs::read(&path).ok().as_deref() == Some(&artifact.bytes) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CodegenError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let temporary = path.with_extension(format!(
            "{}.leptos-ui-theme.tmp",
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or("file")
        ));
        std::fs::write(&temporary, &artifact.bytes).map_err(|source| CodegenError::Io {
            path: temporary.clone(),
            source,
        })?;
        std::fs::rename(&temporary, &path).map_err(|source| CodegenError::Io {
            path: path.clone(),
            source,
        })?;
        changed.push(artifact.path.clone());
    }
    Ok(changed)
}

pub fn check(root: &Path, result: &BuildResult) -> Vec<String> {
    result
        .artifacts
        .iter()
        .filter(|artifact| {
            std::fs::read(root.join(&artifact.path)).ok().as_deref() != Some(&artifact.bytes)
        })
        .map(|artifact| artifact.path.clone())
        .collect()
}

pub fn generate_css(
    config: &ProjectConfig,
    profiles: &[ResolvedProfile],
) -> Result<String, CodegenError> {
    let default = profile(profiles, &config.profiles.default)?;
    let dark = profile(profiles, &config.profiles.system.dark)?;
    let mut blocks = Vec::new();
    blocks.push(selector_block(
        ":root",
        Some(ColorScheme::Light),
        default
            .values
            .iter()
            .filter(|token| emitted_root_domain(token.domain)),
        2,
    )?);
    let dark_block = selector_block(
        &format!(":root:not([{}])", config.selectors.theme),
        Some(ColorScheme::Dark),
        dark.values
            .iter()
            .filter(|token| token.domain == TokenDomain::Theme),
        4,
    )?;
    blocks.push(format!(
        "  @media (prefers-color-scheme: dark) {{\n{dark_block}  }}\n"
    ));
    for current in profiles {
        blocks.push(selector_block(
            &format!("[{}=\"{}\"]", config.selectors.theme, current.id),
            Some(current.color_scheme),
            current
                .values
                .iter()
                .filter(|token| token.domain == TokenDomain::Theme),
            2,
        )?);
    }
    let mut css = String::from(
        "/* Generated by leptos_ui_theme. Do not edit. */\n@layer leptos-ui-kit.tokens, leptos-ui-kit.themes, leptos-ui-kit.components;\n\n@layer leptos-ui-kit.themes {\n",
    );
    css.push_str(&blocks.join("\n"));
    css.push_str("}\n");
    Ok(css)
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
) -> Result<String, CodegenError> {
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
            serialize_css(token)?
        ));
    }
    block.push_str(&format!("{outer}}}\n"));
    Ok(block)
}

fn serialize_css(token: &ResolvedToken) -> Result<String, CodegenError> {
    let fail = |reason: &str| CodegenError::CssValue {
        path: token.path.clone(),
        reason: reason.into(),
    };
    match &token.value {
        serde_json::Value::String(value) => {
            if value.contains([';', '{', '}']) {
                Err(fail("unsafe string value"))
            } else {
                Ok(value.clone())
            }
        }
        serde_json::Value::Number(value) => Ok(value.to_string()),
        serde_json::Value::Bool(value) => Ok(value.to_string()),
        serde_json::Value::Object(object) => {
            if let (Some(value), Some(unit)) = (
                object.get("value").and_then(serde_json::Value::as_f64),
                object.get("unit").and_then(serde_json::Value::as_str),
            ) {
                if !unit
                    .bytes()
                    .all(|byte| byte.is_ascii_alphabetic() || byte == b'%')
                {
                    return Err(fail("invalid unit"));
                }
                return Ok(format_number(value) + unit);
            }
            if let (Some(space), Some(components)) = (
                object.get("colorSpace").and_then(serde_json::Value::as_str),
                object
                    .get("components")
                    .and_then(serde_json::Value::as_array),
            ) {
                if components.len() != 3
                    || !space
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                {
                    return Err(fail("invalid color"));
                }
                let components = components
                    .iter()
                    .map(|value| {
                        value
                            .as_f64()
                            .map(format_number)
                            .ok_or_else(|| fail("invalid color component"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let alpha = object
                    .get("alpha")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(1.0);
                return Ok(format!(
                    "color({space} {} / {})",
                    components.join(" "),
                    format_number(alpha)
                ));
            }
            Err(fail("unsupported object value"))
        }
        _ => Err(fail("unsupported JSON value")),
    }
}

fn format_number(value: f64) -> String {
    let value = if value == 0.0 { 0.0 } else { value };
    let mut rendered = format!("{value:.6}");
    while rendered.contains('.') && rendered.ends_with('0') {
        rendered.pop();
    }
    if rendered.ends_with('.') {
        rendered.pop();
    }
    rendered
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
    use super::format_number;

    #[test]
    fn numbers_are_stable() {
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(1.25), "1.25");
    }
}

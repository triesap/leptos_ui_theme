#![forbid(unsafe_code)]
#![doc = "Deterministic artifact generation for `leptos_ui_theme`."]

use leptos_ui_theme_core::{
    BootstrapMode, ColorScheme, ProjectConfig, ResolvedProfile, ResolvedToken, ThemeCompiler,
    ThemeError, TokenDomain, sha256,
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
        GeneratedArtifact {
            path: compiler.config.outputs.css.clone(),
            bytes: css.into_bytes(),
        },
        GeneratedArtifact {
            path: compiler.config.outputs.rust.clone(),
            bytes: rust.into_bytes(),
        },
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
        artifacts.push(GeneratedArtifact {
            path: external.output_path.clone(),
            bytes,
        });
    }
    let index_bytes = std::fs::read(&selected_index).map_err(|source| CodegenError::Io {
        path: selected_index.clone(),
        source,
    })?;
    let region = html_region(&compiler.config, &profiles, &index_relative, &script)?;
    let patched = patch_index(&index_bytes, &region)?;
    artifacts.push(GeneratedArtifact {
        path: index_relative,
        bytes: patched,
    });
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
    axes: &[ResolvedAxis],
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
    for axis in axes {
        if let Some((query, context)) = &axis.system {
            let current = profile(&axis.contexts, context)?;
            let system_block = selector_block(
                &format!(":root:not([{}])", axis.attribute),
                None,
                current
                    .values
                    .iter()
                    .filter(|token| token.domain == axis.domain),
                4,
            )?;
            blocks.push(format!("  @media {query} {{\n{system_block}  }}\n"));
        }
        for current in &axis.contexts {
            blocks.push(selector_block(
                &format!(":root[{}=\"{}\"]", axis.attribute, current.id),
                None,
                current
                    .values
                    .iter()
                    .filter(|token| token.domain == axis.domain),
                2,
            )?);
        }
    }
    let mut css = String::from(
        "/* Generated by leptos_ui_theme. Do not edit. */\n@layer leptos-ui-kit.tokens, leptos-ui-kit.themes, leptos-ui-kit.components;\n\n@layer leptos-ui-kit.themes {\n",
    );
    css.push_str(&blocks.join("\n"));
    css.push_str("}\n");
    Ok(css)
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
    use super::format_number;

    #[test]
    fn numbers_are_stable() {
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(1.25), "1.25");
    }
}

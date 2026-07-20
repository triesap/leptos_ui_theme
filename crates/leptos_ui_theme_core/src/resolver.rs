use crate::contract::{KitTokenContract, TokenDomain};
use crate::model::{ColorScheme, Profile, ProjectConfig};
use crate::{
    CONFIG_FILE, DtcgType, LogicalPath, SourceLoader, ThemeError, discover_kit, dtcg_alias_target,
    expand_group_extends, read_json, validate_contrast, validate_reserved_members,
    validate_token_value,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct ResolvedToken {
    pub path: String,
    pub token_type: String,
    pub css_custom_property: String,
    pub domain: TokenDomain,
    pub value: serde_json::Value,
    pub provenance: String,
    pub alias_of: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedProfile {
    pub id: String,
    pub label: Option<String>,
    pub color_scheme: ColorScheme,
    pub values: Vec<ResolvedToken>,
}

#[derive(Clone, Debug)]
pub struct ThemeCompiler {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub config: ProjectConfig,
    pub contract_path: PathBuf,
    pub kit_stylesheet_path: PathBuf,
    pub contract: KitTokenContract,
    loader: SourceLoader,
}

#[derive(Debug, Deserialize)]
struct ResolverDocument {
    version: String,
    #[serde(default)]
    sets: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    modifiers: serde_json::Map<String, serde_json::Value>,
    #[serde(rename = "resolutionOrder", default)]
    resolution_order: Vec<serde_json::Value>,
}

#[derive(Clone)]
struct RawToken {
    token_type: String,
    value: serde_json::Value,
    provenance: String,
}

impl ThemeCompiler {
    pub fn load(root: impl Into<PathBuf>) -> Result<Self, ThemeError> {
        let root = root.into();
        let config_path = root.join(CONFIG_FILE);
        let config: ProjectConfig = read_json(&config_path)?;
        config.validate()?;
        let kit = discover_kit(&root, &config.kit, config.limits.clone())?;
        let contract_path = kit.contract_path;
        let kit_stylesheet_path = kit.stylesheet_path;
        let contract = kit.contract;
        let loader = SourceLoader::new(&root, config.limits.clone())?;
        Ok(Self {
            root,
            config_path,
            config,
            contract_path,
            kit_stylesheet_path,
            contract,
            loader,
        })
    }

    pub fn resolve(&self) -> Result<Vec<ResolvedProfile>, ThemeError> {
        let resolver_logical = LogicalPath::new(self.config.resolver.clone())?;
        let resolver: ResolverDocument = self.loader.read_json(&resolver_logical)?;
        validate_resolver(&resolver, &self.config)?;
        self.config
            .profiles
            .named
            .iter()
            .map(|profile| self.resolve_profile(profile, &resolver, &resolver_logical, None))
            .collect()
    }

    pub fn resolve_one(&self, profile: &str) -> Result<ResolvedProfile, ThemeError> {
        self.resolve()?
            .into_iter()
            .find(|candidate| candidate.id == profile)
            .ok_or_else(|| ThemeError::Resolution(format!("unknown profile `{profile}`")))
    }

    pub fn resolve_axis(
        &self,
        modifier: &str,
        contexts: &[String],
    ) -> Result<Vec<ResolvedProfile>, ThemeError> {
        let resolver_logical = LogicalPath::new(self.config.resolver.clone())?;
        let resolver: ResolverDocument = self.loader.read_json(&resolver_logical)?;
        validate_resolver(&resolver, &self.config)?;
        let base = self.config.profile(&self.config.profiles.default)?;
        contexts
            .iter()
            .map(|context| {
                let mut profile = base.clone();
                profile.id = context.clone();
                profile.label = None;
                profile.inputs.insert(modifier.into(), context.clone());
                let domain = match modifier {
                    "density" => TokenDomain::Density,
                    "motion" => TokenDomain::Motion,
                    "contrast" => TokenDomain::Contrast,
                    _ => {
                        return Err(ThemeError::Resolution(format!(
                            "unsupported selection axis `{modifier}`"
                        )));
                    }
                };
                self.resolve_profile(&profile, &resolver, &resolver_logical, Some(domain))
            })
            .collect()
    }

    fn resolve_profile(
        &self,
        profile: &Profile,
        resolver: &ResolverDocument,
        resolver_path: &LogicalPath,
        axis_domain: Option<TokenDomain>,
    ) -> Result<ResolvedProfile, ThemeError> {
        let mut raw = BTreeMap::<String, RawToken>::new();
        for mapping in &self.contract.tokens {
            if let Some(value) = mapping.default.clone() {
                raw.insert(
                    mapping.path.clone(),
                    RawToken {
                        token_type: mapping.token_type.clone(),
                        value,
                        provenance: "contract-default".into(),
                    },
                );
            }
        }

        for order in &resolver.resolution_order {
            let reference = order
                .get("$ref")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| ThemeError::Resolution("resolutionOrder entry lacks $ref".into()))?;
            if let Some(name) = reference.strip_prefix("#/sets/") {
                let set = resolver.sets.get(name).ok_or_else(|| {
                    ThemeError::Resolution(format!("unknown resolver set `{name}`"))
                })?;
                apply_sources(
                    set.get("sources"),
                    resolver_path,
                    &self.loader,
                    &mut raw,
                    &self.config,
                )?;
            } else if let Some(modifier_name) = reference.strip_prefix("#/modifiers/") {
                self.apply_modifier(profile, resolver, modifier_name, resolver_path, &mut raw)?;
            } else {
                return Err(ThemeError::Resolution(format!(
                    "unsupported resolutionOrder reference `{reference}`"
                )));
            }
        }
        for modifier in profile.inputs.keys() {
            let reference = format!("#/modifiers/{modifier}");
            let appears = resolver.resolution_order.iter().any(|entry| {
                entry.get("$ref").and_then(serde_json::Value::as_str) == Some(&reference)
            });
            if !appears {
                self.apply_modifier(profile, resolver, modifier, resolver_path, &mut raw)?;
            }
        }

        resolve_aliases(
            &mut raw,
            self.config.limits.reference_depth,
            self.config.limits.reference_edges,
        )?;
        apply_deprecations(&self.contract, &mut raw)?;
        let mut values = Vec::with_capacity(self.contract.tokens.len());
        for mapping in &self.contract.tokens {
            let terminal = self.contract.terminal_mapping(&mapping.path)?;
            let Some(value) = raw.get(&terminal.path) else {
                if mapping.required {
                    return Err(ThemeError::Resolution(format!(
                        "required token `{}` is unresolved",
                        mapping.path
                    )));
                }
                continue;
            };
            if value.token_type != terminal.token_type {
                return Err(ThemeError::Resolution(format!(
                    "token `{}` has type `{}` but contract requires `{}`",
                    terminal.path, value.token_type, terminal.token_type
                )));
            }
            if value.provenance != "contract-default"
                && (!terminal.theme_override
                    || (terminal.domain != TokenDomain::Theme
                        && Some(terminal.domain) != axis_domain))
            {
                return Err(ThemeError::Resolution(format!(
                    "source is not allowed to override token `{}` in domain `{:?}`",
                    terminal.path, terminal.domain
                )));
            }
            let alias_of = mapping.deprecation.as_ref().map(|_| terminal.path.clone());
            values.push(ResolvedToken {
                path: mapping.path.clone(),
                token_type: mapping.token_type.clone(),
                css_custom_property: mapping.css_custom_property.clone(),
                domain: mapping.domain,
                value: alias_of
                    .as_ref()
                    .map(|_| {
                        serde_json::Value::String(format!("var({})", terminal.css_custom_property))
                    })
                    .unwrap_or_else(|| value.value.clone()),
                provenance: value.provenance.clone(),
                alias_of,
            });
        }
        let resolved = ResolvedProfile {
            id: profile.id.clone(),
            label: profile.label.clone(),
            color_scheme: profile.color_scheme,
            values,
        };
        validate_contrast(&self.contract, &resolved.values)?;
        Ok(resolved)
    }

    fn apply_modifier(
        &self,
        profile: &Profile,
        resolver: &ResolverDocument,
        modifier_name: &str,
        resolver_path: &LogicalPath,
        raw: &mut BTreeMap<String, RawToken>,
    ) -> Result<(), ThemeError> {
        let context = profile.inputs.get(modifier_name).ok_or_else(|| {
            ThemeError::Resolution(format!(
                "profile `{}` omits `{modifier_name}` input",
                profile.id
            ))
        })?;
        let modifier = resolver
            .modifiers
            .get(modifier_name)
            .ok_or_else(|| ThemeError::Resolution(format!("unknown modifier `{modifier_name}`")))?;
        let sources = modifier
            .get("contexts")
            .and_then(|value| value.get(context));
        apply_sources(sources, resolver_path, &self.loader, raw, &self.config)
    }
}

fn validate_resolver(
    resolver: &ResolverDocument,
    config: &ProjectConfig,
) -> Result<(), ThemeError> {
    if resolver.version != "2025.10" {
        return Err(ThemeError::Resolution(format!(
            "unsupported resolver version `{}`",
            resolver.version
        )));
    }
    if resolver.resolution_order.is_empty() {
        return Err(ThemeError::Resolution(
            "resolver resolutionOrder is empty".into(),
        ));
    }
    let context_count = resolver
        .modifiers
        .values()
        .filter_map(|modifier| modifier.get("contexts")?.as_object())
        .map(serde_json::Map::len)
        .sum::<usize>();
    let nodes = resolver.sets.len() + resolver.modifiers.len() + context_count;
    if nodes > config.limits.resolver_nodes as usize
        || context_count > config.limits.resolver_contexts as usize
    {
        return Err(ThemeError::Resolution(
            "resolver exceeds configured node or context limits".into(),
        ));
    }
    Ok(())
}

fn apply_sources(
    sources: Option<&serde_json::Value>,
    resolver_path: &LogicalPath,
    loader: &SourceLoader,
    raw: &mut BTreeMap<String, RawToken>,
    config: &ProjectConfig,
) -> Result<(), ThemeError> {
    let Some(sources) = sources.and_then(serde_json::Value::as_array) else {
        return Err(ThemeError::Resolution(
            "resolver source list is missing".into(),
        ));
    };
    if sources.len() > config.limits.source_files as usize {
        return Err(ThemeError::Resolution(
            "resolver source list exceeds limits.sourceFiles".into(),
        ));
    }
    for source in sources {
        let reference = source
            .get("$ref")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ThemeError::Resolution("resolver source lacks $ref".into()))?;
        let (file_reference, pointer) = reference
            .split_once('#')
            .map_or((reference, None), |(file, pointer)| (file, Some(pointer)));
        if file_reference.is_empty()
            || Path::new(file_reference).is_absolute()
            || file_reference.contains("..")
            || file_reference.contains('\\')
            || file_reference.contains(':')
            || pointer.is_some_and(|pointer| pointer.contains('#'))
        {
            return Err(ThemeError::Security(format!(
                "unsafe resolver reference `{reference}`"
            )));
        }
        let base = Path::new(resolver_path.as_str())
            .parent()
            .unwrap_or_else(|| Path::new(""));
        let joined = base.join(file_reference);
        let joined = joined.to_str().ok_or_else(|| {
            ThemeError::Security(format!("resolver reference is not UTF-8: `{reference}`"))
        })?;
        let logical = LogicalPath::new(joined.to_owned())?;
        let token_root = LogicalPath::new(config.token_root.clone())?;
        if logical.as_str() != token_root.as_str()
            && !logical
                .as_str()
                .starts_with(&format!("{}/", token_root.as_str()))
        {
            return Err(ThemeError::Security(format!(
                "resolver reference escapes the token root: `{reference}`"
            )));
        }
        let value: serde_json::Value = loader.read_json(&logical)?;
        let value = expand_group_extends(&value)?;
        let value = match pointer {
            Some("") | None => &value,
            Some(pointer) => {
                let pointer = decode_uri_fragment(pointer)?;
                if !pointer.starts_with('/') {
                    return Err(ThemeError::Resolution(format!(
                        "JSON Pointer fragment must be empty or start with `/`: `#{pointer}`"
                    )));
                }
                value.pointer(&pointer).ok_or_else(|| {
                    ThemeError::Resolution(format!(
                        "unknown JSON Pointer `#{pointer}` in `{file_reference}`"
                    ))
                })?
            }
        };
        let mut flattened = BTreeMap::new();
        flatten_tokens(value, None, "", Path::new(logical.as_str()), &mut flattened)?;
        if flattened.len() > config.limits.tokens as usize
            || raw.len().saturating_add(flattened.len()) > config.limits.tokens as usize
        {
            return Err(ThemeError::Resolution(
                "token inventory exceeds limits.tokens".into(),
            ));
        }
        raw.extend(flattened);
    }
    Ok(())
}

fn decode_uri_fragment(value: &str) -> Result<String, ThemeError> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(ThemeError::Resolution(
                "JSON Pointer fragment has incomplete percent encoding".into(),
            ));
        }
        let high = hex_digit(bytes[index + 1])?;
        let low = hex_digit(bytes[index + 2])?;
        output.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(output)
        .map_err(|_| ThemeError::Resolution("JSON Pointer fragment is not UTF-8".into()))
}

fn hex_digit(value: u8) -> Result<u8, ThemeError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(ThemeError::Resolution(
            "JSON Pointer fragment has invalid percent encoding".into(),
        )),
    }
}

fn flatten_tokens(
    value: &serde_json::Value,
    inherited_type: Option<&str>,
    prefix: &str,
    path: &Path,
    output: &mut BTreeMap<String, RawToken>,
) -> Result<(), ThemeError> {
    let object = value.as_object().ok_or_else(|| {
        ThemeError::Resolution(format!(
            "token group in {} must be an object",
            path.display()
        ))
    })?;
    validate_reserved_members(object, object.contains_key("$value"))?;
    let declared_type = object
        .get("$type")
        .and_then(serde_json::Value::as_str)
        .or(inherited_type);
    if let Some(root) = object.get("$root") {
        if prefix.is_empty() {
            return Err(ThemeError::Resolution(
                "a document root cannot declare a $root token".into(),
            ));
        }
        let root = root
            .as_object()
            .ok_or_else(|| ThemeError::Resolution(format!("token `{prefix}` must be an object")))?;
        let token_value = root
            .get("$value")
            .ok_or_else(|| ThemeError::Resolution(format!("token `{prefix}` has no $value")))?;
        let token_type = root
            .get("$type")
            .and_then(serde_json::Value::as_str)
            .or(declared_type)
            .ok_or_else(|| ThemeError::Resolution(format!("token `{prefix}` has no type")))?;
        let token_type = DtcgType::parse(token_type)?;
        validate_token_value(token_type, token_value)?;
        output.insert(
            prefix.into(),
            RawToken {
                token_type: token_type.as_str().into(),
                value: token_value.clone(),
                provenance: path.display().to_string(),
            },
        );
    }
    for (name, child) in object {
        if name.starts_with('$') {
            continue;
        }
        let token_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        let child_object = child.as_object().ok_or_else(|| {
            ThemeError::Resolution(format!("token `{token_path}` must be an object"))
        })?;
        if let Some(token_value) = child_object.get("$value") {
            validate_reserved_members(child_object, true)?;
            let token_type = child_object
                .get("$type")
                .and_then(serde_json::Value::as_str)
                .or(declared_type)
                .ok_or_else(|| {
                    ThemeError::Resolution(format!("token `{token_path}` has no type"))
                })?;
            let token_type = DtcgType::parse(token_type)?;
            validate_token_value(token_type, token_value)?;
            output.insert(
                token_path,
                RawToken {
                    token_type: token_type.as_str().into(),
                    value: token_value.clone(),
                    provenance: path.display().to_string(),
                },
            );
        } else {
            flatten_tokens(child, declared_type, &token_path, path, output)?;
        }
    }
    Ok(())
}

fn resolve_aliases(
    values: &mut BTreeMap<String, RawToken>,
    max_depth: u32,
    max_edges: u32,
) -> Result<(), ThemeError> {
    let unresolved = values.clone();
    let mut resolved = BTreeMap::new();
    let mut visiting = BTreeSet::new();
    let mut edges = 0_u32;
    for key in unresolved.keys() {
        let token = resolve_raw_token(
            key,
            &unresolved,
            &mut resolved,
            &mut visiting,
            0,
            max_depth,
            &mut edges,
            max_edges,
        )?;
        resolved.insert(key.clone(), token);
    }
    *values = resolved;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_raw_token(
    key: &str,
    unresolved: &BTreeMap<String, RawToken>,
    resolved: &mut BTreeMap<String, RawToken>,
    visiting: &mut BTreeSet<String>,
    depth: u32,
    max_depth: u32,
    edges: &mut u32,
    max_edges: u32,
) -> Result<RawToken, ThemeError> {
    if let Some(value) = resolved.get(key) {
        return Ok(value.clone());
    }
    if depth > max_depth {
        return Err(ThemeError::Resolution(format!(
            "alias depth exceeds configured maximum at `{key}`"
        )));
    }
    if !visiting.insert(key.to_owned()) {
        return Err(ThemeError::Resolution(format!("alias cycle at `{key}`")));
    }
    let mut token = unresolved
        .get(key)
        .cloned()
        .ok_or_else(|| ThemeError::Resolution(format!("unknown alias target `{key}`")))?;

    if let Some(alias) = dtcg_alias_target(&token.value)? {
        record_reference_edge(edges, max_edges)?;
        let target = resolve_raw_token(
            alias.as_str(),
            unresolved,
            resolved,
            visiting,
            depth.saturating_add(1),
            max_depth,
            edges,
            max_edges,
        )?;
        if target.token_type != token.token_type {
            return Err(ThemeError::Resolution(format!(
                "alias `{key}` has type `{}` but target `{}` has type `{}`",
                token.token_type,
                alias.as_str(),
                target.token_type
            )));
        }
        token.value = target.value;
        token.provenance = format!("{} -> {}", token.provenance, alias.as_str());
    } else {
        token.value = resolve_property_aliases(
            &token.value,
            unresolved,
            resolved,
            visiting,
            depth,
            max_depth,
            edges,
            max_edges,
        )?;
    }

    let token_type = DtcgType::parse(&token.token_type)?;
    validate_token_value(token_type, &token.value)?;
    visiting.remove(key);
    resolved.insert(key.to_owned(), token.clone());
    Ok(token)
}

#[allow(clippy::too_many_arguments)]
fn resolve_property_aliases(
    value: &serde_json::Value,
    unresolved: &BTreeMap<String, RawToken>,
    resolved: &mut BTreeMap<String, RawToken>,
    visiting: &mut BTreeSet<String>,
    depth: u32,
    max_depth: u32,
    edges: &mut u32,
    max_edges: u32,
) -> Result<serde_json::Value, ThemeError> {
    if let Some(alias) = dtcg_alias_target(value)? {
        record_reference_edge(edges, max_edges)?;
        return resolve_raw_token(
            alias.as_str(),
            unresolved,
            resolved,
            visiting,
            depth.saturating_add(1),
            max_depth,
            edges,
            max_edges,
        )
        .map(|token| token.value);
    }
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| {
                resolve_property_aliases(
                    value, unresolved, resolved, visiting, depth, max_depth, edges, max_edges,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(name, value)| {
                resolve_property_aliases(
                    value, unresolved, resolved, visiting, depth, max_depth, edges, max_edges,
                )
                .map(|value| (name.clone(), value))
            })
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(serde_json::Value::Object),
        _ => Ok(value.clone()),
    }
}

fn record_reference_edge(edges: &mut u32, max_edges: u32) -> Result<(), ThemeError> {
    *edges = edges.saturating_add(1);
    if *edges > max_edges {
        return Err(ThemeError::Resolution(
            "alias graph exceeds limits.referenceEdges".into(),
        ));
    }
    Ok(())
}

fn apply_deprecations(
    contract: &KitTokenContract,
    values: &mut BTreeMap<String, RawToken>,
) -> Result<(), ThemeError> {
    for mapping in contract
        .tokens
        .iter()
        .filter(|mapping| mapping.deprecation.is_some())
    {
        let Some(legacy) = values.remove(&mapping.path) else {
            continue;
        };
        let terminal = contract.terminal_mapping(&mapping.path)?;
        match values.get(&terminal.path) {
            Some(current)
                if current.provenance != "contract-default"
                    && (current.token_type != legacy.token_type
                        || current.value != legacy.value) =>
            {
                return Err(ThemeError::Resolution(format!(
                    "deprecated token `{}` conflicts with replacement `{}`",
                    mapping.path, terminal.path
                )));
            }
            Some(current) if current.provenance != "contract-default" => {}
            _ => {
                values.insert(
                    terminal.path.clone(),
                    RawToken {
                        provenance: format!("{} -> {}", legacy.provenance, terminal.path),
                        ..legacy
                    },
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        RawToken, ResolverDocument, apply_deprecations, flatten_tokens, resolve_aliases,
        validate_resolver,
    };
    use crate::KitTokenContract;
    use std::collections::BTreeMap;
    use std::path::Path;

    fn contract() -> KitTokenContract {
        serde_json::from_value(serde_json::json!({
            "$schema": "https://triesap.github.io/leptos_ui_kit/schema/0.2.0/token-contract.schema.json",
            "schemaVersion": "1.0.0",
            "contractId": "leptos-ui-kit",
            "abiVersion": 1,
            "revision": 2,
            "dtcgVersion": "2025.10",
            "dtcgProfile": "format+color+resolver:2025.10",
            "canonicalDigest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "tokens": [
                {
                    "path": "color.legacy",
                    "type": "color",
                    "cssCustomProperty": "--kit-color-legacy",
                    "domain": "theme",
                    "required": false,
                    "order": 1,
                    "themeOverride": true,
                    "deprecation": {
                        "message": "Use color.primary",
                        "replacement": "color.primary"
                    }
                },
                {
                    "path": "color.primary",
                    "type": "color",
                    "cssCustomProperty": "--kit-color-primary",
                    "domain": "theme",
                    "required": true,
                    "order": 2,
                    "themeOverride": true,
                    "default": "#000000"
                }
            ],
            "contrastChecks": []
        }))
        .expect("contract fixture")
    }

    #[test]
    fn group_root_becomes_the_group_token() {
        let document = serde_json::json!({
            "color": {
                "$type": "color",
                "$root": {"$value": "#123456"},
                "accent": {"$value": "#abcdef"}
            }
        });
        let mut output = BTreeMap::new();
        flatten_tokens(&document, None, "", Path::new("tokens.json"), &mut output)
            .expect("flatten tokens");
        assert_eq!(output["color"].value, "#123456");
        assert_eq!(output["color.accent"].value, "#abcdef");
    }

    #[test]
    fn deprecated_assignment_replaces_the_terminal_default() {
        let contract = contract();
        contract.validate().expect("valid contract");
        let mut values = BTreeMap::from([
            (
                "color.legacy".into(),
                RawToken {
                    token_type: "color".into(),
                    value: serde_json::json!("#ffffff"),
                    provenance: "theme.tokens.json".into(),
                },
            ),
            (
                "color.primary".into(),
                RawToken {
                    token_type: "color".into(),
                    value: serde_json::json!("#000000"),
                    provenance: "contract-default".into(),
                },
            ),
        ]);
        apply_deprecations(&contract, &mut values).expect("redirect assignment");
        assert!(!values.contains_key("color.legacy"));
        assert_eq!(values["color.primary"].value, "#ffffff");
        assert!(values["color.primary"].provenance.contains("color.primary"));
    }

    #[test]
    fn conflicting_deprecated_and_terminal_assignments_fail() {
        let contract = contract();
        let mut values = BTreeMap::from([
            (
                "color.legacy".into(),
                RawToken {
                    token_type: "color".into(),
                    value: serde_json::json!("#ffffff"),
                    provenance: "legacy.tokens.json".into(),
                },
            ),
            (
                "color.primary".into(),
                RawToken {
                    token_type: "color".into(),
                    value: serde_json::json!("#000000"),
                    provenance: "primary.tokens.json".into(),
                },
            ),
        ]);
        assert!(apply_deprecations(&contract, &mut values).is_err());
    }

    #[test]
    fn unsupported_resolver_versions_fail() {
        let resolver: ResolverDocument = serde_json::from_value(serde_json::json!({
            "version": "2024.1",
            "resolutionOrder": [{"$ref": "#/sets/base"}]
        }))
        .expect("resolver fixture");
        assert!(validate_resolver(&resolver, &crate::ProjectConfig::default()).is_err());
    }

    #[test]
    fn alias_depth_limit_fails_instead_of_leaving_an_alias() {
        let mut values = BTreeMap::from([
            (
                "a".into(),
                RawToken {
                    token_type: "number".into(),
                    value: serde_json::json!("{b}"),
                    provenance: "test".into(),
                },
            ),
            (
                "b".into(),
                RawToken {
                    token_type: "number".into(),
                    value: serde_json::json!("{c}"),
                    provenance: "test".into(),
                },
            ),
            (
                "c".into(),
                RawToken {
                    token_type: "number".into(),
                    value: serde_json::json!(1),
                    provenance: "test".into(),
                },
            ),
        ]);
        assert!(resolve_aliases(&mut values, 1, 8).is_err());
    }

    #[test]
    fn property_aliases_resolve_inside_composite_values() {
        let mut values = BTreeMap::from([
            (
                "color.shadow".into(),
                RawToken {
                    token_type: "color".into(),
                    value: serde_json::json!("#000000"),
                    provenance: "test".into(),
                },
            ),
            (
                "shadow.card".into(),
                RawToken {
                    token_type: "shadow".into(),
                    value: serde_json::json!({
                        "color": "{color.shadow}",
                        "offsetX": {"value": 0, "unit": "px"},
                        "offsetY": {"value": 1, "unit": "px"},
                        "blur": {"value": 4, "unit": "px"},
                        "spread": {"value": 0, "unit": "px"}
                    }),
                    provenance: "test".into(),
                },
            ),
        ]);
        resolve_aliases(&mut values, 8, 8).expect("resolve property alias");
        assert_eq!(values["shadow.card"].value["color"], "#000000");
    }
}

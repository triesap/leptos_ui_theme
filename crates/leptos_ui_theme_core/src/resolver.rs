use crate::contract::{KitTokenContract, TokenDomain};
use crate::model::{ColorScheme, Profile, ProjectConfig};
use crate::{CONFIG_FILE, ThemeError, discover_kit, read_json, validate_contrast};
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
    pub contract: KitTokenContract,
}

#[derive(Debug, Deserialize)]
struct ResolverDocument {
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
        let kit = discover_kit(&root, &config.kit)?;
        let contract_path = kit.contract_path;
        let contract = kit.contract;
        Ok(Self {
            root,
            config_path,
            config,
            contract_path,
            contract,
        })
    }

    pub fn resolve(&self) -> Result<Vec<ResolvedProfile>, ThemeError> {
        let resolver_path = self.root.join(&self.config.resolver);
        let resolver: ResolverDocument = read_json(&resolver_path)?;
        self.config
            .profiles
            .named
            .iter()
            .map(|profile| self.resolve_profile(profile, &resolver, &resolver_path, None))
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
        let resolver_path = self.root.join(&self.config.resolver);
        let resolver: ResolverDocument = read_json(&resolver_path)?;
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
                self.resolve_profile(&profile, &resolver, &resolver_path, Some(domain))
            })
            .collect()
    }

    fn resolve_profile(
        &self,
        profile: &Profile,
        resolver: &ResolverDocument,
        resolver_path: &Path,
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
            let Some(reference) = order.get("$ref").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if let Some(name) = reference.strip_prefix("#/sets/") {
                let set = resolver.sets.get(name).ok_or_else(|| {
                    ThemeError::Resolution(format!("unknown resolver set `{name}`"))
                })?;
                apply_sources(set.get("sources"), resolver_path, &mut raw)?;
            } else if let Some(modifier_name) = reference.strip_prefix("#/modifiers/") {
                self.apply_modifier(profile, resolver, modifier_name, resolver_path, &mut raw)?;
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

        resolve_aliases(&mut raw, self.config.limits.max_reference_depth)?;
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
        resolver_path: &Path,
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
        apply_sources(sources, resolver_path, raw)
    }
}

fn apply_sources(
    sources: Option<&serde_json::Value>,
    resolver_path: &Path,
    raw: &mut BTreeMap<String, RawToken>,
) -> Result<(), ThemeError> {
    let Some(sources) = sources.and_then(serde_json::Value::as_array) else {
        return Err(ThemeError::Resolution(
            "resolver source list is missing".into(),
        ));
    };
    for source in sources {
        let reference = source
            .get("$ref")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ThemeError::Resolution("resolver source lacks $ref".into()))?;
        if reference.starts_with('#') || reference.contains("..") || reference.contains('\\') {
            return Err(ThemeError::Security(format!(
                "unsafe resolver reference `{reference}`"
            )));
        }
        let base = resolver_path.parent().unwrap_or_else(|| Path::new("."));
        let path = base.join(reference);
        let value: serde_json::Value = read_json(&path)?;
        let mut flattened = BTreeMap::new();
        flatten_tokens(&value, None, "", &path, &mut flattened)?;
        raw.extend(flattened);
    }
    Ok(())
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
        output.insert(
            prefix.into(),
            RawToken {
                token_type: token_type.into(),
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
            let token_type = child_object
                .get("$type")
                .and_then(serde_json::Value::as_str)
                .or(declared_type)
                .ok_or_else(|| {
                    ThemeError::Resolution(format!("token `{token_path}` has no type"))
                })?;
            output.insert(
                token_path,
                RawToken {
                    token_type: token_type.into(),
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
) -> Result<(), ThemeError> {
    let keys: Vec<String> = values.keys().cloned().collect();
    for key in keys {
        let mut seen = BTreeSet::new();
        let mut current = key.clone();
        for _ in 0..=max_depth {
            if !seen.insert(current.clone()) {
                return Err(ThemeError::Resolution(format!("alias cycle at `{key}`")));
            }
            let Some(raw) = values.get(&current).cloned() else {
                return Err(ThemeError::Resolution(format!(
                    "unknown alias target `{current}`"
                )));
            };
            let Some(alias) = raw.value.as_str().and_then(alias_target) else {
                if current != key {
                    let target = values
                        .get(&current)
                        .cloned()
                        .expect("resolved alias target");
                    values.insert(
                        key.clone(),
                        RawToken {
                            provenance: format!("{} -> {}", values[&key].provenance, current),
                            ..target
                        },
                    );
                }
                break;
            };
            current = alias.into();
        }
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

fn alias_target(value: &str) -> Option<&str> {
    value
        .strip_prefix('{')?
        .strip_suffix('}')
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{RawToken, apply_deprecations, flatten_tokens};
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
            "canonicalDigest": "unused",
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
}

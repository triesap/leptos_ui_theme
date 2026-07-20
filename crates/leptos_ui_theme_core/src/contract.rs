use crate::{DtcgType, ThemeError, dtcg_alias_target, read_json, sha256, validate_token_value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KitTokenContract {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: String,
    pub contract_id: String,
    pub abi_version: u32,
    pub revision: u32,
    pub dtcg_version: String,
    pub dtcg_profile: String,
    pub canonical_digest: String,
    pub tokens: Vec<TokenMapping>,
    pub contrast_checks: Vec<ContrastCheck>,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TokenMapping {
    pub path: String,
    #[serde(rename = "type")]
    pub token_type: String,
    pub css_custom_property: String,
    pub domain: TokenDomain,
    pub required: bool,
    pub order: u32,
    pub theme_override: bool,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub deprecation: Option<Deprecation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Deprecation {
    pub message: String,
    pub replacement: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContrastCheck {
    pub id: String,
    pub foreground: String,
    pub background: String,
    pub kind: ContrastKind,
    pub minimum: f64,
    #[serde(default)]
    pub composite_on: Option<Vec<Vec<String>>>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContrastKind {
    Text,
    LargeText,
    NonText,
    FocusIndicator,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum TokenDomain {
    Theme,
    Density,
    Motion,
    Contrast,
    Typography,
    Structural,
}

#[cfg(test)]
mod domain_tests {
    use super::TokenDomain;

    #[test]
    fn token_domain_is_the_closed_v1_vocabulary() {
        let names = [
            "theme",
            "density",
            "motion",
            "contrast",
            "typography",
            "structural",
        ];
        for name in names {
            let domain: TokenDomain = serde_json::from_str(&format!("\"{name}\"")).unwrap();
            assert_eq!(
                serde_json::to_string(&domain).unwrap(),
                format!("\"{name}\"")
            );
        }
        for unknown in ["brand", "system", "future"] {
            assert!(serde_json::from_str::<TokenDomain>(&format!("\"{unknown}\"")).is_err());
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContractCompatibility {
    Exact,
    OlderCompatible,
    NewerCompatible,
}

impl KitTokenContract {
    pub fn load(path: &Path) -> Result<Self, ThemeError> {
        let value: serde_json::Value = read_json(path)?;
        Self::from_value(value).map_err(|error| match error {
            ThemeError::Json { source, .. } => ThemeError::Json {
                path: path.to_path_buf(),
                source,
            },
            other => other,
        })
    }

    pub fn from_value(value: serde_json::Value) -> Result<Self, ThemeError> {
        let contract: Self =
            serde_json::from_value(value.clone()).map_err(|source| ThemeError::Json {
                path: Path::new("token-contract.json").to_path_buf(),
                source,
            })?;
        contract.validate()?;
        let actual = canonical_contract_digest(&value)?;
        let expected = format!("sha256:{actual}");
        if contract.canonical_digest != expected {
            return Err(ThemeError::Contract(format!(
                "canonical digest mismatch: declared {}, computed {expected}",
                contract.canonical_digest
            )));
        }
        Ok(contract)
    }

    pub fn validate(&self) -> Result<ContractCompatibility, ThemeError> {
        if self.schema
            != "https://triesap.github.io/leptos_ui_kit/schema/0.2.0/token-contract.schema.json"
            || self.schema_version != "1.0.0"
            || self.contract_id != "leptos-ui-kit"
            || self.abi_version != 1
            || self.dtcg_version != "2025.10"
            || self.dtcg_profile != "format+color+resolver:2025.10"
        {
            return Err(ThemeError::Contract(
                "unsupported token contract identity".into(),
            ));
        }
        let compatibility = match self.revision {
            1 => ContractCompatibility::OlderCompatible,
            2 => ContractCompatibility::Exact,
            3 => ContractCompatibility::NewerCompatible,
            _ => {
                return Err(ThemeError::Contract(
                    "unsupported token contract revision".into(),
                ));
            }
        };
        if self.tokens.is_empty() {
            return Err(ThemeError::Contract(
                "token mapping inventory is empty".into(),
            ));
        }
        if !valid_digest(&self.canonical_digest) {
            return Err(ThemeError::Contract(
                "canonicalDigest is not a SHA-256 wire value".into(),
            ));
        }
        for namespace in self.extensions.keys() {
            if !valid_extension_namespace(namespace) {
                return Err(ThemeError::Contract(format!(
                    "invalid extension namespace `{namespace}`"
                )));
            }
        }
        let mut paths = BTreeSet::new();
        let mut properties = BTreeSet::new();
        let mut orders = BTreeSet::new();
        let mut previous = None;
        for mapping in &self.tokens {
            if !valid_mapping_path(&mapping.path)
                || !valid_property(&mapping.css_custom_property)
                || !paths.insert(&mapping.path)
                || !properties.insert(&mapping.css_custom_property)
                || !orders.insert(mapping.order)
                || previous.is_some_and(|value| value >= mapping.order)
            {
                return Err(ThemeError::Contract(format!(
                    "invalid or duplicate token mapping `{}`",
                    mapping.path
                )));
            }
            if mapping.required && mapping.default.is_none() {
                return Err(ThemeError::Contract(format!(
                    "required mapping `{}` has no default",
                    mapping.path
                )));
            }
            let token_type = DtcgType::parse(&mapping.token_type).map_err(|_| {
                ThemeError::Contract(format!(
                    "mapping `{}` has unsupported DTCG type `{}`",
                    mapping.path, mapping.token_type
                ))
            })?;
            if mapping.required && !supported_serializer(token_type) {
                return Err(ThemeError::Contract(format!(
                    "required mapping `{}` uses an unsupported ABI v1 serializer",
                    mapping.path
                )));
            }
            if let Some(default) = &mapping.default {
                if contains_alias(default)? {
                    return Err(ThemeError::Contract(format!(
                        "mapping `{}` has a non-concrete default",
                        mapping.path
                    )));
                }
                validate_token_value(token_type, default).map_err(|error| {
                    ThemeError::Contract(format!(
                        "mapping `{}` has an invalid typed default: {error}",
                        mapping.path
                    ))
                })?;
            }
            previous = Some(mapping.order);
        }
        for mapping in &self.tokens {
            let Some(deprecation) = &mapping.deprecation else {
                continue;
            };
            if mapping.required
                || mapping.default.is_some()
                || deprecation.message.trim().is_empty()
            {
                return Err(ThemeError::Contract(format!(
                    "deprecated mapping `{}` must be optional, default-free, and have a message",
                    mapping.path
                )));
            }
            let terminal = self.terminal_mapping(&mapping.path)?;
            if terminal.path == mapping.path
                || terminal.token_type != mapping.token_type
                || terminal.domain != mapping.domain
                || terminal.theme_override != mapping.theme_override
            {
                return Err(ThemeError::Contract(format!(
                    "deprecated mapping `{}` has an incompatible replacement",
                    mapping.path
                )));
            }
        }
        let mut contrast_ids = BTreeSet::new();
        for check in &self.contrast_checks {
            if !valid_identifier(&check.id)
                || !check.minimum.is_finite()
                || !(1.0..=21.0).contains(&check.minimum)
                || !contrast_ids.insert(&check.id)
                || !paths.contains(&check.foreground)
                || !paths.contains(&check.background)
            {
                return Err(ThemeError::Contract(format!(
                    "invalid contrast check `{}`",
                    check.id
                )));
            }
            for path in [&check.foreground, &check.background] {
                let terminal = self.terminal_mapping(path)?;
                if terminal.token_type != "color" {
                    return Err(ThemeError::Contract(format!(
                        "contrast check `{}` references non-color mapping `{path}`",
                        check.id
                    )));
                }
            }
            if let Some(alternatives) = &check.composite_on {
                if alternatives.is_empty() || alternatives.iter().any(Vec::is_empty) {
                    return Err(ThemeError::Contract(format!(
                        "contrast check `{}` has an empty compositing alternative",
                        check.id
                    )));
                }
                for stack in alternatives {
                    let mut seen = BTreeSet::new();
                    for path in stack {
                        if !seen.insert(path)
                            || path == &check.background
                            || self.terminal_mapping(path)?.token_type != "color"
                        {
                            return Err(ThemeError::Contract(format!(
                                "contrast check `{}` has an invalid compositing path `{path}`",
                                check.id
                            )));
                        }
                    }
                }
            }
        }
        Ok(compatibility)
    }

    pub fn terminal_mapping(&self, path: &str) -> Result<&TokenMapping, ThemeError> {
        let mut current = path;
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(current) {
                return Err(ThemeError::Contract(format!(
                    "deprecation replacement cycle at `{path}`"
                )));
            }
            let mapping = self
                .tokens
                .iter()
                .find(|mapping| mapping.path == current)
                .ok_or_else(|| {
                    ThemeError::Contract(format!(
                        "unknown deprecation replacement `{current}` for `{path}`"
                    ))
                })?;
            match &mapping.deprecation {
                Some(deprecation) => current = &deprecation.replacement,
                None => return Ok(mapping),
            }
        }
    }

    pub fn installed_digest(path: &Path) -> Result<String, ThemeError> {
        let bytes = std::fs::read(path).map_err(|source| ThemeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(format!("sha256:{}", sha256(&bytes)))
    }
}

pub fn canonical_contract_digest(value: &serde_json::Value) -> Result<String, ThemeError> {
    let mut semantic = value.clone();
    semantic
        .as_object_mut()
        .ok_or_else(|| ThemeError::Contract("token contract must be an object".into()))?
        .remove("canonicalDigest");
    validate_ijson(&semantic)?;
    let bytes = serde_json_canonicalizer::to_vec(&semantic)
        .map_err(|error| ThemeError::Contract(format!("cannot canonicalize contract: {error}")))?;
    Ok(sha256(&bytes))
}

fn validate_ijson(value: &serde_json::Value) -> Result<(), ThemeError> {
    match value {
        serde_json::Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&integer) {
                    return Err(ThemeError::Contract(
                        "contract integer exceeds the I-JSON interoperable range".into(),
                    ));
                }
            } else if let Some(integer) = value.as_u64() {
                if integer > 9_007_199_254_740_991 {
                    return Err(ThemeError::Contract(
                        "contract integer exceeds the I-JSON interoperable range".into(),
                    ));
                }
            } else {
                let number = value
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .ok_or_else(|| ThemeError::Contract("contract number is not finite".into()))?;
                if number.fract() == 0.0 && number.abs() > 9_007_199_254_740_991.0 {
                    return Err(ThemeError::Contract(
                        "contract integer exceeds the I-JSON interoperable range".into(),
                    ));
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                validate_ijson(value)?;
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                validate_ijson(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn contains_alias(value: &serde_json::Value) -> Result<bool, ThemeError> {
    if dtcg_alias_target(value)?.is_some() {
        return Ok(true);
    }
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                if contains_alias(value)? {
                    return Ok(true);
                }
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                if contains_alias(value)? {
                    return Ok(true);
                }
            }
        }
        _ => {}
    }
    Ok(false)
}

fn supported_serializer(token_type: DtcgType) -> bool {
    matches!(
        token_type,
        DtcgType::Color
            | DtcgType::Dimension
            | DtcgType::Duration
            | DtcgType::Number
            | DtcgType::CubicBezier
            | DtcgType::Shadow
            | DtcgType::FontFamily
    )
}

fn valid_mapping_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment.len() <= 63
                && segment.as_bytes()[0].is_ascii_lowercase()
                && segment.as_bytes()[segment.len() - 1].is_ascii_alphanumeric()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && !segment.contains("--")
        })
}

fn valid_property(value: &str) -> bool {
    value.starts_with("--kit-")
        && value.len() <= 255
        && value[6..]
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.ends_with('-')
        && !value[6..].contains("--")
}

fn valid_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !value.contains("--")
}

fn valid_extension_namespace(value: &str) -> bool {
    value.len() <= 253
        && value.contains('.')
        && value.split('.').all(|segment| {
            let bytes = segment.as_bytes();
            !bytes.is_empty()
                && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
                && (bytes[bytes.len() - 1].is_ascii_lowercase()
                    || bytes[bytes.len() - 1].is_ascii_digit())
                && bytes
                    .iter()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        })
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

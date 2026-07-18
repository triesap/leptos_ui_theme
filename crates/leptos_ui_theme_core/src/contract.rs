use crate::{ThemeError, sha256};
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContractCompatibility {
    Exact,
    OlderCompatible,
    NewerCompatible,
}

impl KitTokenContract {
    pub fn load(path: &Path) -> Result<Self, ThemeError> {
        let bytes = std::fs::read(path).map_err(|source| ThemeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|source| ThemeError::Json {
                path: path.to_path_buf(),
                source,
            })?;
        let contract: Self =
            serde_json::from_value(value.clone()).map_err(|source| ThemeError::Json {
                path: path.to_path_buf(),
                source,
            })?;
        contract.validate()?;
        let actual = canonical_contract_digest(&value)?;
        let expected = contract
            .canonical_digest
            .strip_prefix("sha256:")
            .unwrap_or(&contract.canonical_digest);
        if expected != actual {
            return Err(ThemeError::Contract(format!(
                "canonical digest mismatch: declared {expected}, computed {actual}"
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
            previous = Some(mapping.order);
        }
        let mut contrast_ids = BTreeSet::new();
        for check in &self.contrast_checks {
            if check.id.is_empty()
                || check.id.len() > 63
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
        }
        Ok(compatibility)
    }

    pub fn installed_digest(path: &Path) -> Result<String, ThemeError> {
        let bytes = std::fs::read(path).map_err(|source| ThemeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(sha256(&bytes))
    }
}

pub fn canonical_contract_digest(value: &serde_json::Value) -> Result<String, ThemeError> {
    let mut semantic = value.clone();
    semantic
        .as_object_mut()
        .ok_or_else(|| ThemeError::Contract("token contract must be an object".into()))?
        .remove("canonicalDigest");
    let mut bytes = Vec::new();
    write_canonical(&semantic, &mut bytes)?;
    Ok(sha256(&bytes))
}

fn write_canonical(value: &serde_json::Value, output: &mut Vec<u8>) -> Result<(), ThemeError> {
    match value {
        serde_json::Value::Null => output.extend_from_slice(b"null"),
        serde_json::Value::Bool(value) => {
            output.extend_from_slice(if *value { b"true" } else { b"false" })
        }
        serde_json::Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        serde_json::Value::String(value) => output.extend_from_slice(
            serde_json::to_string(value)
                .map_err(|error| ThemeError::Contract(error.to_string()))?
                .as_bytes(),
        ),
        serde_json::Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical(value, output)?;
            }
            output.push(b']');
        }
        serde_json::Value::Object(values) => {
            output.push(b'{');
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.encode_utf16().cmp(right.encode_utf16()));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                output.extend_from_slice(
                    serde_json::to_string(key)
                        .map_err(|error| ThemeError::Contract(error.to_string()))?
                        .as_bytes(),
                );
                output.push(b':');
                write_canonical(value, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
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

use crate::{ThemeError, read_json, sha256};
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
    pub contrast_checks: Vec<serde_json::Value>,
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
        let contract: Self = read_json(path)?;
        contract.validate()?;
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

use crate::model::KitConfig;
use crate::{KitTokenContract, ThemeError, read_json, sha256};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const LAYER_ORDER: [&str; 3] = [
    "leptos-ui-kit.tokens",
    "leptos-ui-kit.themes",
    "leptos-ui-kit.components",
];

#[derive(Clone, Debug)]
pub struct VerifiedKit {
    pub contract_path: PathBuf,
    pub capability_path: PathBuf,
    pub stylesheet_path: PathBuf,
    pub contract: KitTokenContract,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KitLock {
    pub theme_integration: KitLockRecord,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KitLockRecord {
    pub producer_package: String,
    pub producer_version: String,
    pub producer_checksum: Option<String>,
    pub primitives_package: String,
    pub primitives_requirement: String,
    pub primitives_version: String,
    pub primitives_checksum: String,
    pub presence_abi_version: u32,
    pub contract_path: String,
    pub contract_id: String,
    pub contract_abi_version: u32,
    pub contract_revision: u32,
    pub contract_canonical_digest: String,
    pub contract_bytes_digest: String,
    pub capability_path: String,
    pub capability_bytes_digest: String,
    pub stylesheet_path: String,
    pub stylesheet_bytes_digest: String,
    pub layer_abi_version: u32,
    pub layer_order: Vec<String>,
    pub portal_abi_version: u32,
    pub portal_mount_type: String,
    pub portal_body_host: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KitCapability {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: String,
    pub producer: ProducerCapability,
    pub primitives: PrimitivesCapability,
    pub contract: ContractCapability,
    pub stylesheet: StylesheetCapability,
    pub layer_abi: LayerCapability,
    pub portal_abi: PortalCapability,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerCapability {
    pub package: String,
    pub version: String,
    pub repository: String,
    pub checksum: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PrimitivesCapability {
    pub package: String,
    pub requirement: String,
    pub version: String,
    pub checksum: String,
    pub presence_abi: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContractCapability {
    pub path: String,
    pub contract_id: String,
    pub abi_version: u32,
    pub revision: u32,
    pub canonical_digest: String,
    pub installed_bytes_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StylesheetCapability {
    pub path: String,
    pub installed_bytes_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LayerCapability {
    pub version: u32,
    pub order: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PortalCapability {
    pub version: u32,
    pub mount_type: String,
    pub body_host: bool,
}

pub fn discover_kit(root: &Path, config: &KitConfig) -> Result<VerifiedKit, ThemeError> {
    let mut candidates = Vec::new();
    let mut failures = Vec::new();
    for lock_path in &config.lock_paths {
        match verify_candidate(root, lock_path, config.contract_path.as_deref()) {
            Ok(candidate) => candidates.push(candidate),
            Err(error) => failures.push(format!("{lock_path}: {error}")),
        }
    }
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err(ThemeError::Contract(format!(
            "no valid kit installation candidate: {}",
            failures.join("; ")
        ))),
        _ => Err(ThemeError::Contract(
            "multiple valid kit installation candidates".into(),
        )),
    }
}

fn verify_candidate(
    root: &Path,
    lock_relative: &str,
    explicit_contract: Option<&str>,
) -> Result<VerifiedKit, ThemeError> {
    let lock_path = root.join(lock_relative);
    let lock: KitLock = read_json(&lock_path)?;
    let record = &lock.theme_integration;
    require_constants(record)?;
    if explicit_contract.is_some_and(|path| path != record.contract_path) {
        return Err(ThemeError::Contract(
            "candidate does not match explicit contractPath".into(),
        ));
    }
    let contract_path = root.join(&record.contract_path);
    let capability_path = root.join(&record.capability_path);
    let stylesheet_path = root.join(&record.stylesheet_path);
    verify_digest(&contract_path, &record.contract_bytes_digest)?;
    verify_digest(&capability_path, &record.capability_bytes_digest)?;
    verify_digest(&stylesheet_path, &record.stylesheet_bytes_digest)?;
    let capability: KitCapability = read_json(&capability_path)?;
    verify_capability(&capability, record, &capability_path, &contract_path)?;
    let contract = KitTokenContract::load(&contract_path)?;
    if contract.contract_id != record.contract_id
        || contract.abi_version != record.contract_abi_version
        || contract.revision != record.contract_revision
        || contract.canonical_digest != record.contract_canonical_digest
    {
        return Err(ThemeError::Contract(
            "kit lock and token contract identities differ".into(),
        ));
    }
    Ok(VerifiedKit {
        contract_path,
        capability_path,
        stylesheet_path,
        contract,
    })
}

fn require_constants(record: &KitLockRecord) -> Result<(), ThemeError> {
    let valid = record.producer_package == "leptos_ui_kit_cli"
        && record.producer_checksum.is_none()
        && record.primitives_package == "web_ui_primitives"
        && record.primitives_requirement == ">=0.2.0,<0.3.0"
        && record.presence_abi_version == 2
        && record.layer_abi_version == 1
        && record
            .layer_order
            .iter()
            .map(String::as_str)
            .eq(LAYER_ORDER)
        && record.portal_abi_version == 1
        && record.portal_mount_type == "web_ui_primitives::leptos::PortalMount"
        && record.portal_body_host;
    if valid {
        Ok(())
    } else {
        Err(ThemeError::Contract(
            "kit lock capability constants differ".into(),
        ))
    }
}

fn verify_capability(
    capability: &KitCapability,
    lock: &KitLockRecord,
    capability_path: &Path,
    contract_path: &Path,
) -> Result<(), ThemeError> {
    let manifest_contract = capability_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(&capability.contract.path);
    let valid = capability.schema
        == "https://triesap.github.io/leptos_ui_kit/schema/0.2.0/theme-integration.schema.json"
        && capability.schema_version == "1.0.0"
        && capability.producer.package == lock.producer_package
        && capability.producer.version == lock.producer_version
        && capability.producer.repository == "https://github.com/triesap/leptos_ui_kit"
        && capability.producer.checksum == lock.producer_checksum
        && capability.primitives.package == lock.primitives_package
        && capability.primitives.requirement == lock.primitives_requirement
        && capability.primitives.version == lock.primitives_version
        && capability.primitives.checksum == lock.primitives_checksum
        && capability.primitives.presence_abi == lock.presence_abi_version
        && manifest_contract == contract_path
        && capability.contract.contract_id == lock.contract_id
        && capability.contract.abi_version == lock.contract_abi_version
        && capability.contract.revision == lock.contract_revision
        && capability.contract.canonical_digest == lock.contract_canonical_digest
        && capability.contract.installed_bytes_digest == lock.contract_bytes_digest
        && capability.stylesheet.path == lock.stylesheet_path
        && capability.stylesheet.installed_bytes_digest == lock.stylesheet_bytes_digest
        && capability.layer_abi.version == lock.layer_abi_version
        && capability.layer_abi.order == lock.layer_order
        && capability.portal_abi.version == lock.portal_abi_version
        && capability.portal_abi.mount_type == lock.portal_mount_type
        && capability.portal_abi.body_host == lock.portal_body_host;
    if valid {
        Ok(())
    } else {
        Err(ThemeError::Contract(
            "kit capability manifest and lock differ".into(),
        ))
    }
}

fn verify_digest(path: &Path, expected: &str) -> Result<(), ThemeError> {
    let bytes = std::fs::read(path).map_err(|source| ThemeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let actual = format!("sha256:{}", sha256(&bytes));
    if actual == expected {
        Ok(())
    } else {
        Err(ThemeError::Contract(format!(
            "installed byte digest mismatch for {}",
            path.display()
        )))
    }
}

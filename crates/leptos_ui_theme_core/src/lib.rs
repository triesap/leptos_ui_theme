#![forbid(unsafe_code)]
#![doc = "Core models, validation, and token resolution for `leptos_ui_theme`."]

mod budget;
mod color;
mod contract;
mod diagnostic;
mod dtcg;
mod identity;
mod kit;
mod model;
mod path;
mod resolver;
mod source;

pub use budget::{LimitKind, ResourceBudget};
pub use color::{
    NormalizedColor, Oklch, Srgb, format_css_number, normalize_color, parse_color,
    serialize_color_fallback, serialize_color_modern, validate_contrast,
};
pub use contract::{
    ContractCompatibility, ContrastCheck, ContrastKind, Deprecation, KitTokenContract, TokenDomain,
    TokenMapping, canonical_contract_digest,
};
pub use diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticCollector, DiagnosticLocation, DiagnosticRedirect,
    ErrorCategory, JsonPointer, ProvenanceEntry, ProvenanceOperation, RelatedLocation, Severity,
    SourceLocation,
};
pub use dtcg::{
    DtcgDeprecation, DtcgDocument, DtcgGroup, DtcgNode, DtcgToken, DtcgType,
    alias_target as dtcg_alias_target, apply_shallow_reference_overrides, expand_group_extends,
    parse_json_strict, validate_extensions, validate_reserved_members, validate_token_value,
};
pub use identity::{
    AbiVersion, ContractId, ContractRevision, FileIdentity, IdentityPlatform, Sha256Digest,
    ThemeId, TokenPath,
};
pub use kit::{
    INSTALLED_KIT_CAPABILITY_SCHEMA, InstalledKitCapability, InstalledKitCapabilityRecord,
    KitCapability, VerifiedKit, discover_kit, discover_kit_with_loader,
};
pub use model::{
    AxesConfig, AxisConfig, BootstrapConfig, BootstrapMode, COMPILED_LIMITS, ColorScheme,
    ExternalBootstrap, HtmlConfig, KitConfig, Limits, Outputs, Profile, Profiles, ProjectConfig,
    RuntimeEvidenceConfig, SeededOutputs, SelectionAxis, Selectors, SystemProfile,
    validate_theme_id,
};
pub use path::{LocalReference, LogicalPath, validate_relative_path};
pub use resolver::{ResolvedProfile, ResolvedToken, ThemeCompiler};
pub use source::{OpenedSource, SourceLoader, SourceRole};

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The stable product name used by diagnostics and generated metadata.
pub const PRODUCT_NAME: &str = "leptos_ui_theme";
/// The only supported project configuration filename.
pub const CONFIG_FILE: &str = "leptos-ui-theme.json";
/// The project configuration schema implemented by this release.
pub const PROJECT_SCHEMA: &str =
    "https://triesap.github.io/leptos_ui_theme/schema/0.1.0/project.schema.json";
/// The immutable draft 2020-12 project configuration schema.
pub const PROJECT_SCHEMA_JSON: &str = include_str!("../schemas/project.schema.json");

/// Errors emitted before any output is written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ThemeError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid project configuration: {0}")]
    Config(String),
    #[error("incompatible kit token contract: {0}")]
    Contract(String),
    #[error("token resolution failed: {0}")]
    Resolution(String),
    #[error("unsafe path: {0}")]
    Security(String),
    #[error("resource limit `{resource}` exceeded: observed {observed}, limit {limit}")]
    Limit {
        resource: &'static str,
        limit: u64,
        observed: u64,
    },
}

impl ThemeError {
    #[must_use]
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::Io { .. } => ErrorCategory::Internal,
            Self::Json { .. } | Self::Config(_) | Self::Resolution(_) | Self::Limit { .. } => {
                ErrorCategory::Validation
            }
            Self::Contract(_) => ErrorCategory::Contract,
            Self::Security(_) => ErrorCategory::Security,
        }
    }

    #[must_use]
    pub fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "LUT0070",
            Self::Json { .. } => "LUT0301",
            Self::Config(_) => "LUT0300",
            Self::Contract(_) => "LUT0600",
            Self::Resolution(_) => "LUT0302",
            Self::Security(_) => "LUT0500",
            Self::Limit { .. } => "LUT0303",
        }
    }
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ThemeError> {
    source::read_json_file(path)
}

/// Compute a lowercase SHA-256 digest.
#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{LogicalPath, ProjectConfig, sha256};

    #[test]
    fn default_config_is_valid() {
        ProjectConfig::default().validate().unwrap();
    }

    #[test]
    fn sha256_is_stable() {
        assert_eq!(
            sha256(b"leptos_ui_theme"),
            "33697aab7d70cc50dd8fee884096a5f82132b3e21e01366f7c54f7344144657c"
        );
    }

    #[test]
    fn workspace_paths_reject_parent_traversal() {
        assert!(LogicalPath::new("../shared/kit.json").is_err());
        let mut config = ProjectConfig::default();
        config.kit.lock_paths = vec!["../shared/kit.lock.json".into()];
        assert!(config.validate().is_err());
    }
}

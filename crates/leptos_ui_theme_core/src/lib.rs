#![forbid(unsafe_code)]
#![doc = "Core models, validation, and token resolution for `leptos_ui_theme`."]

mod color;
mod contract;
mod kit;
mod model;
mod resolver;

pub use color::{Srgb, parse_color, validate_contrast};
pub use contract::{
    ContractCompatibility, ContrastCheck, ContrastKind, Deprecation, KitTokenContract, TokenDomain,
    TokenMapping, canonical_contract_digest,
};
pub use kit::{KitCapability, KitLock, VerifiedKit, discover_kit};
pub use model::{
    AxesConfig, AxisConfig, BootstrapConfig, BootstrapMode, ColorScheme, ExternalBootstrap,
    HtmlConfig, KitConfig, Limits, Outputs, Profile, Profiles, ProjectConfig, SeededOutputs,
    Selectors, SystemProfile, validate_theme_id,
};
pub use resolver::{ResolvedProfile, ResolvedToken, ThemeCompiler};

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The stable product name used by diagnostics and generated metadata.
pub const PRODUCT_NAME: &str = "leptos_ui_theme";
/// The only supported project configuration filename.
pub const CONFIG_FILE: &str = "leptos-ui-theme.json";
/// The project configuration schema implemented by this release.
pub const PROJECT_SCHEMA: &str =
    "https://triesap.github.io/leptos_ui_theme/schema/0.1.0/project.schema.json";

/// Errors emitted before any output is written.
#[derive(Debug, thiserror::Error)]
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
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ThemeError> {
    let bytes = std::fs::read(path).map_err(|source| ThemeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| ThemeError::Json {
        path: path.to_path_buf(),
        source,
    })
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
    use super::{ProjectConfig, sha256};

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
}

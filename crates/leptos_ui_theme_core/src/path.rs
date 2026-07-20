use crate::ThemeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::path::PathBuf;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LogicalPath(String);

impl LogicalPath {
    pub fn new(value: impl Into<String>) -> Result<Self, ThemeError> {
        let value = value.into();
        validate_relative_path(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn join(&self, child: &LogicalPath) -> Result<LogicalPath, ThemeError> {
        LogicalPath::new(format!("{}/{}", self.0, child.0))
    }

    #[must_use]
    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(&self.0)
    }
}

impl fmt::Display for LogicalPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for LogicalPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for LogicalPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

pub fn validate_relative_path(value: &str) -> Result<(), ThemeError> {
    if value.is_empty() || value.len() > 4_096 || value.starts_with('/') || value.contains('\\') {
        return Err(ThemeError::Security(value.into()));
    }
    for component in value.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > 255
            || !component.nfc().eq(component.chars())
            || component.ends_with([' ', '.'])
            || component.chars().any(forbidden_path_scalar)
            || is_reserved_windows_component(component)
        {
            return Err(ThemeError::Security(value.into()));
        }
    }
    Ok(())
}

fn forbidden_path_scalar(character: char) -> bool {
    matches!(
        character,
        '\0'..='\u{1f}'
            | '\u{7f}'..='\u{9f}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | ':'
            | '"'
            | '*'
            | '<'
            | '>'
            | '?'
            | '|'
    )
}

fn is_reserved_windows_component(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) || matches_device_number(&upper, "COM")
        || matches_device_number(&upper, "LPT")
}

fn matches_device_number(value: &str, prefix: &str) -> bool {
    let Some(suffix) = value.strip_prefix(prefix) else {
        return false;
    };
    matches!(
        suffix,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_paths_apply_the_same_cross_platform_grammar() {
        for accepted in ["tokens/theme.json", "src/ leading/file.rs", "é/a.json"] {
            assert!(LogicalPath::new(accepted).is_ok(), "{accepted}");
        }
        for rejected in [
            "",
            "/absolute",
            "C:relative",
            "a//b",
            "a/./b",
            "a/../b",
            "a\\b",
            "a/b.",
            "a/b ",
            "a/CON.txt",
            "a/com¹.json",
            "a/\u{202e}b",
            "e\u{301}/a.json",
        ] {
            assert!(LogicalPath::new(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn logical_path_length_boundaries_are_exact() {
        let full = "a".repeat(255);
        let short = "b".repeat(254);
        let mut components = vec![full.as_str(); 15];
        components.extend([short.as_str(), "c"]);
        let exact = components.join("/");
        assert_eq!(exact.len(), 4_096);
        assert!(LogicalPath::new(&exact).is_ok());
        assert!(LogicalPath::new(format!("{exact}d")).is_err());
    }
}

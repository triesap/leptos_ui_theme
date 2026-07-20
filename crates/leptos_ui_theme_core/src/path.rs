use crate::ThemeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::path::{Component, Path, PathBuf};

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

    #[must_use]
    pub fn join(&self, child: &LogicalPath) -> LogicalPath {
        LogicalPath(format!("{}/{}", self.0, child.0))
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
    let path = Path::new(value);
    let first = value.as_bytes().first().copied();
    if value.is_empty()
        || value.contains('\\')
        || value.contains('\0')
        || value.contains("//")
        || path.is_absolute()
        || first.is_some_and(|byte| byte.is_ascii_whitespace())
        || value
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_whitespace())
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ThemeError::Security(value.into()));
    }
    Ok(())
}

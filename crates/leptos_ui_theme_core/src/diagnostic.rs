use crate::{LogicalPath, ThemeError, ThemeId, TokenPath};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCategory {
    Config,
    Dtcg,
    Reference,
    Contract,
    Validation,
    Conflict,
    Security,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct DiagnosticCode(String);

impl DiagnosticCode {
    pub fn new(value: impl Into<String>) -> Result<Self, ThemeError> {
        let value = value.into();
        let bytes = value.as_bytes();
        if bytes.len() == 7
            && bytes.starts_with(b"LUT")
            && bytes[3..].iter().all(u8::is_ascii_digit)
        {
            Ok(Self(value))
        } else {
            Err(ThemeError::Config(format!(
                "invalid diagnostic code `{value}`"
            )))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SourceLocation {
    pub path: LogicalPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<JsonPointer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

impl SourceLocation {
    pub fn new(path: LogicalPath) -> Self {
        Self {
            path,
            pointer: None,
            line: None,
            column: None,
        }
    }

    #[must_use]
    pub fn with_pointer(mut self, pointer: JsonPointer) -> Self {
        self.pointer = Some(pointer);
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct JsonPointer(String);

impl JsonPointer {
    pub fn new(value: impl Into<String>) -> Result<Self, ThemeError> {
        let value = value.into();
        if value.is_empty() || valid_pointer(&value) {
            Ok(Self(value))
        } else {
            Err(ThemeError::Config(format!(
                "invalid JSON Pointer `{value}`"
            )))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid_pointer(value: &str) -> bool {
    if !value.starts_with('/') {
        return false;
    }
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            index += 1;
            if index == bytes.len() || !matches!(bytes[index], b'0' | b'1') {
                return false;
            }
        }
        index += 1;
    }
    true
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RelatedLocation {
    pub message: String,
    pub location: SourceLocation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub category: ErrorCategory,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ThemeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<TokenPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<RelatedLocation>,
}

impl Diagnostic {
    pub fn new(
        code: DiagnosticCode,
        severity: Severity,
        category: ErrorCategory,
        message: impl Into<String>,
    ) -> Result<Self, ThemeError> {
        let message = message.into();
        if message.trim().is_empty() {
            return Err(ThemeError::Config(
                "diagnostic message cannot be empty".into(),
            ));
        }
        Ok(Self {
            code,
            severity,
            category,
            message,
            location: None,
            profile: None,
            token: None,
            help: None,
            related: Vec::new(),
        })
    }

    #[must_use]
    pub fn with_location(mut self, location: SourceLocation) -> Self {
        self.location = Some(location);
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Result<Self, ThemeError> {
        let help = help.into();
        if help.trim().is_empty() {
            return Err(ThemeError::Config("diagnostic help cannot be empty".into()));
        }
        self.help = Some(help);
        Ok(self)
    }
}

use crate::{LogicalPath, ThemeError, ThemeId, TokenPath};
use serde::{Deserialize, Deserializer, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCategory {
    Usage,
    Validation,
    Conflict,
    Security,
    Contract,
    Check,
    Internal,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
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

impl<'de> Deserialize<'de> for DiagnosticCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
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

impl<'de> Deserialize<'de> for JsonPointer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
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

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "DiagnosticLocationWire")]
pub struct DiagnosticLocation {
    pub path: Option<LogicalPath>,
    pub pointer: Option<JsonPointer>,
    pub profile: Option<ThemeId>,
    pub label: Option<String>,
}

impl DiagnosticLocation {
    pub fn for_path(path: LogicalPath) -> Self {
        Self {
            path: Some(path),
            pointer: None,
            profile: None,
            label: None,
        }
    }

    pub fn for_profile(profile: ThemeId) -> Self {
        Self {
            path: None,
            pointer: None,
            profile: Some(profile),
            label: None,
        }
    }

    pub fn validate(&self) -> Result<(), ThemeError> {
        if self.path.is_none() && self.pointer.is_none() && self.profile.is_none() {
            return Err(ThemeError::Config(
                "diagnostic location needs a path, pointer, or profile".into(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn with_pointer(mut self, pointer: JsonPointer) -> Self {
        self.pointer = Some(pointer);
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Result<Self, ThemeError> {
        self.label = Some(label.into());
        self.validate()?;
        Ok(self)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DiagnosticLocationWire {
    path: Option<LogicalPath>,
    pointer: Option<JsonPointer>,
    profile: Option<ThemeId>,
    label: Option<String>,
}

impl TryFrom<DiagnosticLocationWire> for DiagnosticLocation {
    type Error = ThemeError;

    fn try_from(wire: DiagnosticLocationWire) -> Result<Self, Self::Error> {
        let location = Self {
            path: wire.path,
            pointer: wire.pointer,
            profile: wire.profile,
            label: wire.label,
        };
        location.validate()?;
        Ok(location)
    }
}

pub type SourceLocation = DiagnosticLocation;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "DiagnosticRedirectWire")]
pub struct DiagnosticRedirect {
    pub from: TokenPath,
    pub to: TokenPath,
    pub message: String,
}

impl DiagnosticRedirect {
    pub fn validate(&self) -> Result<(), ThemeError> {
        if self.message.is_empty() {
            return Err(ThemeError::Config(
                "diagnostic redirect message cannot be empty".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticRedirectWire {
    from: TokenPath,
    to: TokenPath,
    message: String,
}

impl TryFrom<DiagnosticRedirectWire> for DiagnosticRedirect {
    type Error = ThemeError;

    fn try_from(wire: DiagnosticRedirectWire) -> Result<Self, Self::Error> {
        let redirect = Self {
            from: wire.from,
            to: wire.to,
            message: wire.message,
        };
        redirect.validate()?;
        Ok(redirect)
    }
}

pub type RelatedLocation = DiagnosticRedirect;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "DiagnosticWire")]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub category: ErrorCategory,
    pub severity: Severity,
    pub message: String,
    pub locations: Vec<DiagnosticLocation>,
    pub redirects: Vec<DiagnosticRedirect>,
    pub help: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticWire {
    code: DiagnosticCode,
    category: ErrorCategory,
    severity: Severity,
    message: String,
    locations: Vec<DiagnosticLocation>,
    redirects: Vec<DiagnosticRedirect>,
    help: Option<String>,
}

impl TryFrom<DiagnosticWire> for Diagnostic {
    type Error = ThemeError;

    fn try_from(wire: DiagnosticWire) -> Result<Self, Self::Error> {
        let mut diagnostic = Self {
            code: wire.code,
            category: wire.category,
            severity: wire.severity,
            message: wire.message,
            locations: wire.locations,
            redirects: wire.redirects,
            help: wire.help,
        };
        diagnostic.normalize()?;
        Ok(diagnostic)
    }
}

impl Diagnostic {
    pub fn new(
        code: DiagnosticCode,
        severity: Severity,
        category: ErrorCategory,
        message: impl Into<String>,
    ) -> Result<Self, ThemeError> {
        let message = message.into();
        if message.is_empty() {
            return Err(ThemeError::Config(
                "diagnostic message cannot be empty".into(),
            ));
        }
        Ok(Self {
            code,
            category,
            severity,
            message,
            locations: Vec::new(),
            redirects: Vec::new(),
            help: None,
        })
    }

    pub fn with_location(mut self, location: DiagnosticLocation) -> Result<Self, ThemeError> {
        location.validate()?;
        self.locations.push(location);
        self.normalize()?;
        Ok(self)
    }

    pub fn with_redirect(mut self, redirect: DiagnosticRedirect) -> Result<Self, ThemeError> {
        redirect.validate()?;
        self.redirects.push(redirect);
        self.normalize()?;
        Ok(self)
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Result<Self, ThemeError> {
        self.help = Some(help.into());
        Ok(self)
    }

    pub fn normalize(&mut self) -> Result<(), ThemeError> {
        if self.message.is_empty() {
            return Err(ThemeError::Config(
                "diagnostic message cannot be empty".into(),
            ));
        }
        for location in &self.locations {
            location.validate()?;
        }
        for redirect in &self.redirects {
            redirect.validate()?;
        }
        self.locations.sort();
        self.locations.dedup();
        if self
            .redirects
            .windows(2)
            .any(|pair| pair[0].to != pair[1].from)
        {
            return Err(ThemeError::Config(
                "diagnostic redirects must form one ordered chain".into(),
            ));
        }
        if let Some(first) = self.redirects.first() {
            let mut seen = BTreeSet::from([first.from.clone()]);
            for redirect in &self.redirects {
                if !seen.insert(redirect.to.clone()) {
                    return Err(ThemeError::Config(
                        "diagnostic redirect chain contains a cycle".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceOperation {
    ContractDefault,
    Source,
    Alias,
    Reference,
    SetMerge,
    Modifier,
    ProfileOverride,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceEntry {
    pub path: LogicalPath,
    pub pointer: JsonPointer,
    pub operation: ProvenanceOperation,
    pub value: serde_json::Value,
}

#[derive(Debug)]
pub struct DiagnosticCollector {
    limit: usize,
    saturated: bool,
    diagnostics: Vec<Diagnostic>,
}

impl DiagnosticCollector {
    pub fn new(limit: u32) -> Result<Self, ThemeError> {
        if limit == 0 {
            return Err(ThemeError::Config(
                "diagnostic limit must be positive".into(),
            ));
        }
        Ok(Self {
            limit: limit as usize,
            saturated: false,
            diagnostics: Vec::new(),
        })
    }

    pub fn push(&mut self, mut diagnostic: Diagnostic) -> Result<bool, ThemeError> {
        if self.saturated {
            return Ok(false);
        }
        diagnostic.normalize()?;
        if self.diagnostics.len() < self.limit.saturating_sub(1) {
            self.diagnostics.push(diagnostic);
            return Ok(true);
        }
        self.diagnostics.push(Diagnostic::new(
            DiagnosticCode::new("LUT9999")?,
            Severity::Error,
            ErrorCategory::Validation,
            "diagnostic limit reached; one or more additional diagnostics were not collected",
        )?);
        self.saturated = true;
        Ok(false)
    }

    #[must_use]
    pub fn is_saturated(&self) -> bool {
        self.saturated
    }

    #[must_use]
    pub fn finish(mut self) -> Vec<Diagnostic> {
        self.diagnostics.sort_by(compare_diagnostics);
        self.diagnostics
    }
}

fn compare_diagnostics(left: &Diagnostic, right: &Diagnostic) -> Ordering {
    left.severity
        .cmp(&right.severity)
        .then_with(|| left.code.cmp(&right.code))
        .then_with(|| left.locations.first().cmp(&right.locations.first()))
        .then_with(|| left.redirects.cmp(&right.redirects))
        .then_with(|| left.message.cmp(&right.message))
        .then_with(|| left.help.cmp(&right.help))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validated_newtypes_reject_invalid_deserialization() {
        assert!(serde_json::from_str::<DiagnosticCode>(r#""BAD0000""#).is_err());
        assert!(serde_json::from_str::<JsonPointer>(r#""not-a-pointer""#).is_err());
        assert!(
            serde_json::from_str::<DiagnosticLocation>(
                r#"{"path":null,"pointer":null,"profile":null,"label":null}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Diagnostic>(
                r#"{"code":"LUT0300","category":"validation","severity":"error","message":"","locations":[],"redirects":[],"help":null}"#
            )
            .is_err()
        );
    }

    #[test]
    fn locations_are_sorted_and_deduplicated() {
        let location = DiagnosticLocation::for_path(LogicalPath::new("tokens/a.json").unwrap());
        let diagnostic = Diagnostic::new(
            DiagnosticCode::new("LUT0300").unwrap(),
            Severity::Error,
            ErrorCategory::Validation,
            "invalid token",
        )
        .unwrap()
        .with_location(location.clone())
        .unwrap()
        .with_location(location)
        .unwrap();
        assert_eq!(diagnostic.locations.len(), 1);
    }

    #[test]
    fn collector_stops_at_the_limit_with_one_marker() {
        let mut collector = DiagnosticCollector::new(2).unwrap();
        for code in ["LUT0300", "LUT0301", "LUT0302"] {
            let accepted = collector
                .push(
                    Diagnostic::new(
                        DiagnosticCode::new(code).unwrap(),
                        Severity::Error,
                        ErrorCategory::Validation,
                        "invalid",
                    )
                    .unwrap(),
                )
                .unwrap();
            if code == "LUT0300" {
                assert!(accepted);
            } else {
                assert!(!accepted);
            }
        }
        let diagnostics = collector.finish();
        assert_eq!(diagnostics.len(), 2);
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code.as_str() == "LUT9999")
        );
    }

    #[test]
    fn redirect_cycles_fail() {
        let mut diagnostic = Diagnostic::new(
            DiagnosticCode::new("LUT0600").unwrap(),
            Severity::Warning,
            ErrorCategory::Contract,
            "deprecated",
        )
        .unwrap();
        diagnostic.redirects = vec![
            DiagnosticRedirect {
                from: TokenPath::new("a").unwrap(),
                to: TokenPath::new("b").unwrap(),
                message: "first".into(),
            },
            DiagnosticRedirect {
                from: TokenPath::new("b").unwrap(),
                to: TokenPath::new("a").unwrap(),
                message: "second".into(),
            },
        ];
        assert!(diagnostic.normalize().is_err());
    }

    #[test]
    fn diagnostic_wire_order_and_public_sort_are_stable() {
        let location = DiagnosticLocation::for_path(LogicalPath::new("tokens/a.json").unwrap());
        let warning = Diagnostic::new(
            DiagnosticCode::new("LUT0301").unwrap(),
            Severity::Warning,
            ErrorCategory::Validation,
            "warning",
        )
        .unwrap()
        .with_location(location)
        .unwrap();
        assert_eq!(
            serde_json::to_string(&warning).unwrap(),
            r#"{"code":"LUT0301","category":"validation","severity":"warning","message":"warning","locations":[{"path":"tokens/a.json","pointer":null,"profile":null,"label":null}],"redirects":[],"help":null}"#
        );

        let info = Diagnostic::new(
            DiagnosticCode::new("LUT0001").unwrap(),
            Severity::Info,
            ErrorCategory::Check,
            "info",
        )
        .unwrap();
        let error = Diagnostic::new(
            DiagnosticCode::new("LUT9998").unwrap(),
            Severity::Error,
            ErrorCategory::Internal,
            "error",
        )
        .unwrap();
        let mut collector = DiagnosticCollector::new(4).unwrap();
        collector.push(info).unwrap();
        collector.push(warning).unwrap();
        collector.push(error).unwrap();
        let diagnostics = collector.finish();
        assert_eq!(
            diagnostics
                .iter()
                .map(|item| item.severity)
                .collect::<Vec<_>>(),
            [Severity::Error, Severity::Warning, Severity::Info]
        );
    }
}

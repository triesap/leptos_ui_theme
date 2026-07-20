use crate::{CONFIG_FILE, PROJECT_SCHEMA, ThemeError, validate_relative_path};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;

pub const COMPILED_LIMITS: Limits = Limits {
    file_bytes: 16_777_216,
    files: 8_192,
    aggregate_input_bytes: 536_870_912,
    source_files: 4_096,
    journal_entries: 1_024,
    evidence_manifests: 4_096,
    retained_backups: 4_096,
    retained_backup_bytes: 536_870_912,
    json_depth: 256,
    tokens: 100_000,
    reference_edges: 500_000,
    reference_depth: 256,
    resolver_nodes: 500_000,
    profiles: 256,
    resolver_contexts: 512,
    generated_bytes: 134_217_728,
    generated_artifact_bytes: 16_777_216,
    diagnostics: 10_000,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProjectConfig {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: String,
    pub dtcg_version: String,
    pub kit: KitConfig,
    pub selectors: Selectors,
    pub storage_key: String,
    pub token_root: String,
    pub resolver: String,
    pub profiles: Profiles,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axes: Option<AxesConfig>,
    pub outputs: Outputs,
    pub bootstrap: BootstrapConfig,
    pub html: HtmlConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_evidence: Option<serde_json::Value>,
    pub limits: Limits,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KitConfig {
    pub contract_path: Option<String>,
    pub lock_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selectors {
    pub theme: String,
    pub density: String,
    pub motion: String,
    pub contrast: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profiles {
    pub default: String,
    pub system: SystemProfile,
    pub named: Vec<Profile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SystemProfile {
    pub light: String,
    pub dark: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub label: Option<String>,
    pub color_scheme: ColorScheme,
    pub inputs: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ColorScheme {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionAxis {
    Theme,
    Density,
    Motion,
    Contrast,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AxesConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<AxisConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion: Option<AxisConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contrast: Option<AxisConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AxisConfig {
    pub attribute: String,
    pub default_context: String,
    pub contexts: Vec<String>,
    pub system: Option<SystemAxis>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SystemAxis {
    pub query: String,
    pub context: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Outputs {
    pub css: String,
    pub rust: String,
    pub lock: String,
    pub seeded: SeededOutputs,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SeededOutputs {
    pub module: String,
    pub controller: String,
    pub scope: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapConfig {
    pub mode: BootstrapMode,
    pub external: Option<ExternalBootstrap>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum BootstrapMode {
    InlineCspHash,
    InlineCspNonceTemplate,
    ExternalSync,
    Disabled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExternalBootstrap {
    pub output_path: String,
    pub served_path: String,
    pub public_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HtmlConfig {
    pub index_path: Option<String>,
    pub index_candidates: Option<Vec<String>>,
    pub public_base_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Limits {
    pub file_bytes: u64,
    pub files: u32,
    pub aggregate_input_bytes: u64,
    pub source_files: u32,
    pub journal_entries: u32,
    pub evidence_manifests: u32,
    pub retained_backups: u32,
    pub retained_backup_bytes: u64,
    pub json_depth: u32,
    pub tokens: u32,
    pub reference_edges: u32,
    pub reference_depth: u32,
    pub resolver_nodes: u32,
    pub profiles: u32,
    pub resolver_contexts: u32,
    pub generated_bytes: u64,
    pub generated_artifact_bytes: u64,
    pub diagnostics: u32,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        let profiles = vec![
            Profile {
                id: "light".into(),
                label: None,
                color_scheme: ColorScheme::Light,
                inputs: BTreeMap::from([("theme".into(), "light".into())]),
            },
            Profile {
                id: "dark".into(),
                label: None,
                color_scheme: ColorScheme::Dark,
                inputs: BTreeMap::from([("theme".into(), "dark".into())]),
            },
        ];
        Self {
            schema: PROJECT_SCHEMA.into(),
            schema_version: "1.0.0".into(),
            dtcg_version: "2025.10".into(),
            kit: KitConfig {
                contract_path: None,
                lock_paths: vec!["src/components/ui/_kit/kit.lock.json".into()],
            },
            selectors: Selectors {
                theme: "data-ui-theme".into(),
                density: "data-ui-density".into(),
                motion: "data-ui-motion".into(),
                contrast: "data-ui-contrast".into(),
            },
            storage_key: "leptos-ui-theme".into(),
            token_root: "tokens".into(),
            resolver: "tokens/theme.resolver.json".into(),
            profiles: Profiles {
                default: "light".into(),
                system: SystemProfile {
                    light: "light".into(),
                    dark: "dark".into(),
                },
                named: profiles,
            },
            axes: None,
            outputs: Outputs {
                css: "styles/themes.css".into(),
                rust: "src/theme/generated.rs".into(),
                lock: "src/theme/theme.lock.json".into(),
                seeded: SeededOutputs {
                    module: "src/theme/mod.rs".into(),
                    controller: "src/theme/controller.rs".into(),
                    scope: "src/theme/scope.rs".into(),
                },
            },
            bootstrap: BootstrapConfig {
                mode: BootstrapMode::InlineCspHash,
                external: None,
            },
            html: HtmlConfig {
                index_path: None,
                index_candidates: Some(vec!["index.html".into()]),
                public_base_path: "/".into(),
            },
            runtime_evidence: None,
            limits: Limits {
                file_bytes: 2_097_152,
                files: 1_024,
                aggregate_input_bytes: 67_108_864,
                source_files: 512,
                journal_entries: 128,
                evidence_manifests: 512,
                retained_backups: 128,
                retained_backup_bytes: 67_108_864,
                json_depth: 64,
                tokens: 25_000,
                reference_edges: 125_000,
                reference_depth: 64,
                resolver_nodes: 12_500,
                profiles: 128,
                resolver_contexts: 128,
                generated_bytes: 16_777_216,
                generated_artifact_bytes: 2_097_152,
                diagnostics: 1_250,
            },
        }
    }
}

impl ProjectConfig {
    pub fn validate(&self) -> Result<(), ThemeError> {
        if self.schema != PROJECT_SCHEMA || self.schema_version != "1.0.0" {
            return Err(ThemeError::Config("unsupported project schema".into()));
        }
        if self.dtcg_version != "2025.10" {
            return Err(ThemeError::Config("dtcgVersion must be 2025.10".into()));
        }
        self.limits.validate()?;
        for attribute in [
            &self.selectors.theme,
            &self.selectors.density,
            &self.selectors.motion,
            &self.selectors.contrast,
        ] {
            validate_attribute(attribute)?;
        }
        let selectors = [
            &self.selectors.theme,
            &self.selectors.density,
            &self.selectors.motion,
            &self.selectors.contrast,
        ];
        if selectors.iter().collect::<BTreeSet<_>>().len() != selectors.len() {
            return Err(ThemeError::Config(
                "selector attributes must be unique".into(),
            ));
        }
        if selectors.iter().any(|attribute| {
            matches!(
                attribute.as_str(),
                "data-leptos-ui-theme-bootstrap" | "data-leptos-ui-theme-bootstrap-outcome"
            )
        }) {
            return Err(ThemeError::Config(
                "selector attributes use a reserved runtime name".into(),
            ));
        }
        if self.profiles.named.is_empty()
            || self.profiles.named.len() > self.limits.profiles as usize
        {
            return Err(ThemeError::Config(
                "profile count is outside configured limits".into(),
            ));
        }
        if self.kit.lock_paths.is_empty() || self.kit.lock_paths.len() > 32 {
            return Err(ThemeError::Config(
                "kit.lockPaths must contain between 1 and 32 paths".into(),
            ));
        }
        if self.storage_key.is_empty()
            || self.storage_key.len() > 255
            || !self.storage_key.nfc().eq(self.storage_key.chars())
            || self.storage_key.chars().any(forbidden_storage_scalar)
        {
            return Err(ThemeError::Config("storageKey is invalid".into()));
        }
        let mut ids = BTreeSet::new();
        for profile in &self.profiles.named {
            validate_theme_id(&profile.id)?;
            if !ids.insert(profile.id.as_str()) {
                return Err(ThemeError::Config(format!(
                    "duplicate profile `{}`",
                    profile.id
                )));
            }
            if profile
                .label
                .as_ref()
                .is_some_and(|label| label.trim().is_empty() || label.len() > 255)
            {
                return Err(ThemeError::Config("profile labels cannot be empty".into()));
            }
            if profile.inputs.get("theme").map(String::as_str) != Some(&profile.id) {
                return Err(ThemeError::Config(format!(
                    "profile `{}` must select its own theme context",
                    profile.id
                )));
            }
            for (axis, context) in &profile.inputs {
                if !matches!(axis.as_str(), "theme" | "density" | "motion" | "contrast") {
                    return Err(ThemeError::Config(format!(
                        "profile `{}` uses unknown axis `{axis}`",
                        profile.id
                    )));
                }
                validate_theme_id(context)?;
            }
        }
        if self.profiles.default != self.profiles.system.light
            || !ids.contains(self.profiles.default.as_str())
            || !ids.contains(self.profiles.system.dark.as_str())
        {
            return Err(ThemeError::Config(
                "profiles.default must equal profiles.system.light".into(),
            ));
        }
        match (&self.html.index_path, &self.html.index_candidates) {
            (Some(_), None) => {}
            (None, Some(candidates)) if !candidates.is_empty() && candidates.len() <= 16 => {
                let unique: BTreeSet<_> = candidates.iter().collect();
                if unique.len() != candidates.len() {
                    return Err(ThemeError::Config("duplicate HTML index candidate".into()));
                }
            }
            _ => {
                return Err(ThemeError::Config(
                    "exactly one of html.indexPath and html.indexCandidates must be non-null"
                        .into(),
                ));
            }
        }
        match (&self.bootstrap.mode, &self.bootstrap.external) {
            (BootstrapMode::ExternalSync, Some(external)) => {
                validate_relative_path(&external.output_path)?;
                validate_relative_path(&external.served_path)?;
                if external
                    .public_path
                    .as_ref()
                    .is_some_and(|path| !valid_public_path(path))
                {
                    return Err(ThemeError::Config(
                        "bootstrap.external.publicPath is invalid".into(),
                    ));
                }
            }
            (BootstrapMode::ExternalSync, None) => {
                return Err(ThemeError::Config(
                    "external bootstrap config is missing".into(),
                ));
            }
            (_, None) => {}
            (_, Some(_)) => {
                return Err(ThemeError::Config(
                    "bootstrap.external is allowed only for external-sync".into(),
                ));
            }
        }
        self.validate_axes()?;
        if !valid_public_path(&self.html.public_base_path) {
            return Err(ThemeError::Config("html.publicBasePath is invalid".into()));
        }
        let light = self.profile(&self.profiles.system.light)?;
        let dark = self.profile(&self.profiles.system.dark)?;
        if light.color_scheme != ColorScheme::Light
            || dark.color_scheme != ColorScheme::Dark
            || light.id == dark.id
        {
            return Err(ThemeError::Config(
                "invalid System light/dark profiles".into(),
            ));
        }
        for path in self.all_paths() {
            validate_relative_path(path)?;
        }
        let mut outputs = BTreeSet::new();
        for path in [
            &self.outputs.css,
            &self.outputs.rust,
            &self.outputs.lock,
            &self.outputs.seeded.module,
            &self.outputs.seeded.controller,
            &self.outputs.seeded.scope,
        ] {
            if !outputs.insert(path) {
                return Err(ThemeError::Config(format!(
                    "overlapping output path `{path}`"
                )));
            }
        }
        self.validate_path_boundaries(&outputs)?;
        Ok(())
    }

    fn validate_axes(&self) -> Result<(), ThemeError> {
        let Some(axes) = &self.axes else {
            return Ok(());
        };
        for (name, selector, axis) in [
            ("density", &self.selectors.density, axes.density.as_ref()),
            ("motion", &self.selectors.motion, axes.motion.as_ref()),
            ("contrast", &self.selectors.contrast, axes.contrast.as_ref()),
        ] {
            let Some(axis) = axis else { continue };
            if &axis.attribute != selector || axis.contexts.is_empty() {
                return Err(ThemeError::Config(format!(
                    "invalid {name} axis attribute/contexts"
                )));
            }
            let unique: BTreeSet<_> = axis.contexts.iter().collect();
            if unique.len() != axis.contexts.len()
                || axis.contexts.len() > self.limits.resolver_contexts as usize
                || !unique.contains(&axis.default_context)
            {
                return Err(ThemeError::Config(format!(
                    "invalid {name} axis context inventory"
                )));
            }
            for context in &axis.contexts {
                validate_theme_id(context)?;
            }
            match (name, &axis.system) {
                ("density", None) => {}
                ("density", Some(_)) => {
                    return Err(ThemeError::Config(
                        "density axis cannot have a system query".into(),
                    ));
                }
                (_, Some(system)) => {
                    let expected = if name == "motion" {
                        "(prefers-reduced-motion: reduce)"
                    } else {
                        "(prefers-contrast: more)"
                    };
                    if system.query != expected || !unique.contains(&system.context) {
                        return Err(ThemeError::Config(format!("invalid {name} system mapping")));
                    }
                }
                (_, None) => {}
            }
        }
        Ok(())
    }

    fn validate_path_boundaries(&self, outputs: &BTreeSet<&String>) -> Result<(), ThemeError> {
        let mut protected = vec![
            CONFIG_FILE,
            self.token_root.as_str(),
            self.resolver.as_str(),
        ];
        protected.extend(self.kit.lock_paths.iter().map(String::as_str));
        if let Some(path) = self.kit.contract_path.as_deref() {
            protected.push(path);
        }
        if let Some(path) = self.html.index_path.as_deref() {
            protected.push(path);
        }
        if let Some(candidates) = &self.html.index_candidates {
            protected.extend(candidates.iter().map(String::as_str));
        }
        for output in outputs {
            for input in &protected {
                if paths_overlap(output, input) {
                    return Err(ThemeError::Config(format!(
                        "output `{output}` overlaps protected input `{input}`"
                    )));
                }
            }
        }
        if !is_descendant(&self.resolver, &self.token_root) {
            return Err(ThemeError::Config(
                "resolver must be below tokenRoot".into(),
            ));
        }
        if let Some(external) = &self.bootstrap.external {
            if outputs
                .iter()
                .any(|path| paths_overlap(path, &external.output_path))
                || protected
                    .iter()
                    .any(|path| paths_overlap(path, &external.output_path))
            {
                return Err(ThemeError::Config(format!(
                    "external bootstrap output `{}` overlaps another path",
                    external.output_path
                )));
            }
        }
        Ok(())
    }

    pub fn profile(&self, id: &str) -> Result<&Profile, ThemeError> {
        self.profiles
            .named
            .iter()
            .find(|profile| profile.id == id)
            .ok_or_else(|| ThemeError::Config(format!("unknown profile `{id}`")))
    }

    fn all_paths(&self) -> Vec<&str> {
        let mut paths = vec![
            self.token_root.as_str(),
            self.resolver.as_str(),
            self.outputs.css.as_str(),
            self.outputs.rust.as_str(),
            self.outputs.lock.as_str(),
            self.outputs.seeded.module.as_str(),
            self.outputs.seeded.controller.as_str(),
            self.outputs.seeded.scope.as_str(),
        ];
        if let Some(path) = self.kit.contract_path.as_deref() {
            paths.push(path);
        }
        paths.extend(self.kit.lock_paths.iter().map(String::as_str));
        if let Some(path) = self.html.index_path.as_deref() {
            paths.push(path);
        }
        if let Some(candidates) = &self.html.index_candidates {
            paths.extend(candidates.iter().map(String::as_str));
        }
        paths
    }
}

fn is_descendant(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right || is_descendant(left, right) || is_descendant(right, left)
}

fn valid_public_path(value: &str) -> bool {
    value.starts_with('/')
        && value.ends_with('/')
        && !value.contains("//")
        && !value.contains(['\\', '?', '#', '\0'])
}

pub fn validate_theme_id(value: &str) -> Result<(), ThemeError> {
    crate::ThemeId::new(value).map(|_| ())
}

fn validate_attribute(value: &str) -> Result<(), ThemeError> {
    let suffix = value.strip_prefix("data-").unwrap_or_default();
    let valid = (6..=63).contains(&value.len())
        && suffix
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && suffix.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        });
    if valid {
        Ok(())
    } else {
        Err(ThemeError::Config(format!(
            "invalid selector attribute `{value}`"
        )))
    }
}

fn forbidden_storage_scalar(scalar: char) -> bool {
    matches!(
        scalar,
        '\u{0000}'..='\u{001f}'
            | '\u{007f}'..='\u{009f}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

impl Limits {
    pub fn validate(&self) -> Result<(), ThemeError> {
        for (name, value, compiled) in [
            ("fileBytes", self.file_bytes, COMPILED_LIMITS.file_bytes),
            (
                "files",
                u64::from(self.files),
                u64::from(COMPILED_LIMITS.files),
            ),
            (
                "aggregateInputBytes",
                self.aggregate_input_bytes,
                COMPILED_LIMITS.aggregate_input_bytes,
            ),
            (
                "sourceFiles",
                u64::from(self.source_files),
                u64::from(COMPILED_LIMITS.source_files),
            ),
            (
                "journalEntries",
                u64::from(self.journal_entries),
                u64::from(COMPILED_LIMITS.journal_entries),
            ),
            (
                "evidenceManifests",
                u64::from(self.evidence_manifests),
                u64::from(COMPILED_LIMITS.evidence_manifests),
            ),
            (
                "retainedBackups",
                u64::from(self.retained_backups),
                u64::from(COMPILED_LIMITS.retained_backups),
            ),
            (
                "retainedBackupBytes",
                self.retained_backup_bytes,
                COMPILED_LIMITS.retained_backup_bytes,
            ),
            (
                "jsonDepth",
                u64::from(self.json_depth),
                u64::from(COMPILED_LIMITS.json_depth),
            ),
            (
                "tokens",
                u64::from(self.tokens),
                u64::from(COMPILED_LIMITS.tokens),
            ),
            (
                "referenceEdges",
                u64::from(self.reference_edges),
                u64::from(COMPILED_LIMITS.reference_edges),
            ),
            (
                "referenceDepth",
                u64::from(self.reference_depth),
                u64::from(COMPILED_LIMITS.reference_depth),
            ),
            (
                "resolverNodes",
                u64::from(self.resolver_nodes),
                u64::from(COMPILED_LIMITS.resolver_nodes),
            ),
            (
                "profiles",
                u64::from(self.profiles),
                u64::from(COMPILED_LIMITS.profiles),
            ),
            (
                "resolverContexts",
                u64::from(self.resolver_contexts),
                u64::from(COMPILED_LIMITS.resolver_contexts),
            ),
            (
                "generatedBytes",
                self.generated_bytes,
                COMPILED_LIMITS.generated_bytes,
            ),
            (
                "generatedArtifactBytes",
                self.generated_artifact_bytes,
                COMPILED_LIMITS.generated_artifact_bytes,
            ),
            (
                "diagnostics",
                u64::from(self.diagnostics),
                u64::from(COMPILED_LIMITS.diagnostics),
            ),
        ] {
            if value == 0 || value > compiled {
                return Err(ThemeError::Config(format!(
                    "limits.{name} must be within 1..={compiled}"
                )));
            }
        }
        Ok(())
    }
}

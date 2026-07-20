use crate::{CONFIG_FILE, PROJECT_SCHEMA, ThemeError, validate_relative_path};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

pub const COMPILED_LIMITS: Limits = Limits {
    file_bytes: 16_777_216,
    files: 8_192,
    aggregate_input_bytes: 536_870_912,
    source_files: 4_096,
    journal_entries: 1_024,
    evidence_manifests: 4_096,
    retained_backups: 1_024,
    retained_backup_bytes: 536_870_912,
    json_depth: 256,
    tokens: 200_000,
    reference_edges: 1_000_000,
    reference_depth: 256,
    resolver_nodes: 100_000,
    profiles: 1_024,
    resolver_contexts: 1_024,
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
    pub runtime_evidence: Option<RuntimeEvidenceConfig>,
    pub limits: Limits,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KitConfig {
    #[serde(deserialize_with = "deserialize_required_option")]
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
    #[serde(deserialize_with = "deserialize_required_option")]
    pub label: Option<String>,
    pub color_scheme: ColorScheme,
    pub inputs: IndexMap<String, String>,
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
    #[serde(deserialize_with = "deserialize_required_option")]
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
    #[serde(deserialize_with = "deserialize_required_option")]
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
    #[serde(deserialize_with = "deserialize_required_option")]
    pub public_path: Option<String>,
}

impl Default for ExternalBootstrap {
    fn default() -> Self {
        Self {
            output_path: "public/theme-init.js".into(),
            served_path: "theme-init.js".into(),
            public_path: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HtmlConfig {
    #[serde(deserialize_with = "deserialize_required_option")]
    pub index_path: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub index_candidates: Option<Vec<String>>,
    pub public_base_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEvidenceConfig {
    pub path: String,
    pub required: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
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

impl Default for Limits {
    fn default() -> Self {
        Self {
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
        }
    }
}

impl Default for ProjectConfig {
    fn default() -> Self {
        let profiles = vec![
            Profile {
                id: "light".into(),
                label: None,
                color_scheme: ColorScheme::Light,
                inputs: IndexMap::from([("theme".into(), "light".into())]),
            },
            Profile {
                id: "dark".into(),
                label: None,
                color_scheme: ColorScheme::Dark,
                inputs: IndexMap::from([("theme".into(), "dark".into())]),
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
            limits: Limits::default(),
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
        for path in &self.kit.lock_paths {
            validate_relative_path(path)?;
        }
        ensure_unique_paths(
            "kit.lockPaths",
            self.kit.lock_paths.iter().map(String::as_str),
        )?;
        if let Some(path) = self.kit.contract_path.as_deref() {
            validate_relative_path(path)?;
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
                .is_some_and(|label| label.is_empty() || label.len() > 255)
            {
                return Err(ThemeError::Config("profile labels cannot be empty".into()));
            }
            if !profile.inputs.contains_key("theme") {
                return Err(ThemeError::Config(format!(
                    "profile `{}` must select a theme context",
                    profile.id
                )));
            }
            for (axis, context) in &profile.inputs {
                validate_resolver_identifier(axis)?;
                validate_resolver_identifier(context)?;
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
            (Some(path), None) => validate_relative_path(path)?,
            (None, Some(candidates)) if !candidates.is_empty() && candidates.len() <= 16 => {
                ensure_unique_paths(
                    "html.indexCandidates",
                    candidates.iter().map(String::as_str),
                )?;
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
                if file_name(&external.output_path) != file_name(&external.served_path) {
                    return Err(ThemeError::Config(
                        "bootstrap external outputPath and servedPath filenames differ".into(),
                    ));
                }
                let derived = join_public_path(&self.html.public_base_path, &external.served_path)?;
                if external
                    .public_path
                    .as_ref()
                    .is_some_and(|path| path != &derived)
                {
                    return Err(ThemeError::Config(format!(
                        "bootstrap.external.publicPath must equal `{derived}`"
                    )));
                }
                for index in self.index_paths() {
                    let parent = path_parent(index);
                    if !is_descendant_of_directory(&external.output_path, parent) {
                        return Err(ThemeError::Config(format!(
                            "bootstrap external output `{}` is not below index directory `{parent}`",
                            external.output_path
                        )));
                    }
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
        if !valid_public_base_path(&self.html.public_base_path) {
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
        if let Some(runtime_evidence) = &self.runtime_evidence {
            validate_relative_path(&runtime_evidence.path)?;
        }
        let outputs = [
            &self.outputs.css,
            &self.outputs.rust,
            &self.outputs.lock,
            &self.outputs.seeded.module,
            &self.outputs.seeded.controller,
            &self.outputs.seeded.scope,
        ];
        for (index, path) in outputs.iter().enumerate() {
            for other in &outputs[index + 1..] {
                if paths_collide(path, other) {
                    return Err(ThemeError::Config(format!(
                        "output `{path}` overlaps output `{other}`"
                    )));
                }
            }
        }
        let output_set: BTreeSet<_> = outputs.into_iter().collect();
        if output_set.len() != outputs.len() {
            return Err(ThemeError::Config("output paths must be unique".into()));
        }
        for path in outputs {
            if path == &self.token_root {
                return Err(ThemeError::Config(format!(
                    "output `{path}` cannot be the token root"
                )));
            }
        }
        self.validate_path_boundaries(&output_set)?;
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
                validate_css_context(context)?;
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
        if let Some(runtime_evidence) = &self.runtime_evidence {
            protected.push(&runtime_evidence.path);
        }
        if let Some(path) = self.html.index_path.as_deref() {
            protected.push(path);
        }
        if let Some(candidates) = &self.html.index_candidates {
            protected.extend(candidates.iter().map(String::as_str));
        }
        for output in outputs {
            for input in &protected {
                if paths_collide(output, input) {
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
        if let Some(external) = &self.bootstrap.external
            && (outputs
                .iter()
                .any(|path| paths_collide(path, &external.output_path))
                || protected
                    .iter()
                    .any(|path| paths_collide(path, &external.output_path)))
        {
            return Err(ThemeError::Config(format!(
                "external bootstrap output `{}` overlaps another path",
                external.output_path
            )));
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
        paths.extend(self.kit.lock_paths.iter().map(String::as_str));
        if let Some(path) = self.kit.contract_path.as_deref() {
            paths.push(path);
        }
        if let Some(runtime_evidence) = &self.runtime_evidence {
            paths.push(&runtime_evidence.path);
        }
        if let Some(path) = self.html.index_path.as_deref() {
            paths.push(path);
        }
        if let Some(candidates) = &self.html.index_candidates {
            paths.extend(candidates.iter().map(String::as_str));
        }
        paths
    }

    fn index_paths(&self) -> Vec<&str> {
        self.html
            .index_path
            .iter()
            .map(String::as_str)
            .chain(
                self.html
                    .index_candidates
                    .iter()
                    .flatten()
                    .map(String::as_str),
            )
            .collect()
    }
}

fn is_descendant(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right || is_descendant(left, right) || is_descendant(right, left)
}

fn paths_collide(left: &str, right: &str) -> bool {
    paths_overlap(left, right) || paths_overlap(&fold_path(left), &fold_path(right))
}

fn fold_path(path: &str) -> String {
    path.chars().flat_map(char::to_lowercase).collect()
}

fn ensure_unique_paths<'a>(
    name: &str,
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<(), ThemeError> {
    let mut exact = BTreeSet::new();
    let mut folded = BTreeSet::new();
    for path in paths {
        validate_relative_path(path)?;
        if !exact.insert(path) || !folded.insert(fold_path(path)) {
            return Err(ThemeError::Config(format!(
                "{name} contains colliding paths"
            )));
        }
    }
    Ok(())
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn validate_resolver_identifier(value: &str) -> Result<(), ThemeError> {
    if value.is_empty() || value.len() > 255 || !value.nfc().eq(value.chars()) {
        Err(ThemeError::Config(
            "resolver identifier must be a nonempty NFC string of at most 255 bytes".into(),
        ))
    } else {
        Ok(())
    }
}

fn validate_css_context(value: &str) -> Result<(), ThemeError> {
    validate_resolver_identifier(value)?;
    if value.contains('\0') {
        Err(ThemeError::Config(
            "selected axis contexts cannot contain U+0000".into(),
        ))
    } else {
        Ok(())
    }
}

fn is_descendant_of_directory(path: &str, directory: &str) -> bool {
    directory.is_empty() || is_descendant(path, directory)
}

fn path_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn valid_public_base_path(value: &str) -> bool {
    value == "/"
        || (value.starts_with('/')
            && value.ends_with('/')
            && canonical_public_path(value, true).is_some())
}

fn join_public_path(base: &str, served: &str) -> Result<String, ThemeError> {
    if !valid_public_base_path(base) {
        return Err(ThemeError::Config("html.publicBasePath is invalid".into()));
    }
    let encoded = served
        .split('/')
        .map(encode_url_segment)
        .collect::<Vec<_>>()
        .join("/");
    Ok(format!("{base}{encoded}"))
}

fn encode_url_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn canonical_public_path(value: &str, directory: bool) -> Option<()> {
    if !value.is_ascii()
        || !value.starts_with('/')
        || value.contains(['\\', '?', '#'])
        || directory != value.ends_with('/')
    {
        return None;
    }
    let body = value.strip_prefix('/')?;
    let body = if directory {
        body.strip_suffix('/').unwrap_or(body)
    } else {
        body
    };
    if body.is_empty() {
        return directory.then_some(());
    }
    for segment in body.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return None;
        }
        let bytes = segment.as_bytes();
        let mut decoded_segment = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' {
                if index + 2 >= bytes.len()
                    || !bytes[index + 1].is_ascii_hexdigit()
                    || !bytes[index + 2].is_ascii_hexdigit()
                    || bytes[index + 1].is_ascii_lowercase()
                    || bytes[index + 2].is_ascii_lowercase()
                {
                    return None;
                }
                let decoded = (hex_value(bytes[index + 1])? << 4) | hex_value(bytes[index + 2])?;
                if decoded.is_ascii_alphanumeric()
                    || matches!(decoded, b'-' | b'.' | b'_' | b'~' | b'/' | b'\\' | b'%')
                {
                    return None;
                }
                decoded_segment.push(decoded);
                index += 3;
            } else if !bytes[index].is_ascii_alphanumeric()
                && !matches!(bytes[index], b'-' | b'.' | b'_' | b'~')
            {
                return None;
            } else {
                decoded_segment.push(bytes[index]);
                index += 1;
            }
        }
        let decoded = std::str::from_utf8(&decoded_segment).ok()?;
        if decoded == "." || decoded == ".." {
            return None;
        }
    }
    Some(())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PROJECT_SCHEMA_JSON;

    #[test]
    fn project_schema_identity_and_default_bytes_are_stable() {
        let schema: serde_json::Value = serde_json::from_str(PROJECT_SCHEMA_JSON).unwrap();
        assert_eq!(schema["$id"], PROJECT_SCHEMA);

        let value = serde_json::to_value(ProjectConfig::default()).unwrap();
        assert_eq!(
            value.as_object().unwrap().keys().collect::<Vec<_>>(),
            [
                "$schema",
                "schemaVersion",
                "dtcgVersion",
                "kit",
                "selectors",
                "storageKey",
                "tokenRoot",
                "resolver",
                "profiles",
                "outputs",
                "bootstrap",
                "html",
                "limits",
            ]
        );
        assert_eq!(
            value["kit"]["lockPaths"],
            serde_json::json!(["src/components/ui/_kit/kit.lock.json"])
        );
        assert!(value["html"]["indexPath"].is_null());
        assert_eq!(
            value["html"]["indexCandidates"],
            serde_json::json!(["index.html"])
        );

        let schema_keys = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let mut runtime_keys = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        runtime_keys.extend(["axes", "runtimeEvidence"]);
        assert_eq!(schema_keys, runtime_keys);

        let schema_limit_keys = schema["$defs"]["limits"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let runtime_limit_keys = value["limits"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(schema_limit_keys, runtime_limit_keys);
    }

    #[test]
    fn omitted_limit_members_use_only_frozen_defaults() {
        let limits: Limits = serde_json::from_str("{}").unwrap();
        assert_eq!(
            serde_json::to_value(limits).unwrap(),
            serde_json::to_value(Limits::default()).unwrap()
        );
    }

    #[test]
    fn semantic_validation_rejects_collisions_and_conditionals() {
        let mut config = ProjectConfig::default();
        config.outputs.rust = "styles/themes.css/generated.rs".into();
        assert!(config.validate().is_err());

        let mut config = ProjectConfig::default();
        config.kit.lock_paths = vec!["kit/LOCK.json".into(), "kit/lock.json".into()];
        assert!(config.validate().is_err());

        let mut config = ProjectConfig::default();
        config.bootstrap.mode = BootstrapMode::ExternalSync;
        assert!(config.validate().is_err());

        let mut value = serde_json::to_value(ProjectConfig::default()).unwrap();
        value["unknown"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<ProjectConfig>(value).is_err());

        let mut value = serde_json::to_value(ProjectConfig::default()).unwrap();
        value["kit"].as_object_mut().unwrap().remove("contractPath");
        assert!(serde_json::from_value::<ProjectConfig>(value).is_err());

        let mut value = serde_json::to_value(ProjectConfig::default()).unwrap();
        value["html"].as_object_mut().unwrap().remove("indexPath");
        assert!(serde_json::from_value::<ProjectConfig>(value).is_err());
    }

    #[test]
    fn external_bootstrap_paths_are_derived_canonically() {
        let mut config = ProjectConfig::default();
        config.html.public_base_path = "/app/".into();
        config.bootstrap = BootstrapConfig {
            mode: BootstrapMode::ExternalSync,
            external: Some(ExternalBootstrap {
                output_path: "public/café.js".into(),
                served_path: "assets/café.js".into(),
                public_path: Some("/app/assets/caf%C3%A9.js".into()),
            }),
        };
        assert!(config.validate().is_ok());

        config.bootstrap.external.as_mut().unwrap().public_path =
            Some("/app/assets/caf%c3%a9.js".into());
        assert!(config.validate().is_err());
    }
}

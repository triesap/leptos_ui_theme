use crate::{CodegenError, GeneratedArtifact};
use leptos_ui_theme_core::{LogicalPath, ThemeError, sha256};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Ownership {
    #[serde(rename = "generated/lock-owned")]
    GeneratedLockOwned,
    #[serde(rename = "seeded/app-owned")]
    SeededAppOwned,
    #[serde(rename = "user-authored")]
    UserAuthored,
    #[serde(rename = "external-kit-owned")]
    ExternalKitOwned,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeScope {
    WholeFile,
    HtmlOwnedRegion,
    Directory,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeOperation {
    Create,
    Replace,
    Remove,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesiredArtifactState {
    Present,
    Absent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArtifactManifestEntry {
    pub path: String,
    pub scope: ChangeScope,
    pub ownership: Ownership,
    pub state: DesiredArtifactState,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArtifactManifest {
    pub schema_version: String,
    pub entries: Vec<ArtifactManifestEntry>,
}

impl ArtifactManifest {
    pub fn new(mut entries: Vec<ArtifactManifestEntry>) -> Result<Self, CodegenError> {
        entries.sort_by(|left, right| {
            left.path
                .as_bytes()
                .cmp(right.path.as_bytes())
                .then_with(|| scope_order(left.scope).cmp(&scope_order(right.scope)))
        });
        let mut seen = BTreeSet::new();
        for entry in &entries {
            LogicalPath::new(entry.path.clone()).map_err(CodegenError::Core)?;
            if !seen.insert((entry.path.as_str(), entry.scope))
                || (entry.state == DesiredArtifactState::Present) != entry.digest.is_some()
                || (entry.state == DesiredArtifactState::Absent
                    && entry.ownership != Ownership::GeneratedLockOwned)
                || entry
                    .digest
                    .as_deref()
                    .is_some_and(|digest| !valid_digest(digest))
            {
                return Err(CodegenError::Core(ThemeError::Config(format!(
                    "invalid desired artifact `{}`",
                    entry.path
                ))));
            }
        }
        Ok(Self {
            schema_version: "1.0.0".into(),
            entries,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Snapshot {
    pub path: String,
    pub exists: bool,
    pub digest: Option<String>,
    pub mode: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Change {
    pub operation: ChangeOperation,
    pub scope: ChangeScope,
    pub path: String,
    pub ownership: Ownership,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub before_mode: Option<u32>,
    pub after_mode: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlanV1 {
    pub schema_version: String,
    pub snapshots: Vec<Snapshot>,
    pub changes: Vec<Change>,
    pub digest: String,
}

impl PlanV1 {
    #[must_use]
    pub fn changed_paths(&self) -> Vec<String> {
        self.changes
            .iter()
            .map(|change| change.path.clone())
            .collect()
    }

    pub fn revalidate(&self, root: &Path) -> Result<(), CodegenError> {
        for snapshot in &self.snapshots {
            let current = snapshot_path(root, &snapshot.path)?;
            if current != *snapshot {
                return Err(CodegenError::Conflict(snapshot.path.clone()));
            }
        }
        Ok(())
    }
}

pub fn plan_artifacts(
    root: &Path,
    artifacts: &[GeneratedArtifact],
) -> Result<PlanV1, CodegenError> {
    let manifest = ArtifactManifest::new(
        artifacts
            .iter()
            .map(|artifact| ArtifactManifestEntry {
                path: artifact.path.clone(),
                scope: artifact.scope,
                ownership: artifact.ownership,
                state: DesiredArtifactState::Present,
                digest: Some(format!("sha256:{}", sha256(&artifact.bytes))),
            })
            .collect(),
    )?;
    plan_manifest(root, artifacts, &manifest)
}

pub fn plan_manifest(
    root: &Path,
    artifacts: &[GeneratedArtifact],
    manifest: &ArtifactManifest,
) -> Result<PlanV1, CodegenError> {
    let payloads = artifacts
        .iter()
        .map(|artifact| ((artifact.path.as_str(), artifact.scope), artifact))
        .collect::<std::collections::BTreeMap<_, _>>();
    if payloads.len() != artifacts.len() {
        return Err(CodegenError::Core(ThemeError::Config(
            "duplicate artifact payload".into(),
        )));
    }
    let mut consumed = BTreeSet::new();
    let mut snapshots = Vec::with_capacity(manifest.entries.len());
    let mut changes = Vec::new();
    for entry in &manifest.entries {
        let snapshot = snapshot_path(root, &entry.path)?;
        match entry.state {
            DesiredArtifactState::Present => {
                let artifact = payloads
                    .get(&(entry.path.as_str(), entry.scope))
                    .ok_or_else(|| {
                        CodegenError::Core(ThemeError::Config(format!(
                            "desired artifact `{}` has no payload",
                            entry.path
                        )))
                    })?;
                consumed.insert((entry.path.as_str(), entry.scope));
                let expected = format!("sha256:{}", sha256(&artifact.bytes));
                if entry.digest.as_deref() != Some(&expected)
                    || entry.ownership != artifact.ownership
                {
                    return Err(CodegenError::Core(ThemeError::Config(format!(
                        "desired artifact `{}` differs from its payload",
                        entry.path
                    ))));
                }
                let expected_mode = publication_mode(artifact, &snapshot);
                if snapshot.digest.as_deref() != Some(&expected) || snapshot.mode != expected_mode {
                    let operation = if snapshot.exists {
                        ChangeOperation::Replace
                    } else {
                        ChangeOperation::Create
                    };
                    validate_action(artifact.ownership, artifact.scope, operation)?;
                    changes.push(Change {
                        operation,
                        scope: artifact.scope,
                        path: artifact.path.clone(),
                        ownership: artifact.ownership,
                        before_digest: snapshot.digest.clone(),
                        after_digest: Some(expected),
                        before_mode: snapshot.mode,
                        after_mode: expected_mode,
                    });
                }
            }
            DesiredArtifactState::Absent => {
                if snapshot.exists {
                    validate_action(entry.ownership, entry.scope, ChangeOperation::Remove)?;
                    changes.push(Change {
                        operation: ChangeOperation::Remove,
                        scope: entry.scope,
                        path: entry.path.clone(),
                        ownership: entry.ownership,
                        before_digest: snapshot.digest.clone(),
                        after_digest: None,
                        before_mode: snapshot.mode,
                        after_mode: None,
                    });
                }
            }
        }
        snapshots.push(snapshot);
    }
    if consumed.len() != payloads.len() {
        return Err(CodegenError::Core(ThemeError::Config(
            "artifact payload is outside the desired manifest".into(),
        )));
    }
    snapshots.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    changes.sort_by(|left, right| {
        left.path
            .as_bytes()
            .cmp(right.path.as_bytes())
            .then_with(|| scope_order(left.scope).cmp(&scope_order(right.scope)))
            .then_with(|| operation_order(left.operation).cmp(&operation_order(right.operation)))
    });
    let mut plan = PlanV1 {
        schema_version: "1.0.0".into(),
        snapshots,
        changes,
        digest: String::new(),
    };
    let mut semantic = serde_json::to_value(&plan)?;
    semantic
        .as_object_mut()
        .ok_or_else(|| CodegenError::Core(ThemeError::Config("plan must be an object".into())))?
        .remove("digest");
    let bytes = serde_json_canonicalizer::to_vec(&semantic)?;
    plan.digest = format!("sha256:{}", sha256(&bytes));
    Ok(plan)
}

fn validate_action(
    ownership: Ownership,
    scope: ChangeScope,
    operation: ChangeOperation,
) -> Result<(), CodegenError> {
    let allowed = match ownership {
        Ownership::GeneratedLockOwned => scope != ChangeScope::Directory,
        Ownership::SeededAppOwned => {
            scope == ChangeScope::WholeFile && operation == ChangeOperation::Create
        }
        Ownership::UserAuthored => {
            scope == ChangeScope::WholeFile && operation == ChangeOperation::Create
        }
        Ownership::ExternalKitOwned => false,
    };
    if !allowed {
        return Err(CodegenError::Conflict(format!(
            "{ownership:?} does not permit {operation:?} for {scope:?}"
        )));
    }
    Ok(())
}

fn scope_order(scope: ChangeScope) -> u8 {
    match scope {
        ChangeScope::Directory => 0,
        ChangeScope::WholeFile => 1,
        ChangeScope::HtmlOwnedRegion => 2,
    }
}

fn operation_order(operation: ChangeOperation) -> u8 {
    match operation {
        ChangeOperation::Create => 0,
        ChangeOperation::Replace => 1,
        ChangeOperation::Remove => 2,
    }
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn snapshot_path(root: &Path, relative: &str) -> Result<Snapshot, CodegenError> {
    let logical = LogicalPath::new(relative.to_owned()).map_err(CodegenError::Core)?;
    let path = root.join(logical.to_path_buf());
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Snapshot {
                path: relative.into(),
                exists: false,
                digest: None,
                mode: None,
            });
        }
        Err(source) => {
            return Err(CodegenError::Io {
                path: PathBuf::from(relative),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CodegenError::Core(ThemeError::Security(format!(
            "planned target is not a regular file: `{relative}`"
        ))));
    }
    let bytes = std::fs::read(&path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(relative),
        source,
    })?;
    Ok(Snapshot {
        path: relative.into(),
        exists: true,
        digest: Some(format!("sha256:{}", sha256(&bytes))),
        mode: file_mode(&metadata),
    })
}

fn publication_mode(artifact: &GeneratedArtifact, snapshot: &Snapshot) -> Option<u32> {
    #[cfg(unix)]
    {
        if !snapshot.exists
            || (artifact.ownership == Ownership::GeneratedLockOwned
                && artifact.scope == ChangeScope::WholeFile)
        {
            Some(0o644)
        } else {
            snapshot.mode
        }
    }
    #[cfg(not(unix))]
    {
        let _ = artifact;
        let _ = snapshot;
        None
    }
}

fn file_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

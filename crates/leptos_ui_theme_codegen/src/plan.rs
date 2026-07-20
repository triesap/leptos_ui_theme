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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Snapshot {
    pub path: String,
    pub exists: bool,
    pub digest: Option<String>,
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
    let mut seen = BTreeSet::new();
    let mut snapshots = Vec::with_capacity(artifacts.len());
    let mut changes = Vec::new();
    for artifact in artifacts {
        LogicalPath::new(artifact.path.clone()).map_err(CodegenError::Core)?;
        if !seen.insert(artifact.path.as_str()) {
            return Err(CodegenError::Core(ThemeError::Config(format!(
                "duplicate planned artifact `{}`",
                artifact.path
            ))));
        }
        let snapshot = snapshot_path(root, &artifact.path)?;
        let expected = format!("sha256:{}", sha256(&artifact.bytes));
        if snapshot.digest.as_deref() != Some(&expected) {
            changes.push(Change {
                operation: if snapshot.exists {
                    ChangeOperation::Replace
                } else {
                    ChangeOperation::Create
                },
                scope: artifact.scope,
                path: artifact.path.clone(),
                ownership: artifact.ownership,
                before_digest: snapshot.digest.clone(),
                after_digest: Some(expected),
            });
        }
        snapshots.push(snapshot);
    }
    snapshots.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
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
    })
}

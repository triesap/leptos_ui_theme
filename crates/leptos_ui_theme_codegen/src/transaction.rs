use crate::{ChangeOperation, ChangeScope, CodegenError, GeneratedArtifact, Ownership, PlanV1};
use fs2::FileExt;
use leptos_ui_theme_core::{LogicalPath, ThemeError, sha256};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const STATE_DIRECTORY: &str = ".leptos-ui-theme";
const TRANSACTIONS_DIRECTORY: &str = "transactions";
const APPLY_LOCK: &str = "apply.lock";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplyCommand {
    Init,
    Add,
    Build,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CommitKind {
    Lock,
    Journal,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Journal {
    schema_version: String,
    transaction_id: String,
    command: ApplyCommand,
    commit_kind: CommitKind,
    plan_digest: String,
    operations: Vec<Operation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Operation {
    ordinal: usize,
    operation: ChangeOperation,
    scope: ChangeScope,
    ownership: Ownership,
    path: String,
    pre_digest: Option<String>,
    expected_digest: Option<String>,
    target_mode: Option<u32>,
    stage_path: Option<String>,
    backup_path: Option<String>,
    commit_role: CommitRole,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CommitRole {
    Ordinary,
    ThemeLock,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StateRecord {
    schema_version: String,
    transaction_id: String,
    sequence: usize,
    ordinal: Option<usize>,
    state: OperationState,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum OperationState {
    Allocated,
    Staged,
    BackedUp,
    Installed,
    Committed,
}

pub fn apply_transaction(
    root: &Path,
    artifacts: &[GeneratedArtifact],
    plan: &PlanV1,
    command: ApplyCommand,
    theme_lock_path: Option<&str>,
) -> Result<Vec<String>, CodegenError> {
    apply_transaction_with_wait(
        root,
        artifacts,
        plan,
        command,
        theme_lock_path,
        Duration::ZERO,
    )
}

pub fn apply_transaction_with_wait(
    root: &Path,
    artifacts: &[GeneratedArtifact],
    plan: &PlanV1,
    command: ApplyCommand,
    theme_lock_path: Option<&str>,
    lock_wait: Duration,
) -> Result<Vec<String>, CodegenError> {
    apply_transaction_with_wait_checked(
        root,
        artifacts,
        plan,
        command,
        theme_lock_path,
        lock_wait,
        || Ok(()),
    )
}

pub(crate) fn apply_transaction_with_wait_checked(
    root: &Path,
    artifacts: &[GeneratedArtifact],
    plan: &PlanV1,
    command: ApplyCommand,
    theme_lock_path: Option<&str>,
    lock_wait: Duration,
    revalidate_inputs: impl FnOnce() -> Result<(), CodegenError>,
) -> Result<Vec<String>, CodegenError> {
    if plan.changes.is_empty() {
        revalidate_inputs()?;
        return Ok(Vec::new());
    }
    plan.revalidate(root)?;
    let state = root.join(STATE_DIRECTORY);
    ensure_state_directory(&state)?;
    let lock_path = state.join(APPLY_LOCK);
    let lock = open_lock(&lock_path)?;
    lock_exclusive_with_wait(&lock, lock_wait)?;

    let transactions = state.join(TRANSACTIONS_DIRECTORY);
    let result = (|| {
        ensure_private_directory(&transactions)?;
        recover_locked(root, &transactions)?;
        revalidate_inputs()?;
        plan.revalidate(root)?;
        let artifacts = artifacts
            .iter()
            .map(|artifact| (artifact.path.as_str(), artifact))
            .collect::<BTreeMap<_, _>>();
        let transaction_id = select_transaction_id(root, &transactions, plan)?;
        let journal = build_journal(&transaction_id, plan, command, theme_lock_path, &artifacts)?;
        let active = publish_journal(&transactions, &journal)?;
        let mut sequence = 0;
        for operation in &journal.operations {
            let bytes = artifacts
                .get(operation.path.as_str())
                .map(|artifact| artifact.bytes.as_slice());
            install_operation(root, &active, &journal, operation, bytes, &mut sequence)?;
        }
        verify_final_tree(root, &journal)?;
        if journal.commit_kind == CommitKind::Journal {
            append_state(
                &active,
                &journal,
                None,
                OperationState::Committed,
                &mut sequence,
            )?;
        }
        enter_cleanup(&transactions, &active, &journal.transaction_id)?;
        finish_cleanup(root, &transactions, &journal.transaction_id, &journal)?;
        Ok(plan.changed_paths())
    })();
    let result = match result {
        Ok(paths) => Ok(paths),
        Err(original) => match recover_locked(root, &transactions) {
            Ok(()) => Err(original),
            Err(recovery) => Err(recovery),
        },
    };

    let unlock = FileExt::unlock(&lock);
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(source)) => Err(CodegenError::Io {
            path: PathBuf::from(format!("{STATE_DIRECTORY}/{APPLY_LOCK}")),
            source,
        }),
        (Ok(paths), Ok(())) => Ok(paths),
    }
}

fn lock_exclusive_with_wait(lock: &File, wait: Duration) -> Result<(), CodegenError> {
    let started = Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() >= wait {
                    return Err(CodegenError::Conflict(
                        "the theme apply lock is busy".into(),
                    ));
                }
                let remaining = wait.saturating_sub(started.elapsed());
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(source) => {
                return Err(CodegenError::Io {
                    path: PathBuf::from(format!("{STATE_DIRECTORY}/{APPLY_LOCK}")),
                    source,
                });
            }
        }
    }
}

pub fn recover(root: &Path) -> Result<bool, CodegenError> {
    let state = root.join(STATE_DIRECTORY);
    if !state.exists() {
        return Ok(false);
    }
    validate_directory(&state)?;
    let transactions = state.join(TRANSACTIONS_DIRECTORY);
    if !transactions.exists() {
        return Ok(false);
    }
    validate_directory(&transactions)?;
    let lock_path = state.join(APPLY_LOCK);
    let lock = open_existing_lock(&lock_path)?;
    lock.try_lock_exclusive()
        .map_err(|source| CodegenError::Io {
            path: PathBuf::from(format!("{STATE_DIRECTORY}/{APPLY_LOCK}")),
            source,
        })?;
    let had_lifecycle = !inventory(&transactions)?.is_empty();
    let result = recover_locked(root, &transactions);
    let unlock = FileExt::unlock(&lock);
    result?;
    unlock.map_err(|source| CodegenError::Io {
        path: PathBuf::from(format!("{STATE_DIRECTORY}/{APPLY_LOCK}")),
        source,
    })?;
    Ok(had_lifecycle)
}

pub fn ensure_no_active_transaction(root: &Path) -> Result<(), CodegenError> {
    let transactions = root.join(STATE_DIRECTORY).join(TRANSACTIONS_DIRECTORY);
    if !transactions.exists() {
        return Ok(());
    }
    let entries = inventory(&transactions)?;
    if entries
        .iter()
        .any(|entry| matches!(entry.kind, LifecycleKind::Active))
    {
        return Err(CodegenError::Conflict(
            "an active theme transaction requires recovery".into(),
        ));
    }
    Ok(())
}

fn build_journal(
    transaction_id: &str,
    plan: &PlanV1,
    command: ApplyCommand,
    theme_lock_path: Option<&str>,
    artifacts: &BTreeMap<&str, &GeneratedArtifact>,
) -> Result<Journal, CodegenError> {
    let commit_kind = if matches!(command, ApplyCommand::Init | ApplyCommand::Build) {
        CommitKind::Lock
    } else {
        CommitKind::Journal
    };
    let mut operations = Vec::with_capacity(plan.changes.len());
    let mut ordered_changes = plan
        .changes
        .iter()
        .filter(|change| theme_lock_path != Some(change.path.as_str()))
        .collect::<Vec<_>>();
    ordered_changes.extend(
        plan.changes
            .iter()
            .filter(|change| theme_lock_path == Some(change.path.as_str())),
    );
    for (ordinal, change) in ordered_changes.into_iter().enumerate() {
        let journal_operation = if change.scope == ChangeScope::HtmlOwnedRegion {
            ChangeOperation::Replace
        } else {
            change.operation
        };
        let expected_digest = artifacts
            .get(change.path.as_str())
            .map(|artifact| format!("sha256:{}", sha256(&artifact.bytes)));
        let pre_digest = if change.scope == ChangeScope::HtmlOwnedRegion {
            change.container_before_digest.clone()
        } else {
            change.before_digest.clone()
        };
        match journal_operation {
            ChangeOperation::Create | ChangeOperation::Replace => {
                let planned_container = if change.scope == ChangeScope::HtmlOwnedRegion {
                    &change.container_after_digest
                } else {
                    &change.after_digest
                };
                if *planned_container != expected_digest {
                    return Err(CodegenError::Conflict(change.path.clone()));
                }
            }
            ChangeOperation::Remove => {
                if expected_digest.is_some()
                    || change.after_digest.is_some()
                    || change.before_digest.is_none()
                {
                    return Err(CodegenError::Conflict(change.path.clone()));
                }
            }
        }
        let stage_path = (journal_operation != ChangeOperation::Remove)
            .then(|| {
                sibling_relative(
                    &change.path,
                    &format!(".leptos-ui-theme-{transaction_id}-{ordinal:06}.stage"),
                )
            })
            .transpose()?;
        let backup_path = pre_digest
            .as_ref()
            .map(|_| {
                sibling_relative(
                    &change.path,
                    &format!(".leptos-ui-theme-{transaction_id}-{ordinal:06}.backup"),
                )
            })
            .transpose()?;
        operations.push(Operation {
            ordinal,
            operation: journal_operation,
            scope: change.scope,
            ownership: change.ownership,
            path: change.path.clone(),
            pre_digest,
            expected_digest,
            target_mode: change.after_mode,
            stage_path,
            backup_path,
            commit_role: if theme_lock_path == Some(change.path.as_str()) {
                CommitRole::ThemeLock
            } else {
                CommitRole::Ordinary
            },
        });
    }
    let journal = Journal {
        schema_version: "1.0.0".into(),
        transaction_id: transaction_id.into(),
        command,
        commit_kind,
        plan_digest: plan.digest.clone(),
        operations,
    };
    validate_journal(&journal)?;
    Ok(journal)
}

fn install_operation(
    root: &Path,
    active: &Path,
    journal: &Journal,
    operation: &Operation,
    bytes: Option<&[u8]>,
    sequence: &mut usize,
) -> Result<(), CodegenError> {
    let target = project_path(root, &operation.path)?;
    let stage = operation
        .stage_path
        .as_deref()
        .map(|path| project_path(root, path))
        .transpose()?;
    let backup = operation
        .backup_path
        .as_deref()
        .map(|path| project_path(root, path))
        .transpose()?;
    ensure_parent_chain(root, &target)?;
    verify_pre_state(&target, operation.pre_digest.as_deref(), &operation.path)?;
    if let (Some(stage), Some(relative)) = (&stage, &operation.stage_path) {
        require_absent(stage, relative)?;
    }
    if let (Some(backup), Some(relative)) = (&backup, &operation.backup_path) {
        require_absent(backup, relative)?;
    }

    if operation.operation == ChangeOperation::Remove {
        let backup = backup.ok_or_else(|| CodegenError::Conflict(operation.path.clone()))?;
        std::fs::rename(&target, &backup).map_err(|source| CodegenError::Io {
            path: PathBuf::from(&operation.path),
            source,
        })?;
        sync_parent(&target)?;
        append_state(
            active,
            journal,
            Some(operation.ordinal),
            OperationState::BackedUp,
            sequence,
        )?;
        verify_pre_state(&target, None, &operation.path)?;
        return append_state(
            active,
            journal,
            Some(operation.ordinal),
            OperationState::Installed,
            sequence,
        );
    }
    let stage = stage.ok_or_else(|| CodegenError::Conflict(operation.path.clone()))?;
    let stage_path = operation
        .stage_path
        .as_deref()
        .ok_or_else(|| CodegenError::Conflict(operation.path.clone()))?;
    let bytes = bytes.ok_or_else(|| {
        CodegenError::Core(ThemeError::Config(format!(
            "journal operation has no payload for `{}`",
            operation.path
        )))
    })?;
    let expected_digest = operation
        .expected_digest
        .as_deref()
        .ok_or_else(|| CodegenError::Conflict(operation.path.clone()))?;
    let mut stage_file = create_private_file(&stage)?;
    sync_parent(&stage)?;
    append_state(
        active,
        journal,
        Some(operation.ordinal),
        OperationState::Allocated,
        sequence,
    )?;
    stage_file
        .write_all(bytes)
        .map_err(|source| CodegenError::Io {
            path: PathBuf::from(stage_path),
            source,
        })?;
    set_file_mode(&stage, operation.target_mode)?;
    stage_file.sync_all().map_err(|source| CodegenError::Io {
        path: PathBuf::from(stage_path),
        source,
    })?;
    drop(stage_file);
    verify_digest(&stage, expected_digest, stage_path)?;
    verify_file_mode(&stage, operation.target_mode, stage_path)?;
    append_state(
        active,
        journal,
        Some(operation.ordinal),
        OperationState::Staged,
        sequence,
    )?;

    if let Some(backup) = &backup {
        std::fs::rename(&target, backup).map_err(|source| CodegenError::Io {
            path: PathBuf::from(&operation.path),
            source,
        })?;
        sync_parent(&target)?;
        append_state(
            active,
            journal,
            Some(operation.ordinal),
            OperationState::BackedUp,
            sequence,
        )?;
    }
    std::fs::rename(&stage, &target).map_err(|source| CodegenError::Io {
        path: PathBuf::from(&operation.path),
        source,
    })?;
    sync_parent(&target)?;
    verify_digest(&target, expected_digest, &operation.path)?;
    verify_file_mode(&target, operation.target_mode, &operation.path)?;
    append_state(
        active,
        journal,
        Some(operation.ordinal),
        OperationState::Installed,
        sequence,
    )
}

fn recover_locked(root: &Path, transactions: &Path) -> Result<(), CodegenError> {
    let entries = inventory(transactions)?;
    if entries.len() > 1 {
        return Err(CodegenError::Conflict(
            "multiple theme transaction lifecycle directories exist".into(),
        ));
    }
    let Some(entry) = entries.first() else {
        return Ok(());
    };
    match entry.kind {
        LifecycleKind::Bootstrap => cleanup_bootstrap(transactions, entry),
        LifecycleKind::Active | LifecycleKind::Cleanup => {
            let journal = read_journal(&entry.path)?;
            if journal.transaction_id != entry.transaction_id {
                return Err(CodegenError::Conflict(
                    "transaction directory and journal IDs differ".into(),
                ));
            }
            let committed = transaction_committed(root, &entry.path, &journal)?;
            if committed {
                verify_final_tree(root, &journal)?;
            } else {
                rollback_operations(root, &journal)?;
            }
            if entry.kind == LifecycleKind::Active {
                enter_cleanup(transactions, &entry.path, &entry.transaction_id)?;
            }
            finish_cleanup(root, transactions, &entry.transaction_id, &journal)
        }
    }
}

fn transaction_committed(
    root: &Path,
    directory: &Path,
    journal: &Journal,
) -> Result<bool, CodegenError> {
    match journal.commit_kind {
        CommitKind::Journal => {
            let states = read_states(directory, journal)?;
            Ok(states
                .last()
                .is_some_and(|state| state.state == OperationState::Committed))
        }
        CommitKind::Lock => {
            let lock_operation = journal
                .operations
                .iter()
                .find(|operation| operation.commit_role == CommitRole::ThemeLock)
                .or_else(|| journal.operations.last())
                .ok_or_else(|| CodegenError::Conflict("lock journal has no operations".into()))?;
            let expected = lock_operation.expected_digest.as_deref().ok_or_else(|| {
                CodegenError::Conflict("lock operation has no final digest".into())
            })?;
            let target = project_path(root, &lock_operation.path)?;
            Ok(path_digest(&target)?.as_deref() == Some(expected))
        }
    }
}

fn rollback_operations(root: &Path, journal: &Journal) -> Result<(), CodegenError> {
    for operation in journal.operations.iter().rev() {
        let target = project_path(root, &operation.path)?;
        let stage = operation
            .stage_path
            .as_deref()
            .map(|path| project_path(root, path))
            .transpose()?;
        let target_digest = path_digest(&target)?;
        if let Some(backup_path) = &operation.backup_path {
            let backup = project_path(root, backup_path)?;
            let backup_digest = path_digest(&backup)?;
            if backup_digest.is_some() {
                if backup_digest != operation.pre_digest {
                    return Err(CodegenError::Conflict(operation.path.clone()));
                }
                if target_digest == operation.expected_digest {
                    if target_digest.is_some() {
                        remove_regular(&target, &operation.path)?;
                    }
                } else if target_digest.is_some() {
                    return Err(CodegenError::Conflict(operation.path.clone()));
                }
                std::fs::rename(&backup, &target).map_err(|source| CodegenError::Io {
                    path: PathBuf::from(&operation.path),
                    source,
                })?;
                sync_parent(&target)?;
            } else if target_digest != operation.pre_digest {
                return Err(CodegenError::Conflict(operation.path.clone()));
            }
        } else if target_digest == operation.expected_digest {
            if target_digest.is_some() {
                remove_regular(&target, &operation.path)?;
            }
        } else if target_digest != operation.pre_digest {
            return Err(CodegenError::Conflict(operation.path.clone()));
        }
        if let (Some(stage), Some(relative)) = (&stage, &operation.stage_path)
            && stage.exists()
        {
            remove_regular(stage, relative)?;
        }
    }
    Ok(())
}

fn verify_final_tree(root: &Path, journal: &Journal) -> Result<(), CodegenError> {
    for operation in &journal.operations {
        let target = project_path(root, &operation.path)?;
        if let Some(expected) = &operation.expected_digest {
            verify_digest(&target, expected, &operation.path)?;
            verify_file_mode(&target, operation.target_mode, &operation.path)?;
        } else {
            verify_pre_state(&target, None, &operation.path)?;
        }
    }
    Ok(())
}

fn set_file_mode(path: &Path, mode: Option<u32>) -> Result<(), CodegenError> {
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(
            |source| CodegenError::Io {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

fn verify_file_mode(
    path: &Path,
    expected: Option<u32>,
    relative: &str,
) -> Result<(), CodegenError> {
    #[cfg(unix)]
    if let Some(expected) = expected {
        use std::os::unix::fs::PermissionsExt;
        let actual = std::fs::metadata(path)
            .map_err(|source| CodegenError::Io {
                path: PathBuf::from(relative),
                source,
            })?
            .permissions()
            .mode()
            & 0o777;
        if actual != expected {
            return Err(CodegenError::Conflict(format!(
                "{relative} has mode {actual:04o}, expected {expected:04o}"
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = (path, expected, relative);
    Ok(())
}

fn publish_journal(transactions: &Path, journal: &Journal) -> Result<PathBuf, CodegenError> {
    let bootstrap = transactions.join(format!(".bootstrap-{}", journal.transaction_id));
    ensure_private_directory(&bootstrap)?;
    let pending = bootstrap.join("journal.pending");
    write_canonical_new(&pending, journal, "journal.pending")?;
    let journal_path = bootstrap.join("journal.json");
    std::fs::rename(&pending, &journal_path).map_err(|source| CodegenError::Io {
        path: PathBuf::from("journal.json"),
        source,
    })?;
    sync_directory(&bootstrap)?;
    let active = transactions.join(&journal.transaction_id);
    std::fs::rename(&bootstrap, &active).map_err(|source| CodegenError::Io {
        path: PathBuf::from(format!(
            "{TRANSACTIONS_DIRECTORY}/{}",
            journal.transaction_id
        )),
        source,
    })?;
    sync_directory(transactions)?;
    Ok(active)
}

fn append_state(
    active: &Path,
    journal: &Journal,
    ordinal: Option<usize>,
    state: OperationState,
    sequence: &mut usize,
) -> Result<(), CodegenError> {
    let record = StateRecord {
        schema_version: "1.0.0".into(),
        transaction_id: journal.transaction_id.clone(),
        sequence: *sequence,
        ordinal,
        state,
    };
    let pending = active.join(format!("state-{:08}.json.pending", *sequence));
    write_canonical_new(&pending, &record, "transaction state")?;
    let committed = active.join(format!("state-{:08}.json", *sequence));
    std::fs::rename(&pending, &committed).map_err(|source| CodegenError::Io {
        path: PathBuf::from(format!("state-{:08}.json", *sequence)),
        source,
    })?;
    sync_directory(active)?;
    *sequence += 1;
    Ok(())
}

fn enter_cleanup(
    transactions: &Path,
    active: &Path,
    transaction_id: &str,
) -> Result<(), CodegenError> {
    if active
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name.starts_with(".cleanup-"))
    {
        return Ok(());
    }
    let cleanup = transactions.join(format!(".cleanup-{transaction_id}"));
    std::fs::rename(active, &cleanup).map_err(|source| CodegenError::Io {
        path: PathBuf::from(format!(".cleanup-{transaction_id}")),
        source,
    })?;
    sync_directory(transactions)
}

fn finish_cleanup(
    root: &Path,
    transactions: &Path,
    transaction_id: &str,
    journal: &Journal,
) -> Result<(), CodegenError> {
    let cleanup = transactions.join(format!(".cleanup-{transaction_id}"));
    for operation in &journal.operations {
        if let Some(path) = &operation.backup_path {
            let physical = project_path(root, path)?;
            if physical.exists() {
                verify_digest(
                    &physical,
                    operation
                        .pre_digest
                        .as_deref()
                        .ok_or_else(|| CodegenError::Conflict(operation.path.clone()))?,
                    path,
                )?;
                remove_regular(&physical, path)?;
            }
        }
        if let Some(stage_path) = &operation.stage_path {
            let stage = project_path(root, stage_path)?;
            if stage.exists() {
                return Err(CodegenError::Conflict(stage_path.clone()));
            }
        }
    }
    for entry in std::fs::read_dir(&cleanup).map_err(|source| CodegenError::Io {
        path: PathBuf::from(format!(".cleanup-{transaction_id}")),
        source,
    })? {
        let entry = entry.map_err(|source| CodegenError::Io {
            path: PathBuf::from(format!(".cleanup-{transaction_id}")),
            source,
        })?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| CodegenError::Conflict("non-UTF-8 transaction state name".into()))?;
        if name == "journal.json"
            || state_sequence(&name).is_some()
            || state_pending_sequence(&name).is_some()
        {
            remove_regular(&entry.path(), &name)?;
        } else {
            return Err(CodegenError::Conflict(format!(
                "unknown transaction cleanup member `{name}`"
            )));
        }
    }
    sync_directory(&cleanup)?;
    std::fs::remove_dir(&cleanup).map_err(|source| CodegenError::Io {
        path: PathBuf::from(format!(".cleanup-{transaction_id}")),
        source,
    })?;
    sync_directory(transactions)
}

fn cleanup_bootstrap(transactions: &Path, entry: &LifecycleEntry) -> Result<(), CodegenError> {
    for child in std::fs::read_dir(&entry.path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(&entry.name),
        source,
    })? {
        let child = child.map_err(|source| CodegenError::Io {
            path: PathBuf::from(&entry.name),
            source,
        })?;
        let name = child
            .file_name()
            .into_string()
            .map_err(|_| CodegenError::Conflict("non-UTF-8 bootstrap member".into()))?;
        if !matches!(name.as_str(), "journal.pending" | "journal.json") {
            return Err(CodegenError::Conflict(format!(
                "unknown bootstrap member `{name}`"
            )));
        }
        remove_regular(&child.path(), &name)?;
    }
    std::fs::remove_dir(&entry.path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(&entry.name),
        source,
    })?;
    sync_directory(transactions)
}

fn read_journal(directory: &Path) -> Result<Journal, CodegenError> {
    let bytes = read_regular(&directory.join("journal.json"), "transaction journal")?;
    let journal: Journal = serde_json::from_slice(&bytes)?;
    validate_journal(&journal)?;
    Ok(journal)
}

fn validate_journal(journal: &Journal) -> Result<(), CodegenError> {
    if journal.schema_version != "1.0.0"
        || !valid_transaction_id(&journal.transaction_id)
        || !valid_wire_digest(&journal.plan_digest)
        || journal.operations.is_empty()
        || (matches!(journal.command, ApplyCommand::Init | ApplyCommand::Build)
            != (journal.commit_kind == CommitKind::Lock))
    {
        return Err(CodegenError::Conflict(
            "transaction journal identity is invalid".into(),
        ));
    }
    let mut paths = std::collections::BTreeSet::new();
    for (ordinal, operation) in journal.operations.iter().enumerate() {
        LogicalPath::new(operation.path.clone()).map_err(CodegenError::Core)?;
        if operation.ordinal != ordinal
            || !paths.insert(operation.path.as_str())
            || operation
                .pre_digest
                .as_deref()
                .is_some_and(|digest| !valid_wire_digest(digest))
            || operation
                .expected_digest
                .as_deref()
                .is_some_and(|digest| !valid_wire_digest(digest))
        {
            return Err(CodegenError::Conflict(
                "transaction journal operation is invalid".into(),
            ));
        }
        let expected_stage = (operation.operation != ChangeOperation::Remove)
            .then(|| {
                sibling_relative(
                    &operation.path,
                    &format!(
                        ".leptos-ui-theme-{}-{ordinal:06}.stage",
                        journal.transaction_id
                    ),
                )
            })
            .transpose()?;
        let expected_backup = operation
            .pre_digest
            .as_ref()
            .map(|_| {
                sibling_relative(
                    &operation.path,
                    &format!(
                        ".leptos-ui-theme-{}-{ordinal:06}.backup",
                        journal.transaction_id
                    ),
                )
            })
            .transpose()?;
        let shape_valid = match operation.operation {
            ChangeOperation::Create => {
                operation.pre_digest.is_none()
                    && operation.expected_digest.is_some()
                    && operation.target_mode.is_some() == cfg!(unix)
            }
            ChangeOperation::Replace => {
                operation.pre_digest.is_some() && operation.expected_digest.is_some()
            }
            ChangeOperation::Remove => {
                operation.pre_digest.is_some()
                    && operation.expected_digest.is_none()
                    && operation.target_mode.is_none()
            }
        };
        let ownership_valid = match operation.ownership {
            Ownership::GeneratedLockOwned => operation.scope != ChangeScope::Directory,
            Ownership::SeededAppOwned => {
                operation.scope == ChangeScope::WholeFile
                    && operation.operation == ChangeOperation::Create
            }
            Ownership::UserAuthored => {
                operation.scope == ChangeScope::WholeFile
                    && operation.operation == ChangeOperation::Create
            }
            Ownership::ExternalKitOwned => false,
        };
        if !shape_valid
            || !ownership_valid
            || operation.stage_path != expected_stage
            || operation.backup_path != expected_backup
            || (operation.commit_role == CommitRole::ThemeLock
                && operation.expected_digest.is_none())
        {
            return Err(CodegenError::Conflict(format!(
                "transaction journal operation {} has an invalid shape",
                operation.ordinal
            )));
        }
    }
    let theme_locks = journal
        .operations
        .iter()
        .filter(|operation| operation.commit_role == CommitRole::ThemeLock)
        .count();
    if theme_locks > 1
        || (theme_locks == 1
            && journal
                .operations
                .last()
                .is_none_or(|operation| operation.commit_role != CommitRole::ThemeLock))
        || (journal.commit_kind == CommitKind::Journal && theme_locks != 0)
    {
        return Err(CodegenError::Conflict(
            "transaction commit boundary is invalid".into(),
        ));
    }
    Ok(())
}

fn valid_wire_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn read_states(directory: &Path, journal: &Journal) -> Result<Vec<StateRecord>, CodegenError> {
    let mut states = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|source| CodegenError::Io {
        path: PathBuf::from("transaction state"),
        source,
    })? {
        let entry = entry.map_err(|source| CodegenError::Io {
            path: PathBuf::from("transaction state"),
            source,
        })?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| CodegenError::Conflict("non-UTF-8 transaction state name".into()))?;
        let Some(sequence) = state_sequence(&name) else {
            continue;
        };
        let bytes = read_regular(&entry.path(), &name)?;
        let state: StateRecord = serde_json::from_slice(&bytes)?;
        if state.transaction_id != journal.transaction_id || state.sequence != sequence {
            return Err(CodegenError::Conflict(
                "transaction state identity is inconsistent".into(),
            ));
        }
        states.push(state);
    }
    states.sort_by_key(|state| state.sequence);
    if states
        .iter()
        .enumerate()
        .any(|(sequence, state)| state.sequence != sequence)
    {
        return Err(CodegenError::Conflict(
            "transaction states are not contiguous".into(),
        ));
    }
    Ok(states)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleKind {
    Bootstrap,
    Active,
    Cleanup,
}

struct LifecycleEntry {
    name: String,
    path: PathBuf,
    transaction_id: String,
    kind: LifecycleKind,
}

fn inventory(transactions: &Path) -> Result<Vec<LifecycleEntry>, CodegenError> {
    validate_directory(transactions)?;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(transactions).map_err(|source| CodegenError::Io {
        path: PathBuf::from(TRANSACTIONS_DIRECTORY),
        source,
    })? {
        let entry = entry.map_err(|source| CodegenError::Io {
            path: PathBuf::from(TRANSACTIONS_DIRECTORY),
            source,
        })?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| CodegenError::Conflict("non-UTF-8 lifecycle entry".into()))?;
        let (kind, transaction_id) = if let Some(id) = name.strip_prefix(".bootstrap-") {
            (LifecycleKind::Bootstrap, id)
        } else if let Some(id) = name.strip_prefix(".cleanup-") {
            (LifecycleKind::Cleanup, id)
        } else {
            (LifecycleKind::Active, name.as_str())
        };
        if !valid_transaction_id(transaction_id) {
            return Err(CodegenError::Conflict(format!(
                "unknown transaction lifecycle entry `{name}`"
            )));
        }
        let transaction_id = transaction_id.to_owned();
        validate_directory(&entry.path())?;
        entries.push(LifecycleEntry {
            name,
            path: entry.path(),
            transaction_id,
            kind,
        });
    }
    Ok(entries)
}

fn select_transaction_id(
    root: &Path,
    transactions: &Path,
    plan: &PlanV1,
) -> Result<String, CodegenError> {
    for _ in 0..3 {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|error| {
            CodegenError::Conflict(format!("cannot sample transaction ID: {error}"))
        })?;
        let id = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let lifecycle_absent = [
            transactions.join(&id),
            transactions.join(format!(".bootstrap-{id}")),
            transactions.join(format!(".cleanup-{id}")),
        ]
        .iter()
        .all(|path| !path.exists());
        let sibling_absent = plan.changes.iter().enumerate().all(|(ordinal, change)| {
            let stage = sibling_relative(
                &change.path,
                &format!(".leptos-ui-theme-{id}-{ordinal:06}.stage"),
            );
            let backup = sibling_relative(
                &change.path,
                &format!(".leptos-ui-theme-{id}-{ordinal:06}.backup"),
            );
            stage.is_ok_and(|path| !root.join(path).exists())
                && backup.is_ok_and(|path| !root.join(path).exists())
        });
        if lifecycle_absent && sibling_absent {
            return Ok(id);
        }
    }
    Err(CodegenError::Conflict(
        "transaction ID collision limit exceeded".into(),
    ))
}

fn sibling_relative(path: &str, name: &str) -> Result<String, CodegenError> {
    let parent = Path::new(path).parent().unwrap_or_else(|| Path::new(""));
    let sibling = parent.join(name);
    let sibling = sibling
        .to_str()
        .ok_or_else(|| CodegenError::Core(ThemeError::Security(path.into())))?
        .to_owned();
    LogicalPath::new(sibling.clone()).map_err(CodegenError::Core)?;
    Ok(sibling)
}

fn project_path(root: &Path, relative: &str) -> Result<PathBuf, CodegenError> {
    let logical = LogicalPath::new(relative.to_owned()).map_err(CodegenError::Core)?;
    Ok(root.join(logical.to_path_buf()))
}

fn ensure_state_directory(path: &Path) -> Result<(), CodegenError> {
    ensure_private_directory(path)
}

fn ensure_private_directory(path: &Path) -> Result<(), CodegenError> {
    if !path.exists() {
        create_private_directory(path)?;
        sync_parent(path)?;
    }
    validate_directory(path)
}

fn create_private_directory(path: &Path) -> Result<(), CodegenError> {
    std::fs::create_dir(path).map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |source| CodegenError::Io {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
}

fn validate_directory(path: &Path) -> Result<(), CodegenError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CodegenError::Core(ThemeError::Security(format!(
            "transaction directory is unsafe: {}",
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("?")
        ))));
    }
    Ok(())
}

fn open_lock(path: &Path) -> Result<File, CodegenError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(CodegenError::Core(ThemeError::Security(
                "apply lock path is unsafe".into(),
            )));
        }
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(CodegenError::Io {
                path: PathBuf::from(format!("{STATE_DIRECTORY}/{APPLY_LOCK}")),
                source,
            });
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(format!("{STATE_DIRECTORY}/{APPLY_LOCK}")),
        source,
    })?;
    validate_open_lock_identity(path, &file)?;
    Ok(file)
}

fn open_existing_lock(path: &Path) -> Result<File, CodegenError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(format!("{STATE_DIRECTORY}/{APPLY_LOCK}")),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(CodegenError::Core(ThemeError::Security(
            "apply lock is a symlink".into(),
        )));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| CodegenError::Io {
            path: PathBuf::from(format!("{STATE_DIRECTORY}/{APPLY_LOCK}")),
            source,
        })?;
    validate_open_lock_identity(path, &file)?;
    Ok(file)
}

fn validate_open_lock_identity(path: &Path, file: &File) -> Result<(), CodegenError> {
    let opened = file.metadata().map_err(|source| CodegenError::Io {
        path: PathBuf::from(format!("{STATE_DIRECTORY}/{APPLY_LOCK}")),
        source,
    })?;
    validate_regular_metadata(&opened, APPLY_LOCK)?;
    let linked = std::fs::symlink_metadata(path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(format!("{STATE_DIRECTORY}/{APPLY_LOCK}")),
        source,
    })?;
    if linked.file_type().is_symlink() || !linked.is_file() || !same_file_identity(&opened, &linked)
    {
        return Err(CodegenError::Core(ThemeError::Security(
            "apply lock changed while opening".into(),
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if linked.permissions().mode() & 0o077 != 0 {
            return Err(CodegenError::Core(ThemeError::Security(
                "apply lock permissions are too broad".into(),
            )));
        }
    }
    Ok(())
}

fn same_file_identity(first: &std::fs::Metadata, second: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        first.dev() == second.dev() && first.ino() == second.ino()
    }
    #[cfg(not(unix))]
    {
        first.is_file() && second.is_file() && first.len() == second.len()
    }
}

fn create_private_file(path: &Path) -> Result<File, CodegenError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_canonical_new<T: Serialize>(
    path: &Path,
    value: &T,
    label: &str,
) -> Result<(), CodegenError> {
    let mut bytes = serde_json_canonicalizer::to_vec(value)?;
    bytes.push(b'\n');
    let mut file = create_private_file(path)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|source| CodegenError::Io {
            path: PathBuf::from(label),
            source,
        })
}

fn ensure_parent_chain(root: &Path, target: &Path) -> Result<(), CodegenError> {
    let parent = target
        .parent()
        .ok_or_else(|| CodegenError::Core(ThemeError::Security("target has no parent".into())))?;
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| CodegenError::Core(ThemeError::Security("target escapes root".into())))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(CodegenError::Core(ThemeError::Security(
                    "target parent is unsafe".into(),
                )));
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current).map_err(|source| CodegenError::Io {
                    path: current.clone(),
                    source,
                })?;
                sync_parent(&current)?;
            }
            Err(source) => {
                return Err(CodegenError::Io {
                    path: current.clone(),
                    source,
                });
            }
        }
    }
    Ok(())
}

fn verify_pre_state(
    path: &Path,
    expected: Option<&str>,
    relative: &str,
) -> Result<(), CodegenError> {
    if path_digest(path)?.as_deref() == expected {
        Ok(())
    } else {
        Err(CodegenError::Conflict(relative.into()))
    }
}

fn verify_digest(path: &Path, expected: &str, relative: &str) -> Result<(), CodegenError> {
    if path_digest(path)?.as_deref() == Some(expected) {
        Ok(())
    } else {
        Err(CodegenError::Conflict(relative.into()))
    }
}

fn path_digest(path: &Path) -> Result<Option<String>, CodegenError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CodegenError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CodegenError::Core(ThemeError::Security(
            "transaction target is not a regular file".into(),
        )));
    }
    validate_regular_metadata(&metadata, "transaction target")?;
    let bytes = std::fs::read(path).map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(format!("sha256:{}", sha256(&bytes))))
}

fn require_absent(path: &Path, relative: &str) -> Result<(), CodegenError> {
    match std::fs::symlink_metadata(path) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(CodegenError::Conflict(relative.into())),
        Err(source) => Err(CodegenError::Io {
            path: PathBuf::from(relative),
            source,
        }),
    }
}

fn remove_regular(path: &Path, relative: &str) -> Result<(), CodegenError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(relative),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CodegenError::Core(ThemeError::Security(relative.into())));
    }
    validate_regular_metadata(&metadata, relative)?;
    std::fs::remove_file(path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(relative),
        source,
    })?;
    sync_parent(path)
}

fn read_regular(path: &Path, label: &str) -> Result<Vec<u8>, CodegenError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(label),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CodegenError::Core(ThemeError::Security(label.into())));
    }
    validate_regular_metadata(&metadata, label)?;
    std::fs::read(path).map_err(|source| CodegenError::Io {
        path: PathBuf::from(label),
        source,
    })
}

fn validate_regular_metadata(
    metadata: &std::fs::Metadata,
    label: &str,
) -> Result<(), CodegenError> {
    if !metadata.is_file() {
        return Err(CodegenError::Core(ThemeError::Security(label.into())));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(CodegenError::Core(ThemeError::Security(format!(
                "{label} has multiple hard links"
            ))));
        }
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), CodegenError> {
    let parent = path
        .parent()
        .ok_or_else(|| CodegenError::Core(ThemeError::Security("path has no parent".into())))?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), CodegenError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn state_sequence(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("state-")?.strip_suffix(".json")?;
    (digits.len() == 8).then(|| digits.parse().ok()).flatten()
}

fn state_pending_sequence(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("state-")?.strip_suffix(".json.pending")?;
    (digits.len() == 8).then(|| digits.parse().ok()).flatten()
}

fn valid_transaction_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::{
        APPLY_LOCK, ApplyCommand, STATE_DIRECTORY, TRANSACTIONS_DIRECTORY, build_journal,
        ensure_private_directory, ensure_state_directory, install_operation, open_lock,
        publish_journal, recover,
    };
    use crate::{
        ArtifactManifest, ArtifactManifestEntry, ChangeScope, DesiredArtifactState,
        GeneratedArtifact, Ownership, plan_artifacts, plan_manifest,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn recovery_rolls_back_an_uncommitted_lock_transaction() {
        let root = std::env::temp_dir().join(format!(
            "leptos-ui-theme-recovery-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("generated.txt"), b"before\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join("generated.txt"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let artifacts = vec![
            GeneratedArtifact::generated("generated.txt", b"after\n".to_vec()),
            GeneratedArtifact::generated("theme.lock.json", b"lock\n".to_vec()),
        ];
        let plan = plan_artifacts(&root, &artifacts).unwrap();
        let state = root.join(STATE_DIRECTORY);
        ensure_state_directory(&state).unwrap();
        drop(open_lock(&state.join(APPLY_LOCK)).unwrap());
        let transactions = state.join(TRANSACTIONS_DIRECTORY);
        ensure_private_directory(&transactions).unwrap();
        let artifact_map = artifacts
            .iter()
            .map(|artifact| (artifact.path.as_str(), artifact))
            .collect::<BTreeMap<_, _>>();
        let journal = build_journal(
            "00000000000000000000000000000001",
            &plan,
            ApplyCommand::Build,
            Some("theme.lock.json"),
            &artifact_map,
        )
        .unwrap();
        let active = publish_journal(&transactions, &journal).unwrap();
        let mut sequence = 0;
        let first = &journal.operations[0];
        install_operation(
            &root,
            &active,
            &journal,
            first,
            Some(&artifact_map[first.path.as_str()].bytes),
            &mut sequence,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.join("generated.txt")).unwrap(),
            b"after\n"
        );

        assert!(recover(&root).unwrap());
        assert_eq!(
            std::fs::read(root.join("generated.txt")).unwrap(),
            b"before\n"
        );
        assert!(!root.join("theme.lock.json").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("generated.txt"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert_eq!(std::fs::read_dir(&transactions).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_restores_an_uncommitted_removal() {
        let root = std::env::temp_dir().join(format!(
            "leptos-ui-theme-remove-recovery-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("stale.css"), b"owned\n").unwrap();
        let manifest = ArtifactManifest::new(vec![ArtifactManifestEntry {
            path: "stale.css".into(),
            scope: ChangeScope::WholeFile,
            ownership: Ownership::GeneratedLockOwned,
            state: DesiredArtifactState::Absent,
            digest: None,
        }])
        .unwrap();
        let plan = plan_manifest(&root, &[], &manifest).unwrap();
        let state = root.join(STATE_DIRECTORY);
        ensure_state_directory(&state).unwrap();
        drop(open_lock(&state.join(APPLY_LOCK)).unwrap());
        let transactions = state.join(TRANSACTIONS_DIRECTORY);
        ensure_private_directory(&transactions).unwrap();
        let journal = build_journal(
            "00000000000000000000000000000002",
            &plan,
            ApplyCommand::Add,
            None,
            &BTreeMap::new(),
        )
        .unwrap();
        let active = publish_journal(&transactions, &journal).unwrap();
        let mut sequence = 0;
        install_operation(
            &root,
            &active,
            &journal,
            &journal.operations[0],
            None,
            &mut sequence,
        )
        .unwrap();
        assert!(!root.join("stale.css").exists());
        assert!(recover(&root).unwrap());
        assert_eq!(std::fs::read(root.join("stale.css")).unwrap(), b"owned\n");
        assert_eq!(std::fs::read_dir(&transactions).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_rejects_a_journal_path_outside_the_project() {
        let root = std::env::temp_dir().join(format!(
            "leptos-ui-theme-hostile-journal-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("owned.txt"), b"before\n").unwrap();
        let artifacts = [GeneratedArtifact::generated(
            "owned.txt",
            b"after\n".to_vec(),
        )];
        let plan = plan_artifacts(&root, &artifacts).unwrap();
        let state = root.join(STATE_DIRECTORY);
        ensure_state_directory(&state).unwrap();
        drop(open_lock(&state.join(APPLY_LOCK)).unwrap());
        let transactions = state.join(TRANSACTIONS_DIRECTORY);
        ensure_private_directory(&transactions).unwrap();
        let map = artifacts
            .iter()
            .map(|artifact| (artifact.path.as_str(), artifact))
            .collect();
        let journal = build_journal(
            "00000000000000000000000000000003",
            &plan,
            ApplyCommand::Add,
            None,
            &map,
        )
        .unwrap();
        let active = publish_journal(&transactions, &journal).unwrap();
        let journal_path = active.join("journal.json");
        let mut hostile: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        hostile["operations"][0]["path"] = "../outside.txt".into();
        std::fs::write(&journal_path, serde_json::to_vec(&hostile).unwrap()).unwrap();
        assert!(recover(&root).is_err());
        assert_eq!(std::fs::read(root.join("owned.txt")).unwrap(), b"before\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn apply_lock_never_follows_a_symlink() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "leptos-ui-theme-lock-link-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let state = root.join(STATE_DIRECTORY);
        ensure_state_directory(&state).unwrap();
        std::fs::write(root.join("outside.lock"), b"do not lock").unwrap();
        symlink(root.join("outside.lock"), state.join(APPLY_LOCK)).unwrap();
        assert!(open_lock(&state.join(APPLY_LOCK)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

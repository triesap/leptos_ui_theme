use crate::{
    COMPILED_LIMITS, FileIdentity, IdentityPlatform, LimitKind, Limits, LogicalPath,
    ResourceBudget, ThemeError, parse_json_strict, sha256,
};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, Metadata, OpenOptions};
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum SourceRole {
    General,
    TokenResolver,
    Journal,
    EvidenceManifest,
    RetainedBackup,
    ExistingGenerated,
}

impl SourceRole {
    fn limit_kind(self) -> Option<LimitKind> {
        match self {
            Self::General => None,
            Self::TokenResolver => Some(LimitKind::SourceFiles),
            Self::Journal => Some(LimitKind::JournalEntries),
            Self::EvidenceManifest => Some(LimitKind::EvidenceManifests),
            Self::RetainedBackup => Some(LimitKind::RetainedBackups),
            Self::ExistingGenerated => Some(LimitKind::GeneratedBytes),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpenedSource {
    pub logical_path: LogicalPath,
    pub identity: FileIdentity,
    pub bytes: Arc<[u8]>,
    pub bytes_digest: String,
}

#[derive(Clone, Debug)]
struct CachedSource {
    bytes: Arc<[u8]>,
    bytes_digest: String,
}

#[derive(Debug)]
struct SourceState {
    budget: ResourceBudget,
    addresses: BTreeMap<LogicalPath, FileIdentity>,
    cache: BTreeMap<FileIdentity, CachedSource>,
    roles: BTreeMap<LimitKind, BTreeSet<FileIdentity>>,
}

pub struct SourceLoader {
    root_path: PathBuf,
    root: Dir,
    root_identity: FileIdentity,
    limits: Limits,
    state: Mutex<SourceState>,
}

impl std::fmt::Debug for SourceLoader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceLoader")
            .field("root_path", &self.root_path)
            .field("root_identity", &self.root_identity)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl SourceLoader {
    pub fn new(root: &Path, limits: Limits) -> Result<Self, ThemeError> {
        limits.validate()?;
        let root_path = absolute_path(root)?;
        let root = open_root_directory(&root_path)?;
        let metadata = root.dir_metadata().map_err(|source| ThemeError::Io {
            path: root_path.clone(),
            source,
        })?;
        if !metadata.is_dir() || metadata.is_symlink() {
            return Err(ThemeError::Security(format!(
                "source root is not a secure opened directory: {}",
                root_path.display()
            )));
        }
        let root_identity = identity_from_metadata(&metadata)?;
        Ok(Self {
            root_path,
            root,
            root_identity,
            limits: limits.clone(),
            state: Mutex::new(SourceState {
                budget: ResourceBudget::new(limits)?,
                addresses: BTreeMap::new(),
                cache: BTreeMap::new(),
                roles: BTreeMap::new(),
            }),
        })
    }

    pub fn relimit(self, limits: Limits) -> Result<Self, ThemeError> {
        limits.validate()?;
        Ok(Self {
            root_path: self.root_path,
            root: self.root,
            root_identity: self.root_identity,
            limits: limits.clone(),
            state: Mutex::new(SourceState {
                budget: ResourceBudget::new(limits)?,
                addresses: BTreeMap::new(),
                cache: BTreeMap::new(),
                roles: BTreeMap::new(),
            }),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root_path
    }

    #[must_use]
    pub fn root_identity(&self) -> &FileIdentity {
        &self.root_identity
    }

    pub fn read_source(
        &self,
        logical: &LogicalPath,
        role: SourceRole,
    ) -> Result<OpenedSource, ThemeError> {
        let (mut file, metadata, identity) = self.open_regular(logical)?;
        reject_hard_link(&metadata, logical)?;

        let mut state = self
            .state
            .lock()
            .map_err(|_| ThemeError::Security("source-loader state lock was poisoned".into()))?;
        if let Some(previous) = state.addresses.get(logical)
            && previous != &identity
        {
            return Err(ThemeError::Security(format!(
                "source identity changed during this invocation: `{logical}`"
            )));
        }
        if let Some(cached) = state.cache.get(&identity).cloned() {
            account_role(&mut state, role, &identity, cached.bytes.len() as u64)?;
            state.addresses.insert(logical.clone(), identity.clone());
            return Ok(OpenedSource {
                logical_path: logical.clone(),
                identity,
                bytes: cached.bytes,
                bytes_digest: cached.bytes_digest,
            });
        }

        let consumed = state.budget.consumed(LimitKind::AggregateInputBytes);
        let remaining = state
            .budget
            .limit(LimitKind::AggregateInputBytes)
            .saturating_sub(consumed);
        state.budget.ensure(LimitKind::FileBytes, metadata.len())?;
        state.budget.ensure(
            LimitKind::Files,
            state.budget.consumed(LimitKind::Files).saturating_add(1),
        )?;
        state.budget.ensure(
            LimitKind::AggregateInputBytes,
            consumed.saturating_add(metadata.len()),
        )?;
        ensure_role_capacity(&state, role, &identity, metadata.len())?;
        let maximum = self.limits.file_bytes.min(remaining);
        drop(state);

        let bytes = read_bounded(&mut file, maximum, logical)?;
        let after = file.metadata().map_err(|source| ThemeError::Io {
            path: self.root_path.join(logical.to_path_buf()),
            source,
        })?;
        if identity_from_metadata(&after)? != identity
            || after.len() != metadata.len()
            || after.len() != bytes.len() as u64
            || after.modified().ok() != metadata.modified().ok()
        {
            return Err(ThemeError::Security(format!(
                "source changed while it was being read: `{logical}`"
            )));
        }

        let bytes: Arc<[u8]> = bytes.into();
        let bytes_digest = format!("sha256:{}", sha256(&bytes));
        let mut state = self
            .state
            .lock()
            .map_err(|_| ThemeError::Security("source-loader state lock was poisoned".into()))?;
        if let Some(cached) = state.cache.get(&identity).cloned() {
            if cached.bytes.as_ref() != bytes.as_ref() {
                return Err(ThemeError::Security(format!(
                    "one opened source identity produced different bytes: `{logical}`"
                )));
            }
            account_role(&mut state, role, &identity, cached.bytes.len() as u64)?;
            state.addresses.insert(logical.clone(), identity.clone());
            return Ok(OpenedSource {
                logical_path: logical.clone(),
                identity,
                bytes: cached.bytes,
                bytes_digest: cached.bytes_digest,
            });
        }
        state.budget.consume(LimitKind::Files, 1)?;
        state
            .budget
            .consume(LimitKind::AggregateInputBytes, bytes.len() as u64)?;
        account_role(&mut state, role, &identity, bytes.len() as u64)?;
        state.addresses.insert(logical.clone(), identity.clone());
        state.cache.insert(
            identity.clone(),
            CachedSource {
                bytes: Arc::clone(&bytes),
                bytes_digest: bytes_digest.clone(),
            },
        );
        Ok(OpenedSource {
            logical_path: logical.clone(),
            identity,
            bytes,
            bytes_digest,
        })
    }

    pub fn read_bytes(&self, logical: &LogicalPath) -> Result<Vec<u8>, ThemeError> {
        self.read_source(logical, SourceRole::General)
            .map(|source| source.bytes.to_vec())
    }

    pub fn read_bytes_for(
        &self,
        logical: &LogicalPath,
        role: SourceRole,
    ) -> Result<Vec<u8>, ThemeError> {
        self.read_source(logical, role)
            .map(|source| source.bytes.to_vec())
    }

    pub fn read_json<T: DeserializeOwned>(&self, logical: &LogicalPath) -> Result<T, ThemeError> {
        self.read_json_for(logical, SourceRole::General)
    }

    pub fn read_json_for<T: DeserializeOwned>(
        &self,
        logical: &LogicalPath,
        role: SourceRole,
    ) -> Result<T, ThemeError> {
        let source = self.read_source(logical, role)?;
        parse_json(logical, &source.bytes, self.limits.json_depth)
    }

    pub fn read_tree(
        &self,
        directory: &LogicalPath,
        role: SourceRole,
    ) -> Result<Vec<OpenedSource>, ThemeError> {
        let root = self.open_directory(directory)?;
        let mut stack = vec![(directory.clone(), root)];
        let mut sources = Vec::new();
        let mut entries_seen = 0_u64;
        while let Some((logical_directory, opened_directory)) = stack.pop() {
            let mut entries = opened_directory
                .entries()
                .map_err(|source| ThemeError::Io {
                    path: self.root_path.join(logical_directory.to_path_buf()),
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| ThemeError::Io {
                    path: self.root_path.join(logical_directory.to_path_buf()),
                    source,
                })?;
            entries.sort_by_key(|entry| entry.file_name());
            let mut spellings = BTreeSet::new();
            for entry in entries {
                entries_seen = entries_seen.saturating_add(1);
                if entries_seen > u64::from(self.limits.files) {
                    return Err(ThemeError::Limit {
                        resource: "directoryEntries",
                        limit: u64::from(self.limits.files),
                        observed: entries_seen,
                    });
                }
                let name = entry.file_name().into_string().map_err(|_| {
                    ThemeError::Security(format!(
                        "directory beneath `{logical_directory}` contains a non-UTF-8 name"
                    ))
                })?;
                let folded: String = name.chars().flat_map(char::to_lowercase).collect();
                if !spellings.insert(folded) {
                    return Err(ThemeError::Security(format!(
                        "directory beneath `{logical_directory}` contains colliding names"
                    )));
                }
                let child = logical_directory.join(&LogicalPath::new(name.clone())?)?;
                let file_type = entry.file_type().map_err(|source| ThemeError::Io {
                    path: self.root_path.join(child.to_path_buf()),
                    source,
                })?;
                if file_type.is_symlink() {
                    return Err(ThemeError::Security(format!(
                        "source tree contains a link: `{child}`"
                    )));
                }
                if file_type.is_dir() {
                    let directory =
                        opened_directory
                            .open_dir_nofollow(&name)
                            .map_err(|source| {
                                ThemeError::Security(format!(
                                    "cannot open source directory `{child}`: {source}"
                                ))
                            })?;
                    stack.push((child, directory));
                } else if file_type.is_file() {
                    sources.push(self.read_source(&child, role)?);
                } else {
                    return Err(ThemeError::Security(format!(
                        "source tree contains a nonregular object: `{child}`"
                    )));
                }
            }
        }
        sources.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
        Ok(sources)
    }

    fn open_regular(
        &self,
        logical: &LogicalPath,
    ) -> Result<(File, Metadata, FileIdentity), ThemeError> {
        let mut current = self.root.try_clone().map_err(|source| ThemeError::Io {
            path: self.root_path.clone(),
            source,
        })?;
        let mut components = logical.as_str().split('/').peekable();
        while let Some(component) = components.next() {
            if components.peek().is_some() {
                current = current.open_dir_nofollow(component).map_err(|source| {
                    ThemeError::Security(format!(
                        "cannot open source directory `{component}` beneath `{logical}`: {source}"
                    ))
                })?;
                continue;
            }
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let file = current.open_with(component, &options).map_err(|source| {
                ThemeError::Security(format!(
                    "cannot open source `{logical}` without following links: {source}"
                ))
            })?;
            let metadata = file.metadata().map_err(|source| ThemeError::Io {
                path: self.root_path.join(logical.to_path_buf()),
                source,
            })?;
            if !metadata.is_file() || metadata.is_symlink() {
                return Err(ThemeError::Security(format!(
                    "source is not a regular non-link file: `{logical}`"
                )));
            }
            let identity = identity_from_metadata(&metadata)?;
            return Ok((file, metadata, identity));
        }
        Err(ThemeError::Security("logical source path is empty".into()))
    }

    fn open_directory(&self, logical: &LogicalPath) -> Result<Dir, ThemeError> {
        let mut current = self.root.try_clone().map_err(|source| ThemeError::Io {
            path: self.root_path.clone(),
            source,
        })?;
        for component in logical.as_str().split('/') {
            current = current.open_dir_nofollow(component).map_err(|source| {
                ThemeError::Security(format!(
                    "cannot open directory `{logical}` without following links: {source}"
                ))
            })?;
        }
        Ok(current)
    }
}

fn account_role(
    state: &mut SourceState,
    role: SourceRole,
    identity: &FileIdentity,
    bytes: u64,
) -> Result<(), ThemeError> {
    let Some(kind) = role.limit_kind() else {
        return Ok(());
    };
    let identities = state.roles.entry(kind).or_default();
    if identities.contains(identity) {
        return Ok(());
    }
    let amount = if kind == LimitKind::GeneratedBytes {
        bytes
    } else {
        1
    };
    state.budget.consume(kind, amount)?;
    identities.insert(identity.clone());
    Ok(())
}

fn ensure_role_capacity(
    state: &SourceState,
    role: SourceRole,
    identity: &FileIdentity,
    bytes: u64,
) -> Result<(), ThemeError> {
    let Some(kind) = role.limit_kind() else {
        return Ok(());
    };
    if state
        .roles
        .get(&kind)
        .is_some_and(|identities| identities.contains(identity))
    {
        return Ok(());
    }
    let amount = if kind == LimitKind::GeneratedBytes {
        bytes
    } else {
        1
    };
    state
        .budget
        .ensure(kind, state.budget.consumed(kind).saturating_add(amount))
}

fn read_bounded(
    file: &mut File,
    maximum: u64,
    logical: &LogicalPath,
) -> Result<Vec<u8>, ThemeError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| ThemeError::Io {
            path: logical.to_path_buf(),
            source,
        })?;
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| ThemeError::Io {
            path: logical.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > maximum {
        return Err(ThemeError::Limit {
            resource: "inputBytes",
            limit: maximum,
            observed: bytes.len() as u64,
        });
    }
    Ok(bytes)
}

fn absolute_path(path: &Path) -> Result<PathBuf, ThemeError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|source| ThemeError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
}

fn open_root_directory(path: &Path) -> Result<Dir, ThemeError> {
    let mut anchor = PathBuf::new();
    let mut relative = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => anchor.push(prefix.as_os_str()),
            Component::RootDir => anchor.push(component.as_os_str()),
            Component::Normal(name) => relative.push(name.to_owned()),
            Component::CurDir | Component::ParentDir => {
                return Err(ThemeError::Security(format!(
                    "source root is not lexically normalized: {}",
                    path.display()
                )));
            }
        }
    }
    if anchor.as_os_str().is_empty() {
        return Err(ThemeError::Security(format!(
            "source root has no filesystem anchor: {}",
            path.display()
        )));
    }
    let mut directory =
        Dir::open_ambient_dir(&anchor, ambient_authority()).map_err(|source| ThemeError::Io {
            path: anchor,
            source,
        })?;
    for component in relative {
        directory = directory.open_dir_nofollow(&component).map_err(|source| {
            ThemeError::Security(format!(
                "source root contains a link or non-directory component `{}`: {source}",
                component.to_string_lossy()
            ))
        })?;
    }
    Ok(directory)
}

#[cfg(unix)]
fn identity_from_metadata(metadata: &Metadata) -> Result<FileIdentity, ThemeError> {
    use cap_std::fs::MetadataExt as _;
    FileIdentity::new(
        IdentityPlatform::Unix,
        format!("{:x}", metadata.dev()),
        format!("{:x}", metadata.ino()),
    )
}

#[cfg(windows)]
fn identity_from_metadata(metadata: &Metadata) -> Result<FileIdentity, ThemeError> {
    use cap_std::fs::MetadataExt as _;
    let volume = metadata
        .volume_serial_number()
        .ok_or_else(|| ThemeError::Security("opened file has no Windows volume identity".into()))?;
    let file = metadata
        .file_index()
        .ok_or_else(|| ThemeError::Security("opened file has no Windows file identity".into()))?;
    FileIdentity::new(
        IdentityPlatform::Windows,
        format!("{volume:x}"),
        format!("{file:x}"),
    )
}

#[cfg(unix)]
fn reject_hard_link(metadata: &Metadata, logical: &LogicalPath) -> Result<(), ThemeError> {
    use cap_std::fs::MetadataExt as _;
    if metadata.nlink() == 1 {
        Ok(())
    } else {
        Err(ThemeError::Security(format!(
            "source has multiple hard links: `{logical}`"
        )))
    }
}

#[cfg(windows)]
fn reject_hard_link(metadata: &Metadata, logical: &LogicalPath) -> Result<(), ThemeError> {
    use cap_std::fs::MetadataExt as _;
    if metadata.number_of_links() == Some(1) {
        Ok(())
    } else {
        Err(ThemeError::Security(format!(
            "source has multiple hard links: `{logical}`"
        )))
    }
}

pub(crate) fn read_json_file<T: DeserializeOwned>(path: &Path) -> Result<T, ThemeError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ThemeError::Security("JSON input path is not UTF-8".into()))?;
    let loader = SourceLoader::new(parent, COMPILED_LIMITS)?;
    loader.read_json(&LogicalPath::new(name)?)
}

fn parse_json<T: DeserializeOwned>(
    path: &LogicalPath,
    bytes: &[u8],
    max_depth: u32,
) -> Result<T, ThemeError> {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(ThemeError::Config(format!(
            "JSON input contains a forbidden BOM: `{path}`"
        )));
    }
    let value = parse_json_strict(bytes, max_depth)?;
    serde_json::from_value(value).map_err(|source| ThemeError::Json {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "leptos-ui-theme-source-{}-{label}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn capability_reads_are_cached_and_bounded() {
        let root = temporary_root("bounded");
        fs::create_dir_all(root.join("tokens")).unwrap();
        fs::write(root.join("tokens/a.json"), br#"{"a":1}"#).unwrap();
        let limits = Limits {
            file_bytes: 7,
            aggregate_input_bytes: 7,
            ..Limits::default()
        };
        let loader = SourceLoader::new(&root, limits).unwrap();
        let path = LogicalPath::new("tokens/a.json").unwrap();
        let first = loader
            .read_source(&path, SourceRole::TokenResolver)
            .unwrap();
        let second = loader
            .read_source(&path, SourceRole::TokenResolver)
            .unwrap();
        assert_eq!(first.identity, second.identity);
        assert!(Arc::ptr_eq(&first.bytes, &second.bytes));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacement_after_first_read_fails_closed() {
        let root = temporary_root("replacement");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.json"), b"1").unwrap();
        let loader = SourceLoader::new(&root, Limits::default()).unwrap();
        let path = LogicalPath::new("a.json").unwrap();
        loader.read_bytes(&path).unwrap();
        fs::remove_file(root.join("a.json")).unwrap();
        fs::write(root.join("a.json"), b"2").unwrap();
        assert!(loader.read_bytes(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_hard_links_fail_closed() {
        use std::os::unix::fs::symlink;

        let root = temporary_root("links");
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real/a.json"), b"1").unwrap();
        symlink(root.join("real"), root.join("linked")).unwrap();
        let loader = SourceLoader::new(&root, Limits::default()).unwrap();
        assert!(
            loader
                .read_bytes(&LogicalPath::new("linked/a.json").unwrap())
                .is_err()
        );
        fs::hard_link(root.join("real/a.json"), root.join("hard.json")).unwrap();
        assert!(
            loader
                .read_bytes(&LogicalPath::new("hard.json").unwrap())
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }
}

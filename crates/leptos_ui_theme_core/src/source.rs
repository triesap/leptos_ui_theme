use crate::{COMPILED_LIMITS, Limits, LogicalPath, ThemeError};
use serde::de::{DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct SourceLoader {
    root: PathBuf,
    limits: Limits,
}

impl SourceLoader {
    pub fn new(root: &Path, limits: Limits) -> Result<Self, ThemeError> {
        limits.validate()?;
        let metadata = std::fs::symlink_metadata(root).map_err(|source| ThemeError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ThemeError::Security(format!(
                "project root is not a regular directory: {}",
                root.display()
            )));
        }
        let root = std::fs::canonicalize(root).map_err(|source| ThemeError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        Ok(Self { root, limits })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve_file(&self, logical: &LogicalPath) -> Result<PathBuf, ThemeError> {
        let mut current = self.root.clone();
        for component in logical.to_path_buf().components() {
            current.push(component.as_os_str());
            let metadata =
                std::fs::symlink_metadata(&current).map_err(|source| ThemeError::Io {
                    path: current.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Err(ThemeError::Security(format!(
                    "source path contains a symlink: {logical}"
                )));
            }
        }
        let canonical = std::fs::canonicalize(&current).map_err(|source| ThemeError::Io {
            path: current.clone(),
            source,
        })?;
        if !canonical.starts_with(&self.root) || !canonical.is_file() {
            return Err(ThemeError::Security(format!(
                "source is outside the project or is not a regular file: {logical}"
            )));
        }
        Ok(canonical)
    }

    pub fn read_bytes(&self, logical: &LogicalPath) -> Result<Vec<u8>, ThemeError> {
        let path = self.resolve_file(logical)?;
        let metadata = std::fs::metadata(&path).map_err(|source| ThemeError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.len() > self.limits.file_bytes {
            return Err(ThemeError::Config(format!(
                "source `{logical}` exceeds limits.maxFileBytes"
            )));
        }
        std::fs::read(&path).map_err(|source| ThemeError::Io { path, source })
    }

    pub fn read_json<T: DeserializeOwned>(&self, logical: &LogicalPath) -> Result<T, ThemeError> {
        let path = self.resolve_file(logical)?;
        let bytes = self.read_bytes(logical)?;
        parse_json(&path, &bytes, self.limits.json_depth)
    }
}

pub(crate) fn read_json_file<T: DeserializeOwned>(path: &Path) -> Result<T, ThemeError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| ThemeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > COMPILED_LIMITS.file_bytes
    {
        return Err(ThemeError::Security(format!(
            "JSON input is not a bounded regular file: {}",
            path.display()
        )));
    }
    let bytes = std::fs::read(path).map_err(|source| ThemeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_json(path, &bytes, COMPILED_LIMITS.json_depth)
}

fn parse_json<T: DeserializeOwned>(
    path: &Path,
    bytes: &[u8],
    max_depth: u32,
) -> Result<T, ThemeError> {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) || bytes.contains(&0) {
        return Err(ThemeError::Config(format!(
            "JSON input contains a forbidden byte: {}",
            path.display()
        )));
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let unique =
        UniqueValue::deserialize(&mut deserializer).map_err(|source| ThemeError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    deserializer.end().map_err(|source| ThemeError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    if json_depth(&unique.0, 1) > max_depth {
        return Err(ThemeError::Config(format!(
            "JSON input exceeds limits.maxJsonDepth: {}",
            path.display()
        )));
    }
    serde_json::from_value(unique.0).map_err(|source| ThemeError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn json_depth(value: &Value, depth: u32) -> u32 {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|value| json_depth(value, depth.saturating_add(1)))
            .max()
            .unwrap_or(depth),
        Value::Object(values) => values
            .values()
            .map(|value| json_depth(value, depth.saturating_add(1)))
            .max()
            .unwrap_or(depth),
        _ => depth,
    }
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueValueVisitor)
    }
}

struct UniqueValueVisitor;

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(UniqueValue)
            .ok_or_else(|| E::custom("JSON number is not finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        UniqueValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate object key `{key}`"
                )));
            }
            let value = object.next_value::<UniqueValue>()?;
            values.insert(key, value.0);
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}

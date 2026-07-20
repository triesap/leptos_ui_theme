use crate::ThemeError;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ThemeId(String);

impl ThemeId {
    pub fn new(value: impl Into<String>) -> Result<Self, ThemeError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= 63
            && bytes[0].is_ascii_lowercase()
            && bytes[bytes.len() - 1].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            && !value.contains("--");
        if valid && value != "system" {
            Ok(Self(value))
        } else {
            Err(ThemeError::Config(format!("invalid theme ID `{value}`")))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ThemeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ThemeId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ThemeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TokenPath(String);

impl TokenPath {
    pub fn new(value: impl Into<String>) -> Result<Self, ThemeError> {
        let value = value.into();
        if valid_token_path(&value) {
            Ok(Self(value))
        } else {
            Err(ThemeError::Config(format!(
                "invalid semantic token path `{value}`"
            )))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TokenPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TokenPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

fn valid_token_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment.len() <= 63
                && !segment.starts_with('$')
                && !segment.contains(['{', '}', '/', '\\'])
        })
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ContractId(String);

impl ContractId {
    pub fn new(value: impl Into<String>) -> Result<Self, ThemeError> {
        let value = value.into();
        if ThemeId::new(value.clone()).is_ok() {
            Ok(Self(value))
        } else {
            Err(ThemeError::Contract(format!(
                "invalid contract ID `{value}`"
            )))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ContractId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AbiVersion(u32);

impl AbiVersion {
    pub fn new(value: u32) -> Result<Self, ThemeError> {
        if value == 0 {
            Err(ThemeError::Contract("ABI version must be positive".into()))
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for AbiVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u32::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ContractRevision(u32);

impl ContractRevision {
    pub fn new(value: u32) -> Result<Self, ThemeError> {
        if value == 0 {
            Err(ThemeError::Contract(
                "contract revision must be positive".into(),
            ))
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ContractRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u32::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn new(value: impl Into<String>) -> Result<Self, ThemeError> {
        let value = value.into();
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(ThemeError::Config(
                "SHA-256 digest must use the sha256: prefix".into(),
            ));
        };
        if hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value))
        } else {
            Err(ThemeError::Config("invalid SHA-256 digest".into()))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IdentityPlatform {
    Unix,
    Windows,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FileIdentity {
    platform: IdentityPlatform,
    volume_id: String,
    file_id: String,
}

impl FileIdentity {
    pub fn new(
        platform: IdentityPlatform,
        volume_id: impl Into<String>,
        file_id: impl Into<String>,
    ) -> Result<Self, ThemeError> {
        let volume_id = volume_id.into();
        let file_id = file_id.into();
        if !valid_identity_hex(&volume_id) || !valid_identity_hex(&file_id) {
            return Err(ThemeError::Security("invalid opened-file identity".into()));
        }
        Ok(Self {
            platform,
            volume_id,
            file_id,
        })
    }

    #[must_use]
    pub fn platform(&self) -> IdentityPlatform {
        self.platform
    }

    #[must_use]
    pub fn volume_id(&self) -> &str {
        &self.volume_id
    }

    #[must_use]
    pub fn file_id(&self) -> &str {
        &self.file_id
    }
}

impl Serialize for FileIdentity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("FileIdentity", 3)?;
        state.serialize_field("platform", &self.platform)?;
        state.serialize_field("volumeId", &self.volume_id)?;
        state.serialize_field("fileId", &self.file_id)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for FileIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct Wire {
            platform: IdentityPlatform,
            volume_id: String,
            file_id: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.platform, wire.volume_id, wire.file_id).map_err(serde::de::Error::custom)
    }
}

fn valid_identity_hex(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && (value == "0" || !value.starts_with('0'))
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_identities_reject_zero_during_deserialization() {
        assert!(serde_json::from_str::<AbiVersion>("0").is_err());
        assert!(serde_json::from_str::<ContractRevision>("0").is_err());
    }

    #[test]
    fn file_identity_uses_the_closed_wire_shape() {
        let identity = FileIdentity::new(IdentityPlatform::Unix, "1", "abcdef").unwrap();
        assert_eq!(
            serde_json::to_string(&identity).unwrap(),
            r#"{"platform":"unix","volumeId":"1","fileId":"abcdef"}"#
        );
        assert!(
            serde_json::from_str::<FileIdentity>(
                r#"{"platform":"unix","volumeId":"01","fileId":"2"}"#
            )
            .is_err()
        );
    }
}

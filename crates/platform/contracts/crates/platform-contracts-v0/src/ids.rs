use serde::{Deserialize, Serialize};
use std::fmt;

const MAX_IDENTIFIER_BYTES: usize = 256;

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum IdentifierError {
    #[error("identifier must not be empty")]
    Empty,
    #[error("identifier exceeds {MAX_IDENTIFIER_BYTES} bytes")]
    TooLong,
    #[error("identifier must contain only safe printable ASCII")]
    InvalidCharacter,
}

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                validate_identifier(&value)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

identifier!(OperationId);
identifier!(AttemptId);
identifier!(ServiceId);
identifier!(ProviderId);

fn validate_identifier(value: &str) -> Result<(), IdentifierError> {
    if value.is_empty() {
        return Err(IdentifierError::Empty);
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(IdentifierError::TooLong);
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'))
    {
        return Err(IdentifierError::InvalidCharacter);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestAlgorithm {
    Sha256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContentDigest {
    pub algorithm: DigestAlgorithm,
    pub hex: String,
}

impl ContentDigest {
    pub fn sha256(hex: impl Into<String>) -> Result<Self, IdentifierError> {
        let hex = hex.into();
        validate_sha256(&hex)?;
        Ok(Self {
            algorithm: DigestAlgorithm::Sha256,
            hex,
        })
    }

    pub fn validate(&self) -> Result<(), IdentifierError> {
        match self.algorithm {
            DigestAlgorithm::Sha256 => validate_sha256(&self.hex),
        }
    }
}

impl<'de> Deserialize<'de> for ContentDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            algorithm: DigestAlgorithm,
            hex: String,
        }

        let raw = Raw::deserialize(deserializer)?;
        let value = Self {
            algorithm: raw.algorithm,
            hex: raw.hex,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

fn validate_sha256(value: &str) -> Result<(), IdentifierError> {
    if value.len() != 64 {
        return Err(IdentifierError::TooLong);
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(IdentifierError::InvalidCharacter);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_reject_unsafe_or_ambiguous_values() {
        assert_eq!(OperationId::new(""), Err(IdentifierError::Empty));
        assert_eq!(
            OperationId::new("has space"),
            Err(IdentifierError::InvalidCharacter)
        );
        assert_eq!(
            OperationId::new("has\\slash"),
            Err(IdentifierError::InvalidCharacter)
        );
        assert!(OperationId::new("operation-01").is_ok());
    }

    #[test]
    fn identifier_deserialization_runs_validation() {
        assert!(serde_json::from_str::<ServiceId>("\"gateway\"").is_ok());
        assert!(serde_json::from_str::<ServiceId>("\"bad service\"").is_err());
    }

    #[test]
    fn digest_requires_lowercase_sha256() {
        assert!(ContentDigest::sha256("a".repeat(64)).is_ok());
        assert!(ContentDigest::sha256("A".repeat(64)).is_err());
        assert!(ContentDigest::sha256("a".repeat(63)).is_err());
    }

    #[test]
    fn digest_rejects_unknown_fields() {
        let json = format!(
            r#"{{"algorithm":"sha256","hex":"{}","extra":true}}"#,
            "a".repeat(64)
        );
        assert!(serde_json::from_str::<ContentDigest>(&json).is_err());
    }
}

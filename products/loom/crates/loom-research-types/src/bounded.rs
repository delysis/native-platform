use std::{fmt, marker::PhantomData, ops::Deref, str::FromStr};

use loom_types::{BlobId, CommandId, ProjectId, RevisionId};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, SeqAccess, Visitor},
};
use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BoundError {
    #[error("collection is empty")]
    Empty,
    #[error("collection has {actual} entries; maximum is {maximum}")]
    TooMany { actual: usize, maximum: usize },
    #[error("text has {actual} bytes; maximum is {maximum}")]
    TextTooLong { actual: usize, maximum: usize },
    #[error("text is empty")]
    EmptyText,
    #[error("text contains a prohibited control character")]
    ControlCharacter,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct BoundedVec<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> BoundedVec<T, MAX> {
    pub fn new(values: Vec<T>) -> Result<Self, BoundError> {
        if values.len() > MAX {
            return Err(BoundError::TooMany {
                actual: values.len(),
                maximum: MAX,
            });
        }
        Ok(Self(values))
    }

    pub fn into_inner(self) -> Vec<T> {
        self.0
    }
}

impl<T: fmt::Debug, const MAX: usize> fmt::Debug for BoundedVec<T, MAX> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<T, const MAX: usize> Deref for BoundedVec<T, MAX> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de, T, const MAX: usize> Deserialize<'de> for BoundedVec<T, MAX>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = deserializer.deserialize_seq(BoundedVecVisitor::<T, MAX>(PhantomData))?;
        Self::new(values).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NonEmptyBoundedVec<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> NonEmptyBoundedVec<T, MAX> {
    pub fn new(values: Vec<T>) -> Result<Self, BoundError> {
        if values.is_empty() {
            return Err(BoundError::Empty);
        }
        if values.len() > MAX {
            return Err(BoundError::TooMany {
                actual: values.len(),
                maximum: MAX,
            });
        }
        Ok(Self(values))
    }

    pub fn into_inner(self) -> Vec<T> {
        self.0
    }
}

impl<T: fmt::Debug, const MAX: usize> fmt::Debug for NonEmptyBoundedVec<T, MAX> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<T, const MAX: usize> Deref for NonEmptyBoundedVec<T, MAX> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de, T, const MAX: usize> Deserialize<'de> for NonEmptyBoundedVec<T, MAX>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = deserializer.deserialize_seq(BoundedVecVisitor::<T, MAX>(PhantomData))?;
        Self::new(values).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct BoundedText<const MAX: usize>(String);

impl<const MAX: usize> BoundedText<MAX> {
    pub fn new(value: impl Into<String>) -> Result<Self, BoundError> {
        let value = value.into();
        if value.is_empty() {
            return Err(BoundError::EmptyText);
        }
        if value.len() > MAX {
            return Err(BoundError::TextTooLong {
                actual: value.len(),
                maximum: MAX,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(BoundError::ControlCharacter);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<const MAX: usize> fmt::Display for BoundedText<MAX> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for BoundedText<MAX> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(BoundedTextVisitor::<MAX>)
    }
}

struct BoundedVecVisitor<T, const MAX: usize>(PhantomData<T>);

impl<'de, T, const MAX: usize> Visitor<'de> for BoundedVecVisitor<T, MAX>
where
    T: Deserialize<'de>,
{
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a sequence containing at most {MAX} entries")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        if let Some(size_hint) = sequence.size_hint()
            && size_hint > MAX
        {
            return Err(de::Error::invalid_length(size_hint, &self));
        }
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX));
        while values.len() < MAX {
            let Some(value) = sequence.next_element()? else {
                return Ok(values);
            };
            values.push(value);
        }
        if sequence.next_element::<de::IgnoredAny>()?.is_some() {
            return Err(de::Error::invalid_length(MAX.saturating_add(1), &self));
        }
        Ok(values)
    }
}

struct BoundedTextVisitor<const MAX: usize>;

impl<const MAX: usize> BoundedTextVisitor<MAX> {
    fn validate<E>(value: &str) -> Result<(), E>
    where
        E: de::Error,
    {
        if value.is_empty() {
            return Err(E::custom(BoundError::EmptyText));
        }
        if value.len() > MAX {
            return Err(E::custom(BoundError::TextTooLong {
                actual: value.len(),
                maximum: MAX,
            }));
        }
        if value.chars().any(char::is_control) {
            return Err(E::custom(BoundError::ControlCharacter));
        }
        Ok(())
    }
}

impl<'de, const MAX: usize> Visitor<'de> for BoundedTextVisitor<MAX> {
    type Value = BoundedText<MAX>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "non-empty control-free UTF-8 text of at most {MAX} bytes"
        )
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Self::validate(value)?;
        Ok(BoundedText(value.to_owned()))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Self::validate(value)?;
        Ok(BoundedText(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Self::validate(&value)?;
        Ok(BoundedText(value))
    }
}

pub(crate) fn deserialize_blob_id<'de, D>(deserializer: D) -> Result<BlobId, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(FixedParseVisitor::<BlobId>::new("SHA-256 digest", 64))
}

pub(crate) fn deserialize_optional_blob_id<'de, D>(
    deserializer: D,
) -> Result<Option<BlobId>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_option(OptionalBlobIdVisitor)
}

pub(crate) fn deserialize_revision_id<'de, D>(deserializer: D) -> Result<RevisionId, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(FixedParseVisitor::<RevisionId>::new("revision ULID", 26))
}

pub(crate) fn deserialize_command_id<'de, D>(deserializer: D) -> Result<CommandId, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(FixedParseVisitor::<CommandId>::new("command ULID", 26))
}

pub(crate) fn deserialize_project_id<'de, D>(deserializer: D) -> Result<ProjectId, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_str(FixedParseVisitor::<ProjectId>::new("project ULID", 26))
}

struct OptionalBlobIdVisitor;

impl<'de> Visitor<'de> for OptionalBlobIdVisitor {
    type Value = Option<BlobId>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("null or a 64-character SHA-256 digest")
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_blob_id(deserializer).map(Some)
    }
}

struct FixedParseVisitor<T> {
    label: &'static str,
    exact_len: usize,
    marker: PhantomData<T>,
}

impl<T> FixedParseVisitor<T> {
    const fn new(label: &'static str, exact_len: usize) -> Self {
        Self {
            label,
            exact_len,
            marker: PhantomData,
        }
    }
}

impl<'de, T> Visitor<'de> for FixedParseVisitor<T>
where
    T: FromStr + fmt::Display,
    T::Err: fmt::Display,
{
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} encoded as exactly {} ASCII characters",
            self.label, self.exact_len
        )
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.parse(value)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.parse(value)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.parse(&value)
    }
}

impl<T> FixedParseVisitor<T>
where
    T: FromStr + fmt::Display,
    T::Err: fmt::Display,
{
    fn parse<E>(&self, value: &str) -> Result<T, E>
    where
        E: de::Error,
    {
        if value.len() != self.exact_len || !value.is_ascii() {
            return Err(E::invalid_length(value.len(), self));
        }
        let parsed = value.parse::<T>().map_err(E::custom)?;
        if parsed.to_string() != value {
            return Err(E::custom(format_args!(
                "non-canonical {} encoding",
                self.label
            )));
        }
        Ok(parsed)
    }
}

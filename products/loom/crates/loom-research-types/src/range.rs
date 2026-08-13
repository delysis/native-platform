use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{MAX_RAW_OUTPUT_BYTES, MAX_SOURCE_BYTES};

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RangeError {
    #[error("range start {start} exceeds end {end}")]
    Reversed { start: u64, end: u64 },
    #[error("range must be non-empty")]
    Empty,
    #[error("range end {end} exceeds byte length {length}")]
    OutOfBounds { end: u64, length: usize },
    #[error("range boundary {offset} splits a UTF-8 code point")]
    SplitsUtf8 { offset: u64 },
    #[error("raw output has {actual} bytes; maximum is {maximum}")]
    RawOutputTooLarge { actual: usize, maximum: u64 },
    #[error("source has {actual} bytes; maximum is {maximum}")]
    SourceTooLarge { actual: usize, maximum: usize },
    #[error("bytes are not valid UTF-8")]
    InvalidUtf8,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct ByteRange {
    start: u64,
    end: u64,
}

impl ByteRange {
    pub const fn new(start: u64, end: u64) -> Result<Self, RangeError> {
        if start > end {
            return Err(RangeError::Reversed { start, end });
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn end(self) -> u64 {
        self.end
    }

    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn checked_slice(self, bytes: &[u8]) -> Result<&[u8], RangeError> {
        validate_range(bytes, self.start, self.end)?;
        let start = usize::try_from(self.start).map_err(|_| RangeError::OutOfBounds {
            end: self.end,
            length: bytes.len(),
        })?;
        let end = usize::try_from(self.end).map_err(|_| RangeError::OutOfBounds {
            end: self.end,
            length: bytes.len(),
        })?;
        Ok(&bytes[start..end])
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ByteRangeWire {
    start: u64,
    end: u64,
}

impl<'de> Deserialize<'de> for ByteRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ByteRangeWire::deserialize(deserializer)?;
        Self::new(wire.start, wire.end).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct NonEmptyByteRange {
    start: u64,
    end: u64,
}

impl NonEmptyByteRange {
    pub const fn new(start: u64, end: u64) -> Result<Self, RangeError> {
        if start > end {
            return Err(RangeError::Reversed { start, end });
        }
        if start == end {
            return Err(RangeError::Empty);
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn end(self) -> u64 {
        self.end
    }

    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        false
    }

    pub const fn as_range(self) -> ByteRange {
        ByteRange {
            start: self.start,
            end: self.end,
        }
    }

    pub fn checked_str(self, bytes: &[u8]) -> Result<&str, RangeError> {
        let slice = self.as_range().checked_slice(bytes)?;
        std::str::from_utf8(slice).map_err(|_| RangeError::InvalidUtf8)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NonEmptyByteRangeWire {
    start: u64,
    end: u64,
}

impl<'de> Deserialize<'de> for NonEmptyByteRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = NonEmptyByteRangeWire::deserialize(deserializer)?;
        Self::new(wire.start, wire.end).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct NonEmptyTokenRange {
    start: u32,
    end: u32,
}

impl NonEmptyTokenRange {
    pub const fn new(start: u32, end: u32) -> Result<Self, RangeError> {
        if start > end {
            return Err(RangeError::Reversed {
                start: start as u64,
                end: end as u64,
            });
        }
        if start == end {
            return Err(RangeError::Empty);
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> u32 {
        self.start
    }

    pub const fn end(self) -> u32 {
        self.end
    }

    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        false
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NonEmptyTokenRangeWire {
    start: u32,
    end: u32,
}

impl<'de> Deserialize<'de> for NonEmptyTokenRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = NonEmptyTokenRangeWire::deserialize(deserializer)?;
        Self::new(wire.start, wire.end).map_err(serde::de::Error::custom)
    }
}

pub(crate) fn validate_raw_utf8(bytes: &[u8]) -> Result<&str, RangeError> {
    if bytes.len() as u64 > MAX_RAW_OUTPUT_BYTES {
        return Err(RangeError::RawOutputTooLarge {
            actual: bytes.len(),
            maximum: MAX_RAW_OUTPUT_BYTES,
        });
    }
    std::str::from_utf8(bytes).map_err(|_| RangeError::InvalidUtf8)
}

pub(crate) fn validate_source_utf8(bytes: &[u8]) -> Result<&str, RangeError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(RangeError::SourceTooLarge {
            actual: bytes.len(),
            maximum: MAX_SOURCE_BYTES,
        });
    }
    std::str::from_utf8(bytes).map_err(|_| RangeError::InvalidUtf8)
}

fn validate_range(bytes: &[u8], start: u64, end: u64) -> Result<(), RangeError> {
    if end > bytes.len() as u64 {
        return Err(RangeError::OutOfBounds {
            end,
            length: bytes.len(),
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| RangeError::InvalidUtf8)?;
    for offset in [start, end] {
        let offset_usize = usize::try_from(offset).map_err(|_| RangeError::OutOfBounds {
            end,
            length: bytes.len(),
        })?;
        if !text.is_char_boundary(offset_usize) {
            return Err(RangeError::SplitsUtf8 { offset });
        }
    }
    Ok(())
}

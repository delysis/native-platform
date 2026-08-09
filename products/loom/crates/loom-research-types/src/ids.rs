use std::{fmt, str::FromStr};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, Visitor},
};
use ulid::Ulid;

macro_rules! occurrence_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::new())
            }

            pub const fn from_ulid(value: Ulid) -> Self {
                Self(value)
            }

            pub const fn as_ulid(self) -> Ulid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ulid::from_str(value).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                deserializer.deserialize_str(UlidVisitor).map(Self)
            }
        }
    };
}

struct UlidVisitor;

impl<'de> Visitor<'de> for UlidVisitor {
    type Value = Ulid;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a ULID encoded as exactly 26 ASCII characters")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        parse_ulid(value)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        parse_ulid(value)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        parse_ulid(&value)
    }
}

fn parse_ulid<E>(value: &str) -> Result<Ulid, E>
where
    E: de::Error,
{
    if value.len() != 26 || !value.is_ascii() {
        return Err(E::invalid_length(value.len(), &UlidVisitor));
    }
    let parsed = Ulid::from_str(value).map_err(E::custom)?;
    if parsed.to_string() != value {
        return Err(E::custom("non-canonical ULID encoding"));
    }
    Ok(parsed)
}

occurrence_id!(ModelCallId);
occurrence_id!(CampaignId);
occurrence_id!(StageId);
occurrence_id!(StageAttemptId);
occurrence_id!(TrialCaseId);
occurrence_id!(GeneratedSpanOccurrenceId);
occurrence_id!(CandidateAssemblyId);
occurrence_id!(CandidateProjectionId);
occurrence_id!(MixedAuthorshipAssemblyId);
occurrence_id!(PipelineOperationId);

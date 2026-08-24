#![forbid(unsafe_code)]

//! Exact-byte Overture GeoParquet admission and typed spatial querying.
//!
//! This crate has no network, process, SQL, renderer, or model authority. A
//! caller supplies exact STAC bytes and already-acquired local partition
//! bytes. The crate pins them, opens partitions read-only, and exposes only a
//! structured bbox/theme/type query to an injected heavy GeoParquet engine.

mod identity;
mod query;

pub use identity::{
    ExactStacDocument, OverturePartitionAdmission, OverturePartitionIdentity,
    OvertureReleaseIdentity, VerifiedOverturePartition, admit_overture_partition,
    admit_overture_release,
};
pub use query::{
    EngineErrorClass, EngineFeature, EngineOutput, OvertureEngineError, OvertureEngineRequest,
    OvertureFeature, OvertureFeatureProvenance, OvertureHeavyBackend, OvertureQueryLimits,
    OvertureQueryResult, OvertureSelection, PartitionPushdownReceipt, PushdownMechanism,
    PushdownPredicate, PushdownReceipt, execute_overture_query,
};

use thiserror::Error;

pub const OVERTURE_STAC_HOST: &str = "stac.overturemaps.org";
pub const OVERTURE_STAC_VERSION: &str = "1.1.0";
pub const OVERTURE_BACKEND_CONTRACT: &str = "information.overture.typed_query.v1";
pub const MAX_STAC_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum OvertureError {
    #[error("invalid exact-byte expectation: {0}")]
    InvalidExpectation(&'static str),
    #[error("STAC identity is invalid: {0}")]
    InvalidStacIdentity(String),
    #[error("Overture selection is invalid: {0}")]
    InvalidSelection(&'static str),
    #[error("Overture partition admission is invalid: {0}")]
    InvalidPartition(String),
    #[error("configured Overture limits are invalid")]
    InvalidLimits,
    #[error("bounded Overture query requires at least one admitted partition")]
    NoPartitions,
    #[error("Overture query exceeds its configured {0} limit")]
    LimitExceeded(&'static str),
    #[error("exact byte length mismatch for {kind}: expected {expected}, observed {actual}")]
    ByteLengthMismatch {
        kind: &'static str,
        expected: u64,
        actual: u64,
    },
    #[error("exact SHA-256 mismatch for {0}")]
    Sha256Mismatch(&'static str),
    #[error("local Overture partition path is a symbolic link")]
    SymlinkPartition,
    #[error("local Overture partition is not a regular file")]
    NotRegularPartition,
    #[error("local Overture partition changed while admitted or queried")]
    PartitionChanged,
    #[error("GeoParquet engine returned invalid proof or data: {0}")]
    InvalidEngineOutput(String),
    #[error(transparent)]
    Engine(#[from] OvertureEngineError),
    #[error("{context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Catalog(#[from] information_native_catalog::CatalogError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl OvertureError {
    pub(crate) fn io(context: &'static str, source: std::io::Error) -> Self {
        Self::Io { context, source }
    }
}

#[cfg(test)]
mod tests;

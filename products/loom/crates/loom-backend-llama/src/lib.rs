#![forbid(unsafe_code)]

mod adapter;
mod discovery;
mod download;
mod fit;
mod model;
mod runtime;

pub use discovery::{
    DEFAULT_MAX_DISCOVERY_DEPTH, DEFAULT_MAX_DISCOVERY_ENTRIES, DiscoveredGguf, DiscoveryError,
    DiscoveryWarning, GgufHeaderStatus, ModelDiscoveryOptions, ModelDiscoveryReport,
    ModelDiscoverySource, default_hugging_face_cache_roots, discover_gguf_models,
};
pub use download::{
    DEFAULT_PROGRESS_INTERVAL_BYTES, DownloadCancellation, DownloadControl, DownloadDisposition,
    DownloadError, DownloadPhase, DownloadProgress, GgufDownloadRequest, GgufDownloadResult,
    MAX_MODEL_DOWNLOAD_BYTES, MAX_PROGRESS_INTERVAL_BYTES, MAX_REDIRECTS,
    MIN_PROGRESS_INTERVAL_BYTES, Sha256Digest, download_gguf, validate_gguf_download_request,
};
pub use fit::{
    ByteEstimate, ByteEstimateBasis, FitEstimationError, FitVerdict, ModelFitEstimate,
    ModelFitInput, estimate_model_fit,
};
pub use model::{
    CapabilitySupport, LocalDevicePreference, LocalModelProfile, ModelInspectionError,
    ProbabilitySemantics, RuntimeModelInspection, VerifiedCapabilitySet, VerifiedMediaCapability,
    VerifiedMediaKind, VerifiedModelDescriptor, is_gguf_path, verify_model_inspection,
};
pub use runtime::{
    BatchExecution, BatchRuntime, CompleteModelRelease, JoinedLlamaRuntime, ModelRelease,
    NativeHostRuntime, ProcessExitJoinedLlamaRuntime, RuntimeEvidenceClass,
};

pub use adapter::{
    CandidateProvenanceRecord, ContinuationCase, DEFAULT_EVENT_CAPACITY, ExactContinuationRequest,
    ExactContinuationResult, JoinedLlamaGeneration, LlamaBackend, LlamaBackendError,
    LlamaGenerationControl, LlamaGenerationHandle, MAX_EVENT_CAPACITY,
    model_environment_from_verified, validate_candidate_receipt_binding,
};
pub use llama_native_types::{SamplerKind, SamplingConfig};

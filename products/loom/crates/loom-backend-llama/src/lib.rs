#![forbid(unsafe_code)]

mod adapter;
mod discovery;
mod fit;
mod model;
mod runtime;

pub use discovery::{
    DEFAULT_MAX_DISCOVERY_DEPTH, DEFAULT_MAX_DISCOVERY_ENTRIES, DiscoveredGguf, DiscoveryError,
    DiscoveryWarning, GgufHeaderStatus, ModelDiscoveryOptions, ModelDiscoveryReport,
    ModelDiscoverySource, default_hugging_face_cache_roots, discover_gguf_models,
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
pub use runtime::{BatchExecution, BatchRuntime, NativeHostRuntime, RuntimeEvidenceClass};

pub use adapter::{
    CandidateProvenanceRecord, ContinuationCase, DEFAULT_EVENT_CAPACITY, ExactContinuationRequest,
    ExactContinuationResult, LlamaBackend, LlamaBackendError, LlamaGenerationHandle,
    MAX_EVENT_CAPACITY,
};
pub use llama_native_types::{SamplerKind, SamplingConfig};

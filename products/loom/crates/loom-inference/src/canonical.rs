#[cfg(feature = "native-evidence")]
use llama_native_types::{ModelFingerprint, SamplingConfig};
#[cfg(feature = "native-evidence")]
use loom_research_types::{CallScope, ModelCallId};
use loom_types::BlobId;
#[cfg(feature = "native-evidence")]
use loom_types::ProjectId;
use sha2::{Digest, Sha256};

pub(crate) struct CanonicalDigest(Sha256);

impl CanonicalDigest {
    pub(crate) fn new(domain: &str) -> Self {
        let mut value = Self(Sha256::new());
        value.bytes(domain.as_bytes());
        value.u32(1);
        value
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.u64(value.len() as u64);
        self.0.update(value);
    }

    pub(crate) fn str(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    #[cfg(feature = "native-evidence")]
    pub(crate) fn bool(&mut self, value: bool) {
        self.0.update([u8::from(value)]);
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.0.update(value.to_be_bytes());
    }

    #[cfg(feature = "native-evidence")]
    pub(crate) fn i32(&mut self, value: i32) {
        self.0.update(value.to_be_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.0.update(value.to_be_bytes());
    }

    #[cfg(feature = "native-evidence")]
    pub(crate) fn optional_str(&mut self, value: Option<&str>) {
        self.bool(value.is_some());
        if let Some(value) = value {
            self.str(value);
        }
    }

    pub(crate) fn blob(&mut self, value: BlobId) {
        self.0.update(value.as_bytes());
    }

    #[cfg(feature = "native-evidence")]
    pub(crate) fn project_id(&mut self, value: ProjectId) {
        self.0.update(value.as_ulid().to_bytes());
    }

    #[cfg(feature = "native-evidence")]
    pub(crate) fn model_call_id(&mut self, value: ModelCallId) {
        self.0.update(value.as_ulid().to_bytes());
    }

    #[cfg(feature = "native-evidence")]
    pub(crate) fn scope(&mut self, value: CallScope) {
        self.0.update(value.campaign_id().as_ulid().to_bytes());
        self.0.update(value.stage_id().as_ulid().to_bytes());
        self.0.update(value.attempt_id().as_ulid().to_bytes());
        self.0.update(value.case_id().as_ulid().to_bytes());
    }

    #[cfg(any(feature = "native-evidence", test))]
    pub(crate) fn token_ids_u32(&mut self, values: &[u32]) {
        self.u64(values.len() as u64);
        for value in values {
            self.u32(*value);
        }
    }

    pub(crate) fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }

    pub(crate) fn finish_blob(self) -> BlobId {
        BlobId::from_bytes(self.finish())
    }
}

#[cfg(feature = "native-evidence")]
pub(crate) fn model_fingerprint_id(fingerprint: &ModelFingerprint) -> BlobId {
    let ModelFingerprint {
        model_id: _,
        model_size,
        model_sha256,
        tokenizer_sha256,
        chat_template_sha256,
        multimodal_projector_sha256,
        binding_version,
        build_id,
        backend,
        context_tokens,
        batch_tokens,
        max_sequences,
        rope_config_sha256,
        kv_layout_sha256,
    } = fingerprint;

    let mut digest = CanonicalDigest::new("loom/native-model-fingerprint/v1");
    digest.u64(*model_size);
    digest.str(model_sha256);
    digest.str(tokenizer_sha256);
    digest.str(chat_template_sha256);
    digest.optional_str(multimodal_projector_sha256.as_deref());
    digest.str(binding_version);
    digest.str(build_id);
    digest.str(backend);
    digest.u32(*context_tokens);
    digest.u32(*batch_tokens);
    digest.u32(*max_sequences);
    digest.str(rope_config_sha256);
    digest.str(kv_layout_sha256);
    digest.finish_blob()
}

#[cfg(feature = "native-evidence")]
pub(crate) fn sampling_fingerprint(sampling: &SamplingConfig) -> BlobId {
    BlobId::from_bytes(*sampling.fingerprint().as_bytes())
}

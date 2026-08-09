use std::fmt;

#[cfg(feature = "native-llama")]
use std::str::FromStr;

#[cfg(feature = "native-llama")]
use llama_native_types::ModelFingerprint;
use loom_research_types::{
    CompiledManifest, ManifestArtifactHash, ManifestDocument, ManifestKey, ManifestSourceHash,
    ModelRole,
};
use loom_types::BlobId;
use thiserror::Error;

use crate::canonical::CanonicalDigest;

const BASE_WRITER_BINDING_DOMAIN: &str = "loom/base-writer-binding/v1";
const CRITIC_BINDING_DOMAIN: &str = "loom/critic-binding/v1";

/// An immutable base-writer binding compiled from `loom.model-bindings.v1`.
///
/// The private representation is deliberate: neither a caller-provided model
/// label nor a deserialized struct can become inference authority. Compilation
/// binds this value to the canonical manifest artifact, requires the selected
/// role to be `base_writer`, requires completion capability, and rejects an
/// adapter stack until the live backend can prove exact adapter identity.
///
/// ```compile_fail
/// use loom_inference::BaseWriterBinding;
/// fn require_deserialize<T: for<'de> serde::Deserialize<'de>>() {}
/// require_deserialize::<BaseWriterBinding>();
/// ```
#[derive(Clone, Eq, PartialEq)]
pub struct BaseWriterBinding {
    binding_id: ManifestKey,
    manifest_source_bytes: Vec<u8>,
    manifest_source_hash: ManifestSourceHash,
    manifest_canonical_bytes: Vec<u8>,
    manifest_fingerprint: ManifestArtifactHash,
    fingerprint: BlobId,
    declared_role: ModelRole,
    model_sha256: BlobId,
    model_bytes: u64,
    tokenizer_sha256: BlobId,
    multimodal_projector_sha256: Option<BlobId>,
    architecture: ManifestKey,
    context_tokens: u32,
    capabilities: Vec<ManifestKey>,
}

impl fmt::Debug for BaseWriterBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BaseWriterBinding")
            .field("fingerprint", &self.fingerprint)
            .field("manifest_fingerprint", &self.manifest_fingerprint)
            .finish_non_exhaustive()
    }
}

impl BaseWriterBinding {
    /// Selects and validates one base-writer binding from an intact compiled
    /// model-bindings manifest.
    pub fn compile(
        manifest: &CompiledManifest,
        binding_id: &str,
    ) -> Result<Self, BindingCompileError> {
        manifest
            .verify_integrity()
            .map_err(|_| BindingCompileError::ManifestIntegrity)?;
        let ManifestDocument::ModelBindings(bindings) = manifest.document() else {
            return Err(BindingCompileError::WrongManifestFormat);
        };
        let binding = bindings
            .bindings()
            .iter()
            .find(|binding| binding.id.as_str() == binding_id)
            .ok_or(BindingCompileError::BindingNotFound)?;
        if binding.role != ModelRole::BaseWriter {
            return Err(BindingCompileError::WrongRole);
        }
        if !binding
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == "completion")
        {
            return Err(BindingCompileError::MissingCompletionCapability);
        }
        if !binding.adapters.is_empty() {
            return Err(BindingCompileError::UnverifiableAdapterStack);
        }

        let manifest_fingerprint = manifest.artifact_hash();
        let mut digest = CanonicalDigest::new(BASE_WRITER_BINDING_DOMAIN);
        digest.blob(manifest_fingerprint.as_blob_id());
        digest.str(binding.id.as_str());
        let fingerprint = digest.finish_blob();

        Ok(Self {
            binding_id: binding.id.clone(),
            manifest_source_bytes: manifest.source_bytes().to_vec(),
            manifest_source_hash: manifest.source_hash(),
            manifest_canonical_bytes: manifest.canonical_bytes().to_vec(),
            manifest_fingerprint,
            fingerprint,
            declared_role: binding.role,
            model_sha256: binding.model_sha256,
            model_bytes: binding.model_bytes,
            tokenizer_sha256: binding.tokenizer_sha256,
            multimodal_projector_sha256: binding.multimodal_projector_sha256,
            architecture: binding.architecture.clone(),
            context_tokens: binding.context_tokens,
            capabilities: binding.capabilities.iter().cloned().collect(),
        })
    }

    pub fn binding_id(&self) -> &str {
        self.binding_id.as_str()
    }

    pub fn manifest_source_bytes(&self) -> &[u8] {
        &self.manifest_source_bytes
    }

    pub const fn manifest_source_hash(&self) -> ManifestSourceHash {
        self.manifest_source_hash
    }

    pub fn manifest_canonical_bytes(&self) -> &[u8] {
        &self.manifest_canonical_bytes
    }

    pub const fn manifest_fingerprint(&self) -> ManifestArtifactHash {
        self.manifest_fingerprint
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub const fn declared_role(&self) -> ModelRole {
        self.declared_role
    }

    pub const fn model_sha256(&self) -> BlobId {
        self.model_sha256
    }

    pub const fn model_bytes(&self) -> u64 {
        self.model_bytes
    }

    pub const fn tokenizer_sha256(&self) -> BlobId {
        self.tokenizer_sha256
    }

    pub const fn multimodal_projector_sha256(&self) -> Option<BlobId> {
        self.multimodal_projector_sha256
    }

    pub fn architecture(&self) -> &str {
        self.architecture.as_str()
    }

    pub const fn context_tokens(&self) -> u32 {
        self.context_tokens
    }

    pub fn capabilities(&self) -> &[ManifestKey] {
        &self.capabilities
    }

    #[cfg(feature = "native-llama")]
    pub(crate) fn verify_native_model(
        &self,
        fingerprint: &ModelFingerprint,
    ) -> Result<(), ProfileError> {
        let model_sha256 = BlobId::from_str(&fingerprint.model_sha256)
            .map_err(|_| ProfileError::MalformedModelDigest)?;
        if model_sha256 != self.model_sha256 {
            return Err(ProfileError::ModelDigest);
        }
        if fingerprint.model_size != self.model_bytes {
            return Err(ProfileError::ModelLength {
                expected: self.model_bytes,
                actual: fingerprint.model_size,
            });
        }
        let tokenizer_sha256 = BlobId::from_str(&fingerprint.tokenizer_sha256)
            .map_err(|_| ProfileError::MalformedTokenizerDigest)?;
        if tokenizer_sha256 != self.tokenizer_sha256 {
            return Err(ProfileError::TokenizerDigest);
        }
        let projector_sha256 = fingerprint
            .multimodal_projector_sha256
            .as_deref()
            .map(BlobId::from_str)
            .transpose()
            .map_err(|_| ProfileError::MalformedProjectorDigest)?;
        if projector_sha256 != self.multimodal_projector_sha256 {
            return Err(ProfileError::ProjectorDigest);
        }
        if fingerprint.context_tokens < self.context_tokens {
            return Err(ProfileError::InsufficientContext {
                required: self.context_tokens,
                actual: fingerprint.context_tokens,
            });
        }
        if fingerprint.batch_tokens == 0 || fingerprint.max_sequences == 0 {
            return Err(ProfileError::InvalidRuntimeGeometry);
        }
        Ok(())
    }
}

/// Exact manifest-declared adapter identity for a critic binding.
///
/// This is declaration evidence, not proof that a resident worker loaded the
/// adapter. Native execution fails closed for a non-empty stack until the
/// backend can expose the corresponding live identity.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct CriticAdapterIdentity {
    artifact_sha256: BlobId,
    scale_bits: u64,
}

impl fmt::Debug for CriticAdapterIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CriticAdapterIdentity")
            .field("artifact_sha256", &self.artifact_sha256)
            .field("scale_bits", &format_args!("{:#018x}", self.scale_bits))
            .finish()
    }
}

impl CriticAdapterIdentity {
    pub const fn artifact_sha256(self) -> BlobId {
        self.artifact_sha256
    }

    pub const fn scale_bits(self) -> u64 {
        self.scale_bits
    }
}

/// An immutable local-critic binding compiled only from an intact
/// `loom.model-bindings.v1` artifact.
///
/// It has no conversion to [`BaseWriterBinding`]. Role separation is checked
/// before this value can exist, and Serde is intentionally absent.
///
/// ```compile_fail
/// use loom_inference::CriticBinding;
/// fn require_deserialize<T: for<'de> serde::Deserialize<'de>>() {}
/// require_deserialize::<CriticBinding>();
/// ```
#[derive(Clone, Eq, PartialEq)]
pub struct CriticBinding {
    binding_id: ManifestKey,
    manifest_source_hash: ManifestSourceHash,
    manifest_fingerprint: ManifestArtifactHash,
    fingerprint: BlobId,
    model_sha256: BlobId,
    model_bytes: u64,
    tokenizer_sha256: BlobId,
    multimodal_projector_sha256: Option<BlobId>,
    architecture: ManifestKey,
    context_tokens: u32,
    capabilities: Vec<ManifestKey>,
    adapters: Vec<CriticAdapterIdentity>,
}

impl fmt::Debug for CriticBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CriticBinding")
            .field("fingerprint", &self.fingerprint)
            .field("manifest_fingerprint", &self.manifest_fingerprint)
            .field("adapter_count", &self.adapters.len())
            .finish_non_exhaustive()
    }
}

impl CriticBinding {
    pub fn compile(
        manifest: &CompiledManifest,
        binding_id: &str,
    ) -> Result<Self, CriticBindingCompileError> {
        manifest
            .verify_integrity()
            .map_err(|_| CriticBindingCompileError::ManifestIntegrity)?;
        let ManifestDocument::ModelBindings(bindings) = manifest.document() else {
            return Err(CriticBindingCompileError::WrongManifestFormat);
        };
        let binding = bindings
            .bindings()
            .iter()
            .find(|binding| binding.id.as_str() == binding_id)
            .ok_or(CriticBindingCompileError::BindingNotFound)?;
        if binding.role != ModelRole::Critic {
            return Err(CriticBindingCompileError::WrongRole);
        }
        let has_chat = binding
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == "chat");
        let has_json_schema = binding
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == "json_schema");
        let has_gbnf = binding
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == "gbnf");
        if !has_chat {
            return Err(CriticBindingCompileError::MissingChatCapability);
        }
        if !has_json_schema && !has_gbnf {
            return Err(CriticBindingCompileError::MissingStructuredCapability);
        }

        let adapters = binding
            .adapters
            .iter()
            .map(|adapter| CriticAdapterIdentity {
                artifact_sha256: adapter.artifact_sha256,
                scale_bits: adapter.scale.to_bits(),
            })
            .collect::<Vec<_>>();
        let manifest_fingerprint = manifest.artifact_hash();
        let mut digest = CanonicalDigest::new(CRITIC_BINDING_DOMAIN);
        digest.blob(manifest_fingerprint.as_blob_id());
        digest.str(binding.id.as_str());
        digest.u64(adapters.len() as u64);
        for adapter in &adapters {
            digest.blob(adapter.artifact_sha256);
            digest.u64(adapter.scale_bits);
        }
        let fingerprint = digest.finish_blob();
        Ok(Self {
            binding_id: binding.id.clone(),
            manifest_source_hash: manifest.source_hash(),
            manifest_fingerprint,
            fingerprint,
            model_sha256: binding.model_sha256,
            model_bytes: binding.model_bytes,
            tokenizer_sha256: binding.tokenizer_sha256,
            multimodal_projector_sha256: binding.multimodal_projector_sha256,
            architecture: binding.architecture.clone(),
            context_tokens: binding.context_tokens,
            capabilities: binding.capabilities.iter().cloned().collect(),
            adapters,
        })
    }

    pub fn binding_id(&self) -> &str {
        self.binding_id.as_str()
    }

    pub const fn manifest_source_hash(&self) -> ManifestSourceHash {
        self.manifest_source_hash
    }

    pub const fn manifest_fingerprint(&self) -> ManifestArtifactHash {
        self.manifest_fingerprint
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub const fn model_sha256(&self) -> BlobId {
        self.model_sha256
    }

    pub const fn model_bytes(&self) -> u64 {
        self.model_bytes
    }

    pub const fn tokenizer_sha256(&self) -> BlobId {
        self.tokenizer_sha256
    }

    pub const fn multimodal_projector_sha256(&self) -> Option<BlobId> {
        self.multimodal_projector_sha256
    }

    pub fn architecture(&self) -> &str {
        self.architecture.as_str()
    }

    pub const fn context_tokens(&self) -> u32 {
        self.context_tokens
    }

    pub fn capabilities(&self) -> &[ManifestKey] {
        &self.capabilities
    }

    pub fn adapters(&self) -> &[CriticAdapterIdentity] {
        &self.adapters
    }

    pub fn supports_constraint(&self, capability: &str) -> bool {
        self.capabilities
            .iter()
            .any(|declared| declared.as_str() == capability)
    }

    #[cfg(feature = "native-llama")]
    pub(crate) fn verify_native_model(
        &self,
        fingerprint: &ModelFingerprint,
    ) -> Result<(), ProfileError> {
        if !self.adapters.is_empty() {
            return Err(ProfileError::UnverifiableAdapterStack);
        }
        verify_native_identity(
            self.model_sha256,
            self.model_bytes,
            self.tokenizer_sha256,
            self.multimodal_projector_sha256,
            self.context_tokens,
            fingerprint,
        )
    }
}

#[derive(Clone, Copy, Error, Eq, PartialEq)]
pub enum CriticBindingCompileError {
    #[error("compiled manifest failed its integrity check")]
    ManifestIntegrity,
    #[error("expected a loom.model-bindings.v1 manifest")]
    WrongManifestFormat,
    #[error("selected model binding does not exist")]
    BindingNotFound,
    #[error("selected model binding is not a critic")]
    WrongRole,
    #[error("selected critic does not explicitly declare chat capability")]
    MissingChatCapability,
    #[error("selected critic must declare json_schema or gbnf capability")]
    MissingStructuredCapability,
}

impl fmt::Debug for CriticBindingCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[derive(Error)]
pub enum BindingCompileError {
    #[error("compiled manifest failed its integrity check")]
    ManifestIntegrity,
    #[error("expected a loom.model-bindings.v1 manifest")]
    WrongManifestFormat,
    #[error("selected model binding does not exist")]
    BindingNotFound,
    #[error("selected model binding is not a base writer")]
    WrongRole,
    #[error("selected base writer does not declare completion capability")]
    MissingCompletionCapability,
    #[error("adapter-bound base writers require live adapter identity evidence")]
    UnverifiableAdapterStack,
}

impl fmt::Debug for BindingCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ManifestIntegrity => "BindingCompileError::ManifestIntegrity",
            Self::WrongManifestFormat => "BindingCompileError::WrongManifestFormat",
            Self::BindingNotFound => "BindingCompileError::BindingNotFound",
            Self::WrongRole => "BindingCompileError::WrongRole",
            Self::MissingCompletionCapability => "BindingCompileError::MissingCompletionCapability",
            Self::UnverifiableAdapterStack => "BindingCompileError::UnverifiableAdapterStack",
        })
    }
}

#[cfg(feature = "native-llama")]
#[derive(Error)]
pub enum ProfileError {
    #[error("resident model digest is malformed")]
    MalformedModelDigest,
    #[error("resident model digest does not match the compiled binding")]
    ModelDigest,
    #[error("resident model byte length mismatch: expected {expected}, received {actual}")]
    ModelLength { expected: u64, actual: u64 },
    #[error("resident tokenizer digest is malformed")]
    MalformedTokenizerDigest,
    #[error("resident tokenizer digest does not match the compiled binding")]
    TokenizerDigest,
    #[error("resident projector digest is malformed")]
    MalformedProjectorDigest,
    #[error("resident projector identity does not match the compiled binding")]
    ProjectorDigest,
    #[error("resident context is too small: required {required}, received {actual}")]
    InsufficientContext { required: u32, actual: u32 },
    #[error("the resident model reports zero batch or sequence capacity")]
    InvalidRuntimeGeometry,
    #[error("the resident backend cannot prove the manifest-declared adapter stack")]
    UnverifiableAdapterStack,
}

#[cfg(feature = "native-llama")]
impl fmt::Debug for ProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MalformedModelDigest => "ProfileError::MalformedModelDigest",
            Self::ModelDigest => "ProfileError::ModelDigest",
            Self::ModelLength { .. } => "ProfileError::ModelLength { .. }",
            Self::MalformedTokenizerDigest => "ProfileError::MalformedTokenizerDigest",
            Self::TokenizerDigest => "ProfileError::TokenizerDigest",
            Self::MalformedProjectorDigest => "ProfileError::MalformedProjectorDigest",
            Self::ProjectorDigest => "ProfileError::ProjectorDigest",
            Self::InsufficientContext { .. } => "ProfileError::InsufficientContext { .. }",
            Self::InvalidRuntimeGeometry => "ProfileError::InvalidRuntimeGeometry",
            Self::UnverifiableAdapterStack => "ProfileError::UnverifiableAdapterStack",
        })
    }
}

#[cfg(feature = "native-llama")]
fn verify_native_identity(
    model_sha256: BlobId,
    model_bytes: u64,
    tokenizer_sha256: BlobId,
    multimodal_projector_sha256: Option<BlobId>,
    context_tokens: u32,
    fingerprint: &ModelFingerprint,
) -> Result<(), ProfileError> {
    let live_model = BlobId::from_str(&fingerprint.model_sha256)
        .map_err(|_| ProfileError::MalformedModelDigest)?;
    if live_model != model_sha256 {
        return Err(ProfileError::ModelDigest);
    }
    if fingerprint.model_size != model_bytes {
        return Err(ProfileError::ModelLength {
            expected: model_bytes,
            actual: fingerprint.model_size,
        });
    }
    let live_tokenizer = BlobId::from_str(&fingerprint.tokenizer_sha256)
        .map_err(|_| ProfileError::MalformedTokenizerDigest)?;
    if live_tokenizer != tokenizer_sha256 {
        return Err(ProfileError::TokenizerDigest);
    }
    let live_projector = fingerprint
        .multimodal_projector_sha256
        .as_deref()
        .map(BlobId::from_str)
        .transpose()
        .map_err(|_| ProfileError::MalformedProjectorDigest)?;
    if live_projector != multimodal_projector_sha256 {
        return Err(ProfileError::ProjectorDigest);
    }
    if fingerprint.context_tokens < context_tokens {
        return Err(ProfileError::InsufficientContext {
            required: context_tokens,
            actual: fingerprint.context_tokens,
        });
    }
    if fingerprint.batch_tokens == 0 || fingerprint.max_sequences == 0 {
        return Err(ProfileError::InvalidRuntimeGeometry);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use loom_research_types::compile_manifest;

    use super::*;

    const GEMMA_SHA256: &str = "aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670";

    fn bindings(model_bytes: u64, extra: &str) -> Vec<u8> {
        format!(
            r#"format = "loom.model-bindings.v1"
name = "local-models"
description = "Pinned local model artifacts"

[[bindings]]
id = "writer"
role = "base_writer"
model_sha256 = "{GEMMA_SHA256}"
model_bytes = {model_bytes}
tokenizer_sha256 = "{GEMMA_SHA256}"
architecture = "gemma4"
context_tokens = 64
capabilities = ["completion", "logits"]
adapters = []
{extra}
"#
        )
        .into_bytes()
    }

    fn critic_bindings(role: &str, capabilities: &str, adapters: &str) -> Vec<u8> {
        format!(
            r#"format = "loom.model-bindings.v1"
name = "local-critics"
description = "Pinned local critic artifacts"

[[bindings]]
id = "critic"
role = "{role}"
model_sha256 = "{GEMMA_SHA256}"
model_bytes = 4954576032
tokenizer_sha256 = "{GEMMA_SHA256}"
architecture = "gemma4"
context_tokens = 64
capabilities = {capabilities}
adapters = {adapters}
"#
        )
        .into_bytes()
    }

    #[test]
    fn current_gemma_binding_is_data_not_a_code_enum() {
        let compiled = compile_manifest(&bindings(4_954_576_032, "")).expect("manifest");
        let binding = BaseWriterBinding::compile(&compiled, "writer").expect("binding");

        assert_eq!(binding.binding_id(), "writer");
        assert_eq!(binding.model_bytes(), 4_954_576_032);
        assert_eq!(binding.architecture(), "gemma4");
        assert_eq!(binding.context_tokens(), 64);
    }

    #[test]
    fn manifest_semantics_change_the_opaque_binding_fingerprint() {
        let left = compile_manifest(&bindings(4_954_576_032, "")).expect("left");
        let right = compile_manifest(&bindings(4_954_576_033, "")).expect("right");
        let left = BaseWriterBinding::compile(&left, "writer").expect("left binding");
        let right = BaseWriterBinding::compile(&right, "writer").expect("right binding");

        assert_ne!(left.fingerprint(), right.fingerprint());
    }

    #[test]
    fn debug_omits_manifest_literals() {
        let compiled = compile_manifest(&bindings(4_954_576_032, "")).expect("manifest");
        let binding = BaseWriterBinding::compile(&compiled, "writer").expect("binding");
        let rendered = format!("{binding:?}");

        assert!(!rendered.contains("local-models"));
        assert!(!rendered.contains("gemma4"));
        assert!(!rendered.contains(GEMMA_SHA256));
    }

    #[test]
    fn critic_binding_is_role_and_capability_sealed() {
        let valid = compile_manifest(&critic_bindings(
            "critic",
            r#"["chat", "json_schema"]"#,
            "[]",
        ))
        .expect("valid critic manifest");
        let critic = CriticBinding::compile(&valid, "critic").expect("critic binding");
        assert_eq!(critic.binding_id(), "critic");
        assert!(critic.supports_constraint("json_schema"));
        assert!(critic.adapters().is_empty());

        let wrong_role = compile_manifest(&critic_bindings(
            "base_writer",
            r#"["chat", "json_schema", "completion"]"#,
            "[]",
        ))
        .expect("wrong-role manifest is structurally valid");
        assert_eq!(
            CriticBinding::compile(&wrong_role, "critic"),
            Err(CriticBindingCompileError::WrongRole)
        );
        assert!(matches!(
            BaseWriterBinding::compile(&valid, "critic"),
            Err(BindingCompileError::WrongRole)
        ));
    }

    #[test]
    fn critic_requires_chat_and_one_explicit_structured_constraint() {
        let no_chat = compile_manifest(&critic_bindings("critic", r#"["json_schema"]"#, "[]"))
            .expect("manifest");
        assert_eq!(
            CriticBinding::compile(&no_chat, "critic"),
            Err(CriticBindingCompileError::MissingChatCapability)
        );
        let no_constraint =
            compile_manifest(&critic_bindings("critic", r#"["chat"]"#, "[]")).expect("manifest");
        assert_eq!(
            CriticBinding::compile(&no_constraint, "critic"),
            Err(CriticBindingCompileError::MissingStructuredCapability)
        );
    }

    #[test]
    fn critic_adapter_identity_preserves_artifact_and_exact_scale_bits() {
        let adapter = BlobId::digest(b"critic-adapter");
        let adapters = format!(
            r#"[{{ artifact_sha256 = "{}", scale = 0.375 }}]"#,
            adapter.to_hex()
        );
        let manifest =
            compile_manifest(&critic_bindings("critic", r#"["chat", "gbnf"]"#, &adapters))
                .expect("adapter critic manifest");
        let critic = CriticBinding::compile(&manifest, "critic").expect("critic");
        assert_eq!(critic.adapters().len(), 1);
        assert_eq!(critic.adapters()[0].artifact_sha256(), adapter);
        assert_eq!(critic.adapters()[0].scale_bits(), 0.375_f64.to_bits());
        let debug = format!("{critic:?}");
        assert!(!debug.contains(GEMMA_SHA256));
        assert!(!debug.contains("gemma4"));
    }
}

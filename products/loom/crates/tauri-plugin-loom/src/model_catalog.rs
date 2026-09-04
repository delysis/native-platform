use serde::Serialize;

const CATALOG_SCHEMA_VERSION: u32 = 2;
const GEMMA_CATALOG_ID: &str = "google.gemma-4-12b-it-qat-q4_0";
const GEMMA_DISPLAY_NAME: &str = "Gemma 4 12B QAT Q4_0";
const GEMMA_PUBLISHER: &str = "Google";
const GEMMA_REPOSITORY: &str = "google/gemma-4-12B-it-qat-q4_0-gguf";
const GEMMA_REVISION: &str = "29d097773436b69ff9feafd636ab4cf873786537";
const GEMMA_ARTIFACT_NAME: &str = "gemma-4-12b-it-qat-q4_0.gguf";
const GEMMA_DOWNLOAD_URL: &str = "https://huggingface.co/google/gemma-4-12B-it-qat-q4_0-gguf/resolve/29d097773436b69ff9feafd636ab4cf873786537/gemma-4-12b-it-qat-q4_0.gguf?download=true";
const GEMMA_SHA256: &str = "93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b";
const GEMMA_ARTIFACT_BYTES: u64 = 6_975_879_296;
const GEMMA_PROJECTOR_NAME: &str = "mmproj-gemma-4-12b-it-qat-q4_0.gguf";
const GEMMA_PROJECTOR_DOWNLOAD_URL: &str = "https://huggingface.co/google/gemma-4-12B-it-qat-q4_0-gguf/resolve/29d097773436b69ff9feafd636ab4cf873786537/mmproj-gemma-4-12b-it-qat-q4_0.gguf?download=true";
const GEMMA_PROJECTOR_SHA256: &str =
    "cb018338a7538a9814d994bfe54644c71eb7ed54e31eae2f721e45fd3c260da7";
const GEMMA_PROJECTOR_BYTES: u64 = 175_115_616;
const GEMMA_CONTEXT_TOKENS: u32 = 262_144;
const GEMMA_RECOMMENDED_SYSTEM_MEMORY_BYTES: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ModelCatalogSnapshot {
    schema_version: u32,
    entries: [ModelCatalogEntry; 1],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ModelCatalogEntry {
    catalog_id: &'static str,
    display_name: &'static str,
    publisher: &'static str,
    repository: &'static str,
    revision: &'static str,
    artifact_name: &'static str,
    download_url: &'static str,
    expected_sha256: &'static str,
    expected_bytes: u64,
    max_bytes: u64,
    projector: ModelCatalogArtifact,
    context_tokens: u32,
    license: ModelCatalogLicense,
    memory_fit: ModelCatalogMemoryFit,
    compatibility: ModelCatalogCompatibility,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CatalogModelIdentity {
    pub(crate) catalog_id: &'static str,
    pub(crate) model_sha256: &'static str,
    pub(crate) model_file_bytes: u64,
    pub(crate) projector_name: &'static str,
    pub(crate) projector_sha256: &'static str,
    pub(crate) projector_file_bytes: u64,
    pub(crate) context_tokens: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ModelCatalogArtifact {
    artifact_name: &'static str,
    download_url: &'static str,
    expected_sha256: &'static str,
    expected_bytes: u64,
    max_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ModelCatalogLicense {
    spdx_id: &'static str,
    name: &'static str,
    url: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ModelCatalogMemoryFit {
    weight_bytes: u64,
    recommended_system_memory_bytes: u64,
    description: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ModelCatalogCompatibility {
    local_only: bool,
    hosted_fallback: bool,
    prompt_mode: &'static str,
    native_inspection_required: bool,
    legacy_local_file_name: &'static str,
    legacy_local_file_bytes: u64,
}

pub(crate) fn embedded_model_catalog() -> ModelCatalogSnapshot {
    ModelCatalogSnapshot {
        schema_version: CATALOG_SCHEMA_VERSION,
        entries: [ModelCatalogEntry {
            catalog_id: GEMMA_CATALOG_ID,
            display_name: GEMMA_DISPLAY_NAME,
            publisher: GEMMA_PUBLISHER,
            repository: GEMMA_REPOSITORY,
            revision: GEMMA_REVISION,
            artifact_name: GEMMA_ARTIFACT_NAME,
            download_url: GEMMA_DOWNLOAD_URL,
            expected_sha256: GEMMA_SHA256,
            expected_bytes: GEMMA_ARTIFACT_BYTES,
            max_bytes: GEMMA_ARTIFACT_BYTES,
            projector: ModelCatalogArtifact {
                artifact_name: GEMMA_PROJECTOR_NAME,
                download_url: GEMMA_PROJECTOR_DOWNLOAD_URL,
                expected_sha256: GEMMA_PROJECTOR_SHA256,
                expected_bytes: GEMMA_PROJECTOR_BYTES,
                max_bytes: GEMMA_PROJECTOR_BYTES,
            },
            context_tokens: GEMMA_CONTEXT_TOKENS,
            license: ModelCatalogLicense {
                spdx_id: "Apache-2.0",
                name: "Apache License 2.0",
                url: "https://ai.google.dev/gemma/docs/gemma_4_license",
            },
            memory_fit: ModelCatalogMemoryFit {
                weight_bytes: GEMMA_ARTIFACT_BYTES,
                recommended_system_memory_bytes: GEMMA_RECOMMENDED_SYSTEM_MEMORY_BYTES,
                description: "16 GiB or more system memory recommended.",
            },
            compatibility: ModelCatalogCompatibility {
                local_only: true,
                hosted_fallback: false,
                prompt_mode: "chat_completion",
                native_inspection_required: true,
                legacy_local_file_name: GEMMA_ARTIFACT_NAME,
                legacy_local_file_bytes: GEMMA_ARTIFACT_BYTES,
            },
        }],
    }
}

pub(crate) fn catalog_model_identity(catalog_id: &str) -> Option<CatalogModelIdentity> {
    (catalog_id == GEMMA_CATALOG_ID).then_some(CatalogModelIdentity {
        catalog_id: GEMMA_CATALOG_ID,
        model_sha256: GEMMA_SHA256,
        model_file_bytes: GEMMA_ARTIFACT_BYTES,
        projector_name: GEMMA_PROJECTOR_NAME,
        projector_sha256: GEMMA_PROJECTOR_SHA256,
        projector_file_bytes: GEMMA_PROJECTOR_BYTES,
        context_tokens: GEMMA_CONTEXT_TOKENS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_pins_one_local_only_verified_download() {
        let catalog = embedded_model_catalog();
        assert_eq!(catalog.schema_version, 2);
        assert_eq!(catalog.entries.len(), 1);
        let model = &catalog.entries[0];
        assert_eq!(model.catalog_id, GEMMA_CATALOG_ID);
        assert_eq!(model.revision.len(), 40);
        assert_eq!(model.expected_sha256.len(), 64);
        assert_eq!(model.expected_bytes, 6_975_879_296);
        assert_eq!(model.max_bytes, model.expected_bytes);
        assert_eq!(model.projector.expected_bytes, GEMMA_PROJECTOR_BYTES);
        assert_eq!(model.projector.max_bytes, GEMMA_PROJECTOR_BYTES);
        assert_eq!(model.context_tokens, 262_144);
        assert_eq!(model.memory_fit.weight_bytes, model.expected_bytes);
        assert!(model.memory_fit.recommended_system_memory_bytes >= model.memory_fit.weight_bytes);
        assert!(model.compatibility.local_only);
        assert!(!model.compatibility.hosted_fallback);
        assert!(model.compatibility.native_inspection_required);
        assert_eq!(model.compatibility.prompt_mode, "chat_completion");
        assert_eq!(
            model.compatibility.legacy_local_file_name,
            model.artifact_name
        );
        assert_eq!(
            model.compatibility.legacy_local_file_bytes,
            model.expected_bytes
        );
        assert_eq!(
            catalog_model_identity(GEMMA_CATALOG_ID),
            Some(CatalogModelIdentity {
                catalog_id: "google.gemma-4-12b-it-qat-q4_0",
                model_sha256: "93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b",
                model_file_bytes: 6_975_879_296,
                projector_name: GEMMA_PROJECTOR_NAME,
                projector_sha256: GEMMA_PROJECTOR_SHA256,
                projector_file_bytes: GEMMA_PROJECTOR_BYTES,
                context_tokens: GEMMA_CONTEXT_TOKENS,
            })
        );
        assert_eq!(catalog_model_identity("unknown.catalog-model"), None);
    }
}

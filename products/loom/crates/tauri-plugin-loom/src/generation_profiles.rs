//! Freeze project policy once before admission. A replay reads its frozen
//! evidence, never a newer dotfile or authored persona file.

use super::*;
use loom_config::{FrozenGenerationProfile, MineConfig};
use loom_types::ArtifactId;

pub(super) fn config_failure(error: impl std::fmt::Display) -> IpcFailure {
    IpcFailure::new("mine_configuration_invalid", error.to_string(), false)
}

pub(super) const fn task(preset: WeavePreset) -> GenerationTask {
    match preset {
        WeavePreset::AutomaticProseV2 => GenerationTask::AutomaticProse,
        WeavePreset::LoompadV1 => GenerationTask::Loompad,
        WeavePreset::AutomaticVerseV2 => GenerationTask::AutomaticVerse,
        WeavePreset::ManualV2 => GenerationTask::ManualWriting,
    }
}

pub(super) fn freeze(
    root: &std::path::Path,
    task: GenerationTask,
) -> Result<FrozenGenerationProfile, IpcFailure> {
    MineConfig::read(root)
        .and_then(|config| config.freeze(root, task))
        .map_err(config_failure)
}

pub(super) fn freeze_for_document(
    root: &std::path::Path,
    document_id: &str,
    task: GenerationTask,
) -> Result<
    (
        FrozenGenerationProfile,
        Option<crate::co_writer::AppliedCoWriter>,
    ),
    IpcFailure,
> {
    let applied =
        crate::context_attachments::applied_co_writer(root, document_id).map_err(config_failure)?;
    let profile = if let Some(applied) = &applied {
        applied.validate().map_err(config_failure)?;
        let mut profile = applied.generation.clone();
        profile.task = task;
        profile
    } else {
        freeze(root, task)?
    };
    Ok((profile, applied))
}

pub(super) fn sampling(
    profile: &FrozenGenerationProfile,
    command: CommandId,
    index: u32,
    max_tokens: u32,
    temperature: f32,
    preset: WeavePreset,
) -> Result<SamplingConfig, IpcFailure> {
    let mut sampling = profile
        .resolve(SamplingOverrides {
            seed: Some(generation_seed(command, index, preset)),
            max_tokens: Some(max_tokens),
            temperature: Some(temperature),
            ..Default::default()
        })
        .map_err(config_failure)?;
    // A configured seed is a repeatable family seed, not four identical cases.
    if let Some(seed) = profile.sampling.seed {
        sampling.seed = seed.wrapping_add(index);
    }
    let limit = match profile.task {
        GenerationTask::AutomaticProse | GenerationTask::AutomaticVerse => {
            AUTOMATIC_WEAVE_MAX_TOKENS_V2
        }
        GenerationTask::Loompad => 128,
        GenerationTask::Chat | GenerationTask::ManualWriting => 2_048,
    };
    if sampling.max_tokens > limit {
        return Err(config_failure(format!(
            "this generation task admits at most {limit} output tokens; the profile requests {}",
            sampling.max_tokens
        )));
    }
    Ok(sampling)
}

pub(super) fn preamble(profile: &FrozenGenerationProfile) -> String {
    if profile.context_in_document || profile.context_text().is_empty() {
        return String::new();
    }
    format!(
        "[BEGIN AUTHORED PERSONA CONTEXT]\n{}\n[END AUTHORED PERSONA CONTEXT]\n\n",
        profile.context_text()
    )
}

/// Reserve one byte per token before retrieval, matching Loom's conservative
/// text budget. Actual native tokenization can still reject the final prompt.
pub(super) fn reserve_context(
    window: u32,
    context: &str,
    branches: u32,
    output_tokens: u32,
) -> Result<u32, IpcFailure> {
    let bytes = u32::try_from(context.len()).map_err(config_failure)?;
    let remaining = window.checked_sub(bytes).ok_or_else(|| {
        config_failure("the selected persona exceeds the resident context window")
    })?;
    if remaining <= branches.saturating_mul(output_tokens).saturating_add(512) {
        return Err(config_failure(
            "the selected persona leaves no admitted space for input and output",
        ));
    }
    Ok(remaining)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ProfiledContextEvidence {
    #[serde(flatten)]
    pub retrieval: crate::context_attachments::ContextRetrievalEvidence,
    #[serde(default)]
    pub generation_profile: Option<FrozenGenerationProfile>,
    #[serde(default)]
    pub applied_co_writer: Option<crate::co_writer::AppliedCoWriter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loompad: Option<LoompadBatch>,
    #[serde(default)]
    pub request_sampling: Option<RequestSampling>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(super) struct RequestSampling {
    pub max_tokens: u32,
    pub temperature: f32,
}

pub(super) fn recorded_profile(
    store: &ProjectStore,
    context_recipe_artifact_id: ArtifactId,
    max_tokens: u32,
    temperature: f32,
) -> Result<Option<FrozenGenerationProfile>, IpcFailure> {
    let recipe = store
        .generation_context_recipe(context_recipe_artifact_id)
        .map_err(IpcFailure::store)?;
    let Some(blob) = recipe.retrieval_evidence_blob_id else {
        return Ok(None);
    };
    let bytes = store.read_blob(blob).map_err(IpcFailure::store)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(config_failure)?;
    // Main recorded both a direct retrieval record and, for Loompad, a wrapped
    // record. Neither has a frozen project profile. Validate that original
    // evidence rather than interpreting the current dotfile as historical policy.
    if value.get("generation_profile").is_none()
        && value.get("request_sampling").is_none()
        && value.get("applied_co_writer").is_none()
    {
        let _: crate::context_attachments::ContextRetrievalEvidence =
            serde_json::from_value(value.get("retrieval").unwrap_or(&value).clone())
                .map_err(config_failure)?;
        return Ok(None);
    }
    let evidence: ProfiledContextEvidence =
        serde_json::from_value(value).map_err(config_failure)?;
    if let Some(applied) = &evidence.applied_co_writer {
        applied.validate().map_err(config_failure)?;
    }
    if evidence
        .generation_profile
        .as_ref()
        .is_some_and(|profile| profile.context_in_document)
        && evidence.applied_co_writer.is_none()
    {
        return Err(config_failure(
            "the applied persona source snapshot is missing from generation evidence",
        ));
    }
    match (&evidence.generation_profile, evidence.request_sampling) {
        (None, None) => {}
        (Some(profile), Some(request))
            if request.max_tokens == max_tokens
                && request.temperature.to_bits() == temperature.to_bits() =>
        {
            profile.validate().map_err(config_failure)?;
        }
        _ => {
            return Err(IpcFailure::new(
                "idempotency_conflict",
                "this command ID already identifies different generation settings",
                false,
            ));
        }
    }
    Ok(evidence.generation_profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_profile_changes_actual_case_sampling_and_preserves_task_admission() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".mine.toml"), "[generation]\nautomatic_prose='voice'\n[profiles.voice.sampling]\nseed=77\ntemperature=0.25\nmax_tokens=32").unwrap();
        let profile = freeze(root.path(), GenerationTask::AutomaticProse).unwrap();
        let command = CommandId::new();
        let first = sampling(&profile, command, 0, 48, 0.8, WeavePreset::AutomaticProseV2).unwrap();
        let second =
            sampling(&profile, command, 1, 48, 0.8, WeavePreset::AutomaticProseV2).unwrap();
        assert_eq!(first.temperature.to_bits(), 0.25_f32.to_bits());
        assert_eq!(first.max_tokens, 32);
        assert_eq!((first.seed, second.seed), (77, 78));
        let bad = MineConfig::parse(
            "[generation]\nautomatic_prose='voice'\n[profiles.voice.sampling]\nmax_tokens=49",
        )
        .unwrap()
        .freeze(root.path(), GenerationTask::AutomaticProse)
        .unwrap();
        assert!(sampling(&bad, command, 0, 48, 0.8, WeavePreset::AutomaticProseV2).is_err());
    }

    #[test]
    fn authored_context_reservation_rejects_exhaustion_without_truncating_it() {
        assert_eq!(reserve_context(4096, "persona", 4, 48).unwrap(), 4089);
        assert!(reserve_context(512, "persona", 4, 48).is_err());
        assert!(reserve_context(2048, &"x".repeat(2049), 1, 48).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn recorded_profile_replays_frozen_sources_after_config_changes() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(root.path().join("Project"), "Project").unwrap();
        store
            .create_document_if_absent("draft.md", DocumentContent::Prose("Draft".into()), "test")
            .unwrap();
        let document = store.read_document("draft.md").unwrap();
        std::fs::create_dir_all(store.root().join(".mine/personas")).unwrap();
        std::fs::write(
            store.root().join(".mine/personas/voice.md"),
            "  Exact persona.\r\n",
        )
        .unwrap();
        std::fs::write(store.root().join(".mine.toml"), "[generation]\nmanual_writing='voice'\n[profiles.voice]\ncontext_file='.mine/personas/voice.md'\n[profiles.voice.sampling]\ntemperature=0.25").unwrap();
        let profile = freeze(store.root(), GenerationTask::ManualWriting).unwrap();
        let evidence = ProfiledContextEvidence {
            retrieval: crate::context_attachments::ContextRetrievalEvidence::default(),
            generation_profile: Some(profile),
            applied_co_writer: None,
            loompad: None,
            request_sampling: Some(RequestSampling {
                max_tokens: 512,
                temperature: 0.8,
            }),
        };
        let blob = store
            .store_provenance_blob(&serde_json::to_vec(&evidence).unwrap())
            .unwrap();
        let recipe = store
            .record_context_recipe(&ContextRecipe {
                source_revision_id: document.revision_id,
                ordered_source_artifact_ids: vec![],
                token_budget: 4096,
                retrieval_evidence_blob_id: Some(blob),
            })
            .unwrap();
        std::fs::write(store.root().join(".mine.toml"), "invalid = true").unwrap();
        std::fs::remove_file(store.root().join(".mine/personas/voice.md")).unwrap();
        assert!(freeze(store.root(), GenerationTask::ManualWriting).is_err());
        let replay = recorded_profile(&store, recipe.artifact_id, 512, 0.8)
            .unwrap()
            .unwrap();
        assert_eq!(replay.context_text(), "  Exact persona.\r\n");
        assert_eq!(
            replay
                .resolve(SamplingOverrides::default())
                .unwrap()
                .temperature
                .to_bits(),
            0.25_f32.to_bits()
        );
        assert!(recorded_profile(&store, recipe.artifact_id, 1024, 0.8).is_err());

        let batch = LoompadBatch {
            snapshot_id: BlobId::digest(b"main-loompad").to_string(),
            sample_target: 16,
            batch_offset: 0,
        };
        let original = serde_json::json!({
            "retrieval": crate::context_attachments::ContextRetrievalEvidence::default(),
            "loompad": batch,
        });
        let blob = store
            .store_provenance_blob(&serde_json::to_vec(&original).unwrap())
            .unwrap();
        let recipe = store
            .record_context_recipe(&ContextRecipe {
                source_revision_id: document.revision_id,
                ordered_source_artifact_ids: vec![],
                token_budget: 4096,
                retrieval_evidence_blob_id: Some(blob),
            })
            .unwrap();
        assert!(
            recorded_profile(&store, recipe.artifact_id, 128, 0.8)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            recorded_loompad_batch(&store, recipe.artifact_id).unwrap(),
            Some(batch)
        );
    }
}

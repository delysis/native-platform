    }
}

struct AttachmentResolution<'a> {
    store: &'a RuntimeStore,
    emitted_media: &'a mut BTreeSet<String>,
    budget: &'a mut ActiveAttachmentBudget,
    current_policy_fingerprint: &'a str,
}

pub fn attachment_import(
    scope: &crate::OperationScope,
    conversation_id: &str,
    path: &Path,
) -> Result<CommandResult<AttachmentImportOutput>> {
    attachment_import_with_identity(scope, conversation_id, path, None)
}
// SOURCE WINDOW GAP 1
    let store = RuntimeStore::current()?;
    let mut text_by_message_id = HashMap::new();
    let mut media = Vec::new();
    let mut emitted_media = BTreeSet::new();
    let mut budget = ActiveAttachmentBudget::default();
    let mut resolution = AttachmentResolution {
        store: &store,
        emitted_media: &mut emitted_media,
        budget: &mut budget,
        current_policy_fingerprint: &current_policy_fingerprint,
    };
    for message in active_messages {
        if message.attachment_ids.is_empty() {
            continue;
        }
// SOURCE WINDOW GAP 2
struct ResolvedAttachmentSet {
    text: String,
    media: Vec<MediaInput>,
}

fn resolve_attachment_set(
    conversation_id: &str,
    ids: &[String],
    expected_state: AttachmentState,
    records: &HashMap<&str, &AttachmentRecord>,
    resolution: &mut AttachmentResolution<'_>,
) -> Result<std::result::Result<ResolvedAttachmentSet, AttachmentContextBlocker>> {
    let mut text = Vec::new();
    let mut media = Vec::new();
    for id in ids {
        let Some(record) = records.get(id.as_str()).copied() else {
            return Ok(Err(context_blocker(
                "attachment_reference_missing",
                format!("Attachment {id} no longer exists."),
            )));
        };
        if record.conversation_id != conversation_id {
            return Ok(Err(context_blocker(
                "attachment_ownership_mismatch",
                "An attachment belongs to another conversation.".to_string(),
            )));
        }
        if record.state != expected_state {
            return Ok(Err(context_blocker(
                "attachment_state_invalid",
                format!(
                    "Attachment {} is not {:?}.",
                    record.file_name, expected_state
                ),
            )));
        }
        if record.policy_fingerprint.as_deref() != Some(resolution.current_policy_fingerprint) {
            return Ok(Err(policy_mismatch_blocker(record)));
        }
        if let Err(blocked) = resolution.budget.reserve_reference() {
            return Ok(Err(blocked));
        }
        let Some(namespace) = record.manifest_namespace.as_deref() else {
            return Ok(Err(context_blocker(
                "attachment_manifest_missing",
                "The attachment has no canonical manifest and cannot enter a model prompt."
                    .to_string(),
            )));
        };
        let manifest = resolution
            .store
            .get::<AttachmentManifest>(namespace)?
            .ok_or_else(|| anyhow!("attachment manifest {namespace} is missing"))?;
        if manifest.schema != ATTACHMENT_MANIFEST_SCHEMA || manifest.attachment_id != record.id {
            return Ok(Err(context_blocker(
                "attachment_manifest_invalid",
                "Attachment metadata did not match its canonical manifest.".to_string(),
            )));
        }
        if manifest.policy_fingerprint != resolution.current_policy_fingerprint
            || manifest.artifacts.iter().any(|artifact| {
                artifact.processor.policy_fingerprint != resolution.current_policy_fingerprint
            })
        {
            return Ok(Err(policy_mismatch_blocker(record)));
        }
        if !matches!(manifest.graph.coverage, Coverage::Complete) {
            return Ok(Err(context_blocker(
                "attachment_coverage_incomplete",
                format!(
                    "Attachment {} could not be inspected completely within the configured safety limits.",
                    record.file_name
                ),
            )));
        }
        let canonical = canonical_text(record, &manifest);
        let has_canonical_text = !canonical.is_empty();
        if !canonical.is_empty() {
            if let Err(blocked) = resolution.budget.reserve_text(canonical.len()) {
                return Ok(Err(blocked));
            }
            text.push(canonical);
        }
        let media_before = media.len();
        let mut contains_video = false;
        for artifact in &manifest.artifacts {
            let ArtifactPayload::Media {
                family,
                blob,
                metadata: _,
                validation,
            } = &artifact.payload
            else {
                continue;
            };
            let kind = match family {
                MediaFamily::Image => {
                    if !validation.grade.permits_direct_media() {
                        return Ok(Err(media_transform_blocker(record, *family)));
                    }
                    MediaKind::Image
                }
                MediaFamily::Audio => return Ok(Err(media_transform_blocker(record, *family))),
                MediaFamily::Video => {
                    if !validation.grade.permits_direct_media() {
                        return Ok(Err(media_transform_blocker(record, *family)));
                    }
                    contains_video = true;
                    continue;
                }
            };
            if resolution.emitted_media.contains(&blob.object_id.0) {
                continue;
            }
            if let Err(blocked) = resolution.budget.reserve_media(blob.byte_len) {
                return Ok(Err(blocked));
            }
            resolution.emitted_media.insert(blob.object_id.0.clone());
            let bytes =
                match load_verified_object(resolution.store, &manifest.graph, &blob.object_id) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return Ok(Err(context_blocker(
                            "attachment_content_mismatch",
                            format!(
                                "Attachment {} no longer matches its inspected content hash.",
                                record.file_name
                            ),
                        )));
                    }
                };
            media.push(MediaInput {
                id: format!("{}:{}", record.id, artifact.id.0),
                kind,
                mime: blob.media_type.clone(),
                sha256: blob.sha256.clone(),
                bytes,
            });
        }
        if contains_video {
            return Ok(Err(context_blocker(
                "attachment_video_pipeline_required",
                format!(
                    "Attachment {} contains video. Configure a native-video target or an explicit frame-and-transcription pipeline before sending it.",
                    record.file_name
                ),
            )));
        }
        if !has_canonical_text && media.len() == media_before {
            return Ok(Err(context_blocker(
                "attachment_no_model_representation",
                format!(
                    "Attachment {} has no canonical text or media representation accepted by the selected model.",
                    record.file_name
                ),
            )));
        }
    }
    Ok(Ok(ResolvedAttachmentSet {
        text: text.join("\n\n"),
        media,
    }))
}

fn canonical_text(record: &AttachmentRecord, manifest: &AttachmentManifest) -> String {
    let body = manifest
        .artifacts
        .iter()
        .filter_map(|artifact| match &artifact.payload {
            ArtifactPayload::Text { text, .. } => Some(text.as_str()),
            ArtifactPayload::Media { .. } | ArtifactPayload::Opaque { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if body.is_empty() {
        return String::new();
    }

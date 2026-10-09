//! Resolve retained attachment occurrences without confusing payload sharing
//! with source availability. This is used by the existing chat preparation path.

use super::bindings::ResolvedAttachmentWithBindings;
use super::*;
use llama_native_types::media_identity::MediaAdmission;

pub(super) fn resolve_attachment_set(
    conversation_id: &str,
    ids: &[String],
    expected_state: AttachmentState,
    records: &HashMap<&str, &AttachmentRecord>,
    resolution: &mut AttachmentResolution<'_>,
) -> Result<std::result::Result<ResolvedAttachmentWithBindings, AttachmentContextBlocker>> {
    let mut text = Vec::new();
    let mut media = Vec::new();
    let mut references = Vec::new();
    let mut sources = Vec::new();
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
                "An attachment belongs to another conversation.".into(),
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
                "The attachment has no canonical manifest and cannot enter a model prompt.".into(),
            )));
        };
        let manifest = resolution
            .store
            .get::<AttachmentManifest>(namespace)?
            .ok_or_else(|| anyhow!("attachment manifest {namespace} is missing"))?;
        if manifest.schema != ATTACHMENT_MANIFEST_SCHEMA || manifest.attachment_id != record.id {
            return Ok(Err(context_blocker(
                "attachment_manifest_invalid",
                "Attachment metadata did not match its canonical manifest.".into(),
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
        // Keep product modality policy independent of byte identity. In
        // particular, a valid WAV checksum does not authorize direct audio in
        // Mom's transcription-first path. Preserve existing typed blockers.
        for artifact in &manifest.artifacts {
            if let ArtifactPayload::Media {
                family, validation, ..
            } = &artifact.payload
                && (*family != MediaFamily::Image || !validation.grade.permits_direct_media())
            {
                return Ok(Err(media_transform_blocker(record, *family)));
            }
        }
        // Reuse the existing complete graph/artifact/accounting validation,
        // rather than making prompt preparation weaker than preview admission.
        if let Err(problem) = validate_preview_manifest(record, &manifest) {
            return Ok(Err(context_blocker(
                "attachment_manifest_invalid",
                problem.message,
            )));
        }
        let canonical = canonical_text(record, &manifest);
        match AttachmentContextSource::from_verified(record, &manifest, &canonical) {
            Ok(source) => sources.push(source),
            Err(blocked) => return Ok(Err(blocked)),
        }
        let mut has_representation = !canonical.is_empty();
        if !canonical.is_empty() {
            if let Err(blocked) = resolution.budget.reserve_text(canonical.len()) {
                return Ok(Err(blocked));
            }
            text.push(canonical);
        }
        for artifact in &manifest.artifacts {
            let ArtifactPayload::Media { blob, .. } = &artifact.payload else {
                continue;
            };
            if blob.byte_len > MAX_ACTIVE_ATTACHMENT_MEDIA_BYTES {
                return Ok(Err(context_blocker(
                    "attachment_context_media_bytes_exceeded",
                    "The retained media exceeds the per-request byte budget.".into(),
                )));
            }
            // Always load and verify this occurrence before deduplication.
            // Earlier good bytes cannot authorize a later corrupt occurrence.
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
            if u64::try_from(bytes.len()).ok() != Some(blob.byte_len)
                || blob.object_id.0 != blob.sha256
            {
                return Ok(Err(context_blocker(
                    "attachment_content_mismatch",
                    "Canonical media identity does not match its retained source.".into(),
                )));
            }
            let item = MediaInput {
                id: format!("{}:{}", record.id, artifact.id.0),
                kind: MediaKind::Image,
                mime: blob.media_type.clone(),
                sha256: blob.sha256.clone(),
                bytes,
            };
            let admission = match resolution.emitted_media.admit(&item) {
                Ok(admission) => admission,
                Err(error) => {
                    return Ok(Err(context_blocker(
                        if error.is_limit() {
                            "attachment_context_media_bytes_exceeded"
                        } else {
                            "attachment_content_mismatch"
                        },
                        error.to_string(),
                    )));
                }
            };
            // Even a shared payload remains an available representation for
            // this exact record. Its occurrence stays in message/draft evidence.
            has_representation = true;
            references.push(AttachmentMediaReference::from_verified(
                record, artifact, &item,
            ));
            if admission == MediaAdmission::NewPayload {
                // Preserve Mom's stricter 16-object policy, not Loom's 32.
                if let Err(blocked) = resolution.budget.reserve_media(blob.byte_len) {
                    return Ok(Err(blocked));
                }
                media.push(item);
            }
        }
        if !has_representation {
            return Ok(Err(context_blocker(
                "attachment_no_model_representation",
                format!(
                    "Attachment {} has no canonical text or media representation accepted by the selected model.",
                    record.file_name
                ),
            )));
        }
    }
    Ok(Ok(ResolvedAttachmentWithBindings {
        context: ResolvedAttachmentSet {
            text: text.join("\n\n"),
            media,
        },
        references,
        sources,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\
        \x00\x00\x00\x0dIHDR\x00\x00\x00\x02\x00\x00\x00\x04\x08\x02\x00\x00\x00\x2b\x8d\x79\x6e\
        \x00\x00\x00\x09pHYs\x00\x00\x00\x01\x00\x00\x00\x01\x00\x4f\x25\xc4\xd6\
        \x00\x00\x00\x10IDAT\x78\x9c\x63\xfc\xc3\x00\x02\x2c\x0c\x58\x28\x00\x1b\x74\x01\x0a\x5f\x82\xdc\x5d\
        \x00\x00\x00\x00IEND\xae\x42\x60\x82";

    struct Session(PathBuf);
    impl Session {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("mom-media-identity-{}", Uuid::new_v4()));
            std::fs::create_dir(&path).expect("isolated directory");
            crate::config::set_data_dir_override_for_tests(Some(path.clone()));
            Self(path)
        }
        fn image(&self) -> AttachmentRecord {
            let path = self.0.join("same.png");
            std::fs::write(&path, PNG).expect("fixture bytes");
            attachment_import(&crate::OperationScope::detached(), "chat", &path)
                .expect("import")
                .result
                .expect("imported attachment")
                .attachment
        }
    }
    impl Drop for Session {
        fn drop(&mut self) {
            crate::config::set_data_dir_override_for_tests(None);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn repeated_image_is_available_without_erasing_either_source_occurrence() {
        let session = Session::new();
        let first = session.image();
        let second = session.image();
        assert_ne!(first.id, second.id);
        assert_eq!(first.sha256, second.sha256);
        let before = RuntimeStore::current()
            .expect("store")
            .get_bytes(ATTACHMENTS_NAMESPACE)
            .expect("snapshot");
        let context = prepare_chat_attachments("chat", &[], None)
            .expect("prepare")
            .expect("valid repeated image");
        assert_eq!(context.media.len(), 1);
        assert_eq!(context.media[0].bytes, PNG);
        assert_eq!(context.staged_ids, [first.id, second.id]);
        assert_eq!(
            RuntimeStore::current()
                .expect("store")
                .get_bytes(ATTACHMENTS_NAMESPACE)
                .expect("unchanged"),
            before
        );
    }

    #[test]
    fn repeated_image_references_still_obey_the_reference_budget() {
        let session = Session::new();
        let image = session.image();
        crate::conversation_store::draft_update(
            Some("chat"),
            "question".into(),
            vec![image.id.clone(); MAX_ACTIVE_ATTACHMENT_REFERENCES],
        )
        .expect("bounded repeated references");
        let context = prepare_chat_attachments("chat", &[], None)
            .expect("prepare")
            .expect("at limit");
        assert_eq!(context.media.len(), 1);
        assert_eq!(context.staged_ids.len(), MAX_ACTIVE_ATTACHMENT_REFERENCES);
        crate::conversation_store::draft_update(
            Some("chat"),
            "question".into(),
            vec![image.id; MAX_ACTIVE_ATTACHMENT_REFERENCES + 1],
        )
        .expect("adversarial fixture");
        let blocked = prepare_chat_attachments("chat", &[], None)
            .expect("typed blocker")
            .expect_err("over limit");
        assert_eq!(blocked.blocker.code, "attachment_context_count_exceeded");
    }

    #[test]
    fn prompt_admission_reuses_preview_graph_accounting_checks() {
        let session = Session::new();
        let image = session.image();
        let namespace = image.manifest_namespace.as_deref().expect("manifest");
        let store = RuntimeStore::current().expect("store");
        let mut manifest = store
            .get::<AttachmentManifest>(namespace)
            .expect("read")
            .expect("manifest");
        manifest.graph.usage.media_bytes += 1;
        store
            .put(namespace, &manifest)
            .expect("inconsistent accounting fixture");
        let blocked = prepare_chat_attachments("chat", &[], None)
            .expect("typed blocker")
            .expect_err("invalid graph");
        assert_eq!(blocked.blocker.code, "attachment_manifest_invalid");
    }

    #[test]
    fn scoped_preparation_retains_duplicate_payload_source_bindings() {
        let session = Session::new();
        let first = session.image();
        let second = session.image();
        let scoped =
            prepare_scoped_chat_attachments("chat", &[], CurrentAttachmentSelection::Draft)
                .expect("prepare")
                .expect("admitted");
        assert_eq!(scoped.media.len(), 1);
        assert_eq!(scoped.current_media.len(), 2);
        assert_eq!(scoped.current_media[0].attachment_id, first.id);
        assert_eq!(scoped.current_media[1].attachment_id, second.id);
        assert_eq!(
            scoped.current_media[0].sha256,
            scoped.current_media[1].sha256
        );
        assert_ne!(
            scoped.current_media[0].media_id,
            scoped.current_media[1].media_id
        );
    }

    #[test]
    fn history_only_preparation_does_not_consume_or_read_the_current_draft() {
        let session = Session::new();
        let image = session.image();
        crate::conversation_store::draft_update(
            Some("chat"),
            "unsent private draft".into(),
            vec![image.id],
        )
        .expect("draft");
        let store = RuntimeStore::current().expect("store");
        let before = store.get_bytes(DRAFTS_NAMESPACE).expect("draft bytes");
        let scoped =
            prepare_scoped_chat_attachments("chat", &[], CurrentAttachmentSelection::HistoryOnly)
                .expect("prepare")
                .expect("history only");
        assert!(scoped.draft_snapshot.is_none());
        assert!(
            scoped.current_media.is_empty()
                && scoped.media.is_empty()
                && scoped.staged_ids.is_empty()
        );
        assert_eq!(
            store.get_bytes(DRAFTS_NAMESPACE).expect("unchanged draft"),
            before
        );
    }

    #[test]
    fn an_invented_regeneration_id_cannot_fall_back_to_empty_attachment_context() {
        let _session = Session::new();
        let blocked = prepare_scoped_chat_attachments(
            "chat",
            &[],
            CurrentAttachmentSelection::ExistingUser("__snapshot__"),
        )
        .expect("typed result")
        .expect_err("no matching message");
        assert_eq!(
            blocked.blocker.code,
            "attachment_regeneration_message_missing"
        );
    }
}

//! Message-scoped source occurrences over request-local, shared media payloads.
//!
//! These metadata records are created only after the existing Attachment graph,
//! ownership, coverage, policy and byte checks. They are not read capabilities.
//! A selected occurrence is rechecked against the retained immutable payload;
//! omitting a history unit omits its media even when another unit shares bytes.

use super::*;
use llama_native_types::media_identity::{MediaAdmission, MediaIdentityLedger};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct AttachmentMediaReference {
    pub attachment_id: String,
    pub artifact_id: String,
    pub media_id: String,
    pub kind: MediaKind,
    pub mime: String,
    pub sha256: String,
    pub byte_len: u64,
}

impl AttachmentMediaReference {
    pub(super) fn from_verified(
        record: &AttachmentRecord,
        artifact: &CanonicalArtifact,
        item: &MediaInput,
    ) -> Self {
        Self {
            attachment_id: record.id.clone(),
            artifact_id: artifact.id.0.clone(),
            media_id: item.id.clone(),
            kind: item.kind,
            mime: item.mime.clone(),
            sha256: item.sha256.clone(),
            byte_len: item.bytes.len() as u64,
        }
    }
}

/// The exact conversation/message owns the source reference, not the deduplicated
/// payload's first occurrence ID. The final user input uses its planned message ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct SelectedAttachmentMedia {
    pub conversation_id: String,
    pub message_id: String,
    pub reference: AttachmentMediaReference,
    pub input_index: usize,
}

/// Discard the builder on any error; an earlier successful addition is not a
/// transferable read or execution capability.
pub(crate) struct SelectedMediaBuilder {
    ledger: MediaIdentityLedger,
    payloads: Vec<MediaInput>,
    sources: Vec<SelectedAttachmentMedia>,
}

impl SelectedMediaBuilder {
    pub(crate) fn new() -> Self {
        Self {
            ledger: MediaIdentityLedger::default(),
            payloads: Vec::new(),
            sources: Vec::new(),
        }
    }

    pub(crate) fn add(
        &mut self,
        conversation_id: &str,
        message_id: &str,
        references: &[AttachmentMediaReference],
        pool: &[MediaInput],
    ) -> std::result::Result<(), AttachmentContextBlocker> {
        if !valid_source_id(conversation_id) || !valid_source_id(message_id) {
            return Err(context_blocker(
                "attachment_source_identity_invalid",
                "A selected media source has an invalid conversation or message identity.".into(),
            ));
        }
        for reference in references {
            if !valid_source_id(&reference.attachment_id)
                || !valid_source_id(&reference.artifact_id)
            {
                return Err(context_blocker(
                    "attachment_source_identity_invalid",
                    "A selected media source has an invalid attachment or artifact identity."
                        .into(),
                ));
            }
            // Resolve a verified payload by all semantic identity fields, not
            // by a friendly filename, vector position, or a digest alone.
            let payload = pool
                .iter()
                .find(|payload| {
                    payload.kind == reference.kind
                        && payload.sha256 == reference.sha256
                        && payload.mime == reference.mime
                        && u64::try_from(payload.bytes.len()).ok() == Some(reference.byte_len)
                })
                .ok_or_else(|| {
                    context_blocker(
                        "attachment_selected_media_missing",
                        "A selected attachment occurrence has no matching retained payload.".into(),
                    )
                })?;
            let admission = self
                .ledger
                .admit_parts(
                    &reference.media_id,
                    reference.kind,
                    &reference.mime,
                    &reference.sha256,
                    &payload.bytes,
                )
                .map_err(|error| {
                    context_blocker("attachment_selected_media_invalid", error.to_string())
                })?;
            let input_index = if admission == MediaAdmission::NewPayload {
                if self.payloads.len() >= MAX_ACTIVE_ATTACHMENT_MEDIA_OBJECTS as usize {
                    return Err(context_blocker(
                        "attachment_context_media_count_exceeded",
                        "The selected consult media exceeds Mom's per-request object limit.".into(),
                    ));
                }
                let mut item = payload.clone();
                item.id = native_media_id(reference);
                self.payloads.push(item);
                self.payloads.len() - 1
            } else {
                self.payloads
                    .iter()
                    .position(|item| {
                        item.kind == reference.kind
                            && item.sha256 == reference.sha256
                            && item.mime == reference.mime
                    })
                    .ok_or_else(|| {
                        context_blocker(
                            "attachment_selected_media_invalid",
                            "The media ledger and selected payload table disagree.".into(),
                        )
                    })?
            };
            self.sources.push(SelectedAttachmentMedia {
                conversation_id: conversation_id.into(),
                message_id: message_id.into(),
                reference: reference.clone(),
                input_index,
            });
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> (Vec<MediaInput>, Vec<SelectedAttachmentMedia>) {
        (self.payloads, self.sources)
    }
}

pub(crate) fn native_media_id(reference: &AttachmentMediaReference) -> String {
    let kind = match reference.kind {
        MediaKind::Image => "image",
        MediaKind::Audio => "audio",
    };
    format!("{kind}-sha256-{}", reference.sha256)
}

fn valid_source_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

/// Aggregate selected canonical text and attachment references across both
/// source and host histories. Separate preparation must not multiply limits.
pub(crate) fn validate_selected_attachment_budget(
    references: usize,
    canonical_text_bytes: usize,
) -> std::result::Result<(), AttachmentContextBlocker> {
    if references > MAX_ACTIVE_ATTACHMENT_REFERENCES {
        return Err(context_blocker(
            "attachment_context_count_exceeded",
            "The selected consult context exceeds the aggregate attachment-reference limit.".into(),
        ));
    }
    if u64::try_from(canonical_text_bytes).unwrap_or(u64::MAX) > MAX_ACTIVE_ATTACHMENT_TEXT_BYTES {
        return Err(context_blocker(
            "attachment_context_text_limit_exceeded",
            "The selected consult context exceeds the aggregate canonical-text limit.".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Identity/selection fixtures only, not assertions that bytes decode as media.
    fn payload(id: &str, text: &[u8]) -> MediaInput {
        MediaInput {
            id: id.into(),
            kind: MediaKind::Image,
            mime: "image/png".into(),
            sha256: format!("{:x}", Sha256::digest(text)),
            bytes: text.to_vec(),
        }
    }
    fn reference(id: &str, media: &MediaInput) -> AttachmentMediaReference {
        AttachmentMediaReference {
            attachment_id: format!("attachment-{id}"),
            artifact_id: format!("artifact-{id}"),
            media_id: id.into(),
            kind: media.kind,
            mime: media.mime.clone(),
            sha256: media.sha256.clone(),
            byte_len: media.bytes.len() as u64,
        }
    }

    #[test]
    fn shared_payload_preserves_both_selected_source_occurrences() {
        let item = payload("first", b"shared");
        let mut builder = SelectedMediaBuilder::new();
        builder
            .add(
                "expert",
                "source-message",
                &[reference("first", &item)],
                std::slice::from_ref(&item),
            )
            .expect("source");
        builder
            .add(
                "host",
                "host-message",
                &[reference("second", &item)],
                std::slice::from_ref(&item),
            )
            .expect("alias");
        let (media, sources) = builder.finish();
        assert_eq!(media.len(), 1);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].conversation_id, "expert");
        assert_eq!(sources[1].conversation_id, "host");
        assert_eq!(sources[0].input_index, sources[1].input_index);
    }

    #[test]
    fn discarded_or_other_expert_media_is_not_in_the_request() {
        let own = payload("own", b"own image");
        let other = payload("other", b"other expert image");
        let pool = [own.clone(), other];
        let mut builder = SelectedMediaBuilder::new();
        builder
            .add(
                "expert-a",
                "selected",
                &[reference("selected-alias", &own)],
                &pool,
            )
            .expect("own source");
        let (media, sources) = builder.finish();
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].bytes, own.bytes);
        assert_eq!(
            media[0].id,
            native_media_id(&reference("selected-alias", &own))
        );
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].message_id, "selected");
    }

    #[test]
    fn identical_digest_claim_does_not_hide_changed_payload_bytes() {
        let mut item = payload("image", b"original");
        let source = reference("image", &item);
        item.bytes = b"changed!".to_vec();
        let mut builder = SelectedMediaBuilder::new();
        let error = builder
            .add("expert", "message", &[source], &[item])
            .expect_err("hash mismatch");
        assert_eq!(error.blocker.code, "attachment_selected_media_invalid");
    }

    #[test]
    fn kind_mime_and_length_are_part_of_lookup_identity() {
        let item = payload("image", b"payload");
        for field in ["kind", "mime", "length"] {
            let mut source = reference("image", &item);
            match field {
                "kind" => source.kind = MediaKind::Audio,
                "mime" => source.mime = "image/jpeg".into(),
                _ => source.byte_len += 1,
            }
            let mut builder = SelectedMediaBuilder::new();
            assert_eq!(
                builder
                    .add("expert", "message", &[source], std::slice::from_ref(&item))
                    .expect_err("conflicting identity")
                    .blocker
                    .code,
                "attachment_selected_media_missing"
            );
        }
    }

    #[test]
    fn repeated_source_id_cannot_name_another_payload() {
        let first = payload("first", b"first");
        let second = payload("second", b"second");
        let mut builder = SelectedMediaBuilder::new();
        builder
            .add(
                "expert",
                "message",
                &[reference("same-source", &first)],
                &[first],
            )
            .expect("first");
        assert!(
            builder
                .add(
                    "expert",
                    "message",
                    &[reference("same-source", &second)],
                    &[second]
                )
                .is_err()
        );
    }

    #[test]
    fn all_source_and_host_references_share_one_budget() {
        assert!(
            validate_selected_attachment_budget(
                MAX_ACTIVE_ATTACHMENT_REFERENCES,
                MAX_ACTIVE_ATTACHMENT_TEXT_BYTES as usize
            )
            .is_ok()
        );
        assert!(
            validate_selected_attachment_budget(MAX_ACTIVE_ATTACHMENT_REFERENCES + 1, 0).is_err()
        );
        assert!(
            validate_selected_attachment_budget(0, MAX_ACTIVE_ATTACHMENT_TEXT_BYTES as usize + 1)
                .is_err()
        );
    }

    #[test]
    fn source_metadata_is_bounded_and_never_normalized() {
        for invalid in ["", "bad\nsource", " source", &"x".repeat(257)] {
            let mut builder = SelectedMediaBuilder::new();
            assert!(builder.add(invalid, "message", &[], &[]).is_err());
        }
    }

    #[test]
    fn empty_selection_produces_no_media_even_when_pool_contains_private_inputs() {
        let mut builder = SelectedMediaBuilder::new();
        builder
            .add("expert", "empty", &[], &[payload("private", b"private")])
            .expect("empty");
        let (media, sources) = builder.finish();
        assert!(media.is_empty() && sources.is_empty());
    }
}

/// Path-free exact representation evidence. Artifact IDs retain the existing
/// canonical text segments/page coordinates in the inspected manifest; the
/// manifest digest prevents a later same-ID replacement from changing citations.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AttachmentContextSource {
    pub attachment_id: String,
    pub root_sha256: String,
    pub policy_fingerprint: String,
    pub manifest_sha256: String,
    pub artifact_ids: Vec<String>,
    pub canonical_text_sha256: Option<String>,
    pub canonical_text_bytes: usize,
}

impl AttachmentContextSource {
    pub(super) fn from_verified(
        record: &AttachmentRecord,
        manifest: &AttachmentManifest,
        text: &str,
    ) -> std::result::Result<Self, AttachmentContextBlocker> {
        let bytes = serde_json::to_vec(manifest).map_err(|_| {
            context_blocker(
                "attachment_manifest_invalid",
                "The exact attachment manifest could not be fingerprinted.".into(),
            )
        })?;
        Ok(Self {
            attachment_id: record.id.clone(),
            root_sha256: record.sha256.clone(),
            policy_fingerprint: manifest.policy_fingerprint.clone(),
            manifest_sha256: format!("{:x}", Sha256::digest(bytes)),
            artifact_ids: manifest
                .artifacts
                .iter()
                .map(|artifact| artifact.id.0.clone())
                .collect(),
            canonical_text_sha256: (!text.is_empty())
                .then(|| format!("{:x}", Sha256::digest(text.as_bytes()))),
            canonical_text_bytes: text.len(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SelectedAttachmentSource {
    pub conversation_id: String,
    pub message_id: String,
    pub representation: AttachmentContextSource,
}

pub(super) struct ResolvedAttachmentWithBindings {
    pub(super) context: ResolvedAttachmentSet,
    pub(super) references: Vec<AttachmentMediaReference>,
    pub(super) sources: Vec<AttachmentContextSource>,
}

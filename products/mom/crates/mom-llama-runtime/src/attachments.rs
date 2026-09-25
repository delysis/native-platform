    }
}

struct AttachmentResolution<'a> {
    store: &'a RuntimeStore,
    emitted_media: &'a mut llama_native_types::media_identity::MediaIdentityLedger,
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
    let mut emitted_media = llama_native_types::media_identity::MediaIdentityLedger::default();
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

mod media_context;
use media_context::resolve_attachment_set;

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

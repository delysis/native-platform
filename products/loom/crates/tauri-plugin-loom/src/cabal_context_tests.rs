use super::*;
use crate::context_attachments::{
    self, add_document_context_snapshot, set_document_context_snapshot,
};
use std::io::Cursor;

fn fixture(path: &Path) -> (ProjectStore, Cabal, Cabal, DocumentId) {
    let (mut store, _) = ProjectStore::initialize(path.join("Writing"), "Garden").unwrap();
    store
        .create_document_if_absent(
            "Garden.md",
            DocumentContent::Prose("The garden is ours.\n".into()),
            "fixture",
        )
        .unwrap();
    let source = store
        .list_documents()
        .unwrap()
        .into_iter()
        .find(|doc| doc.relative_path == "Garden.md")
        .unwrap();
    let identity = Identity::generate().unwrap();
    let peer = Identity::generate().unwrap();
    let mut cabal =
        Cabal::create(&path.join("owner.db"), identity.clone(), "Garden", "Alice").unwrap();
    let invitation = cabal.invite(identity.public_key().into()).unwrap();
    let roster = cabal
        .admit(&invitation.token, peer.public_key(), "Bob")
        .unwrap();
    let other = Cabal::import(&path.join("peer.db"), peer, roster).unwrap();
    capture_document(&store, &mut cabal, &source).unwrap();
    (store, cabal, other, source.document_id)
}

fn wav() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut bytes,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    writer.write_sample(42_i16).unwrap();
    writer.finalize().unwrap();
    bytes.into_inner()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reviewed_context_crosses_quic_as_editable_markdown_with_only_selected_media() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, mut cabal, peer, source) = fixture(directory.path());
    let private_path = directory.path().join("private-research.txt");
    std::fs::write(&private_path, "A visible excerpt.\nPRIVATE ORIGINAL ENDING").unwrap();
    let private = context_attachments::import_path(store.root(), &private_path).unwrap();
    let audio_bytes = wav();
    let audio = context_attachments::import_recorded_wav(
        store.root(),
        "shared-tone.wav".into(),
        &audio_bytes,
    )
    .unwrap();
    let mut selected = add_document_context_snapshot(
        store.root(),
        &source.to_string(),
        &[private.id.clone(), audio.id.clone()],
    )
    .unwrap();
    selected.materials[0].excerpt = Some("The corrected excerpt to share.".into());
    context_attachments::set_document_context_snapshot_with_materials(
        store.root(),
        &source.to_string(),
        "Keep the prose simple.",
        &[private.id.clone(), audio.id.clone()],
        Some(&selected.materials),
    )
    .unwrap();
    let review = prepare(&store, &cabal, source).unwrap();
    let serialized = serde_json::to_string(&review).unwrap();
    assert!(!serialized.contains("PRIVATE ORIGINAL ENDING"));
    assert!(
        review
            .material
            .markdown
            .contains("> Material: private-research.txt")
    );
    assert!(
        review
            .material
            .markdown
            .contains("> The corrected excerpt to share.")
    );
    assert_eq!(review.material.files.len(), 1);
    assert_eq!(review.material.files[0].id, audio.id);
    assert!(review.material.files[0].markdown.starts_with("![Audio:"));
    assert!(
        cabal.assets().unwrap().is_empty(),
        "review does not share files"
    );
    let before = store.read_document("Garden.md").unwrap().text;
    let published = publish(&mut store, &mut cabal, &review.request).unwrap();
    assert_eq!(published.path, review.path);
    assert_eq!(store.read_document("Garden.md").unwrap().text, before);
    assert_eq!(cabal.assets().unwrap().len(), 1);
    let private_context =
        context_attachments::document_context_snapshot(store.root(), &source.to_string()).unwrap();
    assert_eq!(private_context.markdown, "Keep the prose simple.");
    assert_eq!(
        private_context.materials, selected.materials,
        "private source versions and excerpts are preserved"
    );

    let owner_network = Network::start(cabal.identity(), NetworkMode::Direct {})
        .await
        .unwrap();
    let peer_network = Network::start(peer.identity(), NetworkMode::Direct {})
        .await
        .unwrap();
    peer.remember_peer(&owner_network.address()).unwrap();
    let cabal = Arc::new(Mutex::new(cabal));
    let peer = Arc::new(Mutex::new(peer));
    owner_network.add(cabal).unwrap();
    peer_network.add(peer.clone()).unwrap();
    peer_network
        .sync_now(peer.clone(), owner_network.address().id)
        .await
        .unwrap();
    let (mut destination, _) =
        ProjectStore::initialize(directory.path().join("PeerWriting"), "Garden").unwrap();
    project_workspace(&mut destination, &mut peer.lock().unwrap(), None).unwrap();
    let shared = destination.read_document(&published.path).unwrap();
    assert_eq!(shared.text, publication::document(&review.material));
    let page = destination.read_document("Garden.md").unwrap();
    let references = crate::document_bindings::resolve_references(
        &destination,
        std::slice::from_ref(&published.path),
    )
    .unwrap();
    let media = crate::terminal_media::resolve(&destination, &page, &references, 32_768).unwrap();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].bytes, audio_bytes);
    let mut completion = context_attachments::resolve_for_generation_with_budget(
        destination.root(),
        &page.document_id.to_string(),
        &page.text,
        32_768,
        1,
        32,
    )
    .unwrap();
    crate::document_bindings::append_completion_context(
        &destination,
        &published.reference,
        &mut completion,
    )
    .unwrap();
    assert_eq!(completion.media.len(), 1);
    assert_eq!(completion.media[0].bytes, audio_bytes);
    assert!(
        completion
            .context_preamble
            .contains("The corrected excerpt to share.")
    );
    // A file already selected by the current document keeps one native input.
    crate::terminal_media::append_references(&destination, &references, &mut completion.media)
        .unwrap();
    assert_eq!(completion.media.len(), 1);
    assert!(
        !destination
            .root()
            .join(".loom/attachments/objects")
            .join(&private.id)
            .exists()
    );
    assert!(
        crate::document_bindings::context_for_markdown(&destination, &published.reference)
            .unwrap()
            .contains("The corrected excerpt to share.")
    );
    owner_network.shutdown().await.unwrap();
    peer_network.shutdown().await.unwrap();
}

#[test]
fn stale_reviews_cannot_share_changed_context_or_changed_membership() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, mut cabal, _, source) = fixture(directory.path());
    set_document_context_snapshot(store.root(), &source.to_string(), "Reviewed", &[]).unwrap();
    let old = prepare(&store, &cabal, source).unwrap();
    set_document_context_snapshot(store.root(), &source.to_string(), "Private later edit", &[])
        .unwrap();
    assert!(publish(&mut store, &mut cabal, &old.request).is_err());
    assert!(
        cabal
            .local_record::<Intent>(&key(source))
            .unwrap()
            .is_none()
    );
    let latest = prepare(&store, &cabal, source).unwrap();
    let identity = Identity::generate().unwrap();
    let invite = cabal.invite(cabal.identity().public_key().into()).unwrap();
    cabal
        .admit(&invite.token, identity.public_key(), "Carol")
        .unwrap();
    assert!(publish(&mut store, &mut cabal, &latest.request).is_err());
    assert_eq!(cabal.views().unwrap().len(), 1);
    assert!(cabal.assets().unwrap().is_empty());
    let reviewed_again = prepare(&store, &cabal, source).unwrap();
    assert!(reviewed_again.members.contains(&"Carol".into()));
    publish(&mut store, &mut cabal, &reviewed_again.request).unwrap();
}

#[test]
fn lost_reply_reopen_and_retry_keep_one_document_and_later_collaborative_edits() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, mut cabal, mut peer, source) = fixture(directory.path());
    set_document_context_snapshot(store.root(), &source.to_string(), "First context.", &[])
        .unwrap();
    let review = prepare(&store, &cabal, source).unwrap();
    let published = publish(&mut store, &mut cabal, &review.request).unwrap();
    peer.apply(cabal.missing(&peer.hashes().unwrap()).unwrap())
        .unwrap();
    let view = peer.view(review.request.publication).unwrap();
    peer.edit(&Edit {
        document: view.id,
        client: Uuid::new_v4(),
        basis: view.heads,
        text: "First context. A friend's addition.".into(),
    })
    .unwrap();
    cabal
        .apply(peer.missing(&cabal.hashes().unwrap()).unwrap())
        .unwrap();
    set_document_context_snapshot(
        store.root(),
        &source.to_string(),
        "Unpublished private changes",
        &[],
    )
    .unwrap();
    let identity = cabal.identity().clone();
    drop(cabal);
    let mut cabal = Cabal::open(&directory.path().join("owner.db"), identity).unwrap();
    let restored = prepare(&store, &cabal, source).unwrap();
    assert!(restored.started && restored.published);
    assert_eq!(restored.request, review.request);
    let retried = publish(&mut store, &mut cabal, &review.request).unwrap();
    assert_eq!(retried.document_id, published.document_id);
    assert_eq!(cabal.views().unwrap().len(), 2);
    assert_eq!(
        store.read_document(&retried.path).unwrap().text,
        "First context. A friend's addition."
    );
    assert_eq!(
        context_attachments::document_context_snapshot(store.root(), &source.to_string())
            .unwrap()
            .markdown,
        "Unpublished private changes"
    );
}

#[test]
fn an_interrupted_publication_retains_approved_bytes_and_requires_new_member_review() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, mut cabal, _, source) = fixture(directory.path());
    let bytes = wav();
    let audio =
        context_attachments::import_recorded_wav(store.root(), "tone.wav".into(), &bytes).unwrap();
    set_document_context_snapshot(
        store.root(),
        &source.to_string(),
        "Approved context",
        std::slice::from_ref(&audio.id),
    )
    .unwrap();
    let review = prepare(&store, &cabal, source).unwrap();
    let object = store
        .root()
        .join(".loom/attachments/objects")
        .join(&audio.id);
    let retained = object.with_extension("retained-test");
    std::fs::rename(&object, &retained).unwrap();
    assert!(publish(&mut store, &mut cabal, &review.request).is_err());
    assert!(cabal.assets().unwrap().is_empty());
    assert!(
        cabal
            .local_record::<Intent>(&key(source))
            .unwrap()
            .is_some()
    );
    std::fs::rename(&retained, &object).unwrap();
    set_document_context_snapshot(
        store.root(),
        &source.to_string(),
        "Private replacement",
        &[],
    )
    .unwrap();
    let identity = Identity::generate().unwrap();
    let invite = cabal.invite(cabal.identity().public_key().into()).unwrap();
    cabal
        .admit(&invite.token, identity.public_key(), "Carol")
        .unwrap();
    assert!(publish(&mut store, &mut cabal, &review.request).is_err());
    assert!(cabal.assets().unwrap().is_empty());
    let again = prepare(&store, &cabal, source).unwrap();
    assert!(again.started && !again.published);
    assert_eq!(again.request.publication, review.request.publication);
    assert_eq!(again.material, review.material);
    assert_ne!(again.request.fingerprint, review.request.fingerprint);
    let result = publish(&mut store, &mut cabal, &again.request).unwrap();
    assert_eq!(
        store.read_document(&result.path).unwrap().text,
        publication::document(&review.material)
    );
    assert_eq!(cabal.assets().unwrap().len(), 1);
}

#[test]
fn failed_intent_persistence_cannot_publish_files_or_create_a_document() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, mut cabal, _, source) = fixture(directory.path());
    let audio =
        context_attachments::import_recorded_wav(store.root(), "tone.wav".into(), &wav()).unwrap();
    set_document_context_snapshot(store.root(), &source.to_string(), "Approved", &[audio.id])
        .unwrap();
    let review = prepare(&store, &cabal, source).unwrap();
    let database = rusqlite::Connection::open(directory.path().join("owner.db")).unwrap();
    database.execute_batch("CREATE TRIGGER fail_context_intent BEFORE INSERT ON metadata WHEN NEW.key LIKE 'local:context-share:%' BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;").unwrap();
    assert!(publish(&mut store, &mut cabal, &review.request).is_err());
    assert!(
        cabal
            .local_record::<Intent>(&key(source))
            .unwrap()
            .is_none()
    );
    assert!(cabal.assets().unwrap().is_empty());
    assert_eq!(cabal.views().unwrap().len(), 1);
    database
        .execute_batch("DROP TRIGGER fail_context_intent")
        .unwrap();
    publish(&mut store, &mut cabal, &review.request).unwrap();
    assert_eq!(cabal.assets().unwrap().len(), 1);
}

#[test]
fn a_removed_member_cannot_publish_previously_reviewed_context() {
    let directory = tempfile::tempdir().unwrap();
    let (_, mut owner, mut peer, _) = fixture(directory.path());
    peer.apply(owner.missing(&peer.hashes().unwrap()).unwrap())
        .unwrap();
    let (mut store, _) =
        ProjectStore::initialize(directory.path().join("PeerWriting"), "Garden").unwrap();
    project_workspace(&mut store, &mut peer, None).unwrap();
    let source = store.read_document("Garden.md").unwrap().document_id;
    set_document_context_snapshot(store.root(), &source.to_string(), "Still private", &[]).unwrap();
    let review = prepare(&store, &peer, source).unwrap();
    owner.revoke(peer.identity().public_key()).unwrap();
    peer.accept_roster(owner.roster().clone()).unwrap();
    assert!(publish(&mut store, &mut peer, &review.request).is_err());
    assert!(prepare(&store, &peer, source).is_err());
    assert!(peer.local_record::<Intent>(&key(source)).unwrap().is_none());
    assert_eq!(peer.views().unwrap().len(), 1);
}

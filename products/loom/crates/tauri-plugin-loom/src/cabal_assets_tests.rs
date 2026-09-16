use super::super::*;
use super::*;
use crate::context_attachments::{self, import_recorded_wav, resolve_for_generation_with_budget};
use std::io::Cursor;

fn wav(sample: i16) -> Vec<u8> {
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
    writer.write_sample(sample).unwrap();
    writer.finalize().unwrap();
    bytes.into_inner()
}

fn store(path: &Path) -> ProjectStore {
    ProjectStore::initialize(path, "Garden").unwrap().0
}
fn pair(path: &Path) -> (Cabal, Cabal) {
    let identity = Identity::generate().unwrap();
    let other = Identity::generate().unwrap();
    let mut alice =
        Cabal::create(&path.join("alice.db"), identity.clone(), "Friends", "Alice").unwrap();
    let invitation = alice.invite(identity.public_key().into()).unwrap();
    let roster = alice
        .admit(&invitation.token, other.public_key(), "Bob")
        .unwrap();
    let bob = Cabal::import(&path.join("bob.db"), other, roster).unwrap();
    (alice, bob)
}
fn copy(alice: &Cabal, bob: &mut Cabal) {
    bob.accept_roster(alice.roster().clone()).unwrap();
    bob.apply(alice.missing(&bob.hashes().unwrap()).unwrap())
        .unwrap();
    for asset in alice.assets().unwrap() {
        while let Some(offset) = bob.begin_asset(&asset).unwrap() {
            let bytes = alice
                .asset_chunk_for(bob.identity().public_key(), &asset.sha256, offset)
                .unwrap();
            bob.accept_asset_chunk(&asset, offset, &bytes).unwrap();
        }
    }
}

#[test]
fn native_capture_projects_exact_audio_and_inspects_it_only_when_a_prompt_needs_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut left = store(&directory.path().join("Alice"));
    let mut right = store(&directory.path().join("Bob"));
    let (mut alice, mut bob) = pair(directory.path());
    let bytes = wav(42);
    let attachment = import_recorded_wav(left.root(), "garden.wav".into(), &bytes).unwrap();
    left.create_document_if_absent(
        "song.md",
        DocumentContent::Prose(format!("Listen to this.\n{}", attachment.inline_markdown)),
        "fixture",
    )
    .unwrap();
    let source = left
        .list_documents()
        .unwrap()
        .into_iter()
        .find(|item| item.relative_path == "song.md")
        .unwrap();
    capture_document(&left, &mut alice, &source).unwrap();
    copy(&alice, &mut bob);
    project_workspace(&mut left, &mut alice, None).unwrap();
    project_workspace(&mut right, &mut bob, None).unwrap();
    let right_doc = right.read_document("song.md").unwrap();
    let public = shared::inline_root(right.root(), &right_doc.document_id.to_string()).unwrap();
    let manifest = public
        .join(".loom/attachments/manifests")
        .join(format!("{}.json", attachment.id));
    assert!(
        !manifest.exists(),
        "receiving a file does not run its parser"
    );
    let media = crate::terminal_media::resolve(&right, &right_doc, &[], 32_768).unwrap();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].bytes, bytes);
    assert!(manifest.exists());
    assert_eq!(right_doc.text, left.read_document("song.md").unwrap().text);
    assert!(
        !right
            .root()
            .join(".loom/attachments/objects")
            .join(&attachment.id)
            .exists(),
        "received files stay out of the private import registry"
    );
    alice.revoke(bob.identity().public_key()).unwrap();
    bob.accept_roster(alice.roster().clone()).unwrap();
    let copies = recover_documents(&mut right, &bob, None).unwrap();
    let recovered = right.read_document(&copies[0]).unwrap();
    assert_eq!(
        crate::terminal_media::resolve(&right, &recovered, &[], 32_768).unwrap()[0].bytes,
        bytes
    );
}

#[test]
fn peer_references_cannot_read_or_publish_private_media_even_after_an_unrelated_local_edit() {
    let directory = tempfile::tempdir().unwrap();
    let mut local = store(&directory.path().join("Writing"));
    let (mut alice, mut bob) = pair(directory.path());
    let private_bytes = wav(91);
    let private =
        import_recorded_wav(local.root(), "private-recording.wav".into(), &private_bytes).unwrap();
    local
        .create_document_if_absent(
            "page.md",
            DocumentContent::Prose("Our page".into()),
            "fixture",
        )
        .unwrap();
    local
        .create_document_if_absent(
            "Recovery/private.md",
            DocumentContent::Prose(private.inline_markdown.clone()),
            "private fixture",
        )
        .unwrap();
    let source = local
        .list_documents()
        .unwrap()
        .into_iter()
        .find(|item| item.relative_path == "page.md")
        .unwrap();
    capture_document(&local, &mut alice, &source).unwrap();
    copy(&alice, &mut bob);
    let view = bob.views().unwrap().pop().unwrap();
    bob.edit(&Edit {
        document: view.id,
        client: Uuid::new_v4(),
        basis: view.heads,
        text: format!("Peer-supplied reference\n{}", private.inline_markdown),
    })
    .unwrap();
    copy(&bob, &mut alice);
    project_workspace(&mut local, &mut alice, None).unwrap();
    let document = local.read_document("page.md").unwrap();
    assert!(crate::terminal_media::resolve(&local, &document, &[], 32_768).is_err());
    assert!(
        shared::original_for_document(
            local.root(),
            &document.document_id.to_string(),
            &document.text,
            &private.id
        )
        .is_err()
    );
    assert!(alice.assets().unwrap().is_empty());
    let view = alice.view(view.id).unwrap();
    let edit = Edit {
        document: view.id,
        client: Uuid::new_v4(),
        basis: view.heads,
        text: format!("{}\nA local sentence.", view.text),
    };
    let basis = alice.view_at(edit.document, &edit.basis).unwrap();
    publish_added(&local, &mut alice, &basis.text, &edit.text).unwrap();
    alice.edit(&edit).unwrap();
    project_workspace(&mut local, &mut alice, None).unwrap();
    assert!(
        alice.assets().unwrap().is_empty(),
        "editing other words does not authorize the peer's reference"
    );
    let private_doc = local.read_document("Recovery/private.md").unwrap();
    assert_eq!(
        crate::terminal_media::resolve(&local, &private_doc, &[], 32_768).unwrap()[0].bytes,
        private_bytes
    );
    let view = alice.view(view.id).unwrap();
    alice
        .edit_metadata(&MetadataEdit {
            document: view.id,
            client: Uuid::new_v4(),
            basis: view.heads,
            name: view.name,
            deleted: true,
        })
        .unwrap();
    let copies = recover_documents(&mut local, &alice, None).unwrap();
    let recovered = local.read_document(&copies[0]).unwrap();
    assert!(crate::terminal_media::resolve(&local, &recovered, &[], 32_768).is_err());
    assert!(
        shared::original_for_document(
            local.root(),
            &recovered.document_id.to_string(),
            &recovered.text,
            &private.id
        )
        .is_err()
    );
}

#[test]
fn a_public_copy_does_not_reuse_a_private_imports_label_or_processing_receipt() {
    use attachment_native_host::ProvidedAttachment;
    let directory = tempfile::tempdir().unwrap();
    let mut local = store(&directory.path().join("Writing"));
    let (mut alice, mut bob) = pair(directory.path());
    let bytes = b"The garden is full of sunflowers.\n";
    let provided = ProvidedAttachment::read_bounded(
        "private-source-name.txt",
        None,
        &mut Cursor::new(bytes),
        1024,
    )
    .unwrap();
    let private = context_attachments::import_provided(local.root(), provided).unwrap();
    let published = bob.publish_asset("public-notes.txt", bytes).unwrap();
    bob.create_document(
        "notes.md",
        &format!("[Attachment](loom-attachment:{})", published.sha256),
    )
    .unwrap();
    copy(&bob, &mut alice);
    project_workspace(&mut local, &mut alice, None).unwrap();
    let document = local.read_document("notes.md").unwrap();
    let revealed = shared::original_for_document(
        local.root(),
        &document.document_id.to_string(),
        &document.text,
        &published.sha256,
    )
    .unwrap();
    assert!(revealed.starts_with(
        shared::inline_root(local.root(), &document.document_id.to_string()).unwrap()
    ));
    assert_eq!(std::fs::read(revealed).unwrap(), bytes);
    assert!(
        shared::original_for_document(
            local.root(),
            &document.document_id.to_string(),
            "No selected attachment",
            &published.sha256
        )
        .is_err()
    );
    let context = resolve_for_generation_with_budget(
        local.root(),
        &document.document_id.to_string(),
        &document.text,
        32_768,
        1,
        16,
    )
    .unwrap();
    assert!(context.context_preamble.contains("public-notes.txt"));
    assert!(!context.context_preamble.contains("private-source-name"));
    let original = std::fs::read_to_string(
        local
            .root()
            .join(".loom/attachments/manifests")
            .join(format!("{}.json", private.id)),
    )
    .unwrap();
    assert!(original.contains("private-source-name.txt"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn dropped_images_share_as_ordinary_markdown_and_both_protocols_refuse_private_hashes() {
    use base64::Engine as _;
    let directory = tempfile::tempdir().unwrap();
    let mut left = store(&directory.path().join("Alice"));
    let mut right = store(&directory.path().join("Bob"));
    let (mut alice, mut bob) = pair(directory.path());
    let png = include_bytes!("../tests/fixtures/native.png");
    let image = crate::attachments::store_image_asset(
        left.root(),
        "image/png",
        &base64::engine::general_purpose::STANDARD.encode(png),
    )
    .unwrap();
    left.create_document_if_absent(
        "picture.md",
        DocumentContent::Prose(format!("![Our picture]({})", image.markdown_path)),
        "fixture",
    )
    .unwrap();
    let source = left
        .list_documents()
        .unwrap()
        .into_iter()
        .find(|item| item.relative_path == "picture.md")
        .unwrap();
    capture_document(&left, &mut alice, &source).unwrap();
    copy(&alice, &mut bob);
    project_workspace(&mut right, &mut bob, None).unwrap();
    let doc = right.read_document("picture.md").unwrap();
    assert_eq!(
        crate::terminal_media::resolve(&right, &doc, &[], 32_768).unwrap()[0].bytes,
        png
    );
    let project_id = right.manifest().project_id;
    let session_id = CommandId::new();
    let state = PluginState::default();
    {
        let mut session = state.session.lock().unwrap();
        session.phase = SessionPhase::Open;
        session.active_session_id = Some(session_id);
        session.store = Some(right);
    }
    let request = LoomAssetRequest {
        project_id,
        session_id,
        document_id: doc.document_id,
        file_name: format!("{}.png", image.sha256),
    };
    assert_eq!(
        read_authorized_loom_asset(&state, &request).unwrap().bytes,
        png
    );
    // A different imported image exists privately on Bob, with no publication.
    let mut encoded_png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([47, 18, 19, 255]),
    ))
    .write_to(&mut encoded_png, image::ImageFormat::Png)
    .unwrap();
    let private_png = encoded_png.into_inner();
    let private = {
        let session = state.session.lock().unwrap();
        crate::attachments::store_image_asset(
            session.store.as_ref().unwrap().root(),
            "image/png",
            &base64::engine::general_purpose::STANDARD.encode(&private_png),
        )
        .unwrap()
    };
    alice
        .create_document(
            "spy.md",
            &format!(
                "![Private]({})\n![Private](loom-attachment:{}/{})",
                private.markdown_path, private.sha256, private.sha256
            ),
        )
        .unwrap();
    copy(&alice, &mut bob);
    let spy = {
        let mut session = state.session.lock().unwrap();
        let store = session.store.as_mut().unwrap();
        project_workspace(store, &mut bob, None).unwrap();
        let spy = store.read_document("spy.md").unwrap();
        assert!(crate::terminal_media::resolve(store, &spy, &[], 32_768).is_err());
        spy
    };
    let private_request = LoomAssetRequest {
        document_id: spy.document_id,
        file_name: format!("{}.png", private.sha256),
        ..request.clone()
    };
    assert!(read_authorized_loom_asset(&state, &private_request).is_err());
    assert!(
        read_authorized_context_media(
            &state,
            &LoomContextMediaRequest {
                project_id,
                session_id,
                document_id: spy.document_id,
                attachment_id: private.sha256.clone(),
                media_sha256: private.sha256
            }
        )
        .is_err()
    );
    // Even a published object needs the requested document to select it.
    assert!(
        read_authorized_loom_asset(
            &state,
            &LoomAssetRequest {
                document_id: spy.document_id,
                ..request
            }
        )
        .is_err()
    );
}

#[test]
fn receiving_an_uninspectable_file_does_not_stall_unrelated_manuscripts() {
    let directory = tempfile::tempdir().unwrap();
    let mut local = store(&directory.path().join("Writing"));
    let (mut alice, mut bob) = pair(directory.path());
    let executable = bob
        .publish_asset("program.exe", b"MZ\x90\0\x03\0\0\0\x04\0\0\0\xff\xff\0\0")
        .unwrap();
    bob.create_document(
        "file.md",
        &format!("[File](loom-attachment:{})", executable.sha256),
    )
    .unwrap();
    bob.create_document("garden.md", "Ordinary writing still arrives.")
        .unwrap();
    copy(&bob, &mut alice);
    project_workspace(&mut local, &mut alice, None).unwrap();
    assert_eq!(
        local.read_document("garden.md").unwrap().text,
        "Ordinary writing still arrives."
    );
    let doc = local.read_document("file.md").unwrap();
    let root = shared::inline_root(local.root(), &doc.document_id.to_string()).unwrap();
    assert!(
        !root
            .join(".loom/attachments/manifests")
            .join(format!("{}.json", executable.sha256))
            .exists()
    );
}

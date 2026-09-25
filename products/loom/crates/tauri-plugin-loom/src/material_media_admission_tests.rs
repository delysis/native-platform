use super::*;
use loom_document::DocumentContent;
use std::io::Cursor;

fn project() -> (tempfile::TempDir, ProjectStore) {
    let directory = tempfile::tempdir().expect("directory");
    let (store, _) = ProjectStore::initialize(directory.path().join("Writing"), "Writing").expect("project");
    (directory, store)
}

fn wav(sample: i16) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut bytes, hound::WavSpec {
        channels: 1, sample_rate: 16_000, bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }).expect("WAV");
    writer.write_sample(sample).expect("sample");
    writer.finalize().expect("finalize");
    bytes.into_inner()
}

#[test]
fn direct_attachment_media_is_bound_to_the_admitted_revision() {
    let (_directory, store) = project();
    let bytes = wav(42);
    let prepared = crate::context_attachments::import_recorded_wav(store.root(), "voice.wav".into(), &bytes).expect("retain");
    let entry = materials::bind_attachment(&store, &prepared.id, Some("Voice")).expect("bind");
    let value = resolve(&store, &entry.id).expect("resolve");
    let media = native_media(&store, [&value]).expect("direct media");
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].bytes, bytes);
    let mut stale = value.clone();
    let Value::Material { material } = &mut stale else { panic!("material"); };
    material.source_revision = "not-the-admitted-source-revision".into();
    assert!(native_media(&store, [&stale]).is_err());
}

#[test]
fn an_exact_document_alias_does_not_duplicate_media_and_never_recurses() {
    let (_directory, mut store) = project();
    let bytes = wav(11);
    let prepared = crate::context_attachments::import_recorded_wav(store.root(), "voice.wav".into(), &bytes).expect("retain");
    store.create_document_if_absent("Voice.md", DocumentContent::Prose(format!("{}\n@Missing\n[file](file:///private/secret.wav)", prepared.inline_markdown)), "fixture").expect("document");
    let first = resolve(&store, "Voice").expect("alias");
    let second = resolve(&store, "Voice.md").expect("exact path");
    let media = native_media(&store, [&first, &second]).expect("one admitted source");
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].bytes, bytes);
}

#[test]
fn a_frozen_document_body_cannot_be_substituted_under_its_old_blob_identity() {
    let (_directory, mut store) = project();
    store.create_document_if_absent("Voice.md", DocumentContent::Prose("Original source".into()), "fixture").expect("document");
    let mut substituted = resolve(&store, "Voice.md").expect("reference");
    let Value::Documents { documents } = &mut substituted else { panic!("documents"); };
    documents[0].text = "Substituted source".into();
    assert!(native_media(&store, [&substituted]).is_err());
}

#[test]
fn conflicting_document_revisions_cannot_be_combined_as_one_source() {
    let (_directory, mut store) = project();
    store.create_document_if_absent("Voice.md", DocumentContent::Prose("First source".into()), "fixture").expect("document");
    let first = resolve(&store, "Voice.md").expect("first");
    store.save_document("Voice.md", DocumentContent::Prose("Second source".into()), "fixture edit").expect("save");
    let second = resolve(&store, "Voice.md").expect("second");
    assert!(native_media(&store, [&first, &second]).is_err());
}

#[test]
fn literal_text_and_empty_retrieval_have_no_media_authority() {
    let (_directory, store) = project();
    let literal = Value::Text("@Missing ![image](file:///private/photo.png)".into());
    let selected = Value::Evidence { evidence: Vec::new(), retrieval: None };
    assert!(native_media(&store, [&literal, &selected]).expect("no media reads").is_empty());
}

#[test]
fn media_count_is_enforced_while_references_are_consumed() {
    let (_directory, mut store) = project();
    let mut values = Vec::new();
    for index in 0..33 {
        let name = format!("voice-{index}.wav");
        let bytes = wav(index);
        let prepared = crate::context_attachments::import_recorded_wav(store.root(), name, &bytes).expect("retain");
        let path = format!("Voice-{index}.md");
        store.create_document_if_absent(&path, DocumentContent::Prose(prepared.inline_markdown), "fixture").expect("document");
        values.push(resolve(&store, &path).expect("reference"));
    }
    // The iterator must not be advanced after the 33rd distinct payload fails.
    let values = values.iter().chain(std::iter::once_with(|| panic!("read after admission failure")));
    assert_eq!(native_media(&store, values).expect_err("bounded media").code, "terminal_media_limit");
}

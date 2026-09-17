use super::*;
use crate::{
    document_bindings::snapshot_folder,
    material_context::{self, Value},
};
use loom_document::DocumentContent;

fn project() -> (tempfile::TempDir, ProjectStore) {
    let root = tempfile::tempdir().unwrap();
    let (store, _) = ProjectStore::initialize(root.path().join("Writing"), "Writing").unwrap();
    (root, store)
}

fn document(store: &mut ProjectStore, path: &str, text: &str) {
    store
        .create_document_if_absent(path, DocumentContent::Prose(text.into()), "writing")
        .unwrap();
}

fn search_folder(
    store: &ProjectStore,
    folder: &FolderSnapshot,
    query: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<MaterialSearch> {
    super::search_folder(
        store,
        folder,
        query,
        &FolderScanBudget::default(),
        cancelled,
    )
}

#[test]
fn folder_search_retains_exact_revision_slices_without_expanding_source_references() {
    let (_root, mut store) = project();
    let exact = "Rain on İstanbul — 雨\r\n@Missing =find(@Other/, \"execute\") rain\r\n";
    document(
        &mut store,
        "Notes/a.md",
        &format!("{}\n{exact}", "unrelated ".repeat(8000)),
    );
    document(&mut store, "Notes/z.md", "Rain elsewhere\n");
    document(
        &mut store,
        "Notes-other/secret.md",
        "Rain outside this scope\n",
    );
    std::fs::write(
        store.root().join("Notes/unregistered.md"),
        "Rain unregistered\n",
    )
    .unwrap();
    let value = material_context::resolve(&store, "Notes/").unwrap();
    assert!(matches!(&value, Value::Folder { folder } if folder.members.len() == 2));
    assert!(
        material_context::exact(&value)
            .unwrap_err()
            .message
            .contains("not an exact text")
    );
    let Value::Evidence {
        evidence,
        retrieval: Some(retrieval),
    } = material_context::search(&store, &value, "rain").unwrap()
    else {
        panic!("evidence")
    };
    assert_eq!(evidence.len(), 3);
    assert_eq!(
        retrieval
            .folder
            .as_ref()
            .unwrap()
            .scanned_document_ids
            .len(),
        2
    );
    assert_eq!(retrieval.material.kind, MaterialKind::Folder);
    assert!(super::super::list(&store).unwrap().is_empty());
    for hit in &evidence {
        let path = hit.locator["path"].as_str().unwrap();
        let loaded = store.read_document(path).unwrap();
        let start = usize::try_from(hit.locator["start_byte"].as_u64().unwrap()).unwrap();
        let end = usize::try_from(hit.locator["end_byte"].as_u64().unwrap()).unwrap();
        assert_eq!(hit.text, loaded.text[start..end]);
        assert_eq!(hit.text_sha256, digest(hit.text.as_bytes()));
        assert_eq!(
            hit.locator["revision_id"],
            serde_json::to_value(loaded.revision_id).unwrap()
        );
        let retained =
            super::super::read_evidence(&store, &retrieval.material.id, &hit.id).unwrap();
        assert_eq!(retained.text, hit.text);
    }
    assert_eq!(
        material_context::evidence_artifact_ids(&evidence)
            .unwrap()
            .len(),
        2
    );
    let (bounded, omitted) =
        material_context::consult_with_budget(&store, &value, "rain", 1000).unwrap();
    assert!(material_context::exact(&bounded).unwrap().len() <= 1000);
    assert!(omitted.is_empty());
    assert!(
        material_context::native_media(&store, [&value])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn admitted_membership_is_frozen_and_changed_members_fail_without_rebinding() {
    let (_root, mut store) = project();
    document(&mut store, "Notes/a.md", "rain one\n");
    let snapshot = snapshot_folder(&store, "Notes/").unwrap();
    document(&mut store, "Notes/b.md", "rain two\n");
    let first = search_folder(&store, &snapshot, "rain", &|| false).unwrap();
    assert_eq!(first.hits.len(), 1);
    assert_eq!(first.folder.as_ref().unwrap().snapshot.members.len(), 1);
    let newer = snapshot_folder(&store, "Notes/").unwrap();
    assert_ne!(newer.source_revision, snapshot.source_revision);
    store
        .save_document(
            "Notes/a.md",
            DocumentContent::Prose("rain changed\n".into()),
            "edited",
        )
        .unwrap();
    assert!(
        search_folder(&store, &snapshot, "rain", &|| false)
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    let retained =
        super::super::read_evidence(&store, &first.material.id, &first.hits[0].id).unwrap();
    assert_eq!(retained.text, "rain one\n");
    let mut current = store.open_document_file("Notes/a.md").unwrap();
    store.rename_document(&mut current, "Renamed").unwrap();
    assert!(search_folder(&store, &snapshot, "rain", &|| false).is_err());
    assert_eq!(
        super::super::read_evidence(&store, &first.material.id, &first.hits[0].id)
            .unwrap()
            .text,
        "rain one\n"
    );
}

#[test]
fn bounded_scan_reports_large_and_unscanned_members_and_cancellation() {
    let (_root, mut store) = project();
    document(
        &mut store,
        "Notes/huge.md",
        &"r".repeat(usize::try_from(MAX_MEMBER_BYTES).unwrap() + 1),
    );
    document(&mut store, "Notes/small.md", "Rain\n");
    let snapshot = snapshot_folder(&store, "Notes/").unwrap();
    let result = search_folder(&store, &snapshot, "absent", &|| false).unwrap();
    assert!(result.hits.is_empty());
    assert!(!result.complete);
    let coverage = result.folder.unwrap();
    assert_eq!(coverage.omitted.len(), 1);
    assert_eq!(coverage.omitted[0].reason, "member_byte_limit");
    assert_eq!(coverage.scanned_document_ids.len(), 1);
    let checks = std::cell::Cell::new(0);
    assert!(
        search_folder(&store, &snapshot, "rain", &|| {
            checks.set(checks.get() + 1);
            checks.get() > 1
        })
        .unwrap_err()
        .to_string()
        .contains("cancelled")
    );
    assert_eq!(
        std::fs::metadata(store.root().join("Notes/huge.md"))
            .unwrap()
            .len(),
        MAX_MEMBER_BYTES + 1
    );
}

#[test]
fn result_limit_and_zero_matches_retain_distinct_coverage() {
    let (_root, mut store) = project();
    document(&mut store, "Notes/a.md", &"rain\n".repeat(20));
    document(&mut store, "Notes/b.md", "rain later\n");
    let snapshot = snapshot_folder(&store, "Notes/").unwrap();
    let result = search_folder(&store, &snapshot, "rain", &|| false).unwrap();
    assert_eq!(result.hits.len(), MAX_HITS as usize);
    assert!(!result.complete);
    assert!(result.folder.as_ref().unwrap().result_limit_reached);
    assert_eq!(result.folder.unwrap().omitted[0].reason, "result_limit");
    let empty = search_folder(&store, &snapshot, "absent", &|| false).unwrap();
    assert!(empty.hits.is_empty());
    assert!(empty.complete);
    assert!(empty.folder.unwrap().omitted.is_empty());
}

#[test]
fn hidden_and_retained_outputs_are_not_ordinary_folder_context() {
    let (_root, mut store) = project();
    document(
        &mut store,
        "Notes/Runs/writing.md",
        "rain ordinary writing\n",
    );
    document(
        &mut store,
        "Notes/.config.md",
        "rain hidden configuration\n",
    );
    let provenance = store.store_provenance_blob(b"fixture").unwrap();
    store
        .create_generated_document_if_absent(
            "Notes/generated.md",
            DocumentContent::Prose("rain generated\n".into()),
            "retained experiment",
            provenance,
        )
        .unwrap();
    let snapshot = snapshot_folder(&store, "Notes/").unwrap();
    assert_eq!(snapshot.excluded, 2);
    assert_eq!(snapshot.members.len(), 1);
    assert_eq!(snapshot.members[0].path, "Notes/Runs/writing.md");
    let result = search_folder(&store, &snapshot, "rain", &|| false).unwrap();
    assert_eq!(result.hits.len(), 1);
}

#[test]
fn folder_snapshot_does_not_accept_external_paths_or_tampered_membership() {
    let (_root, mut store) = project();
    document(&mut store, "Notes/a.md", "rain\n");
    for name in ["/Notes/", "../Notes/", "Notes//", "Notes/../", "Notes\\/"] {
        assert!(snapshot_folder(&store, name).is_err(), "accepted {name}");
    }
    let mut snapshot = snapshot_folder(&store, "Notes/").unwrap();
    snapshot.members[0].path = "Outside/a.md".into();
    assert!(search_folder(&store, &snapshot, "rain", &|| false).is_err());
}

#[test]
fn unchanged_selected_text_still_has_new_plan_identity_when_membership_changes() {
    let (_root, mut store) = project();
    document(&mut store, "Notes/a.md", "rain\n");
    let before = material_context::markdown_plan(&store, "Consult @Notes/", "absent").unwrap();
    document(&mut store, "Notes/b.md", "moon\n");
    let after = material_context::markdown_plan(&store, "Consult @Notes/", "absent").unwrap();
    assert_eq!(
        before.text, after.text,
        "both consultations honestly have no matches"
    );
    let identity = |plan: &material_context::ContextPlan| {
        loom_types::BlobId::digest(&serde_json::to_vec(plan).unwrap())
    };
    assert_ne!(
        identity(&before),
        identity(&after),
        "unselected members still belong to frozen source identity"
    );
    let Value::Evidence {
        retrieval: Some(before),
        ..
    } = before.bindings["Notes/"].unscoped()
    else {
        panic!("retained search")
    };
    let Value::Evidence {
        retrieval: Some(after),
        ..
    } = after.bindings["Notes/"].unscoped()
    else {
        panic!("retained search")
    };
    assert_ne!(before.source_revision, after.source_revision);
    assert_eq!(before.folder.as_ref().unwrap().snapshot.members.len(), 1);
    assert_eq!(after.folder.as_ref().unwrap().snapshot.members.len(), 2);
}

#[test]
fn unchanged_registry_revision_never_authorizes_external_edits_or_symlinks() {
    let (_root, mut store) = project();
    document(&mut store, "Notes/a.md", "rain original\n");
    let snapshot = snapshot_folder(&store, "Notes/").unwrap();
    let path = store.root().join("Notes/a.md");
    std::fs::write(&path, "rain external\n").unwrap();
    assert!(search_folder(&store, &snapshot, "rain", &|| false).is_err());
    let external = store.root().join("outside.txt");
    std::fs::write(&external, "rain original\n").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&external, &path).unwrap();
    assert!(search_folder(&store, &snapshot, "rain", &|| false).is_err());
}

#[test]
fn total_scan_byte_limit_does_not_claim_the_unread_tail_has_no_matches() {
    let (_root, mut store) = project();
    let full_member = "x".repeat(usize::try_from(MAX_MEMBER_BYTES).unwrap());
    for index in 0..8 {
        document(&mut store, &format!("Notes/{index}.md"), &full_member);
    }
    document(&mut store, "Notes/9.md", "rain only in unread tail\n");
    let snapshot = snapshot_folder(&store, "Notes/").unwrap();
    let result = search_folder(&store, &snapshot, "rain", &|| false).unwrap();
    assert!(result.hits.is_empty());
    assert!(!result.complete);
    let coverage = result.folder.unwrap();
    assert_eq!(coverage.scanned_document_ids.len(), 8);
    assert_eq!(coverage.omitted.len(), 1);
    assert_eq!(coverage.omitted[0].reason, "operation_scan_byte_limit");
}

#[test]
fn shared_operation_budget_counts_repeated_reads_with_distinct_exact_queries() {
    let (_root, mut store) = project();
    let text = "rain first\nmoon second\n";
    document(&mut store, "Notes/a.md", text);
    let snapshot = snapshot_folder(&store, "Notes/").unwrap();
    let budget = FolderScanBudget {
        remaining: std::cell::Cell::new(2 * text.len() as u64),
    };
    let rain = super::search_folder(&store, &snapshot, "rain", &budget, &|| false).unwrap();
    let moon = super::search_folder(&store, &snapshot, "moon", &budget, &|| false).unwrap();
    assert_eq!(rain.query, "rain");
    assert_eq!(rain.hits[0].text, "rain first\n");
    assert_eq!(moon.query, "moon");
    assert_eq!(moon.hits[0].text, "moon second\n");
    assert_eq!(rain.source_revision, moon.source_revision);
    assert_eq!(budget.remaining.get(), 0);
    let exhausted = super::search_folder(&store, &snapshot, "rain", &budget, &|| false).unwrap();
    assert!(!exhausted.complete);
    assert!(exhausted.hits.is_empty());
    let coverage = exhausted.folder.unwrap();
    assert_eq!(coverage.snapshot.members.len(), 1);
    assert!(coverage.scanned_document_ids.is_empty());
    assert_eq!(coverage.omitted[0].reason, "operation_scan_byte_limit");
    assert_eq!(
        coverage.omitted[0].document_id,
        snapshot.members[0].document_id
    );
}

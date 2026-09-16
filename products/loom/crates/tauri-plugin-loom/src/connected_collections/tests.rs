use super::*;
use std::fs;
use std::path::PathBuf;

const PRINCIPAL: &str = "writer@example.test";
fn fixture() -> (
    tempfile::TempDir,
    ProjectStore,
    CollectionDefinition,
    PathBuf,
) {
    let temporary = tempfile::tempdir().expect("fixture");
    let root = fs::canonicalize(temporary.path()).expect("canonical root");
    let store = ProjectStore::initialize(root.join("Writing"), "Writing")
        .expect("project")
        .0;
    let definition = CollectionDefinition {
        id: format!("material-{}", digest(b"collection")),
        name: "Research".into(),
        pinned: false,
        workspace_path: None,
        scope: CollectionScope::DriveFolder {
            id: "deliberately-selected-folder".into(),
        },
    };
    let private = root.join("private-grants");
    save_grant(&store, &private, &definition, PRINCIPAL).expect("explicit grant");
    (temporary, store, definition, private)
}
fn remote(id: &str) -> RemoteMember {
    RemoteMember {
        remote_id: id.into(),
        name: format!("{id}.txt"),
        source_uri: format!("https://drive.google.com/file/d/{id}"),
        mime_type: "text/plain".into(),
        listed_modified_time: Some("2026-09-16T00:00:00Z".into()),
    }
}
fn version(
    store: &ProjectStore,
    identity: &CollectionIdentity,
    remote: &RemoteMember,
    bytes: &[u8],
) -> OccurrenceVersion {
    let source = store.root().join(&remote.name);
    fs::write(&source, bytes).expect("original");
    let attachment =
        context_attachments::import_path(store.root(), &source).expect("retain source");
    let receipt = context_attachments::record_import_origin(store.root(), &serde_json::json!({
        "schema":"loom.connected-import.v1", "service":"drive", "account_email":PRINCIPAL,
        "source_uri":remote.source_uri, "remote_id":remote.remote_id, "listed_modified_time":remote.listed_modified_time,
        "source_sha256":attachment.id, "source_bytes":attachment.byte_count, "network_used":true,
    })).expect("origin receipt");
    OccurrenceVersion::new(identity, remote, attachment.id, receipt).expect("occurrence")
}
fn job() -> String {
    crate::CommandId::new().to_string()
}

#[test]
fn identical_bytes_keep_distinct_occurrences_and_immutable_versions_across_resume() {
    let (_temporary, store, definition, private) = fixture();
    let identity = identity(&definition, PRINCIPAL).expect("identity");
    let a = remote("a");
    let b = remote("b");
    let version_a = version(&store, &identity, &a, b"same bytes");
    let version_b = version(&store, &identity, &b, b"same bytes");
    assert_eq!(version_a.attachment_id, version_b.attachment_id);
    assert_ne!(version_a.occurrence_id, version_b.occurrence_id);
    assert_ne!(version_a.origin_receipt_id, version_b.origin_receipt_id);
    let initial = begin_refresh(&store, &identity, &job(), false).expect("begin");
    let page = set_page(
        &store,
        &private,
        &initial,
        vec![a, b],
        Some("private-page-two".into()),
    )
    .expect("list page");
    let one = publish_member(&store, &page, version_a, 10).expect("publish one");
    assert!(matches!(
        publish_member(&store, &page, version_b.clone(), 10),
        Err(CollectionError::Conflict)
    ));
    let stopped = finish_refresh(&store, &one, RefreshPhase::Interrupted).expect("stop");
    assert!(publish_member(&store, &stopped, version_b.clone(), 10).is_err());
    let resumed = begin_refresh(&store, &identity, &job(), true).expect("resume");
    assert_ne!(resumed.checkpoint.job_id, stopped.checkpoint.job_id);
    assert_eq!(resumed.checkpoint.refresh_id, stopped.checkpoint.refresh_id);
    assert_eq!(
        load_continuation(&store, &private, &resumed).expect("private continuation"),
        Some("private-page-two".into())
    );
    let two = publish_member(&store, &resumed, version_b, 10).expect("publish resumed item");
    assert_eq!(
        read_snapshot(&store, &definition.id, &one.snapshot_id)
            .expect("old snapshot")
            .members
            .len(),
        1
    );
    assert_eq!(
        read_snapshot(&store, &definition.id, &two.snapshot_id)
            .expect("current snapshot")
            .members
            .len(),
        2
    );
    let page_done = finish_page(&store, &two).expect("finish page");
    assert!(finish_refresh(&store, &page_done, RefreshPhase::Complete).is_err());
    let last = set_page(&store, &private, &page_done, Vec::new(), None).expect("final page");
    let last = finish_page(&store, &last).expect("account final page");
    let done = finish_refresh(&store, &last, RefreshPhase::Complete).expect("complete");
    assert!(
        read_snapshot(&store, &definition.id, &done.snapshot_id)
            .expect("completed snapshot")
            .completed_listing_refresh
            .is_some()
    );
    revoke_grant(&store, &private, &definition.id).expect("disconnect");
    assert!(require_grant(&store, &private, &definition).is_err());
    assert_eq!(
        read_snapshot(&store, &definition.id, &done.snapshot_id)
            .expect("offline sources")
            .members
            .len(),
        2
    );
    let head_bytes = fs::read(
        store
            .root()
            .join(".loom/collections")
            .join(&definition.id)
            .join("head.json"),
    )
    .expect("public head");
    assert!(
        !String::from_utf8(head_bytes)
            .expect("JSON")
            .contains("private-page-two")
    );
}

#[test]
fn failed_download_remains_listed_and_old_content_stays_readable() {
    let (_temporary, store, definition, private) = fixture();
    let identity = identity(&definition, PRINCIPAL).expect("identity");
    let item = remote("changed");
    let first = begin_refresh(&store, &identity, &job(), false).expect("begin");
    let first = set_page(&store, &private, &first, vec![item.clone()], None).expect("page");
    let old = publish_member(
        &store,
        &first,
        version(&store, &identity, &item, b"old exact bytes"),
        15,
    )
    .expect("publish");
    let old = finish_page(&store, &old).expect("page done");
    let old = finish_refresh(&store, &old, RefreshPhase::Complete).expect("done");
    let fresh = begin_refresh(&store, &identity, &job(), false).expect("fresh");
    let fresh =
        set_page(&store, &private, &fresh, vec![item.clone()], None).expect("observed again");
    let failed =
        fail_member(&store, &fresh, &item.remote_id, "Download failed", 0).expect("record failure");
    let failed = finish_page(&store, &failed).expect("accounted");
    let failed = finish_refresh(&store, &failed, RefreshPhase::Complete)
        .expect("listing complete, failure retained");
    assert_eq!(failed.checkpoint.failures.len(), 1);
    let snapshot =
        read_snapshot(&store, &definition.id, &failed.snapshot_id).expect("retained source");
    assert_eq!(
        snapshot.members[0].observed_in_refresh,
        failed.checkpoint.refresh_id
    );
    assert_eq!(
        snapshot.members[0].attachment_id,
        read_snapshot(&store, &definition.id, &old.snapshot_id)
            .expect("old snapshot")
            .members[0]
            .attachment_id
    );
}

#[test]
fn copied_workspace_and_changed_scope_cannot_reuse_a_local_grant() {
    let (_temporary, store, definition, private) = fixture();
    assert_eq!(
        require_grant(&store, &private, &definition).expect("selected account"),
        PRINCIPAL
    );
    let mut changed = definition.clone();
    changed.scope = CollectionScope::DriveFolder {
        id: "another-scope".into(),
    };
    assert!(matches!(
        require_grant(&store, &private, &changed),
        Err(CollectionError::NeedsAuthorization)
    ));
    let mut renamed = definition.clone();
    renamed.name = "Renamed".into();
    assert!(require_grant(&store, &private, &renamed).is_ok());
    let moved = store.root().with_file_name("Moved");
    let old_root = store.root().to_owned();
    drop(store);
    fs::rename(&old_root, &moved).expect("same project at different canonical root");
    let store = ProjectStore::open(&moved).expect("open moved workspace");
    assert!(matches!(
        require_grant(&store, &private, &definition),
        Err(CollectionError::NeedsAuthorization)
    ));
}

#[test]
fn missing_origin_or_unpublished_source_never_advances_membership() {
    let (_temporary, store, definition, private) = fixture();
    let identity = identity(&definition, PRINCIPAL).expect("identity");
    let item = remote("missing");
    let head = begin_refresh(&store, &identity, &job(), false).expect("begin");
    let head = set_page(&store, &private, &head, vec![item.clone()], None).expect("page");
    let forged = OccurrenceVersion::new(
        &identity,
        &item,
        digest(b"missing bytes"),
        format!("source-{}", digest(b"missing origin")),
    )
    .expect("structurally valid ID");
    assert!(publish_member(&store, &head, forged, 10).is_err());
    assert_eq!(
        read_head(&store, &definition.id).expect("head"),
        Some(head.clone())
    );
    let mut leaf = read_snapshot(&store, &definition.id, &head.snapshot_id).expect("snapshot");
    leaf.completed_listing_refresh = Some(head.checkpoint.refresh_id.clone());
    let orphan = install_snapshot(&store, &leaf).expect("simulate crash after leaf, before head");
    assert_ne!(orphan, head.snapshot_id);
    assert_eq!(
        read_head(&store, &definition.id).expect("same committed head"),
        Some(head)
    );
}

#[test]
fn local_context_freezes_membership_and_retains_evidence_after_disconnect_and_removal() {
    use crate::{material_context, materials, workspace_template};
    let (_temporary, mut store, definition, private) = fixture();
    workspace_template::upsert_collection(&mut store, None, &definition).unwrap();
    let identity = identity(&definition, PRINCIPAL).unwrap();
    let member = remote("paper");
    let publish = |store: &ProjectStore, bytes: &[u8]| {
        let head = begin_refresh(store, &identity, &job(), false).unwrap();
        let head = set_page(store, &private, &head, vec![member.clone()], None).unwrap();
        let head = publish_member(
            store,
            &head,
            version(store, &identity, &member, bytes),
            bytes.len() as u64,
        )
        .unwrap();
        let head = finish_page(store, &head).unwrap();
        finish_refresh(store, &head, RefreshPhase::Complete).unwrap()
    };
    let first = publish(&store, b"Moon orchids bloom in silver light.\r\n");
    let context_value = material_context::resolve(&store, "Research").unwrap();
    assert!(material_context::exact(&context_value).is_err());
    let frozen =
        materials::collections::freeze(&store, materials::resolve(&store, "Research").unwrap())
            .unwrap();
    let second = publish(&store, b"Moon gardens bloom in golden light.\r\n");
    assert_ne!(first.snapshot_id, second.snapshot_id);
    revoke_grant(&store, &private, &definition.id).unwrap();
    let evidence_value = material_context::search(&store, &context_value, "orchids").unwrap();
    assert!(
        material_context::exact(&evidence_value)
            .unwrap()
            .contains("silver")
    );
    assert!(
        material_context::native_media(&store, [&context_value])
            .unwrap()
            .is_empty()
    );
    let budget = materials::FolderScanBudget::default();
    let old =
        materials::collections::search(&store, &frozen, "orchids", &budget, &|| false).unwrap();
    assert_eq!(old.source_revision, first.snapshot_id);
    assert_eq!(old.hits.len(), 1);
    assert!(old.hits[0].text.contains("silver"));
    assert_eq!(
        old.hits[0].locator["collection_snapshot"],
        first.snapshot_id
    );
    let current = materials::search(&store, &definition.id, "gardens").unwrap();
    assert_eq!(current.source_revision, second.snapshot_id);
    assert_eq!(current.hits.len(), 1);
    assert!(current.hits[0].text.contains("golden"));
    budget.charge(budget.remaining()).unwrap();
    let omitted =
        materials::collections::search(&store, &frozen, "orchids", &budget, &|| false).unwrap();
    assert!(omitted.hits.is_empty());
    assert!(!omitted.complete);
    assert!(
        omitted
            .warnings
            .iter()
            .any(|warning| warning.contains("omitted"))
    );
    assert!(materials::collections::search(&store, &frozen, "orchids", &budget, &|| true).is_err());
    let revision = workspace_template::collection_definitions(&mut store)
        .unwrap()
        .revision_id;
    workspace_template::remove_collection(&mut store, revision, &definition.id).unwrap();
    assert!(materials::resolve(&store, "Research").is_err());
    let retained = materials::read_evidence(&store, &definition.id, &old.hits[0].id).unwrap();
    assert_eq!(retained.text, old.hits[0].text);
    assert_eq!(retained.text_sha256, old.hits[0].text_sha256);
}

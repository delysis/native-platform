use loom_text_session::worker::{Client, Command, MAX_PENDING, Response, TextWrite, WriteMode};
use loom_types::{DocumentKind, ProjectId};
use std::time::{Duration, Instant};

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    loom_store::ProjectStore::initialize(root.path(), "Worker fixture").unwrap();
    root
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "Matches the fallible view-admission callback"
)]
fn admit(_: &str, _: DocumentKind) -> Result<(), String> {
    Ok(())
}
fn reply(client: &mut Client) -> loom_text_session::worker::Reply {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(reply) = client.poll().unwrap() {
            return reply;
        }
        assert!(Instant::now() < deadline, "worker timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn capture(
    client: &mut Client,
    project: ProjectId,
    source: loom_text_session::completion::SourceIdentity,
    cursor_byte: u64,
) -> loom_text_session::worker::Reply {
    client
        .submit(
            project,
            Command::CaptureCompletion {
                source,
                cursor_byte,
            },
        )
        .unwrap();
    reply(client)
}

#[test]
fn completion_capture_follows_acknowledged_revisions_and_checks_current_visible_bytes() {
    use loom_text_session::SessionError;
    use loom_text_session::completion::CaptureError;
    let root = fixture();
    let (mut client, snapshot) = Client::open(root.path(), admit, || {}).unwrap();
    let project = snapshot.project.project_id;
    let before = snapshot.manuscript.source_identity;
    let text = "α\r\n**café** 🌍";
    client
        .submit(
            project,
            Command::Write {
                mode: WriteMode::Checkpoint,
                documents: vec![TextWrite {
                    document_id: before.document_id,
                    baseline: snapshot.manuscript.baseline,
                    text: text.into(),
                }],
            },
        )
        .unwrap();
    let Response::Written(writes) = reply(&mut client).result.unwrap() else {
        panic!("checkpoint reply")
    };
    let current = writes[0].source_identity.unwrap();
    assert_ne!(current.revision_id, before.revision_id);
    assert_eq!(
        current.visible_blob_id,
        loom_types::BlobId::digest(text.as_bytes())
    );
    assert!(matches!(
        capture(&mut client, project, before, 0).result,
        Err(SessionError::Conflict)
    ));
    let Response::CompletionSource(captured) =
        capture(&mut client, project, current, 4).result.unwrap()
    else {
        panic!("completion reply")
    };
    assert_eq!(captured.prefix(), "α\r\n");
    assert_eq!(captured.document().text, text);
    assert_eq!(captured.identity(), current);
    assert!(matches!(
        capture(&mut client, project, current, 1).result,
        Err(SessionError::Completion(CaptureError::CursorBoundary))
    ));
    assert!(matches!(
        capture(&mut client, ProjectId::new(), current, 0).result,
        Err(SessionError::ProjectMismatch)
    ));
    let path = root
        .path()
        .join(&snapshot.project.documents[0].relative_path);
    std::fs::write(&path, "external edit").unwrap();
    assert!(capture(&mut client, project, current, 0).result.is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "external edit");
}

#[test]
fn a_draft_cannot_be_submitted_as_a_checkpointed_completion_source() {
    let root = fixture();
    let (mut client, snapshot) = Client::open(root.path(), admit, || {}).unwrap();
    let project = snapshot.project.project_id;
    let before = snapshot.manuscript.source_identity;
    let write = TextWrite {
        document_id: before.document_id,
        baseline: snapshot.manuscript.baseline,
        text: "private draft α\r\n".into(),
    };
    for mode in [WriteMode::Journal, WriteMode::Checkpoint] {
        client
            .submit(
                project,
                Command::Write {
                    mode,
                    documents: vec![write.clone()],
                },
            )
            .unwrap();
        let Response::Written(writes) = reply(&mut client).result.unwrap() else {
            panic!("write reply")
        };
        assert!(writes[0].result.is_ok());
        let identity = writes[0].source_identity.unwrap();
        if mode == WriteMode::Journal {
            assert_eq!(identity, before);
            assert!(matches!(
                capture(&mut client, project, identity, 0).result,
                Err(loom_text_session::SessionError::CompletionNotCheckpointed)
            ));
        } else {
            let cursor = u64::try_from(write.text.len()).unwrap();
            let Response::CompletionSource(captured) =
                capture(&mut client, project, identity, cursor)
                    .result
                    .unwrap()
            else {
                panic!("completion reply")
            };
            assert_eq!(captured.prefix(), write.text);
        }
    }
}

#[test]
fn failed_view_admission_keeps_the_original_project_and_releases_the_rejected_lease() {
    fn reject_marker(text: &str, _: DocumentKind) -> Result<(), String> {
        if text == "unsupported by this view" {
            Err("unsupported marker".into())
        } else {
            Ok(())
        }
    }
    let original = fixture();
    let rejected = fixture();
    let mut project = loom_text_session::TextProject::open(rejected.path()).unwrap();
    let target = loom_text_session::TextTarget::Manuscript(0);
    let baseline = project.read(target).unwrap();
    project
        .save(target, &baseline, "unsupported by this view")
        .unwrap();
    drop(project);
    let (mut client, snapshot) = Client::open(original.path(), reject_marker, || {}).unwrap();
    client
        .submit(
            snapshot.project.project_id,
            Command::Open(rejected.path().into()),
        )
        .unwrap();
    assert!(matches!(
        reply(&mut client).result,
        Err(loom_text_session::SessionError::Admission(_))
    ));
    let mut released = loom_text_session::TextProject::open(rejected.path()).unwrap();
    assert_eq!(released.read(target).unwrap(), "unsupported by this view");
    client
        .submit(
            snapshot.project.project_id,
            Command::Write {
                mode: WriteMode::Checkpoint,
                documents: vec![TextWrite {
                    document_id: snapshot.manuscript.document_id,
                    baseline: snapshot.manuscript.baseline,
                    text: "original remains usable".into(),
                }],
            },
        )
        .unwrap();
    let Response::Written(writes) = reply(&mut client).result.unwrap() else {
        panic!("write response")
    };
    assert!(writes[0].result.is_ok());
    assert_eq!(
        std::fs::read_to_string(
            original
                .path()
                .join(&snapshot.project.documents[0].relative_path)
        )
        .unwrap(),
        "original remains usable"
    );
}

#[test]
fn admission_bounds_unread_work_and_shutdown_drains_before_releasing_the_lease() {
    let root = fixture();
    let (mut client, snapshot) = Client::open(root.path(), admit, || {}).unwrap();
    let request = |n| Command::Write {
        mode: WriteMode::Journal,
        documents: vec![TextWrite {
            document_id: snapshot.manuscript.document_id,
            baseline: snapshot.manuscript.baseline.clone(),
            text: format!("accepted {n}"),
        }],
    };
    for i in 0..MAX_PENDING {
        client
            .submit(snapshot.project.project_id, request(i))
            .unwrap();
    }
    assert!(
        client
            .submit(snapshot.project.project_id, request(MAX_PENDING))
            .is_err()
    );
    drop(client); // No reply polling; a full reply channel must not deadlock join.
    let mut project = loom_text_session::TextProject::open(root.path()).unwrap();
    assert_eq!(
        project
            .read(loom_text_session::TextTarget::Manuscript(0))
            .unwrap(),
        format!("accepted {}", MAX_PENDING - 1)
    );
    assert_eq!(
        std::fs::read_dir(root.path().join(".loom/drafts"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn close_refuses_new_work_and_a_failed_close_reopens_admission() {
    let root = fixture();
    let (mut client, snapshot) = Client::open(root.path(), admit, || {}).unwrap();
    client.submit(ProjectId::new(), Command::Close).unwrap();
    assert!(
        client
            .submit(snapshot.project.project_id, Command::Create)
            .is_err()
    );
    assert!(matches!(
        reply(&mut client).result,
        Err(loom_text_session::SessionError::ProjectMismatch)
    ));
    client
        .submit(snapshot.project.project_id, Command::Close)
        .unwrap();
    assert!(matches!(reply(&mut client).result, Ok(Response::Closed)));
    assert!(
        client
            .submit(snapshot.project.project_id, Command::Create)
            .is_err()
    );
    drop(client);
    assert!(loom_text_session::TextProject::open(root.path()).is_ok());
}

#[test]
fn stable_document_ids_survive_list_reordering_and_old_project_requests_are_refused() {
    let root = fixture();
    let (mut client, snapshot) = Client::open(root.path(), admit, || {}).unwrap();
    client
        .submit(snapshot.project.project_id, Command::Create)
        .unwrap();
    let Response::Opened(created) = reply(&mut client).result.unwrap() else {
        panic!("new document")
    };
    assert_ne!(
        created.manuscript.document_id,
        snapshot.manuscript.document_id
    );
    let write = TextWrite {
        document_id: snapshot.manuscript.document_id,
        baseline: snapshot.manuscript.baseline,
        text: "original document".into(),
    };
    client
        .submit(
            ProjectId::new(),
            Command::Write {
                mode: WriteMode::Checkpoint,
                documents: vec![write.clone()],
            },
        )
        .unwrap();
    assert!(reply(&mut client).result.is_err());
    client
        .submit(
            snapshot.project.project_id,
            Command::Write {
                mode: WriteMode::Checkpoint,
                documents: vec![write],
            },
        )
        .unwrap();
    let Response::Written(writes) = reply(&mut client).result.unwrap() else {
        panic!("write response")
    };
    assert!(writes[0].result.is_ok());
    assert_eq!(writes[0].document_id, snapshot.manuscript.document_id);
    let original = snapshot
        .project
        .documents
        .iter()
        .find(|d| d.document_id == snapshot.manuscript.document_id)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join(&original.relative_path)).unwrap(),
        "original document"
    );
    assert_eq!(
        std::fs::read_to_string(
            root.path()
                .join(&created.project.documents[created.selected].relative_path)
        )
        .unwrap(),
        ""
    );
}

#[test]
fn a_batch_reports_each_acknowledgement_when_a_later_file_conflicts() {
    let root = fixture();
    std::fs::write(root.path().join("Untitled.md"), "").unwrap();
    std::fs::write(root.path().join("Zzz notes.md"), "").unwrap();
    let (mut client, snapshot) = Client::open(root.path(), admit, || {}).unwrap();
    let auxiliary_id = snapshot
        .project
        .documents
        .iter()
        .find(|document| document.relative_path == "Zzz notes.md")
        .unwrap()
        .document_id;
    client
        .submit(
            snapshot.project.project_id,
            Command::OpenAuxiliary(auxiliary_id),
        )
        .unwrap();
    let Response::Auxiliary(auxiliary) = reply(&mut client).result.unwrap() else {
        panic!("auxiliary response")
    };
    std::fs::write(root.path().join("Zzz notes.md"), "outside").unwrap();
    client
        .submit(
            snapshot.project.project_id,
            Command::Write {
                mode: WriteMode::Checkpoint,
                documents: vec![
                    TextWrite {
                        document_id: snapshot.manuscript.document_id,
                        baseline: snapshot.manuscript.baseline,
                        text: "saved manuscript".into(),
                    },
                    TextWrite {
                        document_id: auxiliary.document_id,
                        baseline: auxiliary.baseline,
                        text: "context draft".into(),
                    },
                ],
            },
        )
        .unwrap();
    let Response::Written(writes) = reply(&mut client).result.unwrap() else {
        panic!("write response")
    };
    assert!(writes[0].result.is_ok());
    assert_eq!(writes[0].baseline.as_deref(), Some("saved manuscript"));
    assert!(!writes[0].pending);
    assert!(writes[1].result.is_err());
    assert!(writes[1].pending); // The failed checkpoint still protected its draft.
    assert!(writes[1].baseline.is_none());
    assert_eq!(
        std::fs::read_to_string(root.path().join("Zzz notes.md")).unwrap(),
        "outside"
    );
}

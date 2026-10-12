use super::*;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Instant;

const CHILD_ROOT: &str = "LOOM_ASSET_FIFO_CHILD_ROOT";
const EVIDENCE_ROOT: &str = "LOOM_ASSET_FIFO_EVIDENCE_DIR";
const TEST_NAME: &str =
    "tests::asset_fifo_protocol::unpaired_fifo_is_refused_without_mutating_registered_documents";

#[derive(serde::Serialize, serde::Deserialize)]
struct Fixture {
    project_id: ProjectId,
    selected_document: DocumentId,
    unselected_document: DocumentId,
    relative_asset: String,
    author: serde_json::Value,
}

struct OwnedChild(Child);

impl OwnedChild {
    fn observe(&mut self, ready: Option<&Path>, budget: Duration) -> bool {
        let started = Instant::now();
        loop {
            if ready.is_some_and(Path::exists) {
                return true;
            }
            if self.0.try_wait().expect("observe owned child").is_some() {
                return ready.is_none();
            }
            if started.elapsed() >= budget {
                return false;
            }
            std::thread::yield_now();
        }
    }

    fn stop_and_reap(&mut self) -> ExitStatus {
        let _ = self.0.kill();
        self.0.wait().expect("reap owned protocol child")
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn record(root: &Path, name: &str, value: &serde_json::Value) {
    let pending = root.join(format!("{name}.pending"));
    std::fs::write(&pending, serde_json::to_vec_pretty(value).expect("receipt JSON"))
        .expect("write owned receipt");
    std::fs::rename(pending, root.join(format!("{name}.json"))).expect("publish owned receipt");
}

fn author_snapshot(store: &ProjectStore) -> serde_json::Value {
    let mut registry = store.list_documents().expect("registered documents");
    registry.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let documents: Vec<_> = registry
        .iter()
        .map(|registered| {
            let loaded = store.read_document(&registered.relative_path).expect("author document");
            serde_json::json!({
                "registered": registered,
                "active": loaded,
                "visible_bytes": std::fs::read(store.root().join(&registered.relative_path))
                    .expect("visible author bytes"),
            })
        })
        .collect();
    serde_json::json!({
        "manifest_bytes": std::fs::read(store.root().join(".loom/project.json"))
            .expect("project manifest bytes"),
        "documents": documents,
    })
}

fn open_state(store: ProjectStore) -> (PluginState, CommandId) {
    let state = PluginState::default();
    let snapshot = reserve_project_choice(&state)
        .expect("reserve owned project")
        .finish_without_document_filesystem_watcher(Ok(store))
        .expect("admit owned project without a filesystem observer");
    let session = snapshot.session_id.parse().expect("current session identifier");
    (state, session)
}

fn prepare_fixture(root: &Path) -> Fixture {
    use base64::Engine as _;

    let project = root.join("Project");
    let store = initialize_project(&project, "Owned image protocol fixture".to_owned())
        .expect("initialize plain owned project");
    let project_id = store.manifest().project_id;
    let (state, session_id) = open_state(store);
    let png = png_attachment_fixture(9);
    std::fs::write(root.join("ordinary.png"), &png).expect("retain ordinary PNG bytes");
    let stored = ingest_image_attachment_for_session(
        &state,
        &project_id.to_string(),
        &session_id.to_string(),
        "image/png",
        &base64::engine::general_purpose::STANDARD.encode(&png),
    )
    .expect("ingest a real image through the current session");
    let fixture = {
        let mut session = state.session.lock().expect("owned session");
        let store = session.store.as_mut().expect("owned project store");
        for (path, text) in [
            (
                "picture.md",
                format!("# Author\n\nPreserve <3 and λ.\n\n![Image]({})\n", stored.markdown_path),
            ),
            ("unselected.md", "# Other\n\nNo selected image.\n".to_owned()),
        ] {
            store.create_document_if_absent(path, DocumentContent::Prose(text), "fixture")
                .expect("register author document");
        }
        Fixture {
            project_id,
            selected_document: store.read_document("picture.md").expect("selected document").document_id,
            unselected_document: store.read_document("unselected.md").expect("unselected document").document_id,
            relative_asset: stored.relative_path,
            author: author_snapshot(store),
        }
    };
    drop(state);
    fixture
}

fn request(uri: &str, method: http::Method, range: Option<&str>) -> http::Request<Vec<u8>> {
    let mut request = http::Request::builder().method(method).uri(uri);
    if let Some(range) = range {
        request = request.header(http::header::RANGE, range);
    }
    request.body(Vec::new()).expect("actual protocol request")
}

fn response_receipt(response: &http::Response<Vec<u8>>) -> serde_json::Value {
    let headers: std::collections::BTreeMap<_, _> = response.headers().iter()
        .map(|(name, value)| (name.as_str(), value.to_str().expect("protocol ASCII header")))
        .collect();
    serde_json::json!({ "status": response.status().as_u16(), "headers": headers, "body": response.body() })
}

fn ordinary_image_controls(state: &PluginState, uri: &str, png: &[u8], root: &Path) {
    let get = loom_asset_protocol_response(state, "main", &request(uri, http::Method::GET, None));
    record(root, "ordinary-get", &response_receipt(&get));
    assert_eq!(get.status(), http::StatusCode::OK);
    assert_eq!(get.body(), png);
    assert_eq!(get.headers()[http::header::CONTENT_TYPE], "image/png");
    assert_eq!(get.headers()[http::header::CONTENT_LENGTH], png.len().to_string());
    assert_eq!(get.headers()[http::header::CACHE_CONTROL], "no-store");
    assert_eq!(get.headers()["x-content-type-options"], "nosniff");
    let head = loom_asset_protocol_response(state, "main", &request(uri, http::Method::HEAD, None));
    record(root, "ordinary-head", &response_receipt(&head));
    assert_eq!(head.status(), http::StatusCode::OK);
    assert!(head.body().is_empty());
    assert_eq!(head.headers()[http::header::CONTENT_LENGTH], png.len().to_string());
    let range = loom_asset_protocol_response(state, "main", &request(uri, http::Method::GET, Some("bytes=0-1")));
    record(root, "ordinary-range", &response_receipt(&range));
    assert_eq!(range.status(), http::StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.body(), &png[..2]);
    assert_eq!(range.headers()[http::header::CONTENT_RANGE], format!("bytes 0-1/{}", png.len()));
}

fn ordinary_controls(
    state: &PluginState,
    fixture: &Fixture,
    session_id: CommandId,
    root: &Path,
) -> String {
    let asset = root.join("Project").join(&fixture.relative_asset);
    let file_name = asset.file_name().and_then(std::ffi::OsStr::to_str).expect("canonical image name");
    let token = |session, document| {
        let token = loom_asset_token(fixture.project_id, session, document, file_name)
            .expect("current canonical v3 token");
        format!("loom-asset://localhost/{token}")
    };
    let uri = token(session_id, fixture.selected_document);
    let png = std::fs::read(root.join("ordinary.png")).expect("ordinary PNG");
    ordinary_image_controls(state, &uri, &png, root);
    for (name, view, rejected_uri, status) in [
        ("wrong-view", "auxiliary", uri.clone(), http::StatusCode::FORBIDDEN),
        ("wrong-session", "main", token(CommandId::new(), fixture.selected_document), http::StatusCode::NOT_FOUND),
        ("wrong-document", "main", token(session_id, fixture.unselected_document), http::StatusCode::NOT_FOUND),
    ] {
        let rejected = loom_asset_protocol_response(state, view, &request(&rejected_uri, http::Method::GET, None));
        record(root, name, &response_receipt(&rejected));
        assert_eq!(rejected.status(), status);
        assert!(rejected.body().is_empty());
    }
    uri
}

fn run_protocol_child(root: &Path) {
    let fixture: Fixture = serde_json::from_slice(&std::fs::read(root.join("fixture.json"))
        .expect("owned fixture configuration")).expect("typed fixture configuration");
    let store = ProjectStore::open(root.join("Project")).expect("reopen owned project");
    assert_eq!(store.manifest().project_id, fixture.project_id);
    let (state, session_id) = open_state(store);
    let uri = ordinary_controls(&state, &fixture, session_id, root);
    let author = {
        let session = state.session.lock().expect("owned session");
        author_snapshot(session.store.as_ref().expect("owned store"))
    };
    assert_eq!(author, fixture.author);
    record(root, "ready", &serde_json::json!({ "uri": uri, "author": author }));
    let mut permission = [0];
    std::io::stdin().read_exact(&mut permission).expect("parent fixture ready signal");
    assert_eq!(permission, [1]);
    record(root, "entered", &serde_json::json!({ "uri": uri }));
    let response = loom_asset_protocol_response(&state, "main", &request(&uri, http::Method::GET, None));
    record(root, "returned", &response_receipt(&response));
    drop(state);
}

fn spawn_child(root: &Path) -> OwnedChild {
    let executable = std::env::current_exe().expect("current actual libtest executable");
    let child = OwnedChild(Command::new(&executable)
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_ROOT, root).stdin(Stdio::piped())
        .stdout(std::fs::File::create(root.join("child.stdout")).expect("owned child stdout"))
        .stderr(std::fs::File::create(root.join("child.stderr")).expect("owned child stderr"))
        .spawn().expect("spawn only this protocol owner in a non-native child"));
    let binary = std::fs::metadata(&executable).expect("current binary identity");
    record(root, "child", &serde_json::json!({
        "pid": child.0.id(), "executable": executable, "device": binary.dev(),
        "inode": binary.ino(), "bytes": binary.len(), "root": root,
        "source_manifest_dir": env!("CARGO_MANIFEST_DIR"), "test_name": TEST_NAME,
        "native_application": false, "model_invoked": false,
    }));
    child
}

fn replace_with_fifo(asset: &Path) -> std::fs::Metadata {
    std::fs::remove_file(asset).expect("replace only the canonical owned asset");
    assert!(Command::new("/usr/bin/mkfifo").arg(asset).status().expect("create owned unpaired FIFO").success());
    let before = std::fs::symlink_metadata(asset).expect("owned FIFO identity");
    assert!(before.file_type().is_fifo());
    before
}

fn assert_reaped_outcome(
    root: &Path,
    fixture: &Fixture,
    before: &std::fs::Metadata,
    completed: bool,
    status: ExitStatus,
) {
    let after = std::fs::symlink_metadata(root.join("Project").join(&fixture.relative_asset))
        .expect("preserved owned FIFO");
    let reopened = ProjectStore::open(root.join("Project")).expect("reopen after unconditional child reap");
    let author = author_snapshot(&reopened);
    drop(reopened);
    record(root, "author-after", &author);
    assert_eq!(author, fixture.author, "visible bytes, manifest, registry and active author heads");
    assert!(after.file_type().is_fifo());
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!((after.mode(), after.uid(), after.gid()), (before.mode(), before.uid(), before.gid()));
    assert!(root.join("entered.json").is_file(), "actual current protocol read must be entered");
    if !completed {
        assert!(!root.join("returned.json").exists(), "protocol returned before a cleanup delay; this is not a FIFO-read witness");
    }
    assert!(completed, "authorized DocImage GET waited on an unpaired FIFO; child reaped: {status}; root={}", root.display());
    assert!(status.success(), "protocol child failed: {status}; root={}", root.display());
    let returned: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("returned.json"))
        .expect("actual returned protocol receipt")).expect("returned receipt JSON");
    assert_eq!(returned["status"], http::StatusCode::NOT_FOUND.as_u16());
    assert_eq!(returned["body"], serde_json::json!([]));
}

fn run_parent(root: &Path, fixture: &Fixture) {
    let mut child = spawn_child(root);
    let ready = child.observe(Some(&root.join("ready.json")), Duration::from_secs(30));
    if !ready {
        let status = child.stop_and_reap();
        record(root, "cleanup", &serde_json::json!({ "phase": "ordinary", "status": status.to_string() }));
        panic!("ordinary protocol controls did not finish; child reaped: {status}; root={}", root.display());
    }
    let before = replace_with_fifo(&root.join("Project").join(&fixture.relative_asset));
    let mut stdin = child.0.stdin.take().expect("owned child ready channel");
    stdin.write_all(&[1]).expect("allow actual protocol read after FIFO replacement");
    drop(stdin);
    let started = Instant::now();
    let completed = child.observe(None, Duration::from_secs(5));
    let observed = started.elapsed();
    let status = child.stop_and_reap();
    record(root, "cleanup", &serde_json::json!({
        "phase": "protocol", "completed_before_deadline": completed,
        "deadline_ms": 5000, "observed_ms": observed.as_millis(),
        "status": status.to_string(), "exit_code": status.code(),
    }));
    assert_reaped_outcome(root, fixture, &before, completed, status);
}

#[test]
fn unpaired_fifo_is_refused_without_mutating_registered_documents() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        run_protocol_child(Path::new(&root));
        return;
    }
    let parent = std::env::var_os(EVIDENCE_ROOT).map_or_else(std::env::temp_dir, PathBuf::from);
    let root = tempfile::Builder::new().prefix("loom-asset-fifo-")
        .tempdir_in(parent).expect("parent-owned fixture directory").keep();
    println!("loom_asset_fifo_fixture={}", root.display());
    let fixture = prepare_fixture(&root);
    std::fs::write(root.join("fixture.json"), serde_json::to_vec_pretty(&fixture)
        .expect("fixture JSON")).expect("retain fixture configuration");
    run_parent(&root, &fixture);
}

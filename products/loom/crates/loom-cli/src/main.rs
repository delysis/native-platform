#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use loom_document::{DocumentContent, MergeConflict, MergeOutcome, three_way_merge};
use loom_store::{
    ExternalReconciliationOutcome, ExternalReconciliationRequest, MAX_DOCUMENT_BYTES, ProjectStore,
};
use loom_types::{ArtifactId, BlobId, CommandId, DocumentId, DocumentKind, ProjectId, RevisionId};
use serde::Serialize;

const RECONCILIATION_PREVIEW_PROTOCOL: &str = "loom.reconcile-preview.v1";
const RECONCILIATION_APPLY_PROTOCOL: &str = "loom.reconcile-apply.v1";

#[derive(Debug, Parser)]
#[command(
    name = "loom",
    version,
    about = "Loom Native project and recovery oracle"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize an open-folder Loom project.
    Init {
        path: PathBuf,
        #[arg(long)]
        name: String,
    },
    /// Inspect a project and record an open receipt.
    Open { path: PathBuf },
    /// Checkpoint an existing visible manuscript file.
    Checkpoint {
        project: PathBuf,
        file: PathBuf,
        #[arg(long, value_enum)]
        kind: CliDocumentKind,
        #[arg(long, default_value = "explicit checkpoint")]
        reason: String,
    },
    /// Import a UTF-8 file as a visible manuscript and immutable revision.
    Import {
        project: PathBuf,
        source: PathBuf,
        #[arg(long)]
        to: PathBuf,
        #[arg(long, value_enum)]
        kind: CliDocumentKind,
        #[arg(long, default_value = "human import")]
        reason: String,
    },
    /// Export the latest checkpointed visible manuscript.
    Export {
        project: PathBuf,
        file: PathBuf,
        #[arg(long)]
        to: PathBuf,
    },
    /// Replay unfinished visible-file outbox entries without overwriting conflicts.
    Recover { project: PathBuf },
    /// Preview a three-way merge without changing project state or visible files.
    ReconcilePreview(ReconcilePreviewArgs),
    /// Apply an explicitly resolved external edit against exact bound identities.
    ReconcileApply(ReconcileApplyArgs),
}

#[derive(Debug, Args)]
struct ReconcilePreviewArgs {
    /// Loom project folder.
    project: PathBuf,
    /// Registered project-relative manuscript path.
    file: PathBuf,
    /// Optional bounded UTF-8 app draft; the immutable base is used when absent.
    #[arg(long)]
    app_draft: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ReconcileApplyArgs {
    /// Loom project folder.
    project: PathBuf,
    /// Registered project-relative manuscript path.
    file: PathBuf,
    /// Active revision captured by `reconcile-preview`.
    #[arg(long)]
    expected_revision: RevisionId,
    /// Immutable base blob captured by `reconcile-preview`.
    #[arg(long)]
    expected_base_blob: BlobId,
    /// External visible blob captured by `reconcile-preview`.
    #[arg(long)]
    expected_external_blob: BlobId,
    /// Explicitly resolved bounded UTF-8 manuscript file.
    #[arg(long)]
    resolved: PathBuf,
    /// Stored document kind; a mismatch fails closed.
    #[arg(long, value_enum)]
    kind: CliDocumentKind,
    /// Caller-owned idempotency key. Exact retries reuse the same command ID.
    #[arg(long)]
    command_id: CommandId,
    /// Human-readable provenance reason. This argument has no default.
    #[arg(long)]
    reason: String,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliDocumentKind {
    Hybrid,
    Prose,
    Verse,
}

impl From<CliDocumentKind> for DocumentKind {
    fn from(value: CliDocumentKind) -> Self {
        match value {
            CliDocumentKind::Hybrid => Self::Hybrid,
            CliDocumentKind::Prose => Self::Prose,
            CliDocumentKind::Verse => Self::Verse,
        }
    }
}

#[derive(Debug, Serialize)]
struct ProjectView<'a> {
    root: &'a Path,
    manifest: &'a loom_types::ProjectManifest,
    documents: Vec<loom_store::DocumentSummary>,
    pending_outbox_entries: u64,
    receipt: loom_types::CommandReceipt,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum AppDraftSource {
    ImmutableBase,
    File,
}

#[derive(Debug, Serialize)]
struct AppDraftBinding {
    source: AppDraftSource,
    blob_id: BlobId,
}

#[derive(Debug, Serialize)]
struct ReconciliationBinding {
    project_id: ProjectId,
    project_schema_version: u32,
    document_id: DocumentId,
    relative_path: String,
    kind: DocumentKind,
    active_revision_id: RevisionId,
    active_artifact_id: ArtifactId,
    base_blob_id: BlobId,
    external_blob_id: Option<BlobId>,
    app_draft: AppDraftBinding,
    visible_matches_active: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ReconciliationPreviewOutcome {
    Merged {
        content: String,
        merged_blob_id: BlobId,
    },
    Conflict {
        conflicts: Vec<MergeConflict>,
    },
    ExternalFileDeleted,
}

#[derive(Debug, Serialize)]
struct ReconciliationPreview {
    protocol: &'static str,
    binding: ReconciliationBinding,
    outcome: ReconciliationPreviewOutcome,
}

#[derive(Debug, Serialize)]
struct ReconciliationApplyBinding {
    relative_path: String,
    kind: DocumentKind,
    command_id: CommandId,
    expected_active_revision_id: RevisionId,
    expected_base_blob_id: BlobId,
    expected_external_blob_id: BlobId,
    resolved_blob_id: BlobId,
}

#[derive(Debug, Serialize)]
struct ReconciliationApplyView {
    protocol: &'static str,
    binding: ReconciliationApplyBinding,
    outcome: ExternalReconciliationOutcome,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("loom: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Init { path, name } => {
            let (store, receipt) = ProjectStore::initialize(path, name)?;
            print_json(&ProjectView {
                root: store.root(),
                manifest: store.manifest(),
                documents: store.list_documents()?,
                pending_outbox_entries: store.pending_outbox_count()?,
                receipt,
            })?;
        }
        Command::Open { path } => {
            let mut store = ProjectStore::open(path)?;
            let receipt = store.record_open()?;
            print_json(&ProjectView {
                root: store.root(),
                manifest: store.manifest(),
                documents: store.list_documents()?,
                pending_outbox_entries: store.pending_outbox_count()?,
                receipt,
            })?;
        }
        Command::Checkpoint {
            project,
            file,
            kind,
            reason,
        } => {
            let mut store = ProjectStore::open(project)?;
            print_json(&store.checkpoint_visible(file, kind.into(), reason)?)?;
        }
        Command::Import {
            project,
            source,
            to,
            kind,
            reason,
        } => {
            let mut store = ProjectStore::open(project)?;
            print_json(&store.import_file(source, to, kind.into(), reason)?)?;
        }
        Command::Export { project, file, to } => {
            let mut store = ProjectStore::open(project)?;
            print_json(&store.export_document(file, to)?)?;
        }
        Command::Recover { project } => {
            let mut store = ProjectStore::open(project)?;
            print_json(&store.recover()?)?;
        }
        Command::ReconcilePreview(arguments) => {
            let store = ProjectStore::open(&arguments.project)?;
            print_json(&reconciliation_preview(
                &store,
                &arguments.file,
                arguments.app_draft.as_deref(),
            )?)?;
        }
        Command::ReconcileApply(arguments) => {
            let mut store = ProjectStore::open(&arguments.project)?;
            print_json(&reconciliation_apply(&mut store, &arguments)?)?;
        }
    }
    Ok(())
}

fn reconciliation_preview(
    store: &ProjectStore,
    relative_path: &Path,
    app_draft_path: Option<&Path>,
) -> Result<ReconciliationPreview, Box<dyn std::error::Error>> {
    let snapshot = store.reconciliation_snapshot(relative_path)?;
    reject_flat_hybrid_reconciliation(snapshot.kind)?;
    let (app_draft, app_draft_binding) = if let Some(path) = app_draft_path {
        let text = read_bounded_utf8_file(path)?;
        let blob_id = BlobId::digest(text.as_bytes());
        (
            text,
            AppDraftBinding {
                source: AppDraftSource::File,
                blob_id,
            },
        )
    } else {
        (
            snapshot.base_text.clone(),
            AppDraftBinding {
                source: AppDraftSource::ImmutableBase,
                blob_id: snapshot.active_blob_id,
            },
        )
    };
    let external_blob_id = snapshot.visible.as_ref().map(|visible| visible.blob_id);
    let outcome = match snapshot.visible.as_ref() {
        Some(visible) => {
            let base = canonical_merge_text(snapshot.kind, &snapshot.base_text)?;
            let app = canonical_merge_text(snapshot.kind, &app_draft)?;
            let external = canonical_merge_text(snapshot.kind, &visible.text)?;
            match three_way_merge(snapshot.kind, &base, &app, &external)? {
                MergeOutcome::Merged { content } => ReconciliationPreviewOutcome::Merged {
                    merged_blob_id: BlobId::digest(content.as_bytes()),
                    content,
                },
                MergeOutcome::Conflict { conflicts } => {
                    ReconciliationPreviewOutcome::Conflict { conflicts }
                }
            }
        }
        None => ReconciliationPreviewOutcome::ExternalFileDeleted,
    };
    Ok(ReconciliationPreview {
        protocol: RECONCILIATION_PREVIEW_PROTOCOL,
        binding: ReconciliationBinding {
            project_id: store.manifest().project_id,
            project_schema_version: store.manifest().schema_version,
            document_id: snapshot.document_id,
            relative_path: snapshot.relative_path,
            kind: snapshot.kind,
            active_revision_id: snapshot.active_revision_id,
            active_artifact_id: snapshot.active_artifact_id,
            base_blob_id: snapshot.active_blob_id,
            external_blob_id,
            app_draft: app_draft_binding,
            visible_matches_active: snapshot.visible_matches_active,
        },
        outcome,
    })
}

fn reconciliation_apply(
    store: &mut ProjectStore,
    arguments: &ReconcileApplyArgs,
) -> Result<ReconciliationApplyView, Box<dyn std::error::Error>> {
    if arguments.reason.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reconciliation reason must not be empty",
        )
        .into());
    }
    let relative_path = arguments.file.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "project-relative manuscript path is not UTF-8",
        )
    })?;
    let kind = DocumentKind::from(arguments.kind);
    reject_flat_hybrid_reconciliation(kind)?;
    let resolved_text = read_bounded_utf8_file(&arguments.resolved)?;
    let resolved_content = DocumentContent::from_visible(kind, resolved_text.into_bytes())?;
    let resolved_projection = resolved_content.project_visible()?;
    let resolved_blob_id = BlobId::digest(&resolved_projection.bytes);
    let request = ExternalReconciliationRequest {
        relative_path: relative_path.to_owned(),
        expected_active_revision_id: arguments.expected_revision,
        expected_base_blob_id: arguments.expected_base_blob,
        expected_visible_blob_id: arguments.expected_external_blob,
        resolved_content,
        reason: arguments.reason.clone(),
    };
    let outcome = store.reconcile_external_idempotent(arguments.command_id, request)?;
    Ok(ReconciliationApplyView {
        protocol: RECONCILIATION_APPLY_PROTOCOL,
        binding: ReconciliationApplyBinding {
            relative_path: relative_path.to_owned(),
            kind,
            command_id: arguments.command_id,
            expected_active_revision_id: arguments.expected_revision,
            expected_base_blob_id: arguments.expected_base_blob,
            expected_external_blob_id: arguments.expected_external_blob,
            resolved_blob_id,
        },
        outcome,
    })
}

fn reject_flat_hybrid_reconciliation(kind: DocumentKind) -> Result<(), io::Error> {
    if kind == DocumentKind::Hybrid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hybrid reconciliation requires persisted block metadata and is not available",
        ));
    }
    Ok(())
}

fn canonical_merge_text(
    kind: DocumentKind,
    text: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let content = DocumentContent::from_visible(kind, text.as_bytes().to_vec())?;
    let projection = content.project_visible()?;
    Ok(String::from_utf8(projection.bytes)?)
}

fn read_bounded_utf8_file(path: &Path) -> Result<String, io::Error> {
    let initial_metadata = fs::symlink_metadata(path)?;
    if initial_metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("input path is a symbolic link: {}", path.display()),
        ));
    }
    validate_regular_file_metadata(path, &initial_metadata)?;

    let file = File::open(path)?;
    let opened_metadata = file.metadata()?;
    validate_regular_file_metadata(path, &opened_metadata)?;
    ensure_same_file(path, &initial_metadata, &opened_metadata)?;

    let capacity = usize::try_from(opened_metadata.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(MAX_DOCUMENT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let actual_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_bytes > MAX_DOCUMENT_BYTES {
        return Err(file_too_large_error(path, actual_bytes));
    }
    String::from_utf8(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("input file is not valid UTF-8: {}: {error}", path.display()),
        )
    })
}

fn validate_regular_file_metadata(path: &Path, metadata: &fs::Metadata) -> Result<(), io::Error> {
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("input path is not a regular file: {}", path.display()),
        ));
    }
    if metadata.len() > MAX_DOCUMENT_BYTES {
        return Err(file_too_large_error(path, metadata.len()));
    }
    Ok(())
}

fn file_too_large_error(path: &Path, actual_bytes: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "input file has {actual_bytes} bytes; limit is {MAX_DOCUMENT_BYTES} bytes: {}",
            path.display()
        ),
    )
}

#[cfg(unix)]
fn ensure_same_file(
    path: &Path,
    initial: &fs::Metadata,
    opened: &fs::Metadata,
) -> Result<(), io::Error> {
    use std::os::unix::fs::MetadataExt;

    if initial.dev() != opened.dev() || initial.ino() != opened.ino() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("input path changed while it was opened: {}", path.display()),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_same_file(
    path: &Path,
    initial: &fs::Metadata,
    opened: &fs::Metadata,
) -> Result<(), io::Error> {
    use std::os::windows::fs::MetadataExt;

    let identity_matches = initial
        .volume_serial_number()
        .zip(initial.file_index())
        .zip(opened.volume_serial_number().zip(opened.file_index()))
        .is_some_and(
            |((initial_volume, initial_file), (opened_volume, opened_file))| {
                initial_volume == opened_volume && initial_file == opened_file
            },
        );
    if !identity_matches {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "input path changed or could not be identified while it was opened: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn ensure_same_file(
    path: &Path,
    _initial: &fs::Metadata,
    _opened: &fs::Metadata,
) -> Result<(), io::Error> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "this platform cannot verify input file identity: {}",
            path.display()
        ),
    ))
}

fn print_json(value: &impl Serialize) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::CommandFactory;
    use loom_store::StoreCounts;
    use tempfile::{TempDir, tempdir};

    use super::*;

    struct Fixture {
        _directory: TempDir,
        root: PathBuf,
        store: ProjectStore,
        base: loom_store::LoadedDocument,
    }

    impl Fixture {
        fn new(text: &str) -> Self {
            Self::new_with_kind(text, DocumentKind::Prose)
        }

        fn new_with_kind(text: &str, kind: DocumentKind) -> Self {
            let directory = tempdir().expect("temporary directory");
            let root = directory.path().join("Novel");
            let (mut store, _) =
                ProjectStore::initialize(&root, "Novel").expect("initialize project");
            store
                .save_document(
                    "manuscript/001.md",
                    DocumentContent::from_visible(kind, text.as_bytes().to_vec())
                        .expect("valid base content"),
                    "initial",
                )
                .expect("save base");
            let base = store.read_document("manuscript/001.md").expect("read base");
            Self {
                _directory: directory,
                root,
                store,
                base,
            }
        }

        fn visible_path(&self) -> PathBuf {
            self.root.join("manuscript/001.md")
        }
    }

    #[test]
    fn reconciliation_commands_have_stable_help_and_required_apply_bindings() {
        Cli::command().debug_assert();
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("reconcile-preview"));
        assert!(help.contains("reconcile-apply"));

        let error = Cli::try_parse_from(["loom", "reconcile-apply", "Novel", "chapter.md"])
            .expect_err("apply requires all identities and explicit resolution");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        let rendered = error.to_string();
        for required in [
            "--expected-revision",
            "--expected-base-blob",
            "--expected-external-blob",
            "--resolved",
            "--kind",
            "--command-id",
            "--reason",
        ] {
            assert!(rendered.contains(required), "missing help for {required}");
        }
    }

    #[test]
    fn preview_defaults_app_side_to_base_and_never_writes() {
        let fixture = Fixture::new("base\n");
        fs::write(fixture.visible_path(), "external\n").expect("write external edit");
        let counts: StoreCounts = fixture.store.counts().expect("counts before preview");
        let pending = fixture
            .store
            .pending_outbox_count()
            .expect("outbox before preview");

        let preview = reconciliation_preview(&fixture.store, Path::new("manuscript/001.md"), None)
            .expect("preview");

        assert_eq!(preview.binding.active_revision_id, fixture.base.revision_id);
        assert_eq!(preview.binding.base_blob_id, fixture.base.blob_id);
        assert_eq!(
            preview.binding.external_blob_id,
            Some(BlobId::digest(b"external\n"))
        );
        assert!(matches!(
            preview.binding.app_draft.source,
            AppDraftSource::ImmutableBase
        ));
        assert!(matches!(
            preview.outcome,
            ReconciliationPreviewOutcome::Merged { ref content, .. } if content == "external\n"
        ));
        assert_eq!(
            fixture.store.counts().expect("counts after preview"),
            counts
        );
        assert_eq!(
            fixture
                .store
                .pending_outbox_count()
                .expect("outbox after preview"),
            pending
        );
        assert_eq!(
            fs::read_to_string(fixture.visible_path()).expect("visible file after preview"),
            "external\n"
        );
    }

    #[test]
    fn preview_reports_conflict_and_deleted_external_without_applying() {
        let fixture = Fixture::new("abc\n");
        fs::write(fixture.visible_path(), "ayc\n").expect("write external edit");
        let draft = fixture.root.join("draft.txt");
        fs::write(&draft, "axc\n").expect("write app draft");
        let conflict =
            reconciliation_preview(&fixture.store, Path::new("manuscript/001.md"), Some(&draft))
                .expect("conflict preview");
        assert!(matches!(
            conflict.outcome,
            ReconciliationPreviewOutcome::Conflict { ref conflicts } if !conflicts.is_empty()
        ));
        assert!(matches!(
            conflict.binding.app_draft.source,
            AppDraftSource::File
        ));
        assert_eq!(conflict.binding.app_draft.blob_id, BlobId::digest(b"axc\n"));

        fs::remove_file(fixture.visible_path()).expect("delete external file");
        let deleted =
            reconciliation_preview(&fixture.store, Path::new("manuscript/001.md"), Some(&draft))
                .expect("deleted preview");
        assert!(matches!(
            deleted.outcome,
            ReconciliationPreviewOutcome::ExternalFileDeleted
        ));
        assert_eq!(deleted.binding.external_blob_id, None);
    }

    #[test]
    fn apply_requires_exact_bindings_and_records_explicit_resolution() {
        let mut fixture = Fixture::new("base\n");
        fs::write(fixture.visible_path(), "external\n").expect("write external edit");
        let external_blob_id = BlobId::digest(b"external\n");
        let resolved = fixture.root.join("resolved.txt");
        fs::write(&resolved, "resolved\n").expect("write resolution");
        let command_id = CommandId::new();
        let arguments = ReconcileApplyArgs {
            project: fixture.root.clone(),
            file: PathBuf::from("manuscript/001.md"),
            expected_revision: fixture.base.revision_id,
            expected_base_blob: fixture.base.blob_id,
            expected_external_blob: external_blob_id,
            resolved,
            kind: CliDocumentKind::Prose,
            command_id,
            reason: "author resolved external edit".into(),
        };

        let applied =
            reconciliation_apply(&mut fixture.store, &arguments).expect("apply reconciliation");

        assert_eq!(applied.binding.command_id, command_id);
        assert_eq!(applied.binding.expected_external_blob_id, external_blob_id);
        assert_eq!(
            applied.binding.resolved_blob_id,
            BlobId::digest(b"resolved\n")
        );
        assert_eq!(applied.outcome.save.receipt.command_id, command_id);
        assert_eq!(
            applied.outcome.visible_projection,
            loom_store::VisibleProjectionState::Applied
        );
        let json = serde_json::to_value(&applied).expect("serialize apply output");
        assert_eq!(json["outcome"]["visible_projection"]["status"], "applied");
        assert_eq!(
            fixture
                .store
                .read_document("manuscript/001.md")
                .expect("read resolved document")
                .text,
            "resolved\n"
        );
    }

    #[test]
    fn prose_preview_and_apply_share_canonical_crlf_projection() {
        let mut fixture = Fixture::new("base\n");
        fs::write(fixture.visible_path(), "external\r\n").expect("write CRLF external edit");
        let external_blob_id = BlobId::digest(b"external\r\n");
        let preview = reconciliation_preview(&fixture.store, Path::new("manuscript/001.md"), None)
            .expect("preview CRLF edit");
        assert_eq!(preview.binding.external_blob_id, Some(external_blob_id));
        assert!(matches!(
            preview.outcome,
            ReconciliationPreviewOutcome::Merged {
                ref content,
                merged_blob_id,
            } if content == "external\n" && merged_blob_id == BlobId::digest(b"external\n")
        ));

        let resolved = fixture.root.join("resolved.txt");
        fs::write(&resolved, "external\r\n").expect("write CRLF resolution");
        let arguments = ReconcileApplyArgs {
            project: fixture.root.clone(),
            file: PathBuf::from("manuscript/001.md"),
            expected_revision: fixture.base.revision_id,
            expected_base_blob: fixture.base.blob_id,
            expected_external_blob: external_blob_id,
            resolved,
            kind: CliDocumentKind::Prose,
            command_id: CommandId::new(),
            reason: "accept canonicalized external prose".into(),
        };
        let applied =
            reconciliation_apply(&mut fixture.store, &arguments).expect("apply CRLF resolution");
        assert_eq!(
            applied.binding.resolved_blob_id,
            BlobId::digest(b"external\n")
        );
        assert_eq!(
            fixture
                .store
                .read_document("manuscript/001.md")
                .expect("read canonical prose")
                .text,
            "external\n"
        );
    }

    #[test]
    fn verse_preview_preserves_crlf_and_whitespace_exactly() {
        let fixture = Fixture::new_with_kind("first\r\n", DocumentKind::Verse);
        let external = "first\r\n\r\n  second  \r\n";
        fs::write(fixture.visible_path(), external).expect("write exact verse edit");

        let preview = reconciliation_preview(&fixture.store, Path::new("manuscript/001.md"), None)
            .expect("preview verse edit");
        assert_eq!(
            preview.binding.external_blob_id,
            Some(BlobId::digest(external.as_bytes()))
        );
        assert!(matches!(
            preview.outcome,
            ReconciliationPreviewOutcome::Merged {
                ref content,
                merged_blob_id,
            } if content == external && merged_blob_id == BlobId::digest(external.as_bytes())
        ));
    }

    #[test]
    fn flat_hybrid_preview_and_apply_fail_closed() {
        let mut fixture = Fixture::new_with_kind("hybrid\n", DocumentKind::Hybrid);
        fs::write(fixture.visible_path(), "external\n").expect("write hybrid external edit");
        let counts = fixture.store.counts().expect("counts before hybrid checks");
        let preview = reconciliation_preview(&fixture.store, Path::new("manuscript/001.md"), None)
            .expect_err("hybrid preview needs block metadata");
        assert!(preview.to_string().contains("persisted block metadata"));

        let resolved = fixture.root.join("resolved.txt");
        fs::write(&resolved, "resolved\n").expect("write hybrid resolution");
        let arguments = ReconcileApplyArgs {
            project: fixture.root.clone(),
            file: PathBuf::from("manuscript/001.md"),
            expected_revision: fixture.base.revision_id,
            expected_base_blob: fixture.base.blob_id,
            expected_external_blob: BlobId::digest(b"external\n"),
            resolved,
            kind: CliDocumentKind::Hybrid,
            command_id: CommandId::new(),
            reason: "unsafe flat hybrid resolution".into(),
        };
        let apply = reconciliation_apply(&mut fixture.store, &arguments)
            .expect_err("hybrid apply needs block metadata");
        assert!(apply.to_string().contains("persisted block metadata"));
        assert_eq!(fixture.store.counts().expect("counts after checks"), counts);
        assert_eq!(
            fs::read_to_string(fixture.visible_path()).expect("unchanged external hybrid"),
            "external\n"
        );
    }

    #[test]
    fn apply_rejects_empty_reason_before_mutating() {
        let mut fixture = Fixture::new("base\n");
        fs::write(fixture.visible_path(), "external\n").expect("write external edit");
        let resolved = fixture.root.join("resolved.txt");
        fs::write(&resolved, "resolved\n").expect("write resolution");
        let counts = fixture.store.counts().expect("counts before apply");
        let arguments = ReconcileApplyArgs {
            project: fixture.root.clone(),
            file: PathBuf::from("manuscript/001.md"),
            expected_revision: fixture.base.revision_id,
            expected_base_blob: fixture.base.blob_id,
            expected_external_blob: BlobId::digest(b"external\n"),
            resolved,
            kind: CliDocumentKind::Prose,
            command_id: CommandId::new(),
            reason: " \t".into(),
        };

        let error = reconciliation_apply(&mut fixture.store, &arguments)
            .expect_err("empty reason must fail");
        assert!(error.to_string().contains("reason must not be empty"));
        assert_eq!(fixture.store.counts().expect("counts after apply"), counts);
    }

    #[test]
    fn bounded_reader_rejects_oversize_and_invalid_utf8() {
        let directory = tempdir().expect("temporary directory");
        let oversized = directory.path().join("oversized.txt");
        let oversized_file = File::create(&oversized).expect("create oversized file");
        oversized_file
            .set_len(MAX_DOCUMENT_BYTES + 1)
            .expect("size sparse file");
        let oversized_error =
            read_bounded_utf8_file(&oversized).expect_err("oversized input must fail");
        assert_eq!(oversized_error.kind(), io::ErrorKind::InvalidData);

        let invalid = directory.path().join("invalid.txt");
        fs::write(&invalid, [0xff, 0xfe]).expect("write invalid UTF-8");
        let invalid_error = read_bounded_utf8_file(&invalid).expect_err("invalid UTF-8 must fail");
        assert_eq!(invalid_error.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_reader_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let target = directory.path().join("target.txt");
        let link = directory.path().join("link.txt");
        fs::write(&target, "draft").expect("write target");
        symlink(&target, &link).expect("create symbolic link");

        let error = read_bounded_utf8_file(&link).expect_err("symbolic link must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}

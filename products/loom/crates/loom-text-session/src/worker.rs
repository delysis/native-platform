//! One leased project owner; bounded requests/replies and orderly shutdown.
use crate::{MAX_TEXT_BYTES, SessionError, TextProject, TextTarget, completion};
use loom_store::DocumentSummary;
use loom_types::{DocumentId, DocumentKind, ProjectId};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread::JoinHandle,
};

pub const MAX_PENDING: usize = 8;
pub type Admission = fn(&str, DocumentKind) -> Result<(), String>;

#[derive(Clone, Debug)]
pub struct ProjectInfo {
    pub project_id: ProjectId,
    pub root: PathBuf,
    pub documents: Vec<DocumentSummary>,
    pub settings: crate::configuration::Snapshot,
}
impl ProjectInfo {
    pub fn entries(&self) -> &[DocumentSummary] {
        &self.documents
    }
}
#[derive(Debug)]
pub struct OpenText {
    pub document_id: DocumentId,
    pub kind: DocumentKind,
    pub text: String,
    pub baseline: String,
    pub pending: bool,
    pub source_identity: completion::SourceIdentity,
}
#[derive(Debug)]
pub struct Snapshot {
    pub project: ProjectInfo,
    pub selected: usize,
    pub manuscript: OpenText,
    pub context: Option<OpenText>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    Journal,
    Checkpoint,
    Discard,
}
#[derive(Clone, Debug)]
pub struct TextWrite {
    pub document_id: DocumentId,
    pub baseline: String,
    pub text: String,
}
#[derive(Debug)]
pub enum Command {
    Write {
        mode: WriteMode,
        documents: Vec<TextWrite>,
    },
    Select(DocumentId),
    OpenAuxiliary(DocumentId),
    Create,
    Open(PathBuf),
    CaptureCompletion {
        source: completion::SourceIdentity,
        cursor_byte: u64,
    },
    Close,
}
#[derive(Debug)]
pub struct WriteReply {
    pub document_id: DocumentId,
    pub baseline: Option<String>,
    pub pending: bool,
    pub source_identity: Option<completion::SourceIdentity>,
    pub result: Result<(), SessionError>,
}
#[derive(Debug)]
pub enum Response {
    Written(Vec<WriteReply>),
    Opened(Box<Snapshot>),
    Auxiliary(OpenText),
    CompletionSource(completion::CapturedSource),
    Closed,
}
#[derive(Debug)]
pub struct Reply {
    pub ticket: u64,
    pub result: Result<Response, SessionError>,
}
struct Request {
    ticket: u64,
    project_id: ProjectId,
    command: Command,
}
struct NotifyOnDrop<F: Fn()>(F);
impl<F: Fn()> Drop for NotifyOnDrop<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

#[derive(Debug)]
pub struct Client {
    requests: Option<SyncSender<Request>>,
    replies: Receiver<Reply>,
    thread: Option<JoinHandle<()>>,
    next_ticket: u64,
    pending: usize,
    close_ticket: Option<u64>,
}
impl Client {
    /// Initial admission completes before a window is created. Every subsequent
    /// operation is asynchronous and owned by this worker, including project open.
    pub fn open(
        root: &Path,
        admission: Admission,
        notify: impl Fn() + Send + 'static,
    ) -> Result<(Self, Snapshot), String> {
        let (send, requests) = mpsc::sync_channel::<Request>(MAX_PENDING);
        let (replies, receive) = mpsc::sync_channel(MAX_PENDING);
        let (ready, startup) = mpsc::sync_channel(1);
        let root = root.to_owned();
        let thread = std::thread::Builder::new()
            .name("loom-project".into())
            .spawn(move || {
                let notify = NotifyOnDrop(notify);
                let opened = TextProject::open(&root).and_then(|mut project| {
                    let initial = snapshot(&mut project, 0, admission)?;
                    Ok((project, initial))
                });
                let mut project = match opened {
                    Ok((project, initial)) => {
                        if ready.send(Ok(initial)).is_err() {
                            return;
                        }
                        project
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                for request in requests {
                    let result = if request.project_id == project.project_id() {
                        perform(&mut project, request.command, admission)
                    } else {
                        Err(SessionError::ProjectMismatch)
                    };
                    if replies
                        .send(Reply {
                            ticket: request.ticket,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    (notify.0)();
                }
                // Accepted work is drained before releasing the exclusive lease.
            })
            .map_err(|e| e.to_string())?;
        match startup.recv() {
            Ok(Ok(snapshot)) => Ok((
                Self {
                    requests: Some(send),
                    replies: receive,
                    thread: Some(thread),
                    next_ticket: 1,
                    pending: 0,
                    close_ticket: None,
                },
                snapshot,
            )),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error.to_string())
            }
            Err(error) => {
                let _ = thread.join();
                Err(error.to_string())
            }
        }
    }

    pub fn submit(&mut self, project_id: ProjectId, command: Command) -> Result<u64, String> {
        if self.close_ticket.is_some() {
            return Err("Project storage is closing".into());
        }
        if self.pending >= MAX_PENDING {
            return Err("Project storage is busy".into());
        }
        if let Command::Write { documents, .. } = &command {
            if documents.is_empty()
                || documents.len() > 2
                || documents
                    .iter()
                    .any(|d| d.text.len() > MAX_TEXT_BYTES || d.baseline.len() > MAX_TEXT_BYTES)
            {
                return Err("Invalid or oversized document batch".into());
            }
            if documents.len() == 2 && documents[0].document_id == documents[1].document_id {
                return Err("A document can appear only once in a batch".into());
            }
        }
        let ticket = self.next_ticket;
        let next = ticket
            .checked_add(1)
            .ok_or("Project request sequence exhausted")?;
        let closing = matches!(command, Command::Close);
        self.requests
            .as_ref()
            .ok_or("Project worker closed")?
            .try_send(Request {
                ticket,
                project_id,
                command,
            })
            .map_err(|e| e.to_string())?;
        self.next_ticket = next;
        self.pending += 1;
        if closing {
            self.close_ticket = Some(ticket);
        }
        Ok(ticket)
    }

    pub fn poll(&mut self) -> Result<Option<Reply>, String> {
        match self.replies.try_recv() {
            Ok(reply) => {
                self.pending = self.pending.saturating_sub(1);
                if self.close_ticket == Some(reply.ticket) && reply.result.is_err() {
                    self.close_ticket = None;
                }
                Ok(Some(reply))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err("Project worker stopped; unsaved edits remain in the view".into())
            }
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.requests.take();
        // At most MAX_PENDING outstanding replies fit without a UI consumer.
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            eprintln!("Project worker failed during shutdown");
        }
    }
}

fn snapshot(
    project: &mut TextProject,
    selected: usize,
    admission: Admission,
) -> Result<Snapshot, SessionError> {
    let kind = project
        .entries()
        .get(selected)
        .ok_or(SessionError::UnknownDocument)?
        .kind;
    let manuscript = open_text(project, TextTarget::Manuscript(selected), kind, admission)?;
    Ok(Snapshot {
        project: ProjectInfo {
            project_id: project.project_id(),
            root: project.root().into(),
            documents: project.entries().to_vec(),
            settings: crate::configuration::read(&mut project.store)?,
        },
        selected,
        manuscript,
        context: None,
    })
}

fn open_text(
    project: &mut TextProject,
    target: TextTarget,
    kind: DocumentKind,
    admission: Admission,
) -> Result<OpenText, SessionError> {
    let text = project.read(target)?;
    admission(&text, kind).map_err(SessionError::Admission)?;
    Ok(OpenText {
        document_id: project.document_id(target)?,
        kind,
        text,
        baseline: project.baseline(target)?.into(),
        pending: project.has_pending_writes(target),
        source_identity: project.source_identity(target)?,
    })
}

fn perform(
    project: &mut TextProject,
    command: Command,
    admission: Admission,
) -> Result<Response, SessionError> {
    match command {
        Command::Write { mode, documents } => Ok(Response::Written(
            documents
                .iter()
                .map(|write| write_one(project, mode, write))
                .collect(),
        )),
        Command::Select(id) => {
            let TextTarget::Manuscript(index) = project.target_by_id(id)?;
            snapshot(project, index, admission)
                .map(Box::new)
                .map(Response::Opened)
        }
        Command::OpenAuxiliary(id) => {
            let target = project.target_by_id(id)?;
            let kind = project.descriptor(target)?.1;
            open_text(project, target, kind, admission).map(Response::Auxiliary)
        }
        Command::Create => {
            let index = project.new_manuscript()?;
            snapshot(project, index, admission)
                .map(Box::new)
                .map(Response::Opened)
        }
        Command::Open(path) => {
            let mut next = TextProject::open(&path)?;
            let snapshot = snapshot(&mut next, 0, admission)?;
            project.close()?;
            *project = next;
            Ok(Response::Opened(Box::new(snapshot)))
        }
        Command::CaptureCompletion {
            source,
            cursor_byte,
        } => {
            let target = project.target_by_id(source.document_id)?;
            if project.source_identity(target)? != source {
                return Err(SessionError::Conflict);
            }
            if project.has_pending_writes(target) {
                return Err(SessionError::CompletionNotCheckpointed);
            }
            let relative_path = project.descriptor(target)?.0;
            let captured = completion::capture(
                &project.store,
                completion::SourceRequest {
                    document: crate::persistence::DocumentAddress {
                        document_id: source.document_id,
                        relative_path,
                    },
                    source_revision_id: source.revision_id,
                    visible_blob_id: source.visible_blob_id,
                    cursor_byte,
                },
            )?;
            if captured.document().text.len() > MAX_TEXT_BYTES {
                return Err(SessionError::TextLimit);
            }
            admission(&captured.document().text, captured.document().kind)
                .map_err(SessionError::Admission)?;
            Ok(Response::CompletionSource(captured))
        }
        Command::Close => {
            project.close()?;
            Ok(Response::Closed)
        }
    }
}

fn write_one(project: &mut TextProject, mode: WriteMode, write: &TextWrite) -> WriteReply {
    let target = project.target_by_id(write.document_id);
    let result = target
        .as_ref()
        .map_err(|_| SessionError::UnknownDocument)
        .and_then(|&target| {
            if project.baseline(target)? != write.baseline {
                return Err(SessionError::Conflict);
            }
            match mode {
                WriteMode::Journal => project.journal(target, &write.baseline, &write.text),
                WriteMode::Checkpoint => project.save(target, &write.baseline, &write.text),
                WriteMode::Discard => project.discard(target),
            }
        });
    let pending = target
        .as_ref()
        .is_ok_and(|&t| project.has_pending_writes(t));
    let baseline = if result.is_ok() {
        target
            .ok()
            .and_then(|t| project.baseline(t).ok())
            .map(str::to_owned)
    } else {
        None
    };
    let source_identity = if result.is_ok() {
        project
            .target_by_id(write.document_id)
            .ok()
            .and_then(|target| project.source_identity(target).ok())
    } else {
        None
    };
    WriteReply {
        document_id: write.document_id,
        baseline,
        pending,
        source_identity,
        result,
    }
}

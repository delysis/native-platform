//! View adapter: widgets, storage replies and presentation state. The worker owns
//! the project lease and all file I/O; the shared service owns persistence rules.
pub use crate::text_field::TextField;
use easl_native_text::TextSystem;
use loom_text_session::{
    autosave::Schedule,
    worker::{self, Command, ProjectInfo, Response, Snapshot, TextWrite, WriteMode},
};
use loom_types::DocumentId;
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
const _: () = assert!(loom_text_session::MAX_TEXT_BYTES <= easl_native_text::MAX_TEXT_BYTES);

#[derive(Debug)]
pub enum Destination {
    Document(usize),
    NewDocument,
    Project(PathBuf),
    Close,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Save,
    Discard,
}
#[derive(Debug)]
struct Transition {
    destination: Destination,
    disposition: Disposition,
}
#[derive(Clone, Debug)]
struct Capture {
    index: usize,
    revision: u64,
    write: TextWrite,
}
#[derive(Clone, Debug)]
struct Batch {
    mode: WriteMode,
    captures: Vec<Capture>,
}
#[derive(Debug)]
enum FlightKind {
    Auxiliary,
    Writes(Batch),
    Transition,
}
#[derive(Debug)]
struct Flight {
    ticket: u64,
    kind: FlightKind,
}

#[derive(Debug)]
pub struct Documents {
    pub project: ProjectInfo,
    pub workspace: crate::workspace::Workspace,
    pub selected: usize,
    pub manuscript: TextField,
    pub context: TextField,
    pub status: String,
    client: worker::Client,
    identities: [Option<DocumentId>; 2],
    pending: [bool; 2],
    journaled: [Option<u64>; 2],
    schedule: Schedule,
    flight: Option<Flight>,
    retry: Option<Batch>,
    save_requested: bool,
    transition: Option<Transition>,
    closed: bool,
    storage_error: Option<String>,
}
impl Documents {
    #[cfg(test)]
    pub fn open(root: &Path) -> Result<Self, String> {
        if !root.join(".loom").exists() {
            loom_store::ProjectStore::initialize(root, "Native view fixture")
                .map_err(|error| error.to_string())?;
        }
        Self::open_with_notify(root, || {})
    }
    pub fn open_with_notify(
        root: &Path,
        notify: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        let (client, snapshot) = worker::Client::open(root, admission, notify)?;
        let (manuscript, context) = fields(&snapshot)?;
        let recovered = manuscript.dirty() || context.dirty();
        Ok(Self {
            workspace: crate::workspace::Workspace::new(snapshot.project.project_id),
            client,
            selected: snapshot.selected,
            identities: [
                Some(snapshot.manuscript.document_id),
                snapshot.context.as_ref().map(|text| text.document_id),
            ],
            pending: [
                snapshot.manuscript.pending,
                snapshot.context.as_ref().is_some_and(|text| text.pending),
            ],
            journaled: [Some(manuscript.revision()), Some(context.revision())],
            project: snapshot.project,
            manuscript,
            context,
            status: if recovered {
                "Recovered an unsaved local draft"
            } else {
                "Saved locally"
            }
            .into(),
            schedule: Schedule::default(),
            flight: None,
            retry: None,
            save_requested: false,
            transition: None,
            closed: false,
            storage_error: None,
        })
    }
    pub fn root(&self) -> &Path {
        &self.project.root
    }
    pub fn document_id(&self) -> DocumentId {
        self.project.entries()[self.selected].document_id
    }
    pub fn can_switch_views(&self) -> bool {
        !self.transitioning() && !self.is_composing()
    }
    pub fn control_text(&self, slot: u32) -> String {
        if (100..116).contains(&slot) {
            self.project
                .entries()
                .get((slot - 100) as usize)
                .map(|entry| {
                    entry.display_title.clone().unwrap_or_else(|| {
                        Path::new(&entry.relative_path)
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    })
                })
                .unwrap_or_default()
        } else {
            self.pane_text(slot)
        }
    }
    pub fn pane_text(&self, slot: u32) -> String {
        self.workspace.text(&self.project, self.document_id(), slot)
    }
    pub fn title(&self) -> String {
        let entry = &self.project.entries()[self.selected];
        entry.display_title.clone().unwrap_or_else(|| {
            Path::new(&entry.relative_path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
    }
    pub fn dirty(&self) -> bool {
        self.manuscript.dirty()
            || self.context.dirty()
            || self.pending.iter().any(|p| *p)
            || self.retry.is_some()
    }
    pub fn busy(&self) -> bool {
        self.flight.is_some()
    }
    pub fn transitioning(&self) -> bool {
        self.transition.is_some()
            || matches!(
                self.flight.as_ref().map(|f| &f.kind),
                Some(FlightKind::Transition)
            )
    }
    pub fn closed(&self) -> bool {
        self.closed
    }
    pub fn has_auxiliary(&self) -> bool {
        self.identities[1].is_some()
    }
    pub fn source_mode(&self, id: u32) -> bool {
        match id {
            1 => self.manuscript.view_source_mode(1),
            2 => self.context.view_source_mode(1),
            3..=9 => self.manuscript.view_source_mode(id - 1),
            _ => false,
        }
    }
    pub fn field(&mut self, id: u32, system: &mut TextSystem) -> Result<&mut TextField, String> {
        let (field, view) = match id {
            1 => (&mut self.manuscript, 1),
            2 if self.identities[1].is_some() => (&mut self.context, 1),
            3..=9 => (&mut self.manuscript, id - 1),
            _ => return Err("Unknown text field".into()),
        };
        field.activate_view(view, system)?;
        Ok(field)
    }
    pub fn open_auxiliary(&mut self, document_id: DocumentId) -> Result<(), String> {
        if self.busy() || self.context.dirty() || self.pending[1] || self.context.is_composing() {
            return Err(
                "Save or finish the current pane edit before opening another document".into(),
            );
        }
        if self.identities[0] == Some(document_id) {
            return Err("This document is already open in the main editor".into());
        }
        let ticket = self
            .client
            .submit(self.project.project_id, Command::OpenAuxiliary(document_id))?;
        self.flight = Some(Flight {
            ticket,
            kind: FlightKind::Auxiliary,
        });
        Ok(())
    }
    pub fn save(&mut self, system: &mut TextSystem) -> Result<(), String> {
        if let Some(error) = &self.storage_error {
            return Err(error.clone());
        }
        self.save_requested = true;
        self.schedule.resume();
        self.tick(system, Instant::now()).map(|_| ())
    }
    pub fn begin_transition(
        &mut self,
        destination: Destination,
        disposition: Disposition,
        system: &mut TextSystem,
    ) -> Result<(), String> {
        if let Some(error) = &self.storage_error {
            return Err(error.clone());
        }
        if self.transitioning() {
            return Ok(());
        }
        self.transition = Some(Transition {
            destination,
            disposition,
        });
        self.schedule.resume();
        self.tick(system, Instant::now()).map(|_| ())
    }
    pub fn deadline(&self) -> Option<Instant> {
        if self.busy() || self.transitioning() || self.closed {
            None
        } else {
            self.schedule.deadline(!self.is_composing())
        }
    }
    fn is_composing(&self) -> bool {
        self.manuscript.is_composing() || self.context.is_composing()
    }
    /// Drain completed I/O, then admit at most one next operation. Replies never
    /// install their captured text over newer widget edits or IME presentation.
    pub fn tick(&mut self, _system: &mut TextSystem, now: Instant) -> Result<bool, String> {
        let result = self.tick_inner(now);
        if let Err(error) = &result {
            self.fail(error.clone());
        }
        result
    }
    fn tick_inner(&mut self, now: Instant) -> Result<bool, String> {
        if self.storage_error.is_some() {
            return Ok(false);
        }
        let mut changed = false;
        loop {
            let reply = match self.client.poll() {
                Ok(Some(reply)) => reply,
                Ok(None) => break,
                Err(error) => {
                    self.storage_error = Some(error.clone());
                    if let Some(Flight {
                        kind: FlightKind::Writes(batch),
                        ..
                    }) = self.flight.take()
                    {
                        self.retry = Some(batch);
                    }
                    return Err(error);
                }
            };
            let flight = self.flight.take().ok_or("Unexpected project reply")?;
            if reply.ticket != flight.ticket {
                return Err("Project reply identity mismatch".into());
            }
            self.accept(reply.result, flight.kind)?;
            changed = true;
        }
        let unjournaled = [&self.manuscript, &self.context]
            .iter()
            .enumerate()
            .any(|(i, f)| f.dirty() && self.journaled[i] != Some(f.revision()));
        self.schedule.observe(
            now,
            [self.manuscript.revision(), self.context.revision()],
            self.dirty(),
            unjournaled,
        );
        if self.busy() || self.closed {
            return Ok(changed);
        }
        // Keep the focused IME candidate available until the OS commits/cancels it.
        if self.transition.is_some() && self.is_composing() {
            let status = "Finish text composition to continue";
            if self.status != status {
                self.status = status.into();
                changed = true;
            }
            return Ok(changed);
        }
        if (self.save_requested || self.transition.is_some()) && self.retry.is_some() {
            let batch = self.retry.take().ok_or("Missing retry")?;
            self.dispatch(batch)?;
            return Ok(true);
        }
        if let Some(transition) = &self.transition {
            if self.dirty() {
                let mode = if transition.disposition == Disposition::Save {
                    WriteMode::Checkpoint
                } else {
                    WriteMode::Discard
                };
                if let Some(batch) = self.capture(mode)? {
                    self.dispatch(batch)?;
                    return Ok(true);
                }
            }
            let transition = self.transition.take().ok_or("Missing transition")?;
            let command = match transition.destination {
                Destination::Document(index) => Command::Select(
                    self.project
                        .entries()
                        .get(index)
                        .ok_or("Unknown manuscript")?
                        .document_id,
                ),
                Destination::NewDocument => Command::Create,
                Destination::Project(path) => Command::Open(path),
                Destination::Close => Command::Close,
            };
            let ticket = self.client.submit(self.project.project_id, command)?;
            self.flight = Some(Flight {
                ticket,
                kind: FlightKind::Transition,
            });
            self.status = "Opening…".into();
            return Ok(true);
        }
        let mode = if self.save_requested {
            self.save_requested = false;
            Some(WriteMode::Checkpoint)
        } else {
            self.schedule.due(now, !self.is_composing())
        };
        if let Some(mode) = mode
            && let Some(batch) = self.capture(mode)?
        {
            self.dispatch(batch)?;
            changed = true;
        }
        Ok(changed)
    }
    fn capture(&mut self, mode: WriteMode) -> Result<Option<Batch>, String> {
        let mut captures = Vec::new();
        for (index, field) in [&mut self.manuscript, &mut self.context]
            .into_iter()
            .enumerate()
        {
            let needed = match mode {
                WriteMode::Journal => {
                    field.dirty() && self.journaled[index] != Some(field.revision())
                }
                _ => field.dirty() || self.pending[index],
            };
            if !needed {
                continue;
            }
            captures.push(Capture {
                index,
                revision: field.revision(),
                write: TextWrite {
                    document_id: self.identities[index]
                        .ok_or("No document bound to this editor")?,
                    baseline: field.saved.clone(),
                    text: field.text(),
                },
            });
        }
        Ok((!captures.is_empty()).then_some(Batch { mode, captures }))
    }
    fn dispatch(&mut self, batch: Batch) -> Result<(), String> {
        let command = Command::Write {
            mode: batch.mode,
            documents: batch.captures.iter().map(|c| c.write.clone()).collect(),
        };
        let ticket = self.client.submit(self.project.project_id, command)?;
        self.status = match batch.mode {
            WriteMode::Journal => "Protecting local draft…",
            WriteMode::Checkpoint => "Saving locally…",
            WriteMode::Discard => "Discarding unsaved changes…",
        }
        .into();
        self.flight = Some(Flight {
            ticket,
            kind: FlightKind::Writes(batch),
        });
        Ok(())
    }
    fn accept(
        &mut self,
        result: Result<Response, loom_text_session::SessionError>,
        kind: FlightKind,
    ) -> Result<(), String> {
        match (result, kind) {
            (Ok(Response::Written(replies)), FlightKind::Writes(batch)) => {
                self.accept_writes(replies, batch)
            }
            (Ok(Response::Opened(snapshot)), FlightKind::Transition) => {
                let (manuscript, context) = fields(&snapshot)?;
                self.identities = [
                    Some(snapshot.manuscript.document_id),
                    snapshot.context.as_ref().map(|text| text.document_id),
                ];
                self.pending = [
                    snapshot.manuscript.pending,
                    snapshot.context.as_ref().is_some_and(|text| text.pending),
                ];
                self.journaled = [Some(manuscript.revision()), Some(context.revision())];
                self.workspace.reset_project(snapshot.project.project_id);
                self.project = snapshot.project;
                self.selected = snapshot.selected;
                self.manuscript = manuscript;
                self.context = context;
                self.retry = None;
                self.save_requested = false;
                self.schedule = Schedule::default();
                self.status = if self.dirty() {
                    "Recovered an unsaved local draft"
                } else {
                    "Opened local manuscript"
                }
                .into();
                Ok(())
            }
            (Ok(Response::Auxiliary(text)), FlightKind::Auxiliary) => {
                let mut field = TextField::with_kind(
                    &text.text,
                    true,
                    text.kind == loom_types::DocumentKind::Verse,
                )?;
                field.saved = text.baseline;
                self.identities[1] = Some(text.document_id);
                self.pending[1] = text.pending;
                self.journaled[1] = Some(field.revision());
                self.context = field;
                Ok(())
            }
            (Ok(Response::Closed), FlightKind::Transition) => {
                self.closed = true;
                Ok(())
            }
            (Err(error), kind) => {
                if let FlightKind::Writes(batch) = kind {
                    self.retry = Some(batch);
                }
                self.fail(error.to_string());
                Ok(())
            }
            _ => Err("Unexpected project response kind".into()),
        }
    }
    fn accept_writes(
        &mut self,
        replies: Vec<worker::WriteReply>,
        batch: Batch,
    ) -> Result<(), String> {
        if replies.len() != batch.captures.len()
            || replies
                .iter()
                .zip(&batch.captures)
                .any(|(r, c)| r.document_id != c.write.document_id)
        {
            return Err("Project write reply identity mismatch".into());
        }
        let mut failed = Vec::new();
        let mut error = None;
        for (reply, capture) in replies.into_iter().zip(batch.captures) {
            let field = if capture.index == 0 {
                &mut self.manuscript
            } else {
                &mut self.context
            };
            self.pending[capture.index] = reply.pending;
            if let Err(problem) = reply.result {
                error = Some(problem.to_string());
                failed.push(capture);
                continue;
            }
            field.saved = reply.baseline.ok_or("Saved source missing from reply")?;
            self.journaled[capture.index] = Some(capture.revision);
            if batch.mode == WriteMode::Discard
                && self
                    .transition
                    .as_ref()
                    .is_some_and(|t| t.disposition == Disposition::Discard)
                && field.revision() == capture.revision
            {
                *field = TextField::with_kind(&field.saved, capture.index == 0, field.verse())?;
            }
        }
        if let Some(error) = error {
            self.retry = Some(Batch {
                mode: batch.mode,
                captures: failed,
            });
            self.fail(error);
        } else {
            self.status = if batch.mode == WriteMode::Journal {
                "Draft protected locally"
            } else if self.dirty() {
                "Newer edits waiting to save"
            } else if batch.mode == WriteMode::Discard {
                "Unsaved changes discarded"
            } else {
                "All changes saved locally"
            }
            .into();
        }
        Ok(())
    }
    fn fail(&mut self, error: String) {
        self.status = error;
        self.schedule.pause();
        self.save_requested = false;
        self.transition = None;
    }
    #[cfg(test)]
    pub fn path(&self) -> PathBuf {
        self.root()
            .join(&self.project.entries()[self.selected].relative_path)
    }
    #[cfg(test)]
    fn settle(&mut self, fonts: &mut TextSystem) -> Result<(), String> {
        self.settle_at(fonts, Instant::now())
    }
    #[cfg(test)]
    fn settle_at(&mut self, fonts: &mut TextSystem, now: Instant) -> Result<(), String> {
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while self.busy() || self.transitioning() || self.save_requested {
            if Instant::now() >= deadline {
                let (stage, ticket) = self.flight.as_ref().map_or(("idle", None), |flight| {
                    let stage = match &flight.kind {
                        FlightKind::Auxiliary => "auxiliary",
                        FlightKind::Transition => "transition",
                        FlightKind::Writes(batch) => match batch.mode {
                            WriteMode::Journal => "journal",
                            WriteMode::Checkpoint => "checkpoint",
                            WriteMode::Discard => "discard",
                        },
                    };
                    (stage, Some(flight.ticket))
                });
                // Never Debug-print Flight/Batch: their captures contain text.
                return Err(format!(
                    "Storage response timeout: stage={stage}, ticket={ticket:?}, \
                     transition={}, save_requested={}, retry={}, composing={}",
                    self.transitioning(),
                    self.save_requested,
                    self.retry.is_some(),
                    self.is_composing(),
                ));
            }
            self.tick(fonts, now)?;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if self.retry.is_some() {
            Err(self.status.clone())
        } else {
            Ok(())
        }
    }
}
fn admission(text: &str, kind: loom_types::DocumentKind) -> Result<(), String> {
    crate::document_model::create(text, kind == loom_types::DocumentKind::Verse).map(|_| ())
}
fn fields(snapshot: &Snapshot) -> Result<(TextField, TextField), String> {
    let mut manuscript = TextField::with_kind(
        &snapshot.manuscript.text,
        true,
        snapshot.manuscript.kind == loom_types::DocumentKind::Verse,
    )?;
    manuscript.saved.clone_from(&snapshot.manuscript.baseline);
    let mut context = TextField::new(
        snapshot
            .context
            .as_ref()
            .map_or("", |text| text.text.as_str()),
        false,
    )?;
    if let Some(text) = &snapshot.context {
        context.saved.clone_from(&text.baseline);
    }
    Ok((manuscript, context))
}
#[cfg(test)]
mod tests {
    use super::*;
    use loom_text_session::{TextProject, TextTarget};

    #[test]
    fn pane_edits_save_once_through_the_original_document_and_share_undo() {
        let directory = tempfile::tempdir().unwrap();
        let mut system = TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        docs.field(3, &mut system)
            .unwrap()
            .command(
                &mut system,
                easl_native_text::EditCommand::Insert("from the right pane".into()),
            )
            .unwrap();
        assert!(docs.dirty());
        assert_eq!(
            docs.field(1, &mut system).unwrap().editor.text(),
            "from the right pane"
        );
        docs.save(&mut system).unwrap();
        docs.settle(&mut system).unwrap();
        assert!(!docs.dirty());
        assert_eq!(
            std::fs::read_to_string(docs.path()).unwrap(),
            "from the right pane"
        );
        docs.field(4, &mut system)
            .unwrap()
            .command(&mut system, easl_native_text::EditCommand::Undo)
            .unwrap();
        assert_eq!(docs.manuscript.text(), "");
        assert!(docs.dirty());
        assert_eq!(docs.project.entries().len(), 1);
        docs.save(&mut system).unwrap();
        docs.settle(&mut system).unwrap();
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), "");
    }
    fn insert(field: &mut TextField, fonts: &mut TextSystem, text: &str) {
        field
            .command(fonts, easl_native_text::EditCommand::Insert(text.into()))
            .unwrap();
    }

    #[test]
    fn scheduled_journal_and_idle_checkpoint_persist_without_a_save_command() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        insert(&mut docs.manuscript, &mut fonts, "human café");
        let now = Instant::now();
        docs.tick(&mut fonts, now).unwrap();
        let journal = now + loom_text_session::autosave::DRAFT_INTERVAL;
        docs.tick(&mut fonts, journal).unwrap();
        assert!(docs.busy());
        docs.settle_at(&mut fonts, journal).unwrap();
        assert!(docs.dirty());
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), "");
        let draft_dir = directory.path().join(".loom/drafts");
        let protected = std::fs::read_dir(draft_dir)
            .unwrap()
            .map(|p| std::fs::read_to_string(p.unwrap().path()).unwrap())
            .collect::<Vec<_>>();
        assert!(protected.iter().any(|text| text == "human café"));
        let checkpoint = now + loom_text_session::autosave::CHECKPOINT_IDLE;
        docs.tick(&mut fonts, checkpoint).unwrap();
        docs.settle_at(&mut fonts, checkpoint).unwrap();
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), "human café");
        assert!(!docs.dirty());
    }

    #[test]
    fn idle_checkpoint_waits_for_composition_and_resumes_after_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        docs.manuscript.toggle_source(&mut fonts).unwrap();
        insert(&mut docs.manuscript, &mut fonts, "human\r\n");
        docs.manuscript
            .command(
                &mut fonts,
                easl_native_text::EditCommand::Preedit("候補".into(), None),
            )
            .unwrap();
        let now = Instant::now();
        docs.tick(&mut fonts, now).unwrap();
        let later = now + std::time::Duration::from_secs(10);
        docs.tick(&mut fonts, later).unwrap();
        docs.settle_at(&mut fonts, later).unwrap();
        assert!(docs.manuscript.is_composing());
        assert!(docs.deadline().is_none());
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), "");
        assert!(docs.pending[0]);
        docs.manuscript
            .command(&mut fonts, easl_native_text::EditCommand::CancelCompose)
            .unwrap();
        docs.tick(&mut fonts, later).unwrap();
        docs.settle_at(&mut fonts, later).unwrap();
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), "human\r\n");
        assert!(!docs.dirty());
    }

    #[test]
    fn a_transition_waits_for_the_existing_ime_candidate_and_saves_its_commit() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        let path = docs.path();
        insert(&mut docs.manuscript, &mut fonts, "human");
        docs.manuscript
            .command(
                &mut fonts,
                easl_native_text::EditCommand::Preedit("候補".into(), None),
            )
            .unwrap();
        docs.begin_transition(Destination::NewDocument, Disposition::Save, &mut fonts)
            .unwrap();
        assert!(docs.transitioning());
        assert!(!docs.busy());
        assert!(docs.manuscript.is_composing());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        // The host calls tick before sleeping. An unchanged candidate must not
        // request another frame and keep the event loop spinning.
        assert!(!docs.tick(&mut fonts, Instant::now()).unwrap());
        docs.manuscript
            .command(
                &mut fonts,
                easl_native_text::EditCommand::Commit("確定".into()),
            )
            .unwrap();
        docs.settle(&mut fonts).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "human確定");
        assert_eq!(docs.manuscript.text(), "");
        assert!(!docs.dirty());
    }

    #[test]
    fn discard_handles_a_journal_reply_already_queued_before_transition() {
        let directory = tempfile::tempdir().unwrap();
        loom_store::ProjectStore::initialize(directory.path(), "Journal ordering fixture").unwrap();
        let (notify, ready) = std::sync::mpsc::sync_channel(8);
        let mut docs = Documents::open_with_notify(directory.path(), move || {
            let _ = notify.try_send(());
        })
        .unwrap();
        let mut fonts = TextSystem::new();
        docs.manuscript.toggle_source(&mut fonts).unwrap();
        let original_id = docs.identities[0].unwrap();
        insert(&mut docs.manuscript, &mut fonts, "saved\r\n");
        docs.save(&mut fonts).unwrap();
        ready
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("checkpoint acknowledgement was not published");
        docs.settle(&mut fonts).unwrap();
        let original_path = docs.path();
        insert(&mut docs.manuscript, &mut fonts, "unsaved");
        let now = Instant::now();
        docs.tick(&mut fonts, now).unwrap();
        docs.tick(
            &mut fonts,
            now + loom_text_session::autosave::DRAFT_INTERVAL,
        )
        .unwrap();
        ready
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("journal acknowledgement was not published");
        // Notification follows reply publication. The client has deliberately
        // not drained it yet: this is a known interleaving, not a sleep race.
        assert!(docs.busy());
        assert!(matches!(
            docs.flight.as_ref().map(|flight| &flight.kind),
            Some(FlightKind::Writes(batch)) if batch.mode == WriteMode::Journal
        ));
        docs.begin_transition(Destination::NewDocument, Disposition::Discard, &mut fonts)
            .unwrap();
        docs.settle(&mut fonts).unwrap();
        assert_eq!(
            std::fs::read(&original_path).unwrap().as_slice(),
            b"saved\r\n"
        );
        let original = docs
            .project
            .entries()
            .iter()
            .position(|entry| entry.document_id == original_id)
            .unwrap();
        docs.begin_transition(
            Destination::Document(original),
            Disposition::Save,
            &mut fonts,
        )
        .unwrap();
        docs.settle(&mut fonts).unwrap();
        assert_eq!(docs.manuscript.source().as_bytes(), b"saved\r\n");
        assert!(!docs.dirty());
    }

    #[test]
    fn discard_waits_for_an_accepted_journal_and_leaves_only_the_saved_source() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        let original_id = docs.identities[0].unwrap();
        insert(&mut docs.manuscript, &mut fonts, "saved");
        docs.save(&mut fonts).unwrap();
        docs.settle(&mut fonts).unwrap();
        let path = docs.path();
        insert(&mut docs.manuscript, &mut fonts, " unsaved");
        let now = Instant::now();
        docs.tick(&mut fonts, now).unwrap();
        docs.tick(
            &mut fonts,
            now + loom_text_session::autosave::DRAFT_INTERVAL,
        )
        .unwrap();
        assert!(docs.busy());
        docs.begin_transition(Destination::NewDocument, Disposition::Discard, &mut fonts)
            .unwrap();
        docs.settle(&mut fonts).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "saved");
        let original = docs
            .project
            .entries()
            .iter()
            .position(|d| d.document_id == original_id)
            .unwrap();
        docs.begin_transition(
            Destination::Document(original),
            Disposition::Save,
            &mut fonts,
        )
        .unwrap();
        docs.settle(&mut fonts).unwrap();
        assert_eq!(docs.manuscript.text(), "saved");
        assert!(!docs.dirty());
    }

    #[test]
    fn an_older_save_reply_never_replaces_newer_typing() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        insert(&mut docs.manuscript, &mut fonts, "first");
        docs.save(&mut fonts).unwrap();
        assert!(docs.busy());
        insert(&mut docs.manuscript, &mut fonts, " and newer");
        docs.settle(&mut fonts).unwrap();
        assert_eq!(docs.manuscript.text(), "first and newer");
        assert_eq!(docs.manuscript.saved, "first");
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), "first");
        assert!(docs.dirty());
        docs.save(&mut fonts).unwrap();
        docs.settle(&mut fonts).unwrap();
        assert_eq!(
            std::fs::read_to_string(docs.path()).unwrap(),
            "first and newer"
        );
        assert!(!docs.dirty());
    }

    #[test]
    fn a_partial_failure_preserves_acknowledged_baselines_and_retries_only_failed_writes() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = TextSystem::new();
        std::fs::write(directory.path().join("Untitled.md"), "").unwrap();
        let context = directory.path().join("Zzz notes.md");
        std::fs::write(&context, "").unwrap();
        let mut docs = Documents::open(directory.path()).unwrap();
        let auxiliary = docs
            .project
            .documents
            .iter()
            .find(|document| document.relative_path == "Zzz notes.md")
            .unwrap()
            .document_id;
        docs.open_auxiliary(auxiliary).unwrap();
        docs.settle(&mut fonts).unwrap();
        std::fs::write(&context, "external").unwrap();
        insert(&mut docs.manuscript, &mut fonts, "first");
        insert(&mut docs.context, &mut fonts, "local context");
        docs.save(&mut fonts).unwrap();
        insert(&mut docs.manuscript, &mut fonts, " plus new");
        assert!(docs.settle(&mut fonts).is_err());
        assert_eq!(docs.manuscript.saved, "first");
        assert_eq!(docs.manuscript.text(), "first plus new");
        assert_eq!(docs.context.saved, "");
        assert_eq!(std::fs::read_to_string(&context).unwrap(), "external");
        docs.tick(
            &mut fonts,
            Instant::now() + std::time::Duration::from_mins(1),
        )
        .unwrap();
        assert!(!docs.busy()); // A failure cannot cause an automatic retry loop.
        std::fs::write(&context, "").unwrap();
        docs.save(&mut fonts).unwrap();
        docs.settle(&mut fonts).unwrap();
        assert_eq!(std::fs::read_to_string(&context).unwrap(), "local context");
        assert_eq!(
            std::fs::read_to_string(docs.path()).unwrap(),
            "first plus new"
        );
        assert!(!docs.dirty());
    }

    #[test]
    fn switching_waits_for_the_in_flight_save_and_the_latest_captured_edit() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        let original = docs.path();
        insert(&mut docs.manuscript, &mut fonts, "first");
        docs.save(&mut fonts).unwrap();
        insert(&mut docs.manuscript, &mut fonts, " plus latest");
        docs.begin_transition(Destination::NewDocument, Disposition::Save, &mut fonts)
            .unwrap();
        assert!(docs.transitioning());
        docs.settle(&mut fonts).unwrap();
        assert_eq!(
            std::fs::read_to_string(original).unwrap(),
            "first plus latest"
        );
        assert_eq!(docs.project.entries().len(), 2);
        assert_eq!(docs.manuscript.text(), "");
        assert!(!docs.transitioning());
        assert!(!docs.dirty());
    }
    #[test]
    fn prose_save_preserves_source_bytes_and_does_not_add_an_undo_transaction() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = easl_native_text::TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        let source = "**café**\r\n\r\n```rust\r\nA\t\r\nB\r\n```\r";
        docs.manuscript.toggle_source(&mut fonts).unwrap();
        docs.manuscript
            .command(
                &mut fonts,
                easl_native_text::EditCommand::Paste(source.into()),
            )
            .unwrap();
        assert_eq!(docs.manuscript.text(), source);
        docs.save(&mut fonts).unwrap();
        docs.settle(&mut fonts).unwrap();
        assert_eq!(docs.manuscript.text(), source);
        assert_eq!(docs.manuscript.saved, source);
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), source);
        assert!(!docs.dirty());
        docs.manuscript
            .command(&mut fonts, easl_native_text::EditCommand::Undo)
            .unwrap();
        assert_eq!(docs.manuscript.text(), "");
        assert!(docs.dirty());
        docs.manuscript
            .command(&mut fonts, easl_native_text::EditCommand::Redo)
            .unwrap();
        assert_eq!(docs.manuscript.text(), source);
        assert!(!docs.dirty());
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), source);
    }

    #[test]
    fn saving_during_composition_preserves_the_candidate_and_saves_only_committed_source() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = easl_native_text::TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        docs.manuscript
            .command(
                &mut fonts,
                easl_native_text::EditCommand::Insert("human".into()),
            )
            .unwrap();
        docs.manuscript
            .command(
                &mut fonts,
                easl_native_text::EditCommand::Preedit("候補".into(), None),
            )
            .unwrap();
        docs.save(&mut fonts).unwrap();
        docs.settle(&mut fonts).unwrap();
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), "human");
        assert!(docs.manuscript.editor.inner().raw_text().contains("候補"));
        docs.manuscript
            .command(
                &mut fonts,
                easl_native_text::EditCommand::Commit("確定".into()),
            )
            .unwrap();
        assert_eq!(docs.manuscript.text(), "human確定");
        assert!(docs.dirty());
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), "human");
    }
    #[test]
    fn a_recovered_draft_is_visible_dirty_and_consumed_by_a_confirmed_save() {
        let mut fonts = easl_native_text::TextSystem::new();
        let directory = tempfile::tempdir().unwrap();
        // This fixture exercises draft recovery, not the OS credential store.
        drop(loom_store::ProjectStore::open_folder(directory.path()).unwrap());
        let mut project = TextProject::open(directory.path()).unwrap();
        let target = TextTarget::Manuscript(0);
        let baseline = project.read(target).unwrap();
        project
            .journal(target, &baseline, "**recovered café**")
            .unwrap();
        drop(project);
        let mut docs = Documents::open(directory.path()).unwrap();
        assert_eq!(docs.manuscript.text(), "**recovered café**");
        assert_eq!(docs.manuscript.saved, baseline);
        assert!(docs.dirty());
        assert_eq!(docs.status, "Recovered an unsaved local draft");
        docs.save(&mut fonts).unwrap();
        docs.settle(&mut fonts).unwrap();
        assert!(!docs.dirty());
        assert_eq!(
            std::fs::read_to_string(docs.path()).unwrap(),
            "**recovered café**"
        );
        drop(docs);
        let docs = Documents::open(directory.path()).unwrap();
        assert!(!docs.dirty());
        assert_eq!(docs.manuscript.text(), "**recovered café**");
    }
    #[test]
    fn unicode_edit_undo_and_store_reopen_preserve_prose() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = easl_native_text::TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        docs.manuscript
            .command(
                &mut fonts,
                easl_native_text::EditCommand::Insert("café — 你好\n".into()),
            )
            .unwrap();
        assert!(docs.dirty());
        docs.manuscript
            .command(&mut fonts, easl_native_text::EditCommand::Undo)
            .unwrap();
        assert_eq!(docs.manuscript.text(), "");
        docs.manuscript
            .command(&mut fonts, easl_native_text::EditCommand::Redo)
            .unwrap();
        assert_eq!(docs.manuscript.text(), "café — 你好\n\n");
        docs.save(&mut fonts).unwrap();
        docs.settle(&mut fonts).unwrap();
        assert_eq!(
            std::fs::read_to_string(docs.path()).unwrap(),
            "café — 你好\n\n"
        );
        docs.begin_transition(Destination::Close, Disposition::Save, &mut fonts)
            .unwrap();
        docs.settle(&mut fonts).unwrap();
        drop(docs);
        let docs = Documents::open(directory.path()).unwrap();
        assert_eq!(docs.manuscript.text(), "café — 你好\n\n");
        assert!(!docs.dirty());
    }
    #[test]
    fn external_edit_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let mut fonts = easl_native_text::TextSystem::new();
        let mut docs = Documents::open(directory.path()).unwrap();
        docs.manuscript
            .command(
                &mut fonts,
                easl_native_text::EditCommand::Insert("draft".into()),
            )
            .unwrap();
        std::fs::write(docs.path(), "outside").unwrap();
        docs.save(&mut fonts).unwrap();
        assert!(docs.settle(&mut fonts).is_err());
        assert_eq!(std::fs::read_to_string(docs.path()).unwrap(), "outside");
    }
}

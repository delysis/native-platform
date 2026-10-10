use std::collections::BTreeMap;

use loom_document::DocumentContent;
use loom_store::{IdempotentSaveOutcome, LoadedDocument, TransientDraft, VisibleProjectionState};
use loom_types::{CommandId, DocumentKind};

use crate::{MAX_TEXT_BYTES, SessionError, TextProject, TextTarget, persistence};

#[derive(Debug)]
struct PendingCheckpoint {
    command_id: CommandId,
    text: String,
    draft_version: u64,
    outcome: Option<IdempotentSaveOutcome>,
}

#[derive(Debug)]
struct CapturedText {
    baseline: LoadedDocument,
    draft: Option<TransientDraft>,
    pending: Option<PendingCheckpoint>,
}

#[derive(Debug, Default)]
pub(super) struct OpenDocuments(BTreeMap<String, CapturedText>);

impl TextProject {
    pub fn project_id(&self) -> loom_types::ProjectId {
        self.store.manifest().project_id
    }

    pub fn document_id(&self, target: TextTarget) -> Result<loom_types::DocumentId, SessionError> {
        let path = self.descriptor(target)?.0;
        Ok(self
            .loaded
            .0
            .get(path)
            .ok_or(SessionError::DocumentNotLoaded)?
            .baseline
            .document_id)
    }

    pub fn target_by_id(&self, id: loom_types::DocumentId) -> Result<TextTarget, SessionError> {
        if let Some(index) = self.entries.iter().position(|d| d.document_id == id) {
            return Ok(TextTarget::Manuscript(index));
        }
        Err(SessionError::UnknownDocument)
    }
    /// Open exact source identity and recover a draft only when it names that
    /// same document, kind and revision. The view reads `baseline` separately.
    pub fn read(&mut self, target: TextTarget) -> Result<String, SessionError> {
        let path = self.descriptor(target)?.0.to_owned();
        if self
            .loaded
            .0
            .get(&path)
            .is_some_and(|s| s.pending.is_some())
        {
            return Err(SessionError::RecoveryRequired);
        }
        let baseline = self.store.read_document(&path)?;
        let TextTarget::Manuscript(index) = target;
        if self.entries[index].document_id != baseline.document_id {
            return Err(SessionError::Conflict);
        }
        check_size(&baseline.text)?;
        let draft = self.store.load_transient_draft(&path)?;
        if let Some(draft) = &draft {
            check_size(&draft.text)?;
            if draft.document_id != baseline.document_id
                || draft.source_revision_id != baseline.revision_id
                || draft.kind != baseline.kind
            {
                return Err(SessionError::StaleDraft);
            }
        }
        let text = draft.as_ref().map_or(&baseline.text, |d| &d.text).clone();
        self.loaded.0.insert(
            path,
            CapturedText {
                baseline,
                draft,
                pending: None,
            },
        );
        Ok(text)
    }

    pub fn baseline(&self, target: TextTarget) -> Result<&str, SessionError> {
        let path = self.descriptor(target)?.0;
        Ok(&self
            .loaded
            .0
            .get(path)
            .ok_or(SessionError::DocumentNotLoaded)?
            .baseline
            .text)
    }

    pub fn source_identity(
        &self,
        target: TextTarget,
    ) -> Result<crate::completion::SourceIdentity, SessionError> {
        let path = self.descriptor(target)?.0;
        let state = self
            .loaded
            .0
            .get(path)
            .ok_or(SessionError::DocumentNotLoaded)?;
        Ok(crate::completion::SourceIdentity::of(&state.baseline))
    }

    pub fn has_pending_writes(&self, target: TextTarget) -> bool {
        self.descriptor(target)
            .ok()
            .and_then(|(p, _)| self.loaded.0.get(p))
            .is_some_and(|s| s.draft.is_some() || s.pending.is_some())
    }

    /// Journal committed editor text, never temporary IME presentation. This
    /// uses the same bounded draft slots and exact retry rules as the webview.
    pub fn journal(
        &mut self,
        target: TextTarget,
        baseline: &str,
        text: &str,
    ) -> Result<(), SessionError> {
        check_size(text)?;
        let (path, kind) = self.descriptor(target)?;
        let path = path.to_owned();
        let state = self
            .loaded
            .0
            .get_mut(&path)
            .ok_or(SessionError::DocumentNotLoaded)?;
        if state.baseline.text != baseline {
            return Err(SessionError::Conflict);
        }
        if state.pending.is_some() {
            return Err(SessionError::CheckpointPending);
        }
        let draft = persistence::write_draft(
            &mut self.store,
            persistence::DraftWrite {
                document: address(&state.baseline),
                source_revision_id: state.baseline.revision_id,
                expected_version: state.draft.as_ref().map_or(0, |d| d.version),
                content: content(kind, text)?,
            },
        )?;
        state.draft = Some(draft.draft);
        Ok(())
    }

    /// Save against the captured source, retaining the exact command across an
    /// uncertain or pending projection. Only Applied advances the view baseline.
    pub fn save(
        &mut self,
        target: TextTarget,
        baseline: &str,
        text: &str,
    ) -> Result<(), SessionError> {
        check_size(text)?;
        let (path, kind) = self.descriptor(target)?;
        let path = path.to_owned();
        let state = self
            .loaded
            .0
            .get(&path)
            .ok_or(SessionError::DocumentNotLoaded)?;
        if state.baseline.text != baseline {
            return Err(SessionError::Conflict);
        }
        if let Some(pending) = &state.pending {
            if pending.text != text {
                return Err(SessionError::CheckpointPending);
            }
        } else {
            // Reverting to the saved source clears only our exact draft version.
            if text == baseline {
                return self.discard(target);
            }
            if state.draft.as_ref().is_none_or(|d| d.text != text) {
                self.journal(target, baseline, text)?;
            }
        }
        let state = self
            .loaded
            .0
            .get_mut(&path)
            .ok_or(SessionError::DocumentNotLoaded)?;
        let pending = state.pending.get_or_insert_with(|| PendingCheckpoint {
            command_id: CommandId::new(),
            text: text.to_owned(),
            draft_version: state.draft.as_ref().map_or(0, |d| d.version),
            outcome: None,
        });
        let result = persistence::checkpoint(
            &mut self.store,
            persistence::Checkpoint {
                document: address(&state.baseline),
                source_revision_id: state.baseline.revision_id,
                visible_blob_id: state.baseline.blob_id,
                command_id: pending.command_id,
                draft_version: Some(pending.draft_version),
                content: content(kind, &pending.text)?,
            },
        );
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                // This store is exclusively leased and the call has returned.
                // Absence of a durable receipt proves this command did not commit.
                // If the read itself fails, retain the request for exact retry.
                if self
                    .store
                    .load_receipt(pending.command_id)
                    .is_ok_and(|r| r.is_none())
                {
                    state.pending = None;
                }
                return Err(error.into());
            }
        };
        if outcome.visible_projection != VisibleProjectionState::Applied {
            pending.outcome = Some(outcome);
            return Err(SessionError::RecoveryRequired);
        }
        // Both prose and verse retain the authored UTF-8 bytes.
        state.baseline.text.clone_from(&pending.text);
        state.baseline.revision_id = outcome.save.revision_id;
        state.baseline.artifact_id = outcome.save.artifact_id;
        state.baseline.blob_id = outcome.save.blob_id;
        state.draft = None;
        state.pending = None;
        if let Some(entry) = self.entries.iter_mut().find(|e| e.relative_path == path) {
            entry.active_revision_id = Some(outcome.save.revision_id);
        }
        Ok(())
    }

    pub fn pending_checkpoint(&self, target: TextTarget) -> Option<&IdempotentSaveOutcome> {
        let path = self.descriptor(target).ok()?.0;
        self.loaded.0.get(path)?.pending.as_ref()?.outcome.as_ref()
    }

    pub fn discard(&mut self, target: TextTarget) -> Result<(), SessionError> {
        let path = self.descriptor(target)?.0.to_owned();
        let state = self
            .loaded
            .0
            .get_mut(&path)
            .ok_or(SessionError::DocumentNotLoaded)?;
        if state.pending.is_some() {
            return Err(SessionError::CheckpointPending);
        }
        if let Some(draft) = &state.draft {
            persistence::clear_draft(&mut self.store, address(&state.baseline), draft.version)?;
            state.draft = None;
        }
        Ok(())
    }
}

fn address(document: &LoadedDocument) -> persistence::DocumentAddress<'_> {
    persistence::DocumentAddress {
        document_id: document.document_id,
        relative_path: &document.relative_path,
    }
}

fn content(kind: DocumentKind, text: &str) -> Result<DocumentContent, SessionError> {
    match kind {
        DocumentKind::Prose => Ok(DocumentContent::Prose(text.into())),
        DocumentKind::Verse => Ok(DocumentContent::Verse(text.into())),
        DocumentKind::Hybrid => Err(SessionError::StructuredDocument),
    }
}

fn check_size(text: &str) -> Result<(), SessionError> {
    if text.len() > MAX_TEXT_BYTES {
        Err(SessionError::TextLimit)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unacknowledged_checkpoint_retries_the_same_command_without_more_history() {
        let root = tempfile::tempdir().unwrap();
        let mut project = TextProject::open(root.path()).unwrap();
        let target = TextTarget::Manuscript(0);
        let original = project.read(target).unwrap();
        let path = project.entries()[0].relative_path.clone();
        let text = "café\r\nrestored";
        project.journal(target, &original, text).unwrap();
        let state = project.loaded.0.get_mut(&path).unwrap();
        let source = state.baseline.clone();
        let command_id = CommandId::new();
        let draft_version = state.draft.as_ref().unwrap().version;
        state.pending = Some(PendingCheckpoint {
            command_id,
            text: text.into(),
            draft_version,
            outcome: None,
        });
        // Commit the captured request, then deliberately lose its acknowledgement
        // before advancing the native session's baseline.
        persistence::checkpoint(
            &mut project.store,
            persistence::Checkpoint {
                document: address(&source),
                source_revision_id: source.revision_id,
                visible_blob_id: source.blob_id,
                command_id,
                draft_version: Some(draft_version),
                content: content(source.kind, text).unwrap(),
            },
        )
        .unwrap();
        let counts = project.store.counts().unwrap();
        assert!(matches!(
            project.discard(target),
            Err(SessionError::CheckpointPending)
        ));
        assert!(matches!(
            project.save(target, &original, "different request"),
            Err(SessionError::CheckpointPending)
        ));
        project.save(target, &original, text).unwrap();
        assert_eq!(project.store.counts().unwrap(), counts);
        assert_eq!(project.baseline(target).unwrap(), "café\r\nrestored");
        assert_eq!(
            project.store.read_document(&path).unwrap().text,
            "café\r\nrestored"
        );
        assert!(!project.has_pending_writes(target));
    }
}

//! Coalesced deadlines. Hosts supply committed editor revisions and execute the
//! returned work; selection changes and IME candidates never reset the clock.
use crate::worker::WriteMode;
use std::time::{Duration, Instant};

pub const DRAFT_INTERVAL: Duration = Duration::from_millis(750);
pub const CHECKPOINT_IDLE: Duration = Duration::from_millis(900);

#[derive(Debug, Default)]
pub struct Schedule {
    revisions: Option<[u64; 2]>,
    journal: Option<Instant>,
    checkpoint: Option<Instant>,
    paused: bool,
}
impl Schedule {
    pub fn observe(&mut self, now: Instant, revisions: [u64; 2], dirty: bool, unjournaled: bool) {
        let changed = self.revisions != Some(revisions);
        self.revisions = Some(revisions);
        if self.paused {
            return;
        }
        if !dirty {
            self.journal = None;
            self.checkpoint = None;
            return;
        }
        if changed || self.checkpoint.is_none() {
            self.checkpoint = Some(now + CHECKPOINT_IDLE);
        }
        if unjournaled && self.journal.is_none() {
            self.journal = Some(now + DRAFT_INTERVAL);
        }
        if !unjournaled {
            self.journal = None;
        }
    }
    /// While composition is active, only committed-source journals are due.
    /// Keep the checkpoint deadline so it resumes as soon as composition ends.
    pub fn deadline(&self, allow_checkpoint: bool) -> Option<Instant> {
        if self.paused {
            None
        } else {
            self.journal
                .into_iter()
                .chain(self.checkpoint.filter(|_| allow_checkpoint))
                .min()
        }
    }
    pub fn due(&mut self, now: Instant, allow_checkpoint: bool) -> Option<WriteMode> {
        if self.paused {
            return None;
        }
        if allow_checkpoint && self.checkpoint.is_some_and(|t| t <= now) {
            self.checkpoint = None;
            self.journal = None;
            Some(WriteMode::Checkpoint)
        } else if self.journal.is_some_and(|t| t <= now) {
            self.journal = None;
            Some(WriteMode::Journal)
        } else {
            None
        }
    }
    pub fn pause(&mut self) {
        self.paused = true;
        self.journal = None;
        self.checkpoint = None;
    }
    pub fn resume(&mut self) {
        self.paused = false;
    }
}

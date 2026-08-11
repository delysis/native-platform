use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use loom_research_types::{
    CampaignId, StageAttemptId, StageId, TrialRunId, TrialRunOrigin, TrialRunRecord,
};
use loom_types::{ArtifactId, BlobId, ProjectId, now_unix_ms};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::file_io::{atomic_replace_private, read_bounded};
use crate::paths::{ensure_private_directory, reject_symlink_target};
use crate::provenance::insert_blob_row;
use crate::research_execution::insert_trial_run_row;
use crate::research_session::{
    ExclusiveResearchSessionLease, PersistedResearchSubjectSnapshot, ResearchSessionKind,
    ResearchSubjectLocator, load_research_subject_snapshot,
};
use crate::schema::configure;
use crate::store::DATABASE_FILE;
use crate::{
    MAX_RESEARCH_EXECUTION_RECORD_BYTES, ProjectStore, ResearchExecutionRecordKind, Result,
    StoreError,
};

pub const MAX_PERSISTED_TRIAL_JOURNAL_EVENTS: usize = 4_096;
pub const MAX_PERSISTED_CAMPAIGN_JOURNAL_EVENTS: usize = 262_144;
pub const MAX_RESEARCH_JOURNAL_EVENT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum canonical bytes associated with one journal subject, including
/// events and their attempt, budget, and search-decision witnesses.
pub const MAX_RESEARCH_JOURNAL_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct JournalAccounting {
    event_count: usize,
    total_record_bytes: u64,
}

impl JournalAccounting {
    fn preflight_new_event(
        self,
        event_index: u32,
        maximum_events: usize,
        added_record_bytes: u64,
    ) -> Result<Self> {
        let event_index =
            usize::try_from(event_index).map_err(|_| StoreError::ResearchJournalEventLimit {
                max_events: maximum_events,
            })?;
        if self.event_count >= maximum_events || event_index >= maximum_events {
            return Err(StoreError::ResearchJournalEventLimit {
                max_events: maximum_events,
            });
        }
        let total_record_bytes = self
            .total_record_bytes
            .checked_add(added_record_bytes)
            .ok_or(StoreError::ResearchJournalTotalTooLarge {
                max_bytes: MAX_RESEARCH_JOURNAL_TOTAL_BYTES,
            })?;
        if total_record_bytes > MAX_RESEARCH_JOURNAL_TOTAL_BYTES {
            return Err(StoreError::ResearchJournalTotalTooLarge {
                max_bytes: MAX_RESEARCH_JOURNAL_TOTAL_BYTES,
            });
        }
        Ok(Self {
            event_count: self.event_count + 1,
            total_record_bytes,
        })
    }
}

/// Store-neutral resource vector used by both execution journals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResearchJournalBudget {
    pub writer_tokens: u64,
    pub controller_tokens: u64,
    pub evaluations: u32,
    pub wall_time_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrialStageOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl TrialStageOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CampaignTrialOutcome {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl CampaignTrialOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchDecisionPersistenceKind {
    BlockedFactorialScheduled,
    NestedPoolRecorded,
    SuccessiveHalvingApplied,
    PressureAdvanced,
    PressureStopped,
    MapElitesInitialized,
    MapElitesAdvanced,
}

impl SearchDecisionPersistenceKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::BlockedFactorialScheduled => "blocked_factorial_scheduled",
            Self::NestedPoolRecorded => "nested_pool_recorded",
            Self::SuccessiveHalvingApplied => "successive_halving_applied",
            Self::PressureAdvanced => "pressure_advanced",
            Self::PressureStopped => "pressure_stopped",
            Self::MapElitesInitialized => "map_elites_initialized",
            Self::MapElitesAdvanced => "map_elites_advanced",
        }
    }
}

/// Database mutations causally attached to one trial event. The store knows
/// table invariants, not trial reducer semantics.
#[derive(Clone, Copy, Debug)]
pub enum TrialJournalMutation<'a> {
    Prepared,
    AttemptReserved {
        attempt_id: StageAttemptId,
        stage_id: StageId,
        attempt_ordinal: u16,
        reservation: ResearchJournalBudget,
        canonical_attempt_bytes: &'a [u8],
        canonical_reservation_bytes: &'a [u8],
    },
    AttemptStarted {
        attempt_id: StageAttemptId,
    },
    AttemptFinished {
        attempt_id: StageAttemptId,
        outcome: TrialStageOutcome,
        terminal_output_fingerprint: Option<BlobId>,
        charge: ResearchJournalBudget,
        canonical_charge_bytes: &'a [u8],
    },
    AttemptAbandoned {
        attempt_id: StageAttemptId,
    },
    TrialClosed,
}

#[derive(Clone, Copy, Debug)]
pub struct TrialJournalEventPersistence<'a> {
    pub trial_run_id: TrialRunId,
    pub trial_fingerprint: BlobId,
    pub event_index: u32,
    pub previous_event_fingerprint: Option<BlobId>,
    pub event_fingerprint: BlobId,
    pub canonical_event_bytes: &'a [u8],
    pub mutation: TrialJournalMutation<'a>,
}

/// Database mutations causally attached to one campaign event.
#[derive(Clone, Copy, Debug)]
pub enum CampaignJournalMutation<'a> {
    Prepared,
    Started,
    PauseRequested,
    Paused,
    Resumed,
    CancelRequested,
    TrialReserved {
        attempt_id: TrialRunId,
        trial_fingerprint: BlobId,
        attempt_ordinal: u16,
        reservation: ResearchJournalBudget,
        canonical_run_bytes: &'a [u8],
        canonical_attempt_bytes: &'a [u8],
        canonical_reservation_bytes: &'a [u8],
    },
    TrialDispatched {
        attempt_id: TrialRunId,
    },
    TrialFinished {
        attempt_id: TrialRunId,
        outcome: CampaignTrialOutcome,
        charge: ResearchJournalBudget,
        canonical_charge_bytes: &'a [u8],
    },
    TrialReservationReleased {
        attempt_id: TrialRunId,
    },
    SearchDecisionRecorded {
        decision_index: u32,
        kind: SearchDecisionPersistenceKind,
        parent_archive_fingerprint: Option<BlobId>,
        canonical_decision_bytes: &'a [u8],
    },
    CampaignClosed,
}

#[derive(Clone, Copy, Debug)]
pub struct CampaignJournalEventPersistence<'a> {
    pub campaign_fingerprint: BlobId,
    pub event_index: u32,
    pub previous_event_fingerprint: Option<BlobId>,
    pub event_fingerprint: BlobId,
    pub canonical_event_bytes: &'a [u8],
    pub mutation: CampaignJournalMutation<'a>,
}

/// One bounded exact event record loaded from the append-only store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistedResearchJournalEvent {
    event_index: u32,
    previous_event_fingerprint: Option<BlobId>,
    event_fingerprint: BlobId,
    record_fingerprint: BlobId,
    canonical_record_bytes: Vec<u8>,
}

impl PersistedResearchJournalEvent {
    pub const fn event_index(&self) -> u32 {
        self.event_index
    }

    pub const fn previous_event_fingerprint(&self) -> Option<BlobId> {
        self.previous_event_fingerprint
    }

    pub const fn event_fingerprint(&self) -> BlobId {
        self.event_fingerprint
    }

    pub const fn record_fingerprint(&self) -> BlobId {
        self.record_fingerprint
    }

    pub fn canonical_record_bytes(&self) -> &[u8] {
        &self.canonical_record_bytes
    }
}

/// A non-cloneable connection bound to one exact, current store lease.
///
/// The separate connection permits a campaign journal and its child trial
/// journals to commit independently without lending mutable access to the
/// entire project store. The retained OS lease prevents another process from
/// opening the project if the originating `ProjectStore` is dropped early.
///
/// ```compile_fail
/// use loom_store::ResearchJournalWriter;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<ResearchJournalWriter>();
/// ```
pub struct ResearchJournalWriter {
    root: PathBuf,
    connection: Connection,
    kind: ResearchSessionKind,
    subject_fingerprint: BlobId,
    trial_run_id: Option<TrialRunId>,
    campaign_id: CampaignId,
    project_id: ProjectId,
    session_id: ArtifactId,
    lease_fingerprint: BlobId,
    store_authority_domain_fingerprint: BlobId,
    accounting: JournalAccounting,
    session_lease: ExclusiveResearchSessionLease,
}

impl fmt::Debug for ResearchJournalWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResearchJournalWriter")
            .field("kind", &self.kind)
            .field("subject_fingerprint", &self.subject_fingerprint)
            .field("trial_run_id", &self.trial_run_id)
            .field("campaign_id", &self.campaign_id)
            .field("project_id", &self.project_id)
            .field("session_id", &self.session_id)
            .field("lease_fingerprint", &self.lease_fingerprint)
            .field(
                "store_authority_domain_fingerprint",
                &self.store_authority_domain_fingerprint,
            )
            .finish_non_exhaustive()
    }
}

impl ProjectStore {
    /// Opens an append-only writer for the exact lease minted by this open
    /// store instance. A lease from another store, project, or prior reopen is
    /// rejected before a second `SQLite` connection is created.
    pub fn open_research_journal_writer(
        &self,
        lease: ExclusiveResearchSessionLease,
    ) -> Result<ResearchJournalWriter> {
        let expected_lease_fingerprint = research_lease_fingerprint(
            self.session_nonce.as_bytes(),
            lease.kind(),
            lease.subject_fingerprint(),
            lease.record_fingerprint(),
            self.manifest.project_id,
            lease.session_id(),
        );
        if lease.project_id() != self.manifest.project_id
            || lease.lease_fingerprint() != expected_lease_fingerprint
        {
            return Err(StoreError::ResearchJournalLeaseMismatch);
        }

        let campaign_id = match lease.snapshot() {
            PersistedResearchSubjectSnapshot::Campaign(snapshot) => snapshot.campaign_id(),
            PersistedResearchSubjectSnapshot::Trial(snapshot) => snapshot.campaign_id(),
        };
        let database_path = self.root.join(".loom").join(DATABASE_FILE);
        reject_symlink_target(&database_path)?;
        let connection = Connection::open(database_path)?;
        configure(&connection)?;
        let locator = match lease.trial_run_id() {
            Some(run_id) => ResearchSubjectLocator::TrialRun(run_id),
            None => ResearchSubjectLocator::Campaign(lease.subject_fingerprint()),
        };
        let current = load_research_subject_snapshot(&connection, locator)?;
        if current.as_ref() != Some(lease.snapshot()) {
            return Err(StoreError::ResearchJournalLeaseMismatch);
        }
        let accounting =
            load_journal_accounting(&connection, lease.kind(), lease.trial_run_id(), campaign_id)?;
        let store_authority_domain_fingerprint = self.research_authority_domain_fingerprint();

        Ok(ResearchJournalWriter {
            root: self.root.clone(),
            connection,
            kind: lease.kind(),
            subject_fingerprint: lease.subject_fingerprint(),
            trial_run_id: lease.trial_run_id(),
            campaign_id,
            project_id: lease.project_id(),
            session_id: lease.session_id(),
            lease_fingerprint: lease.lease_fingerprint(),
            store_authority_domain_fingerprint,
            accounting,
            session_lease: lease,
        })
    }
}

impl ResearchJournalWriter {
    pub const fn store_authority_domain_fingerprint(&self) -> BlobId {
        self.store_authority_domain_fingerprint
    }

    pub const fn kind(&self) -> ResearchSessionKind {
        self.kind
    }

    pub const fn subject_fingerprint(&self) -> BlobId {
        self.subject_fingerprint
    }

    pub const fn trial_run_id(&self) -> Option<TrialRunId> {
        self.trial_run_id
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub const fn session_id(&self) -> ArtifactId {
        self.session_id
    }

    pub const fn lease_fingerprint(&self) -> BlobId {
        self.lease_fingerprint
    }

    pub fn append_trial_event(&mut self, input: TrialJournalEventPersistence<'_>) -> Result<()> {
        self.ensure_trial_subject(input.trial_run_id, input.trial_fingerprint)?;
        let columns = trial_event_columns(input.mutation);
        if trial_event_replay_is_exact(&self.connection, input, &columns)? {
            return Ok(());
        }
        let added_record_bytes = trial_append_record_bytes(input)?;
        let next_accounting = self.accounting.preflight_new_event(
            input.event_index,
            MAX_PERSISTED_TRIAL_JOURNAL_EVENTS,
            added_record_bytes,
        )?;
        put_record_blob(&self.root, input.canonical_event_bytes)?;
        prepare_trial_supporting_blobs(&self.root, input.mutation)?;

        let occurred_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        persist_trial_supporting_rows(
            &transaction,
            self.campaign_id,
            input.trial_run_id,
            input.event_fingerprint,
            input.mutation,
            occurred_at_ms,
        )?;
        let event_record_fingerprint = register_execution_record(
            &transaction,
            ResearchExecutionRecordKind::TrialEvent,
            input.canonical_event_bytes,
            occurred_at_ms,
        )?;
        let (event_kind, attempt_id, attempt_outcome, terminal_output_fingerprint) = columns;
        transaction.execute(
            "INSERT OR IGNORE INTO research_trial_events(
                trial_run_id, trial_fingerprint, event_index, previous_event_fingerprint,
                event_fingerprint, event_kind, stage_attempt_id,
                attempt_outcome, terminal_output_fingerprint,
                record_fingerprint, occurred_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                input.trial_run_id.to_string(),
                input.trial_fingerprint.to_string(),
                i64::from(input.event_index),
                input
                    .previous_event_fingerprint
                    .map(|value| value.to_string()),
                input.event_fingerprint.to_string(),
                event_kind,
                attempt_id.as_deref(),
                attempt_outcome,
                terminal_output_fingerprint.map(|value| value.to_string()),
                event_record_fingerprint.to_string(),
                occurred_at_ms,
            ],
        )?;
        verify_trial_event_row(
            &transaction,
            input,
            event_kind,
            attempt_id,
            attempt_outcome,
            terminal_output_fingerprint,
            event_record_fingerprint,
        )?;
        transaction.commit()?;
        self.accounting = next_accounting;
        Ok(())
    }

    pub fn append_campaign_event(
        &mut self,
        input: CampaignJournalEventPersistence<'_>,
    ) -> Result<()> {
        self.ensure_subject(ResearchSessionKind::Campaign, input.campaign_fingerprint)?;
        let columns = campaign_event_columns(input.mutation);
        if campaign_event_replay_is_exact(&self.connection, self.campaign_id, &input, &columns)? {
            return Ok(());
        }
        let added_record_bytes = campaign_append_record_bytes(&input)?;
        let next_accounting = self.accounting.preflight_new_event(
            input.event_index,
            MAX_PERSISTED_CAMPAIGN_JOURNAL_EVENTS,
            added_record_bytes,
        )?;
        put_record_blob(&self.root, input.canonical_event_bytes)?;
        prepare_campaign_supporting_blobs(&self.root, input.mutation)?;

        let occurred_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        persist_campaign_supporting_rows(
            &transaction,
            self.campaign_id,
            input.event_fingerprint,
            input.mutation,
            occurred_at_ms,
        )?;
        let event_record_fingerprint = register_execution_record(
            &transaction,
            ResearchExecutionRecordKind::CampaignEvent,
            input.canonical_event_bytes,
            occurred_at_ms,
        )?;
        let (event_kind, attempt_id, attempt_outcome, terminal_output_fingerprint) = columns;
        debug_assert!(terminal_output_fingerprint.is_none());
        transaction.execute(
            "INSERT OR IGNORE INTO research_campaign_events(
                campaign_id, event_index, previous_event_fingerprint,
                event_fingerprint, event_kind, trial_attempt_id,
                attempt_outcome, record_fingerprint, occurred_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                self.campaign_id.to_string(),
                i64::from(input.event_index),
                input
                    .previous_event_fingerprint
                    .map(|value| value.to_string()),
                input.event_fingerprint.to_string(),
                event_kind,
                attempt_id.as_deref(),
                attempt_outcome,
                event_record_fingerprint.to_string(),
                occurred_at_ms,
            ],
        )?;
        verify_campaign_event_row(
            &transaction,
            self.campaign_id,
            &input,
            event_kind,
            attempt_id,
            attempt_outcome,
            event_record_fingerprint,
        )?;
        transaction.commit()?;
        self.accounting = next_accounting;
        Ok(())
    }

    pub fn load_trial_events(&self) -> Result<Vec<PersistedResearchJournalEvent>> {
        let run_id = self
            .trial_run_id
            .ok_or(StoreError::ResearchJournalLeaseMismatch)?;
        self.ensure_trial_subject(run_id, self.trial_fingerprint()?)?;
        let subject = run_id.to_string();
        let expected_count = load_raw_event_count(
            &self.connection,
            "SELECT COUNT(*) FROM research_trial_events WHERE trial_run_id = ?1",
            &subject,
            MAX_PERSISTED_TRIAL_JOURNAL_EVENTS,
        )?;
        let events = load_journal_events(
            &self.connection,
            &self.root,
            "SELECT event.event_index, event.previous_event_fingerprint,
                    event.event_fingerprint, event.record_fingerprint,
                    record.record_blob_id, blob.byte_len
             FROM research_trial_events event
             JOIN research_execution_records record
               ON record.record_fingerprint = event.record_fingerprint
              AND record.record_kind = 'trial_event'
             JOIN blobs blob ON blob.blob_id = record.record_blob_id
             WHERE event.trial_run_id = ?1
             ORDER BY event.event_index",
            &subject,
            MAX_PERSISTED_TRIAL_JOURNAL_EVENTS,
        )?;
        require_loaded_count(events.len(), expected_count)?;
        Ok(events)
    }

    pub fn load_campaign_events(&self) -> Result<Vec<PersistedResearchJournalEvent>> {
        self.ensure_subject(ResearchSessionKind::Campaign, self.subject_fingerprint)?;
        let campaign_id = self.campaign_id.to_string();
        let expected_count = load_raw_event_count(
            &self.connection,
            "SELECT COUNT(*) FROM research_campaign_events WHERE campaign_id = ?1",
            &campaign_id,
            MAX_PERSISTED_CAMPAIGN_JOURNAL_EVENTS,
        )?;
        let events = load_journal_events(
            &self.connection,
            &self.root,
            "SELECT event.event_index, event.previous_event_fingerprint,
                    event.event_fingerprint, event.record_fingerprint,
                    record.record_blob_id, blob.byte_len
             FROM research_campaign_events event
             JOIN research_execution_records record
               ON record.record_fingerprint = event.record_fingerprint
              AND record.record_kind = 'campaign_event'
             JOIN blobs blob ON blob.blob_id = record.record_blob_id
             WHERE event.campaign_id = ?1
             ORDER BY event.event_index",
            &campaign_id,
            MAX_PERSISTED_CAMPAIGN_JOURNAL_EVENTS,
        )?;
        require_loaded_count(events.len(), expected_count)?;
        Ok(events)
    }

    fn ensure_subject(&self, kind: ResearchSessionKind, subject: BlobId) -> Result<()> {
        if self.kind != kind || self.subject_fingerprint != subject {
            return Err(StoreError::ResearchJournalLeaseMismatch);
        }
        Ok(())
    }

    fn ensure_trial_subject(&self, run_id: TrialRunId, trial_fingerprint: BlobId) -> Result<()> {
        if self.kind != ResearchSessionKind::Trial
            || self.trial_run_id != Some(run_id)
            || self.trial_fingerprint()? != trial_fingerprint
        {
            return Err(StoreError::ResearchJournalLeaseMismatch);
        }
        Ok(())
    }

    fn trial_fingerprint(&self) -> Result<BlobId> {
        match self.session_lease.snapshot() {
            PersistedResearchSubjectSnapshot::Trial(snapshot) => Ok(snapshot.trial_fingerprint()),
            PersistedResearchSubjectSnapshot::Campaign(_) => {
                Err(StoreError::ResearchJournalLeaseMismatch)
            }
        }
    }
}

fn trial_append_record_bytes(input: TrialJournalEventPersistence<'_>) -> Result<u64> {
    let supporting = match input.mutation {
        TrialJournalMutation::AttemptReserved {
            canonical_attempt_bytes,
            canonical_reservation_bytes,
            ..
        } => [
            Some(canonical_attempt_bytes),
            Some(canonical_reservation_bytes),
            None,
        ],
        TrialJournalMutation::AttemptFinished {
            canonical_charge_bytes,
            ..
        } => [Some(canonical_charge_bytes), None, None],
        TrialJournalMutation::Prepared
        | TrialJournalMutation::AttemptStarted { .. }
        | TrialJournalMutation::AttemptAbandoned { .. }
        | TrialJournalMutation::TrialClosed => [None, None, None],
    };
    append_record_bytes(input.canonical_event_bytes, &supporting)
}

fn campaign_append_record_bytes(input: &CampaignJournalEventPersistence<'_>) -> Result<u64> {
    let supporting = match input.mutation {
        CampaignJournalMutation::TrialReserved {
            canonical_run_bytes,
            canonical_attempt_bytes,
            canonical_reservation_bytes,
            ..
        } => [
            Some(canonical_run_bytes),
            Some(canonical_attempt_bytes),
            Some(canonical_reservation_bytes),
        ],
        CampaignJournalMutation::TrialFinished {
            canonical_charge_bytes,
            ..
        } => [Some(canonical_charge_bytes), None, None],
        CampaignJournalMutation::SearchDecisionRecorded {
            canonical_decision_bytes,
            ..
        } => [Some(canonical_decision_bytes), None, None],
        CampaignJournalMutation::Prepared
        | CampaignJournalMutation::Started
        | CampaignJournalMutation::PauseRequested
        | CampaignJournalMutation::Paused
        | CampaignJournalMutation::Resumed
        | CampaignJournalMutation::CancelRequested
        | CampaignJournalMutation::TrialDispatched { .. }
        | CampaignJournalMutation::TrialReservationReleased { .. }
        | CampaignJournalMutation::CampaignClosed => [None, None, None],
    };
    append_record_bytes(input.canonical_event_bytes, &supporting)
}

fn append_record_bytes(event: &[u8], supporting: &[Option<&[u8]>]) -> Result<u64> {
    ensure_journal_record_size(event)?;
    let mut total =
        u64::try_from(event.len()).map_err(|_| StoreError::ResearchJournalTotalTooLarge {
            max_bytes: MAX_RESEARCH_JOURNAL_TOTAL_BYTES,
        })?;
    for bytes in supporting.iter().flatten() {
        ensure_journal_record_size(bytes)?;
        total = total
            .checked_add(u64::try_from(bytes.len()).map_err(|_| {
                StoreError::ResearchJournalTotalTooLarge {
                    max_bytes: MAX_RESEARCH_JOURNAL_TOTAL_BYTES,
                }
            })?)
            .ok_or(StoreError::ResearchJournalTotalTooLarge {
                max_bytes: MAX_RESEARCH_JOURNAL_TOTAL_BYTES,
            })?;
    }
    Ok(total)
}

fn prepare_trial_supporting_blobs(root: &Path, mutation: TrialJournalMutation<'_>) -> Result<()> {
    match mutation {
        TrialJournalMutation::AttemptReserved {
            canonical_attempt_bytes,
            canonical_reservation_bytes,
            ..
        } => {
            ensure_journal_record_size(canonical_attempt_bytes)?;
            ensure_journal_record_size(canonical_reservation_bytes)?;
            put_record_blob(root, canonical_attempt_bytes)?;
            put_record_blob(root, canonical_reservation_bytes)?;
        }
        TrialJournalMutation::AttemptFinished {
            canonical_charge_bytes,
            ..
        } => {
            ensure_journal_record_size(canonical_charge_bytes)?;
            put_record_blob(root, canonical_charge_bytes)?;
        }
        TrialJournalMutation::Prepared
        | TrialJournalMutation::AttemptStarted { .. }
        | TrialJournalMutation::AttemptAbandoned { .. }
        | TrialJournalMutation::TrialClosed => {}
    }
    Ok(())
}

fn prepare_campaign_supporting_blobs(
    root: &Path,
    mutation: CampaignJournalMutation<'_>,
) -> Result<()> {
    match mutation {
        CampaignJournalMutation::TrialReserved {
            canonical_run_bytes,
            canonical_attempt_bytes,
            canonical_reservation_bytes,
            ..
        } => {
            ensure_journal_record_size(canonical_run_bytes)?;
            ensure_journal_record_size(canonical_attempt_bytes)?;
            ensure_journal_record_size(canonical_reservation_bytes)?;
            put_record_blob(root, canonical_run_bytes)?;
            put_record_blob(root, canonical_attempt_bytes)?;
            put_record_blob(root, canonical_reservation_bytes)?;
        }
        CampaignJournalMutation::TrialFinished {
            canonical_charge_bytes,
            ..
        } => {
            ensure_journal_record_size(canonical_charge_bytes)?;
            put_record_blob(root, canonical_charge_bytes)?;
        }
        CampaignJournalMutation::SearchDecisionRecorded {
            canonical_decision_bytes,
            ..
        } => {
            ensure_journal_record_size(canonical_decision_bytes)?;
            put_record_blob(root, canonical_decision_bytes)?;
        }
        CampaignJournalMutation::Prepared
        | CampaignJournalMutation::Started
        | CampaignJournalMutation::PauseRequested
        | CampaignJournalMutation::Paused
        | CampaignJournalMutation::Resumed
        | CampaignJournalMutation::CancelRequested
        | CampaignJournalMutation::TrialDispatched { .. }
        | CampaignJournalMutation::TrialReservationReleased { .. }
        | CampaignJournalMutation::CampaignClosed => {}
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct RowPersistenceContext<'transaction, 'connection> {
    transaction: &'transaction Transaction<'connection>,
    campaign_id: CampaignId,
    conflict_subject: BlobId,
    occurred_at_ms: i64,
}

#[derive(Clone, Copy)]
struct StageReservationPersistence<'a> {
    trial_run_id: TrialRunId,
    attempt_id: StageAttemptId,
    stage_id: StageId,
    attempt_ordinal: u16,
    reservation: ResearchJournalBudget,
    canonical_attempt_bytes: &'a [u8],
    canonical_reservation_bytes: &'a [u8],
}

#[derive(Clone, Copy)]
struct CampaignTrialReservationPersistence<'a> {
    attempt_id: TrialRunId,
    trial_fingerprint: BlobId,
    attempt_ordinal: u16,
    reservation: ResearchJournalBudget,
    canonical_run_bytes: &'a [u8],
    canonical_attempt_bytes: &'a [u8],
    canonical_reservation_bytes: &'a [u8],
}

fn persist_trial_supporting_rows(
    transaction: &Transaction<'_>,
    campaign_id: CampaignId,
    trial_run_id: TrialRunId,
    conflict_subject: BlobId,
    mutation: TrialJournalMutation<'_>,
    occurred_at_ms: i64,
) -> Result<()> {
    let context = RowPersistenceContext {
        transaction,
        campaign_id,
        conflict_subject,
        occurred_at_ms,
    };
    match mutation {
        TrialJournalMutation::AttemptReserved {
            attempt_id,
            stage_id,
            attempt_ordinal,
            reservation,
            canonical_attempt_bytes,
            canonical_reservation_bytes,
        } => persist_stage_reservation(
            context,
            StageReservationPersistence {
                trial_run_id,
                attempt_id,
                stage_id,
                attempt_ordinal,
                reservation,
                canonical_attempt_bytes,
                canonical_reservation_bytes,
            },
        ),
        TrialJournalMutation::AttemptFinished {
            attempt_id,
            charge,
            canonical_charge_bytes,
            ..
        } => persist_budget_charge(
            context,
            "research_campaign_budget_charges",
            BudgetAttemptId::Stage(attempt_id),
            charge,
            canonical_charge_bytes,
        ),
        TrialJournalMutation::Prepared
        | TrialJournalMutation::AttemptStarted { .. }
        | TrialJournalMutation::AttemptAbandoned { .. }
        | TrialJournalMutation::TrialClosed => Ok(()),
    }
}

fn persist_campaign_supporting_rows(
    transaction: &Transaction<'_>,
    campaign_id: CampaignId,
    conflict_subject: BlobId,
    mutation: CampaignJournalMutation<'_>,
    occurred_at_ms: i64,
) -> Result<()> {
    let context = RowPersistenceContext {
        transaction,
        campaign_id,
        conflict_subject,
        occurred_at_ms,
    };
    match mutation {
        CampaignJournalMutation::TrialReserved {
            attempt_id,
            trial_fingerprint,
            attempt_ordinal,
            reservation,
            canonical_run_bytes,
            canonical_attempt_bytes,
            canonical_reservation_bytes,
        } => persist_campaign_trial_reservation(
            context,
            CampaignTrialReservationPersistence {
                attempt_id,
                trial_fingerprint,
                attempt_ordinal,
                reservation,
                canonical_run_bytes,
                canonical_attempt_bytes,
                canonical_reservation_bytes,
            },
        ),
        CampaignJournalMutation::TrialFinished {
            attempt_id,
            charge,
            canonical_charge_bytes,
            ..
        } => persist_budget_charge(
            context,
            "research_campaign_trial_budget_charges",
            BudgetAttemptId::Trial(attempt_id),
            charge,
            canonical_charge_bytes,
        ),
        CampaignJournalMutation::SearchDecisionRecorded {
            decision_index,
            kind,
            parent_archive_fingerprint,
            canonical_decision_bytes,
        } => persist_search_decision(
            context,
            decision_index,
            kind,
            parent_archive_fingerprint,
            canonical_decision_bytes,
        ),
        CampaignJournalMutation::Prepared
        | CampaignJournalMutation::Started
        | CampaignJournalMutation::PauseRequested
        | CampaignJournalMutation::Paused
        | CampaignJournalMutation::Resumed
        | CampaignJournalMutation::CancelRequested
        | CampaignJournalMutation::TrialDispatched { .. }
        | CampaignJournalMutation::TrialReservationReleased { .. }
        | CampaignJournalMutation::CampaignClosed => Ok(()),
    }
}

fn persist_stage_reservation(
    context: RowPersistenceContext<'_, '_>,
    input: StageReservationPersistence<'_>,
) -> Result<()> {
    let StageReservationPersistence {
        trial_run_id,
        attempt_id,
        stage_id,
        attempt_ordinal,
        reservation,
        canonical_attempt_bytes,
        canonical_reservation_bytes,
    } = input;
    if attempt_ordinal == 0 || reservation.wall_time_ms == 0 {
        return Err(StoreError::InvalidResearchJournalMutation);
    }
    let attempt_record = register_execution_record(
        context.transaction,
        ResearchExecutionRecordKind::StageAttempt,
        canonical_attempt_bytes,
        context.occurred_at_ms,
    )?;
    context.transaction.execute(
        "INSERT OR IGNORE INTO research_campaign_stage_attempts(
            stage_attempt_id, trial_run_id, stage_id, attempt_ordinal,
            record_fingerprint, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            attempt_id.to_string(),
            trial_run_id.to_string(),
            stage_id.to_string(),
            i64::from(attempt_ordinal),
            attempt_record.to_string(),
            context.occurred_at_ms,
        ],
    )?;
    verify_stage_attempt(
        context.transaction,
        attempt_id,
        trial_run_id,
        stage_id,
        attempt_ordinal,
        attempt_record,
        context.conflict_subject,
    )?;
    persist_stage_budget_reservation(
        context,
        attempt_id,
        reservation,
        canonical_reservation_bytes,
    )
}

fn persist_stage_budget_reservation(
    context: RowPersistenceContext<'_, '_>,
    attempt_id: StageAttemptId,
    reservation: ResearchJournalBudget,
    canonical_reservation_bytes: &[u8],
) -> Result<()> {
    let record = register_execution_record(
        context.transaction,
        ResearchExecutionRecordKind::BudgetReservation,
        canonical_reservation_bytes,
        context.occurred_at_ms,
    )?;
    let values = sql_budget(reservation)?;
    let reservation_id = reservation_id(attempt_id);
    context.transaction.execute(
        "INSERT OR IGNORE INTO research_campaign_budget_reservations(
            reservation_id, campaign_id, stage_attempt_id,
            writer_tokens, controller_tokens, evaluations, wall_time_ms,
            record_fingerprint, reserved_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            reservation_id.to_string(),
            context.campaign_id.to_string(),
            attempt_id.to_string(),
            values[0],
            values[1],
            values[2],
            values[3],
            record.to_string(),
            context.occurred_at_ms,
        ],
    )?;
    verify_stage_reservation(
        context.transaction,
        reservation_id,
        context.campaign_id,
        attempt_id,
        values,
        record,
        context.conflict_subject,
    )
}

fn persist_campaign_trial_reservation(
    context: RowPersistenceContext<'_, '_>,
    input: CampaignTrialReservationPersistence<'_>,
) -> Result<()> {
    let CampaignTrialReservationPersistence {
        attempt_id,
        trial_fingerprint,
        attempt_ordinal,
        reservation,
        canonical_run_bytes,
        canonical_attempt_bytes,
        canonical_reservation_bytes,
    } = input;
    if attempt_ordinal == 0
        || reservation.writer_tokens == 0
        || reservation.evaluations == 0
        || reservation.wall_time_ms == 0
    {
        return Err(StoreError::InvalidResearchJournalMutation);
    }
    let run: TrialRunRecord = serde_json::from_slice(canonical_run_bytes)
        .map_err(|_| StoreError::InvalidResearchJournalMutation)?;
    let expected_origin = TrialRunOrigin::Campaign {
        campaign_id: context.campaign_id,
        campaign_fingerprint: load_campaign_fingerprint(context.transaction, context.campaign_id)?,
    };
    if run.trial_run_id() != attempt_id
        || run.trial_fingerprint() != trial_fingerprint
        || run.origin() != expected_origin
        || run
            .canonical_bytes()
            .map_err(|_| StoreError::InvalidResearchJournalMutation)?
            != canonical_run_bytes
    {
        return Err(StoreError::InvalidResearchJournalMutation);
    }
    let run_record = register_execution_record(
        context.transaction,
        ResearchExecutionRecordKind::TrialRun,
        canonical_run_bytes,
        context.occurred_at_ms,
    )?;
    insert_trial_run_row(
        context.transaction,
        attempt_id,
        trial_fingerprint,
        "campaign",
        Some(context.campaign_id),
        run_record,
        context.occurred_at_ms,
    )?;
    let attempt_record = register_execution_record(
        context.transaction,
        ResearchExecutionRecordKind::CampaignTrialAttempt,
        canonical_attempt_bytes,
        context.occurred_at_ms,
    )?;
    context.transaction.execute(
        "INSERT OR IGNORE INTO research_campaign_trial_attempts(
            trial_attempt_id, trial_fingerprint, attempt_ordinal,
            record_fingerprint, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            attempt_id.to_string(),
            trial_fingerprint.to_string(),
            i64::from(attempt_ordinal),
            attempt_record.to_string(),
            context.occurred_at_ms,
        ],
    )?;
    verify_campaign_trial_attempt(
        context.transaction,
        attempt_id,
        trial_fingerprint,
        attempt_ordinal,
        attempt_record,
        context.conflict_subject,
    )?;
    persist_campaign_trial_budget_reservation(
        context,
        attempt_id,
        reservation,
        canonical_reservation_bytes,
    )
}

fn persist_campaign_trial_budget_reservation(
    context: RowPersistenceContext<'_, '_>,
    attempt_id: TrialRunId,
    reservation: ResearchJournalBudget,
    canonical_reservation_bytes: &[u8],
) -> Result<()> {
    let record = register_execution_record(
        context.transaction,
        ResearchExecutionRecordKind::BudgetReservation,
        canonical_reservation_bytes,
        context.occurred_at_ms,
    )?;
    let values = sql_budget(reservation)?;
    let reservation_id = trial_reservation_id(attempt_id);
    context.transaction.execute(
        "INSERT OR IGNORE INTO research_campaign_trial_budget_reservations(
            reservation_id, trial_attempt_id, writer_tokens,
            controller_tokens, evaluations, wall_time_ms,
            record_fingerprint, reserved_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            reservation_id.to_string(),
            attempt_id.to_string(),
            values[0],
            values[1],
            values[2],
            values[3],
            record.to_string(),
            context.occurred_at_ms,
        ],
    )?;
    verify_campaign_trial_reservation(
        context.transaction,
        reservation_id,
        attempt_id,
        values,
        record,
        context.conflict_subject,
    )
}

fn persist_budget_charge(
    context: RowPersistenceContext<'_, '_>,
    table: &str,
    attempt_id: BudgetAttemptId,
    charge: ResearchJournalBudget,
    canonical_charge_bytes: &[u8],
) -> Result<()> {
    let record = register_execution_record(
        context.transaction,
        ResearchExecutionRecordKind::BudgetCharge,
        canonical_charge_bytes,
        context.occurred_at_ms,
    )?;
    let values = sql_budget(charge)?;
    let reservation_id = attempt_id.reservation_id();
    let sql = match table {
        "research_campaign_budget_charges" => {
            "INSERT OR IGNORE INTO research_campaign_budget_charges(
                reservation_id, writer_tokens, controller_tokens,
                evaluations, wall_time_ms, record_fingerprint, charged_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        }
        "research_campaign_trial_budget_charges" => {
            "INSERT OR IGNORE INTO research_campaign_trial_budget_charges(
                reservation_id, writer_tokens, controller_tokens,
                evaluations, wall_time_ms, record_fingerprint, charged_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        }
        _ => return Err(StoreError::InvalidResearchJournalMutation),
    };
    context.transaction.execute(
        sql,
        params![
            reservation_id.to_string(),
            values[0],
            values[1],
            values[2],
            values[3],
            record.to_string(),
            context.occurred_at_ms,
        ],
    )?;
    verify_charge(
        context.transaction,
        table,
        reservation_id,
        values,
        record,
        context.conflict_subject,
    )
}

fn persist_search_decision(
    context: RowPersistenceContext<'_, '_>,
    decision_index: u32,
    kind: SearchDecisionPersistenceKind,
    parent_archive_fingerprint: Option<BlobId>,
    canonical_decision_bytes: &[u8],
) -> Result<()> {
    let record = register_execution_record(
        context.transaction,
        ResearchExecutionRecordKind::SearchDecision,
        canonical_decision_bytes,
        context.occurred_at_ms,
    )?;
    context.transaction.execute(
        "INSERT OR IGNORE INTO research_campaign_search_decisions(
            campaign_id, decision_index, decision_kind,
            parent_archive_fingerprint, record_fingerprint, decided_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            context.campaign_id.to_string(),
            i64::from(decision_index),
            kind.as_str(),
            parent_archive_fingerprint.map(|value| value.to_string()),
            record.to_string(),
            context.occurred_at_ms,
        ],
    )?;
    verify_search_decision(
        context.transaction,
        context.campaign_id,
        decision_index,
        kind,
        parent_archive_fingerprint,
        record,
        context.conflict_subject,
    )
}

fn register_execution_record(
    transaction: &Transaction<'_>,
    kind: ResearchExecutionRecordKind,
    canonical_bytes: &[u8],
    created_at_ms: i64,
) -> Result<BlobId> {
    ensure_journal_record_size(canonical_bytes)?;
    let fingerprint = BlobId::digest(canonical_bytes);
    insert_blob_row(
        transaction,
        fingerprint,
        canonical_bytes.len(),
        created_at_ms,
    )?;
    let stored_len = transaction
        .query_row(
            "SELECT byte_len FROM blobs WHERE blob_id = ?1",
            [fingerprint.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if stored_len != Some(sql_len(canonical_bytes.len())?) {
        return Err(StoreError::ResearchExecutionRecordConflict { fingerprint });
    }
    transaction.execute(
        "INSERT OR IGNORE INTO research_execution_records(
            record_fingerprint, record_kind, record_blob_id, created_at_ms
         ) VALUES (?1, ?2, ?1, ?3)",
        params![fingerprint.to_string(), kind.as_str(), created_at_ms],
    )?;
    let stored = transaction
        .query_row(
            "SELECT record_kind, record_blob_id
             FROM research_execution_records WHERE record_fingerprint = ?1",
            [fingerprint.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if stored.as_ref() != Some(&(kind.as_str().to_owned(), fingerprint.to_string())) {
        return Err(StoreError::ResearchExecutionRecordConflict { fingerprint });
    }
    Ok(fingerprint)
}

fn trial_event_columns(mutation: TrialJournalMutation<'_>) -> EventColumns {
    match mutation {
        TrialJournalMutation::Prepared => ("prepared", None, None, None),
        TrialJournalMutation::AttemptReserved { attempt_id, .. } => {
            ("attempt_reserved", Some(attempt_id.to_string()), None, None)
        }
        TrialJournalMutation::AttemptStarted { attempt_id } => {
            ("attempt_started", Some(attempt_id.to_string()), None, None)
        }
        TrialJournalMutation::AttemptFinished {
            attempt_id,
            outcome,
            terminal_output_fingerprint,
            ..
        } => (
            "attempt_finished",
            Some(attempt_id.to_string()),
            Some(outcome.as_str()),
            terminal_output_fingerprint,
        ),
        TrialJournalMutation::AttemptAbandoned { attempt_id } => (
            "attempt_abandoned",
            Some(attempt_id.to_string()),
            Some("abandoned"),
            None,
        ),
        TrialJournalMutation::TrialClosed => ("trial_closed", None, None, None),
    }
}

type EventColumns = (
    &'static str,
    Option<String>,
    Option<&'static str>,
    Option<BlobId>,
);

fn campaign_event_columns(mutation: CampaignJournalMutation<'_>) -> EventColumns {
    match mutation {
        CampaignJournalMutation::Prepared => ("prepared", None, None, None),
        CampaignJournalMutation::Started => ("started", None, None, None),
        CampaignJournalMutation::PauseRequested => ("pause_requested", None, None, None),
        CampaignJournalMutation::Paused => ("paused", None, None, None),
        CampaignJournalMutation::Resumed => ("resumed", None, None, None),
        CampaignJournalMutation::CancelRequested => ("cancel_requested", None, None, None),
        CampaignJournalMutation::TrialReserved { attempt_id, .. } => {
            ("trial_reserved", Some(attempt_id.to_string()), None, None)
        }
        CampaignJournalMutation::TrialDispatched { attempt_id } => {
            ("trial_dispatched", Some(attempt_id.to_string()), None, None)
        }
        CampaignJournalMutation::TrialFinished {
            attempt_id,
            outcome,
            ..
        } => (
            "trial_finished",
            Some(attempt_id.to_string()),
            Some(outcome.as_str()),
            None,
        ),
        CampaignJournalMutation::TrialReservationReleased { attempt_id } => (
            "trial_reservation_released",
            Some(attempt_id.to_string()),
            Some("released"),
            None,
        ),
        CampaignJournalMutation::SearchDecisionRecorded { .. } => {
            ("search_decision_recorded", None, None, None)
        }
        CampaignJournalMutation::CampaignClosed => ("campaign_closed", None, None, None),
    }
}

type StoredJournalEventRow = (
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

fn trial_event_replay_is_exact(
    connection: &Connection,
    input: TrialJournalEventPersistence<'_>,
    columns: &EventColumns,
) -> Result<bool> {
    let stored = query_journal_event_row(
        connection,
        "SELECT previous_event_fingerprint, event_fingerprint, event_kind,
                stage_attempt_id, attempt_outcome, terminal_output_fingerprint,
                record_fingerprint
         FROM research_trial_events
         WHERE trial_run_id = ?1 AND event_index = ?2",
        &input.trial_run_id.to_string(),
        input.event_index,
    )?;
    replay_is_exact(
        stored,
        &expected_event_row(
            input.previous_event_fingerprint,
            input.event_fingerprint,
            columns,
            BlobId::digest(input.canonical_event_bytes),
        ),
        input.event_fingerprint,
    )
}

fn campaign_event_replay_is_exact(
    connection: &Connection,
    campaign_id: CampaignId,
    input: &CampaignJournalEventPersistence<'_>,
    columns: &EventColumns,
) -> Result<bool> {
    let stored = query_journal_event_row(
        connection,
        "SELECT previous_event_fingerprint, event_fingerprint, event_kind,
                trial_attempt_id, attempt_outcome, NULL, record_fingerprint
         FROM research_campaign_events
         WHERE campaign_id = ?1 AND event_index = ?2",
        &campaign_id.to_string(),
        input.event_index,
    )?;
    replay_is_exact(
        stored,
        &expected_event_row(
            input.previous_event_fingerprint,
            input.event_fingerprint,
            columns,
            BlobId::digest(input.canonical_event_bytes),
        ),
        input.event_fingerprint,
    )
}

fn replay_is_exact(
    stored: Option<StoredJournalEventRow>,
    expected: &StoredJournalEventRow,
    conflict_subject: BlobId,
) -> Result<bool> {
    match stored {
        None => Ok(false),
        Some(stored) if &stored == expected => Ok(true),
        Some(_) => Err(StoreError::ResearchExecutionSubjectConflict {
            subject: conflict_subject,
        }),
    }
}

fn query_journal_event_row(
    connection: &Connection,
    sql: &str,
    subject: &str,
    event_index: u32,
) -> Result<Option<StoredJournalEventRow>> {
    connection
        .query_row(sql, params![subject, i64::from(event_index)], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .optional()
        .map_err(StoreError::from)
}

fn expected_event_row(
    previous_event_fingerprint: Option<BlobId>,
    event_fingerprint: BlobId,
    columns: &EventColumns,
    record_fingerprint: BlobId,
) -> StoredJournalEventRow {
    (
        previous_event_fingerprint.map(|value| value.to_string()),
        event_fingerprint.to_string(),
        columns.0.to_owned(),
        columns.1.clone(),
        columns.2.map(str::to_owned),
        columns.3.map(|value| value.to_string()),
        record_fingerprint.to_string(),
    )
}

fn verify_trial_event_row(
    transaction: &Transaction<'_>,
    input: TrialJournalEventPersistence<'_>,
    event_kind: &'static str,
    attempt_id: Option<String>,
    attempt_outcome: Option<&'static str>,
    terminal_output_fingerprint: Option<BlobId>,
    record_fingerprint: BlobId,
) -> Result<()> {
    let stored = query_journal_event_row(
        transaction,
        "SELECT previous_event_fingerprint, event_fingerprint, event_kind,
                stage_attempt_id, attempt_outcome, terminal_output_fingerprint,
                record_fingerprint
         FROM research_trial_events
         WHERE trial_run_id = ?1 AND event_index = ?2",
        &input.trial_run_id.to_string(),
        input.event_index,
    );
    let stored = stored?;
    let columns = (
        event_kind,
        attempt_id,
        attempt_outcome,
        terminal_output_fingerprint,
    );
    let expected = expected_event_row(
        input.previous_event_fingerprint,
        input.event_fingerprint,
        &columns,
        record_fingerprint,
    );
    require_exact(stored.as_ref() == Some(&expected), input.event_fingerprint)
}

fn verify_campaign_event_row(
    transaction: &Transaction<'_>,
    campaign_id: CampaignId,
    input: &CampaignJournalEventPersistence<'_>,
    event_kind: &'static str,
    attempt_id: Option<String>,
    attempt_outcome: Option<&'static str>,
    record_fingerprint: BlobId,
) -> Result<()> {
    let stored = query_journal_event_row(
        transaction,
        "SELECT previous_event_fingerprint, event_fingerprint, event_kind,
                trial_attempt_id, attempt_outcome, NULL, record_fingerprint
         FROM research_campaign_events
         WHERE campaign_id = ?1 AND event_index = ?2",
        &campaign_id.to_string(),
        input.event_index,
    );
    let stored = stored?;
    let columns = (event_kind, attempt_id, attempt_outcome, None);
    let expected = expected_event_row(
        input.previous_event_fingerprint,
        input.event_fingerprint,
        &columns,
        record_fingerprint,
    );
    require_exact(stored.as_ref() == Some(&expected), input.event_fingerprint)
}

fn verify_stage_attempt(
    transaction: &Transaction<'_>,
    attempt_id: StageAttemptId,
    trial_run_id: TrialRunId,
    stage_id: StageId,
    ordinal: u16,
    record: BlobId,
    conflict_subject: BlobId,
) -> Result<()> {
    let stored = transaction
        .query_row(
            "SELECT trial_run_id, stage_id, attempt_ordinal, record_fingerprint
             FROM research_campaign_stage_attempts WHERE stage_attempt_id = ?1",
            [attempt_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    require_exact(
        stored
            == Some((
                trial_run_id.to_string(),
                stage_id.to_string(),
                i64::from(ordinal),
                record.to_string(),
            )),
        conflict_subject,
    )
}

fn verify_campaign_trial_attempt(
    transaction: &Transaction<'_>,
    attempt_id: TrialRunId,
    trial_fingerprint: BlobId,
    ordinal: u16,
    record: BlobId,
    conflict_subject: BlobId,
) -> Result<()> {
    let stored = transaction
        .query_row(
            "SELECT trial_fingerprint, attempt_ordinal, record_fingerprint
             FROM research_campaign_trial_attempts WHERE trial_attempt_id = ?1",
            [attempt_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    require_exact(
        stored
            == Some((
                trial_fingerprint.to_string(),
                i64::from(ordinal),
                record.to_string(),
            )),
        conflict_subject,
    )
}

fn verify_stage_reservation(
    transaction: &Transaction<'_>,
    reservation_id: ArtifactId,
    campaign_id: CampaignId,
    attempt_id: StageAttemptId,
    values: [i64; 4],
    record: BlobId,
    conflict_subject: BlobId,
) -> Result<()> {
    let stored = transaction
        .query_row(
            "SELECT campaign_id, stage_attempt_id, writer_tokens,
                    controller_tokens, evaluations, wall_time_ms, record_fingerprint
             FROM research_campaign_budget_reservations WHERE reservation_id = ?1",
            [reservation_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    [row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?],
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?;
    require_exact(
        stored
            == Some((
                campaign_id.to_string(),
                attempt_id.to_string(),
                values,
                record.to_string(),
            )),
        conflict_subject,
    )
}

fn verify_campaign_trial_reservation(
    transaction: &Transaction<'_>,
    reservation_id: ArtifactId,
    attempt_id: TrialRunId,
    values: [i64; 4],
    record: BlobId,
    conflict_subject: BlobId,
) -> Result<()> {
    let stored = transaction
        .query_row(
            "SELECT trial_attempt_id, writer_tokens, controller_tokens,
                    evaluations, wall_time_ms, record_fingerprint
             FROM research_campaign_trial_budget_reservations
             WHERE reservation_id = ?1",
            [reservation_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    [row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?],
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    require_exact(
        stored == Some((attempt_id.to_string(), values, record.to_string())),
        conflict_subject,
    )
}

fn verify_charge(
    transaction: &Transaction<'_>,
    table: &str,
    reservation_id: ArtifactId,
    values: [i64; 4],
    record: BlobId,
    conflict_subject: BlobId,
) -> Result<()> {
    let sql = match table {
        "research_campaign_budget_charges" => {
            "SELECT writer_tokens, controller_tokens, evaluations,
                    wall_time_ms, record_fingerprint
             FROM research_campaign_budget_charges WHERE reservation_id = ?1"
        }
        "research_campaign_trial_budget_charges" => {
            "SELECT writer_tokens, controller_tokens, evaluations,
                    wall_time_ms, record_fingerprint
             FROM research_campaign_trial_budget_charges WHERE reservation_id = ?1"
        }
        _ => return Err(StoreError::InvalidResearchJournalMutation),
    };
    let stored = transaction
        .query_row(sql, [reservation_id.to_string()], |row| {
            Ok((
                [row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?],
                row.get::<_, String>(4)?,
            ))
        })
        .optional()?;
    require_exact(
        stored == Some((values, record.to_string())),
        conflict_subject,
    )
}

fn verify_search_decision(
    transaction: &Transaction<'_>,
    campaign_id: CampaignId,
    decision_index: u32,
    kind: SearchDecisionPersistenceKind,
    parent_archive_fingerprint: Option<BlobId>,
    record: BlobId,
    conflict_subject: BlobId,
) -> Result<()> {
    let stored = transaction
        .query_row(
            "SELECT decision_kind, parent_archive_fingerprint, record_fingerprint
             FROM research_campaign_search_decisions
             WHERE campaign_id = ?1 AND decision_index = ?2",
            params![campaign_id.to_string(), i64::from(decision_index)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    require_exact(
        stored
            == Some((
                kind.as_str().to_owned(),
                parent_archive_fingerprint.map(|value| value.to_string()),
                record.to_string(),
            )),
        conflict_subject,
    )
}

fn load_journal_accounting(
    connection: &Connection,
    kind: ResearchSessionKind,
    trial_run_id: Option<TrialRunId>,
    campaign_id: CampaignId,
) -> Result<JournalAccounting> {
    let (event_count_sql, associated_records_sql, subject, maximum_events) = match kind {
        ResearchSessionKind::Trial => (
            "SELECT COUNT(*) FROM research_trial_events WHERE trial_run_id = ?1",
            TRIAL_ASSOCIATED_RECORDS_SQL,
            trial_run_id
                .ok_or(StoreError::ResearchJournalLeaseMismatch)?
                .to_string(),
            MAX_PERSISTED_TRIAL_JOURNAL_EVENTS,
        ),
        ResearchSessionKind::Campaign => (
            "SELECT COUNT(*) FROM research_campaign_events WHERE campaign_id = ?1",
            CAMPAIGN_ASSOCIATED_RECORDS_SQL,
            campaign_id.to_string(),
            MAX_PERSISTED_CAMPAIGN_JOURNAL_EVENTS,
        ),
    };
    let event_count = load_raw_event_count(connection, event_count_sql, &subject, maximum_events)?;
    let total_record_bytes =
        load_associated_record_bytes(connection, associated_records_sql, &subject)?;
    Ok(JournalAccounting {
        event_count,
        total_record_bytes,
    })
}

const TRIAL_ASSOCIATED_RECORDS_SQL: &str = "WITH associated(record_fingerprint) AS (
         SELECT record_fingerprint
         FROM research_trial_events
         WHERE trial_run_id = ?1
         UNION ALL
         SELECT attempt.record_fingerprint
         FROM research_campaign_stage_attempts attempt
         WHERE attempt.trial_run_id = ?1
         UNION ALL
         SELECT reservation.record_fingerprint
         FROM research_campaign_budget_reservations reservation
         JOIN research_campaign_stage_attempts attempt USING (stage_attempt_id)
         WHERE attempt.trial_run_id = ?1
         UNION ALL
         SELECT charge.record_fingerprint
         FROM research_campaign_budget_charges charge
         JOIN research_campaign_budget_reservations reservation USING (reservation_id)
         JOIN research_campaign_stage_attempts attempt USING (stage_attempt_id)
         WHERE attempt.trial_run_id = ?1
     ), exact(record_fingerprint, byte_len) AS (
         SELECT associated.record_fingerprint, blob.byte_len
         FROM associated
         JOIN research_execution_records record USING (record_fingerprint)
         JOIN blobs blob ON blob.blob_id = record.record_blob_id
     )
     SELECT (SELECT COUNT(*) FROM associated), COUNT(*), COALESCE(SUM(byte_len), 0)
     FROM exact";

const CAMPAIGN_ASSOCIATED_RECORDS_SQL: &str = "WITH associated(record_fingerprint) AS (
         SELECT record_fingerprint
         FROM research_campaign_events
         WHERE campaign_id = ?1
         UNION ALL
         SELECT attempt.record_fingerprint
         FROM research_campaign_trial_attempts attempt
         JOIN research_trial_specs trial USING (trial_fingerprint)
         WHERE trial.campaign_id = ?1
         UNION ALL
         SELECT reservation.record_fingerprint
         FROM research_campaign_trial_budget_reservations reservation
         JOIN research_campaign_trial_attempts attempt USING (trial_attempt_id)
         JOIN research_trial_specs trial USING (trial_fingerprint)
         WHERE trial.campaign_id = ?1
         UNION ALL
         SELECT charge.record_fingerprint
         FROM research_campaign_trial_budget_charges charge
         JOIN research_campaign_trial_budget_reservations reservation USING (reservation_id)
         JOIN research_campaign_trial_attempts attempt USING (trial_attempt_id)
         JOIN research_trial_specs trial USING (trial_fingerprint)
         WHERE trial.campaign_id = ?1
         UNION ALL
         SELECT record_fingerprint
         FROM research_campaign_search_decisions
         WHERE campaign_id = ?1
     ), exact(record_fingerprint, byte_len) AS (
         SELECT associated.record_fingerprint, blob.byte_len
         FROM associated
         JOIN research_execution_records record USING (record_fingerprint)
         JOIN blobs blob ON blob.blob_id = record.record_blob_id
     )
     SELECT (SELECT COUNT(*) FROM associated), COUNT(*), COALESCE(SUM(byte_len), 0)
     FROM exact";

fn load_associated_record_bytes(connection: &Connection, sql: &str, subject: &str) -> Result<u64> {
    let (associated_count, exact_count, total_bytes) =
        connection.query_row(sql, [subject], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
    if associated_count < 0 || exact_count < 0 || associated_count != exact_count {
        return Err(corrupt_journal(
            "associated records are missing exact execution-record or blob bindings",
        ));
    }
    let total_bytes = u64::try_from(total_bytes)
        .map_err(|_| corrupt_journal("associated record byte total is negative"))?;
    if total_bytes > MAX_RESEARCH_JOURNAL_TOTAL_BYTES {
        return Err(StoreError::ResearchJournalTotalTooLarge {
            max_bytes: MAX_RESEARCH_JOURNAL_TOTAL_BYTES,
        });
    }
    Ok(total_bytes)
}

fn load_journal_events(
    connection: &Connection,
    root: &Path,
    sql: &str,
    subject: &str,
    maximum_events: usize,
) -> Result<Vec<PersistedResearchJournalEvent>> {
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query([subject])?;
    let mut events = Vec::new();
    let mut total_bytes = 0_u64;
    while let Some(row) = rows.next()? {
        if events.len() == maximum_events {
            return Err(StoreError::ResearchJournalEventLimit {
                max_events: maximum_events,
            });
        }
        let event_index = u32::try_from(row.get::<_, i64>(0)?)
            .map_err(|_| corrupt_journal("event index is outside u32"))?;
        let previous = row
            .get::<_, Option<String>>(1)?
            .map(|value| parse_blob(&value, "previous event fingerprint"))
            .transpose()?;
        let event_fingerprint = parse_blob(&row.get::<_, String>(2)?, "event fingerprint")?;
        let record_fingerprint = parse_blob(&row.get::<_, String>(3)?, "event record fingerprint")?;
        let record_blob = parse_blob(&row.get::<_, String>(4)?, "event record blob")?;
        if record_blob != record_fingerprint {
            return Err(corrupt_journal(
                "event execution record does not point to its own fingerprint",
            ));
        }
        let byte_len = u64::try_from(row.get::<_, i64>(5)?)
            .map_err(|_| corrupt_journal("event record has a negative byte length"))?;
        if byte_len == 0
            || byte_len > u64::try_from(MAX_RESEARCH_JOURNAL_EVENT_BYTES).unwrap_or(u64::MAX)
        {
            return Err(StoreError::ResearchJournalRecordTooLarge {
                actual_bytes: byte_len,
                max_bytes: MAX_RESEARCH_JOURNAL_EVENT_BYTES,
            });
        }
        total_bytes =
            total_bytes
                .checked_add(byte_len)
                .ok_or(StoreError::ResearchJournalTotalTooLarge {
                    max_bytes: MAX_RESEARCH_JOURNAL_TOTAL_BYTES,
                })?;
        if total_bytes > MAX_RESEARCH_JOURNAL_TOTAL_BYTES {
            return Err(StoreError::ResearchJournalTotalTooLarge {
                max_bytes: MAX_RESEARCH_JOURNAL_TOTAL_BYTES,
            });
        }
        let canonical_record_bytes = read_record_blob(root, record_blob, byte_len)?;
        events.push(PersistedResearchJournalEvent {
            event_index,
            previous_event_fingerprint: previous,
            event_fingerprint,
            record_fingerprint,
            canonical_record_bytes,
        });
    }
    Ok(events)
}

fn load_raw_event_count(
    connection: &Connection,
    sql: &str,
    subject: &str,
    maximum_events: usize,
) -> Result<usize> {
    let raw = connection.query_row(sql, [subject], |row| row.get::<_, i64>(0))?;
    let count = usize::try_from(raw).map_err(|_| corrupt_journal("negative event count"))?;
    if count > maximum_events {
        return Err(StoreError::ResearchJournalEventLimit {
            max_events: maximum_events,
        });
    }
    Ok(count)
}

fn require_loaded_count(actual: usize, expected: usize) -> Result<()> {
    if actual != expected {
        return Err(corrupt_journal(
            "event rows are missing exact execution-record or blob bindings",
        ));
    }
    Ok(())
}

fn put_record_blob(root: &Path, bytes: &[u8]) -> Result<BlobId> {
    ensure_journal_record_size(bytes)?;
    let blob_id = BlobId::digest(bytes);
    let path = journal_blob_path(root, blob_id);
    reject_symlink_target(&path)?;
    if path.exists() {
        let maximum =
            u64::try_from(bytes.len()).map_err(|_| StoreError::ResearchJournalRecordTooLarge {
                actual_bytes: u64::MAX,
                max_bytes: MAX_RESEARCH_JOURNAL_EVENT_BYTES,
            })?;
        let existing = read_bounded(&path, maximum)?;
        if existing != bytes {
            return Err(StoreError::CorruptBlob {
                path,
                expected: blob_id,
                actual: BlobId::digest(&existing),
            });
        }
        return Ok(blob_id);
    }
    let parent = path
        .parent()
        .ok_or_else(|| corrupt_journal("event record blob path has no parent"))?;
    ensure_private_directory(parent)?;
    atomic_replace_private(&path, bytes)?;
    Ok(blob_id)
}

fn read_record_blob(root: &Path, blob_id: BlobId, exact_len: u64) -> Result<Vec<u8>> {
    let path = journal_blob_path(root, blob_id);
    reject_symlink_target(&path)?;
    if !path.exists() {
        return Err(StoreError::MissingBlob { blob_id, path });
    }
    let bytes = read_bounded(&path, exact_len)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != exact_len {
        return Err(corrupt_journal(
            "event record blob length disagrees with SQLite",
        ));
    }
    let actual = BlobId::digest(&bytes);
    if actual != blob_id {
        return Err(StoreError::CorruptBlob {
            path,
            expected: blob_id,
            actual,
        });
    }
    Ok(bytes)
}

fn journal_blob_path(root: &Path, blob_id: BlobId) -> PathBuf {
    let hash = blob_id.to_hex();
    root.join(".loom/blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..])
}

fn ensure_journal_record_size(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Err(StoreError::EmptyResearchExecutionRecord);
    }
    if bytes.len() > MAX_RESEARCH_JOURNAL_EVENT_BYTES
        || bytes.len() > MAX_RESEARCH_EXECUTION_RECORD_BYTES
    {
        return Err(StoreError::ResearchJournalRecordTooLarge {
            actual_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            max_bytes: MAX_RESEARCH_JOURNAL_EVENT_BYTES,
        });
    }
    Ok(())
}

fn sql_budget(value: ResearchJournalBudget) -> Result<[i64; 4]> {
    Ok([
        sql_u64(value.writer_tokens)?,
        sql_u64(value.controller_tokens)?,
        i64::from(value.evaluations),
        sql_u64(value.wall_time_ms)?,
    ])
}

fn sql_u64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::InvalidResearchJournalMutation)
}

fn sql_len(value: usize) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::ResearchJournalRecordTooLarge {
        actual_bytes: u64::MAX,
        max_bytes: MAX_RESEARCH_JOURNAL_EVENT_BYTES,
    })
}

#[derive(Clone, Copy)]
enum BudgetAttemptId {
    Stage(StageAttemptId),
    Trial(TrialRunId),
}

impl BudgetAttemptId {
    fn reservation_id(self) -> ArtifactId {
        match self {
            Self::Stage(attempt_id) => reservation_id(attempt_id),
            Self::Trial(run_id) => trial_reservation_id(run_id),
        }
    }
}

fn reservation_id(attempt_id: StageAttemptId) -> ArtifactId {
    ArtifactId::from_ulid(attempt_id.as_ulid())
}

fn trial_reservation_id(run_id: TrialRunId) -> ArtifactId {
    ArtifactId::from_ulid(run_id.as_ulid())
}

fn load_campaign_fingerprint(
    transaction: &Transaction<'_>,
    campaign_id: CampaignId,
) -> Result<BlobId> {
    let value = transaction.query_row(
        "SELECT campaign_fingerprint FROM research_campaigns WHERE campaign_id = ?1",
        [campaign_id.to_string()],
        |row| row.get::<_, String>(0),
    )?;
    parse_blob(&value, "campaign_fingerprint")
}

fn require_exact(exact: bool, subject: BlobId) -> Result<()> {
    if exact {
        Ok(())
    } else {
        Err(StoreError::ResearchExecutionSubjectConflict { subject })
    }
}

fn parse_blob(value: &str, field: &str) -> Result<BlobId> {
    BlobId::from_str(value).map_err(|error| corrupt_journal(format!("invalid {field}: {error}")))
}

fn corrupt_journal(message: impl Into<String>) -> StoreError {
    StoreError::CorruptDatabase(format!(
        "invalid persisted research journal: {}",
        message.into()
    ))
}

fn research_lease_fingerprint(
    session_nonce: &[u8; 32],
    kind: ResearchSessionKind,
    subject_fingerprint: BlobId,
    record_fingerprint: BlobId,
    project_id: ProjectId,
    session_id: ArtifactId,
) -> BlobId {
    let mut material = Vec::with_capacity(98);
    material.extend_from_slice(b"loom/research-session/v1\0");
    material.extend_from_slice(session_nonce);
    material.push(kind.domain_tag());
    material.extend_from_slice(subject_fingerprint.as_bytes());
    material.extend_from_slice(record_fingerprint.as_bytes());
    material.extend_from_slice(&project_id.as_ulid().to_bytes());
    material.extend_from_slice(&session_id.as_ulid().to_bytes());
    BlobId::digest(&material)
}

#[cfg(test)]
mod tests {
    use loom_research_types::{CampaignId, TrialRunId, TrialRunOrigin, TrialRunRecord};
    use tempfile::tempdir;

    use super::*;
    use crate::{FrozenCampaignPersistence, ResearchBudgetMaximum};

    #[test]
    fn exact_event_replay_is_idempotent_and_conflict_rolls_back() {
        let (_directory, store, subject) = seeded_campaign_store();
        let lease = store
            .acquire_campaign_session(subject)
            .expect("campaign lease");
        let mut writer = store
            .open_research_journal_writer(lease)
            .expect("affine journal writer");
        let canonical = b"{\"format\":\"test.campaign-event.v1\",\"event\":0}";
        let event = CampaignJournalEventPersistence {
            campaign_fingerprint: subject,
            event_index: 0,
            previous_event_fingerprint: None,
            event_fingerprint: BlobId::digest(b"domain event zero"),
            canonical_event_bytes: canonical,
            mutation: CampaignJournalMutation::Prepared,
        };
        writer
            .append_campaign_event(event)
            .expect("first exact append");
        writer
            .append_campaign_event(event)
            .expect("exact idempotent replay");
        assert_eq!(writer.load_campaign_events().expect("load").len(), 1);

        let conflict = CampaignJournalEventPersistence {
            event_fingerprint: BlobId::digest(b"conflicting domain event zero"),
            canonical_event_bytes:
                b"{\"format\":\"test.campaign-event.v1\",\"event\":\"conflict\"}",
            ..event
        };
        assert!(matches!(
            writer.append_campaign_event(conflict),
            Err(StoreError::ResearchExecutionSubjectConflict { .. })
        ));
        let loaded = writer.load_campaign_events().expect("load after conflict");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].event_fingerprint(), event.event_fingerprint);
        assert_eq!(loaded[0].canonical_record_bytes(), canonical);
    }

    #[test]
    fn sqlite_failure_commits_no_event_and_retry_is_clean() {
        let (_directory, store, subject) = seeded_campaign_store();
        let lease = store
            .acquire_campaign_session(subject)
            .expect("campaign lease");
        let mut writer = store
            .open_research_journal_writer(lease)
            .expect("affine journal writer");
        let event = CampaignJournalEventPersistence {
            campaign_fingerprint: subject,
            event_index: 0,
            previous_event_fingerprint: None,
            event_fingerprint: BlobId::digest(b"query-only event"),
            canonical_event_bytes: b"{\"format\":\"test.campaign-event.v1\",\"event\":0}",
            mutation: CampaignJournalMutation::Prepared,
        };

        writer
            .connection
            .pragma_update(None, "query_only", true)
            .expect("enable query-only failure");
        assert!(matches!(
            writer.append_campaign_event(event),
            Err(StoreError::Sqlite(_))
        ));
        assert!(
            writer
                .load_campaign_events()
                .expect("no partial database event")
                .is_empty()
        );

        writer
            .connection
            .pragma_update(None, "query_only", false)
            .expect("restore writes");
        writer
            .append_campaign_event(event)
            .expect("retry after database failure");
        assert_eq!(writer.load_campaign_events().expect("load retry").len(), 1);
    }

    #[test]
    fn accounting_reopens_with_event_and_support_witness_bytes() {
        let (_directory, store, subject) = seeded_campaign_store();
        let lease = store
            .acquire_campaign_session(subject)
            .expect("campaign lease");
        let mut writer = store
            .open_research_journal_writer(lease)
            .expect("journal writer");
        let prepared_bytes = b"{\"format\":\"test.campaign-event.v1\",\"event\":0}";
        let prepared_fingerprint = BlobId::digest(b"accounted prepared event");
        writer
            .append_campaign_event(CampaignJournalEventPersistence {
                campaign_fingerprint: subject,
                event_index: 0,
                previous_event_fingerprint: None,
                event_fingerprint: prepared_fingerprint,
                canonical_event_bytes: prepared_bytes,
                mutation: CampaignJournalMutation::Prepared,
            })
            .expect("prepared event");
        let decision_bytes = b"{\"format\":\"test.search-decision.v1\",\"decision\":0}";
        let decision_event_bytes = b"{\"format\":\"test.campaign-event.v1\",\"event\":1}";
        writer
            .append_campaign_event(CampaignJournalEventPersistence {
                campaign_fingerprint: subject,
                event_index: 1,
                previous_event_fingerprint: Some(prepared_fingerprint),
                event_fingerprint: BlobId::digest(b"accounted decision event"),
                canonical_event_bytes: decision_event_bytes,
                mutation: CampaignJournalMutation::SearchDecisionRecorded {
                    decision_index: 0,
                    kind: SearchDecisionPersistenceKind::BlockedFactorialScheduled,
                    parent_archive_fingerprint: None,
                    canonical_decision_bytes: decision_bytes,
                },
            })
            .expect("decision event");
        drop(writer);

        let lease = store
            .acquire_campaign_session(subject)
            .expect("reopened campaign lease");
        let writer = store
            .open_research_journal_writer(lease)
            .expect("reopened writer");
        assert_eq!(writer.accounting.event_count, 2);
        assert_eq!(
            writer.accounting.total_record_bytes,
            u64::try_from(prepared_bytes.len() + decision_event_bytes.len() + decision_bytes.len())
                .expect("fixture byte count")
        );
    }

    #[test]
    fn append_limits_fail_before_blob_write_but_exact_replay_remains_valid() {
        let (_directory, store, subject) = seeded_campaign_store();
        let lease = store
            .acquire_campaign_session(subject)
            .expect("campaign lease");
        let mut writer = store
            .open_research_journal_writer(lease)
            .expect("journal writer");
        let canonical = b"{\"format\":\"test.campaign-event.v1\",\"event\":0}";
        let event = CampaignJournalEventPersistence {
            campaign_fingerprint: subject,
            event_index: 0,
            previous_event_fingerprint: None,
            event_fingerprint: BlobId::digest(b"limit replay event"),
            canonical_event_bytes: canonical,
            mutation: CampaignJournalMutation::Prepared,
        };
        writer.append_campaign_event(event).expect("initial event");
        writer.accounting = JournalAccounting {
            event_count: MAX_PERSISTED_CAMPAIGN_JOURNAL_EVENTS,
            total_record_bytes: MAX_RESEARCH_JOURNAL_TOTAL_BYTES,
        };
        writer
            .append_campaign_event(event)
            .expect("exact replay at both limits");

        let new_bytes = b"{\"format\":\"test.campaign-event.v1\",\"event\":1}";
        let new_blob = journal_blob_path(&writer.root, BlobId::digest(new_bytes));
        assert!(!new_blob.exists());
        let new_event = CampaignJournalEventPersistence {
            campaign_fingerprint: subject,
            event_index: 1,
            previous_event_fingerprint: Some(event.event_fingerprint),
            event_fingerprint: BlobId::digest(b"event above count limit"),
            canonical_event_bytes: new_bytes,
            mutation: CampaignJournalMutation::Started,
        };
        assert!(matches!(
            writer.append_campaign_event(new_event),
            Err(StoreError::ResearchJournalEventLimit { .. })
        ));
        assert!(!new_blob.exists());

        writer.accounting.event_count = 1;
        assert!(matches!(
            writer.append_campaign_event(new_event),
            Err(StoreError::ResearchJournalTotalTooLarge { .. })
        ));
        assert!(!new_blob.exists());
        assert_eq!(writer.load_campaign_events().expect("one event").len(), 1);
    }

    #[test]
    fn accounting_accepts_exact_total_boundary_and_counts_support_bytes() {
        let added = 17_u64;
        let next = JournalAccounting {
            event_count: MAX_PERSISTED_TRIAL_JOURNAL_EVENTS - 1,
            total_record_bytes: MAX_RESEARCH_JOURNAL_TOTAL_BYTES - added,
        }
        .preflight_new_event(
            u32::try_from(MAX_PERSISTED_TRIAL_JOURNAL_EVENTS - 1).expect("bounded event index"),
            MAX_PERSISTED_TRIAL_JOURNAL_EVENTS,
            added,
        )
        .expect("exact byte and count boundary");
        assert_eq!(next.event_count, MAX_PERSISTED_TRIAL_JOURNAL_EVENTS);
        assert_eq!(next.total_record_bytes, MAX_RESEARCH_JOURNAL_TOTAL_BYTES);

        let attempt = b"attempt";
        let reservation = b"reservation";
        let campaign_id = CampaignId::new();
        let campaign_fingerprint = BlobId::digest(b"campaign");
        let trial_run_id = TrialRunId::new();
        let run = TrialRunRecord::new(
            trial_run_id,
            BlobId::digest(b"trial"),
            TrialRunOrigin::Campaign {
                campaign_id,
                campaign_fingerprint,
            },
        )
        .canonical_bytes()
        .expect("canonical run");
        let input = CampaignJournalEventPersistence {
            campaign_fingerprint,
            event_index: 0,
            previous_event_fingerprint: None,
            event_fingerprint: BlobId::digest(b"event"),
            canonical_event_bytes: b"event-record",
            mutation: CampaignJournalMutation::TrialReserved {
                attempt_id: trial_run_id,
                trial_fingerprint: BlobId::digest(b"trial"),
                attempt_ordinal: 1,
                reservation: ResearchJournalBudget {
                    writer_tokens: 1,
                    controller_tokens: 0,
                    evaluations: 1,
                    wall_time_ms: 1,
                },
                canonical_run_bytes: &run,
                canonical_attempt_bytes: attempt,
                canonical_reservation_bytes: reservation,
            },
        };
        assert_eq!(
            campaign_append_record_bytes(&input).expect("bounded append bytes"),
            u64::try_from(
                input.canonical_event_bytes.len() + run.len() + attempt.len() + reservation.len()
            )
            .expect("fixture byte count")
        );
    }

    fn seeded_campaign_store() -> (tempfile::TempDir, ProjectStore, BlobId) {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "journal tests").expect("project");
        let campaign_id = CampaignId::new();
        let subject = BlobId::digest(b"persisted journal test campaign");
        let manifest = b"format = 'loom.campaign.v1'";
        store
            .persist_frozen_campaign(FrozenCampaignPersistence {
                campaign_id,
                campaign_fingerprint: subject,
                project_id: store.manifest().project_id,
                manifest_source_bytes: manifest,
                manifest_fingerprint: BlobId::digest(manifest),
                project_input_fingerprint: BlobId::digest(b"journal test project input"),
                seed: 7,
                maximum: ResearchBudgetMaximum {
                    writer_tokens: 10,
                    controller_tokens: 10,
                    evaluations: 2,
                    wall_time_ms: 1_000,
                },
                canonical_record_bytes: b"canonical journal test campaign",
            })
            .expect("persist campaign");
        (directory, store, subject)
    }
}

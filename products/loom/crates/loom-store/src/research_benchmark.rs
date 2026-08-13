use loom_benchmark::{
    BenchmarkRunClass, BenchmarkSeal, BlindedPairOutcome, DiagnosticBenchmarkRunJournal,
    EncryptedHumanLabelArchive, FrontierReviewedProvisionalProfile, HumanAdjudicationArchiveRecord,
    HumanConfirmedProfile, HumanLabelEncryptionAlgorithm, VerifiedBenchmarkRunJournalRecord,
};
use loom_types::{BlobId, now_unix_ms};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;

use crate::provenance::insert_blob_row;
use crate::{
    MAX_RESEARCH_EXECUTION_RECORD_BYTES, ProjectStore, ResearchExecutionRecordKind, Result,
    StoreError,
};

/// A persisted seal is inspectable evidence only. Qualification still requires
/// `loom-benchmark`'s private move-only seal and journal leases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticBenchmarkSeal {
    suite: BlobId,
    seal: BlobId,
    suite_record: BlobId,
    seal_record: BlobId,
}

impl PersistedDiagnosticBenchmarkSeal {
    pub const fn suite_fingerprint(self) -> BlobId {
        self.suite
    }

    pub const fn seal_fingerprint(self) -> BlobId {
        self.seal
    }

    pub const fn suite_record_fingerprint(self) -> BlobId {
        self.suite_record
    }

    pub const fn seal_record_fingerprint(self) -> BlobId {
        self.seal_record
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedBenchmarkJournal {
    seal_fingerprint: BlobId,
    journal_fingerprint: BlobId,
    journal_record_fingerprint: BlobId,
    run_count: u64,
    status: PersistedBenchmarkJournalStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistedBenchmarkJournalStatus {
    Diagnostic,
    Qualification,
}

impl PersistedBenchmarkJournal {
    pub const fn seal_fingerprint(self) -> BlobId {
        self.seal_fingerprint
    }

    pub const fn journal_record_fingerprint(self) -> BlobId {
        self.journal_record_fingerprint
    }

    pub const fn journal_fingerprint(self) -> BlobId {
        self.journal_fingerprint
    }

    pub const fn run_count(self) -> u64 {
        self.run_count
    }

    pub const fn status(self) -> PersistedBenchmarkJournalStatus {
        self.status
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticHumanLabelArchive {
    packet_fingerprint: BlobId,
    record_fingerprint: BlobId,
}

impl PersistedDiagnosticHumanLabelArchive {
    pub const fn packet_fingerprint(self) -> BlobId {
        self.packet_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticBenchmarkResult {
    result_fingerprint: BlobId,
    record_fingerprint: BlobId,
}

impl PersistedDiagnosticBenchmarkResult {
    pub const fn result_fingerprint(self) -> BlobId {
        self.result_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }
}

#[derive(Serialize)]
struct BenchmarkSuiteRecord<'a> {
    format: &'static str,
    schedule_fingerprint: BlobId,
    manifest_source_hash: BlobId,
    manifest_artifact_hash: BlobId,
    campaign_artifact_hash: BlobId,
    case_count: usize,
    seal: &'a BenchmarkSeal,
}

#[derive(Serialize)]
struct BenchmarkSealRecord<'a> {
    format: &'static str,
    seal: &'a BenchmarkSeal,
}

#[derive(Serialize)]
struct BenchmarkJournalRecord<'a> {
    format: &'static str,
    journal: &'a VerifiedBenchmarkRunJournalRecord,
}

#[derive(Serialize)]
struct HumanLabelArchiveRecord<'a> {
    format: &'static str,
    archive: &'a EncryptedHumanLabelArchive,
}

#[derive(Serialize)]
struct ProvisionalResultRecord<'a> {
    format: &'static str,
    seal_fingerprint: BlobId,
    journal_fingerprint: BlobId,
    journal_record_fingerprint: BlobId,
    profile: &'a FrontierReviewedProvisionalProfile,
}

#[derive(Serialize)]
struct HumanConfirmedResultRecord<'a> {
    format: &'static str,
    seal_fingerprint: BlobId,
    journal_fingerprint: BlobId,
    journal_record_fingerprint: BlobId,
    profile: &'a HumanConfirmedProfile,
    human_label_archive: &'a EncryptedHumanLabelArchive,
    adjudication_archive: &'a HumanAdjudicationArchiveRecord,
}

struct ContenderBlob {
    ordinal: i64,
    profile_fingerprint: BlobId,
    blob_id: BlobId,
    byte_len: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BenchmarkJournalNamespace {
    Diagnostic,
    Qualification,
}

impl BenchmarkJournalNamespace {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Diagnostic => "diagnostic",
            Self::Qualification => "qualification",
        }
    }

    const fn persisted_status(self) -> PersistedBenchmarkJournalStatus {
        match self {
            Self::Diagnostic => PersistedBenchmarkJournalStatus::Diagnostic,
            Self::Qualification => PersistedBenchmarkJournalStatus::Qualification,
        }
    }
}

struct PreparedExecutionRecord {
    fingerprint: BlobId,
    canonical: Vec<u8>,
}

struct PreparedBenchmarkRun {
    run_id: BlobId,
    seal_fingerprint: BlobId,
    run_class: &'static str,
    assignment_fingerprint: BlobId,
    requested_model_fingerprint: BlobId,
    observed_model_fingerprint: BlobId,
    outcome: &'static str,
    record: PreparedExecutionRecord,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreparedBenchmarkMembership {
    sequence: i64,
    event_hash: BlobId,
    run_id: BlobId,
    run_class: &'static str,
    assignment_fingerprint: BlobId,
}

struct PreparedBenchmarkJournal {
    seal_fingerprint: BlobId,
    journal_fingerprint: BlobId,
    chain_head: BlobId,
    record: PreparedExecutionRecord,
    primary_run_count: i64,
    supplemental_run_count: i64,
    runs: Vec<PreparedBenchmarkRun>,
    membership: Vec<PreparedBenchmarkMembership>,
}

impl ProjectStore {
    pub fn persist_diagnostic_benchmark_seal(
        &mut self,
        seal: &BenchmarkSeal,
    ) -> Result<PersistedDiagnosticBenchmarkSeal> {
        seal.verify_integrity()
            .map_err(|error| StoreError::InvalidResearchDiagnostic(error.to_string()))?;
        let source_blob_id = self.put_blob(seal.exact_manifest_source())?;
        if source_blob_id != seal.manifest_source_hash() {
            return Err(StoreError::InvalidResearchDiagnostic(
                "benchmark manifest source hash changed".into(),
            ));
        }
        let suite_canonical = serde_json::to_vec(&BenchmarkSuiteRecord {
            format: "loom.diagnostic-benchmark-suite.v1",
            schedule_fingerprint: seal.schedule_fingerprint(),
            manifest_source_hash: seal.manifest_source_hash(),
            manifest_artifact_hash: seal.manifest_artifact_hash(),
            campaign_artifact_hash: seal.campaign_artifact_hash(),
            case_count: seal.cases().len(),
            seal,
        })?;
        let suite_record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::BenchmarkSuite,
            &suite_canonical,
        )?;
        let seal_canonical = serde_json::to_vec(&BenchmarkSealRecord {
            format: "loom.diagnostic-benchmark-seal.v1",
            seal,
        })?;
        let seal_record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::BenchmarkSeal,
            &seal_canonical,
        )?;
        let review_bytes = serde_json::to_vec(seal.review())?;
        let review_fingerprint = BlobId::digest(&review_bytes);
        let contenders = self.prepare_contender_blobs(seal)?;
        let contender_count = i64::try_from(contenders.len()).map_err(|_| {
            StoreError::InvalidResearchDiagnostic("benchmark contender count overflow".into())
        })?;
        let case_count = i64::try_from(seal.cases().len()).map_err(|_| {
            StoreError::InvalidResearchDiagnostic("benchmark case count overflow".into())
        })?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_blob_row(
            &transaction,
            source_blob_id,
            seal.exact_manifest_source().len(),
            created_at_ms,
        )?;
        for contender in &contenders {
            insert_blob_row(
                &transaction,
                contender.blob_id,
                contender.byte_len,
                created_at_ms,
            )?;
        }
        insert_benchmark_suite(
            &transaction,
            seal,
            source_blob_id,
            suite_record.fingerprint(),
            case_count,
            created_at_ms,
        )?;
        insert_benchmark_seal(
            &transaction,
            seal,
            review_fingerprint,
            seal_record.fingerprint(),
            contender_count,
            created_at_ms,
        )?;
        insert_benchmark_contenders(&transaction, seal.fingerprint(), &contenders)?;
        transaction.commit()?;
        Ok(PersistedDiagnosticBenchmarkSeal {
            suite: seal.schedule_fingerprint(),
            seal: seal.fingerprint(),
            suite_record: suite_record.fingerprint(),
            seal_record: seal_record.fingerprint(),
        })
    }

    pub fn persist_diagnostic_benchmark_journal(
        &mut self,
        seal: &BenchmarkSeal,
        journal: &DiagnosticBenchmarkRunJournal,
    ) -> Result<PersistedBenchmarkJournal> {
        self.persist_benchmark_journal(
            seal,
            journal.record(),
            BenchmarkJournalNamespace::Diagnostic,
        )
    }

    fn persist_benchmark_journal(
        &mut self,
        seal: &BenchmarkSeal,
        journal: &VerifiedBenchmarkRunJournalRecord,
        namespace: BenchmarkJournalNamespace,
    ) -> Result<PersistedBenchmarkJournal> {
        seal.verify_integrity()
            .map_err(|error| StoreError::InvalidResearchDiagnostic(error.to_string()))?;
        if journal.seal_fingerprint() != seal.fingerprint() {
            return Err(StoreError::InvalidResearchDiagnostic(
                "benchmark journal belongs to another seal".into(),
            ));
        }
        let seal_exists: i64 = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM research_benchmark_seals WHERE seal_fingerprint = ?1
             )",
            [journal.seal_fingerprint().to_string()],
            |row| row.get(0),
        )?;
        if seal_exists != 1 {
            return Err(StoreError::InvalidResearchDiagnostic(
                "benchmark journal seal is not persisted".into(),
            ));
        }
        let prepared = self.prepare_benchmark_journal(seal, journal)?;
        let run_count = u64::try_from(prepared.membership.len()).map_err(|_| {
            StoreError::InvalidResearchDiagnostic("benchmark run count overflow".into())
        })?;
        let journal_record_fingerprint = prepared.record.fingerprint;
        persist_prepared_benchmark_journal(&mut self.connection, &prepared, namespace)?;
        Ok(PersistedBenchmarkJournal {
            seal_fingerprint: journal.seal_fingerprint(),
            journal_fingerprint: journal.fingerprint(),
            journal_record_fingerprint,
            run_count,
            status: namespace.persisted_status(),
        })
    }

    pub fn persist_diagnostic_human_label_archive(
        &mut self,
        archive: &EncryptedHumanLabelArchive,
    ) -> Result<PersistedDiagnosticHumanLabelArchive> {
        let seal_exists: i64 = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM research_benchmark_seals WHERE seal_fingerprint = ?1
             )",
            [archive.seal_fingerprint().to_string()],
            |row| row.get(0),
        )?;
        if seal_exists != 1 {
            return Err(StoreError::InvalidResearchDiagnostic(
                "human-label archive seal is not persisted".into(),
            ));
        }
        let nonce_blob_id = self.put_blob(archive.nonce())?;
        let ciphertext_blob_id = self.put_blob(archive.ciphertext())?;
        let canonical = serde_json::to_vec(&HumanLabelArchiveRecord {
            format: "loom.diagnostic-encrypted-human-label-archive.v1",
            archive,
        })?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::HumanLabelPacket,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_blob_row(
            &transaction,
            nonce_blob_id,
            archive.nonce().len(),
            created_at_ms,
        )?;
        insert_blob_row(
            &transaction,
            ciphertext_blob_id,
            archive.ciphertext().len(),
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_human_label_packets(
                packet_fingerprint, seal_fingerprint, label_schema_fingerprint,
                encryption_algorithm, key_id_fingerprint, nonce_blob_id,
                ciphertext_blob_id, associated_data_fingerprint,
                record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                archive.fingerprint().to_string(),
                archive.seal_fingerprint().to_string(),
                archive.label_schema_fingerprint().to_string(),
                human_encryption_algorithm(archive.algorithm()),
                archive.key_id_fingerprint().to_string(),
                nonce_blob_id.to_string(),
                ciphertext_blob_id.to_string(),
                archive.associated_data_fingerprint().to_string(),
                record.fingerprint().to_string(),
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_human_label_packets
             WHERE packet_fingerprint = ?1 AND seal_fingerprint = ?2
               AND label_schema_fingerprint = ?3 AND encryption_algorithm = ?4
               AND key_id_fingerprint = ?5 AND nonce_blob_id = ?6
               AND ciphertext_blob_id = ?7 AND associated_data_fingerprint = ?8
               AND record_fingerprint = ?9",
            params![
                archive.fingerprint().to_string(),
                archive.seal_fingerprint().to_string(),
                archive.label_schema_fingerprint().to_string(),
                human_encryption_algorithm(archive.algorithm()),
                archive.key_id_fingerprint().to_string(),
                nonce_blob_id.to_string(),
                ciphertext_blob_id.to_string(),
                archive.associated_data_fingerprint().to_string(),
                record.fingerprint().to_string(),
            ],
            |row| row.get(0),
        )?;
        ensure_exact_benchmark_row(exact, archive.fingerprint())?;
        transaction.commit()?;
        Ok(PersistedDiagnosticHumanLabelArchive {
            packet_fingerprint: archive.fingerprint(),
            record_fingerprint: record.fingerprint(),
        })
    }

    pub fn persist_diagnostic_provisional_benchmark_result(
        &mut self,
        seal: &BenchmarkSeal,
        journal: &DiagnosticBenchmarkRunJournal,
        profile: &FrontierReviewedProvisionalProfile,
    ) -> Result<PersistedDiagnosticBenchmarkResult> {
        let journal_record = journal.record();
        seal.verify_integrity()
            .map_err(|error| StoreError::InvalidResearchDiagnostic(error.to_string()))?;
        if profile.seal_fingerprint() != seal.fingerprint()
            || profile.journal_chain_head() != journal_record.chain_head()
            || profile.journal_record_fingerprint() != journal_record.fingerprint()
            || seal.contender(profile.candidate().fingerprint()).is_none()
        {
            return Err(StoreError::InvalidResearchDiagnostic(
                "provisional profile is not bound to the supplied seal and contender".into(),
            ));
        }
        let persisted_journal = self.persist_benchmark_journal(
            seal,
            journal_record,
            BenchmarkJournalNamespace::Qualification,
        )?;
        let canonical = serde_json::to_vec(&ProvisionalResultRecord {
            format: "loom.diagnostic-frontier-reviewed-result.v1",
            seal_fingerprint: seal.fingerprint(),
            journal_fingerprint: persisted_journal.journal_fingerprint(),
            journal_record_fingerprint: persisted_journal.journal_record_fingerprint(),
            profile,
        })?;
        self.persist_diagnostic_benchmark_result(
            seal.fingerprint(),
            profile.fingerprint(),
            "frontier_reviewed_provisional",
            None,
            persisted_journal.journal_fingerprint(),
            &canonical,
        )
    }

    pub fn persist_diagnostic_human_confirmed_benchmark_result(
        &mut self,
        seal: &BenchmarkSeal,
        journal: &DiagnosticBenchmarkRunJournal,
        profile: &HumanConfirmedProfile,
        human_label_archive: &EncryptedHumanLabelArchive,
        adjudication_archive: &HumanAdjudicationArchiveRecord,
    ) -> Result<PersistedDiagnosticBenchmarkResult> {
        let journal_record = journal.record();
        seal.verify_integrity()
            .map_err(|error| StoreError::InvalidResearchDiagnostic(error.to_string()))?;
        profile
            .validate_confirmation_evidence(human_label_archive, adjudication_archive)
            .map_err(|error| StoreError::InvalidResearchDiagnostic(error.to_string()))?;
        if profile.benchmark_seal_fingerprint() != seal.fingerprint()
            || profile.provisional().journal_chain_head() != journal_record.chain_head()
            || profile.provisional().journal_record_fingerprint() != journal_record.fingerprint()
            || seal.contender(profile.profile_fingerprint()).is_none()
        {
            return Err(StoreError::InvalidResearchDiagnostic(
                "human-confirmed profile is not bound to the supplied seal and contender".into(),
            ));
        }
        let persisted_journal = self.persist_benchmark_journal(
            seal,
            journal_record,
            BenchmarkJournalNamespace::Qualification,
        )?;
        self.persist_diagnostic_human_label_archive(human_label_archive)?;
        let canonical = serde_json::to_vec(&HumanConfirmedResultRecord {
            format: "loom.diagnostic-human-confirmed-result.v1",
            seal_fingerprint: seal.fingerprint(),
            journal_fingerprint: persisted_journal.journal_fingerprint(),
            journal_record_fingerprint: persisted_journal.journal_record_fingerprint(),
            profile,
            human_label_archive,
            adjudication_archive,
        })?;
        self.persist_diagnostic_benchmark_result(
            seal.fingerprint(),
            profile.fingerprint(),
            "human_confirmed",
            Some(human_label_archive.fingerprint()),
            persisted_journal.journal_fingerprint(),
            &canonical,
        )
    }

    fn prepare_contender_blobs(&self, seal: &BenchmarkSeal) -> Result<Vec<ContenderBlob>> {
        let mut contenders = Vec::with_capacity(seal.contenders().len() + 1);
        let baseline_bytes = serde_json::to_vec(seal.baseline())?;
        let baseline_blob_id = self.put_blob(&baseline_bytes)?;
        contenders.push(ContenderBlob {
            ordinal: 0,
            profile_fingerprint: seal.baseline().fingerprint(),
            blob_id: baseline_blob_id,
            byte_len: baseline_bytes.len(),
        });
        for (index, contender) in seal.contenders().iter().enumerate() {
            let bytes = serde_json::to_vec(contender)?;
            let blob_id = self.put_blob(&bytes)?;
            let ordinal = i64::try_from(index + 1).map_err(|_| {
                StoreError::InvalidResearchDiagnostic("benchmark contender ordinal overflow".into())
            })?;
            contenders.push(ContenderBlob {
                ordinal,
                profile_fingerprint: contender.fingerprint(),
                blob_id,
                byte_len: bytes.len(),
            });
        }
        Ok(contenders)
    }

    fn prepare_benchmark_journal(
        &self,
        seal: &BenchmarkSeal,
        journal: &VerifiedBenchmarkRunJournalRecord,
    ) -> Result<PreparedBenchmarkJournal> {
        let journal_canonical = serde_json::to_vec(&BenchmarkJournalRecord {
            format: "loom.diagnostic-benchmark-run-journal.v1",
            journal,
        })?;
        let journal_record = prepare_execution_record(
            self,
            ResearchExecutionRecordKind::BenchmarkJournal,
            journal_canonical,
        )?;
        let mut runs = Vec::with_capacity(journal.events().len());
        let mut membership = Vec::with_capacity(journal.events().len());
        let mut primary_run_count = 0_i64;
        let mut supplemental_run_count = 0_i64;
        for (expected_sequence, event) in journal.events().iter().enumerate() {
            let sequence = i64::try_from(event.sequence()).map_err(|_| {
                StoreError::InvalidResearchDiagnostic("benchmark event sequence overflow".into())
            })?;
            if usize::try_from(sequence).ok() != Some(expected_sequence) {
                return Err(StoreError::InvalidResearchDiagnostic(
                    "benchmark journal event sequence is not contiguous".into(),
                ));
            }
            let run = event.run();
            if seal.assignment(run.assignment_id()).is_none() {
                return Err(StoreError::InvalidResearchDiagnostic(
                    "benchmark run assignment is outside its exact seal".into(),
                ));
            }
            verify_run_blobs_registered(self, run)?;
            let run_class = benchmark_run_class(run.class());
            match run.class() {
                BenchmarkRunClass::Primary => {
                    primary_run_count = primary_run_count.checked_add(1).ok_or_else(|| {
                        StoreError::InvalidResearchDiagnostic(
                            "benchmark primary run count overflow".into(),
                        )
                    })?;
                }
                BenchmarkRunClass::Supplemental { .. } => {
                    supplemental_run_count =
                        supplemental_run_count.checked_add(1).ok_or_else(|| {
                            StoreError::InvalidResearchDiagnostic(
                                "benchmark supplemental run count overflow".into(),
                            )
                        })?;
                }
            }
            membership.push(PreparedBenchmarkMembership {
                sequence,
                event_hash: event.event_hash(),
                run_id: run.run_id(),
                run_class,
                assignment_fingerprint: run.assignment_id(),
            });
            runs.push(PreparedBenchmarkRun {
                run_id: run.run_id(),
                seal_fingerprint: seal.fingerprint(),
                run_class,
                assignment_fingerprint: run.assignment_id(),
                requested_model_fingerprint: run.requested_model_fingerprint(),
                observed_model_fingerprint: run.observed_model_fingerprint(),
                outcome: blinded_pair_outcome(run.outcome()),
                record: prepare_execution_record(
                    self,
                    ResearchExecutionRecordKind::BenchmarkRun,
                    serde_json::to_vec(run)?,
                )?,
            });
        }
        Ok(PreparedBenchmarkJournal {
            seal_fingerprint: seal.fingerprint(),
            journal_fingerprint: journal.fingerprint(),
            chain_head: journal.chain_head(),
            record: journal_record,
            primary_run_count,
            supplemental_run_count,
            runs,
            membership,
        })
    }

    fn persist_diagnostic_benchmark_result(
        &mut self,
        seal_fingerprint: BlobId,
        result_fingerprint: BlobId,
        status: &'static str,
        human_label_packet_fingerprint: Option<BlobId>,
        journal_fingerprint: BlobId,
        canonical: &[u8],
    ) -> Result<PersistedDiagnosticBenchmarkResult> {
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::BenchmarkResult,
            canonical,
        )?;
        let (primary_run_count, supplemental_run_count) = benchmark_run_counts(
            &self.connection,
            journal_fingerprint,
            BenchmarkJournalNamespace::Qualification,
        )?;
        if primary_run_count == 0 {
            return Err(StoreError::InvalidResearchDiagnostic(
                "benchmark result requires recorded primary runs".into(),
            ));
        }
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_benchmark_results(
                result_fingerprint, seal_fingerprint, result_status,
                primary_run_count, supplemental_run_count,
                human_label_packet_fingerprint, journal_fingerprint,
                journal_namespace, record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'qualification', ?8, ?9)",
            params![
                result_fingerprint.to_string(),
                seal_fingerprint.to_string(),
                status,
                primary_run_count,
                supplemental_run_count,
                human_label_packet_fingerprint.map(|value| value.to_string()),
                journal_fingerprint.to_string(),
                record.fingerprint().to_string(),
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_benchmark_results
             WHERE result_fingerprint = ?1 AND seal_fingerprint = ?2
               AND result_status = ?3 AND primary_run_count = ?4
               AND supplemental_run_count = ?5
               AND human_label_packet_fingerprint IS ?6
               AND journal_fingerprint = ?7
               AND journal_namespace = 'qualification'
               AND record_fingerprint = ?8",
            params![
                result_fingerprint.to_string(),
                seal_fingerprint.to_string(),
                status,
                primary_run_count,
                supplemental_run_count,
                human_label_packet_fingerprint.map(|value| value.to_string()),
                journal_fingerprint.to_string(),
                record.fingerprint().to_string(),
            ],
            |row| row.get(0),
        )?;
        ensure_exact_benchmark_row(exact, result_fingerprint)?;
        transaction.commit()?;
        Ok(PersistedDiagnosticBenchmarkResult {
            result_fingerprint,
            record_fingerprint: record.fingerprint(),
        })
    }
}

fn insert_benchmark_suite(
    transaction: &Transaction<'_>,
    seal: &BenchmarkSeal,
    source_blob_id: BlobId,
    record_fingerprint: BlobId,
    case_count: i64,
    created_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO research_benchmark_suites(
            suite_fingerprint, manifest_source_blob_id, manifest_fingerprint,
            case_count, genre_function_count, record_fingerprint, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, 5, ?5, ?6)",
        params![
            seal.schedule_fingerprint().to_string(),
            source_blob_id.to_string(),
            seal.manifest_artifact_hash().to_string(),
            case_count,
            record_fingerprint.to_string(),
            created_at_ms,
        ],
    )?;
    let exact: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_benchmark_suites
         WHERE suite_fingerprint = ?1 AND manifest_source_blob_id = ?2
           AND manifest_fingerprint = ?3 AND case_count = ?4
           AND genre_function_count = 5 AND record_fingerprint = ?5",
        params![
            seal.schedule_fingerprint().to_string(),
            source_blob_id.to_string(),
            seal.manifest_artifact_hash().to_string(),
            case_count,
            record_fingerprint.to_string(),
        ],
        |row| row.get(0),
    )?;
    ensure_exact_benchmark_row(exact, seal.schedule_fingerprint())
}

fn insert_benchmark_seal(
    transaction: &Transaction<'_>,
    seal: &BenchmarkSeal,
    review_fingerprint: BlobId,
    record_fingerprint: BlobId,
    contender_count: i64,
    created_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO research_benchmark_seals(
            seal_fingerprint, suite_fingerprint,
            benchmark_manifest_fingerprint, assignment_matrix_fingerprint,
            frontier_review_binding_fingerprint, sealed_contender_count,
            record_fingerprint, sealed_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            seal.fingerprint().to_string(),
            seal.schedule_fingerprint().to_string(),
            seal.manifest_artifact_hash().to_string(),
            seal.label_mapping_fingerprint().to_string(),
            review_fingerprint.to_string(),
            contender_count,
            record_fingerprint.to_string(),
            created_at_ms,
        ],
    )?;
    let exact: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_benchmark_seals
         WHERE seal_fingerprint = ?1 AND suite_fingerprint = ?2
           AND benchmark_manifest_fingerprint = ?3
           AND assignment_matrix_fingerprint = ?4
           AND frontier_review_binding_fingerprint = ?5
           AND sealed_contender_count = ?6 AND record_fingerprint = ?7",
        params![
            seal.fingerprint().to_string(),
            seal.schedule_fingerprint().to_string(),
            seal.manifest_artifact_hash().to_string(),
            seal.label_mapping_fingerprint().to_string(),
            review_fingerprint.to_string(),
            contender_count,
            record_fingerprint.to_string(),
        ],
        |row| row.get(0),
    )?;
    ensure_exact_benchmark_row(exact, seal.fingerprint())
}

fn insert_benchmark_contenders(
    transaction: &Transaction<'_>,
    seal_fingerprint: BlobId,
    contenders: &[ContenderBlob],
) -> Result<()> {
    for contender in contenders {
        transaction.execute(
            "INSERT OR IGNORE INTO research_benchmark_contenders(
                seal_fingerprint, contender_ordinal, profile_fingerprint,
                frozen_profile_blob_id
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                seal_fingerprint.to_string(),
                contender.ordinal,
                contender.profile_fingerprint.to_string(),
                contender.blob_id.to_string(),
            ],
        )?;
    }
    let mut statement = transaction.prepare(
        "SELECT contender_ordinal, profile_fingerprint, frozen_profile_blob_id
         FROM research_benchmark_contenders WHERE seal_fingerprint = ?1
         ORDER BY contender_ordinal",
    )?;
    let stored = statement
        .query_map([seal_fingerprint.to_string()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let expected = contenders
        .iter()
        .map(|contender| {
            (
                contender.ordinal,
                contender.profile_fingerprint.to_string(),
                contender.blob_id.to_string(),
            )
        })
        .collect::<Vec<_>>();
    if stored != expected {
        return Err(StoreError::ResearchExecutionSubjectConflict {
            subject: seal_fingerprint,
        });
    }
    Ok(())
}

fn verify_run_blobs_registered(
    store: &ProjectStore,
    run: &loom_benchmark::VerifiedBenchmarkRunRecord,
) -> Result<()> {
    verify_registered_blob(
        store,
        run.packet().exact_prompt_blob_id(),
        Some(run.packet().exact_prompt_byte_len()),
    )?;
    verify_registered_blob(
        store,
        run.packet().output_schema_blob_id(),
        Some(run.packet().output_schema_byte_len()),
    )?;
    verify_registered_blob(store, run.raw_jsonl_blob_id(), None)?;
    verify_registered_blob(store, run.final_output_blob_id(), None)
}

fn verify_registered_blob(
    store: &ProjectStore,
    blob_id: BlobId,
    exact_byte_len: Option<u64>,
) -> Result<()> {
    let byte_len = store
        .connection
        .query_row(
            "SELECT byte_len FROM blobs WHERE blob_id = ?1",
            [blob_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(byte_len) = byte_len else {
        return Err(StoreError::UnregisteredBlob(blob_id));
    };
    let observed = u64::try_from(byte_len)
        .map_err(|_| StoreError::CorruptDatabase("negative blob byte length".into()))?;
    if exact_byte_len.is_some_and(|expected| expected != observed) || observed == 0 {
        return Err(StoreError::InvalidResearchDiagnostic(
            "benchmark run blob length does not match its receipt".into(),
        ));
    }
    let maximum = u64::try_from(MAX_RESEARCH_EXECUTION_RECORD_BYTES).map_err(|_| {
        StoreError::InvalidResearchDiagnostic("benchmark blob bound overflow".into())
    })?;
    let bytes = store.read_blob_bounded(blob_id, maximum)?;
    if u64::try_from(bytes.len()).ok() != Some(observed) {
        return Err(StoreError::InvalidResearchDiagnostic(
            "benchmark run blob bytes differ from the registered length".into(),
        ));
    }
    Ok(())
}

fn prepare_execution_record(
    store: &ProjectStore,
    _kind: ResearchExecutionRecordKind,
    canonical: Vec<u8>,
) -> Result<PreparedExecutionRecord> {
    if canonical.is_empty() {
        return Err(StoreError::EmptyResearchExecutionRecord);
    }
    if canonical.len() > MAX_RESEARCH_EXECUTION_RECORD_BYTES {
        return Err(StoreError::ResearchExecutionRecordTooLarge {
            actual_bytes: canonical.len(),
            max_bytes: MAX_RESEARCH_EXECUTION_RECORD_BYTES,
        });
    }
    let fingerprint = store.put_blob(&canonical)?;
    Ok(PreparedExecutionRecord {
        fingerprint,
        canonical,
    })
}

fn persist_prepared_benchmark_journal(
    connection: &mut rusqlite::Connection,
    prepared: &PreparedBenchmarkJournal,
    namespace: BenchmarkJournalNamespace,
) -> Result<()> {
    let created_at_ms = now_unix_ms();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for run in &prepared.runs {
        insert_prepared_execution_record(
            &transaction,
            ResearchExecutionRecordKind::BenchmarkRun,
            &run.record,
            created_at_ms,
        )?;
        insert_prepared_benchmark_run(&transaction, run, created_at_ms)?;
    }
    insert_prepared_execution_record(
        &transaction,
        ResearchExecutionRecordKind::BenchmarkJournal,
        &prepared.record,
        created_at_ms,
    )?;
    insert_benchmark_membership(&transaction, prepared, namespace)?;
    insert_prepared_benchmark_journal(&transaction, prepared, namespace, created_at_ms)?;
    verify_exact_benchmark_membership(&transaction, prepared, namespace)?;
    transaction.commit()?;
    Ok(())
}

fn insert_prepared_execution_record(
    transaction: &Transaction<'_>,
    kind: ResearchExecutionRecordKind,
    record: &PreparedExecutionRecord,
    created_at_ms: i64,
) -> Result<()> {
    insert_blob_row(
        transaction,
        record.fingerprint,
        record.canonical.len(),
        created_at_ms,
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO research_execution_records(
            record_fingerprint, record_kind, record_blob_id, created_at_ms
         ) VALUES (?1, ?2, ?1, ?3)",
        params![record.fingerprint.to_string(), kind.as_str(), created_at_ms],
    )?;
    let exact: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_execution_records
         WHERE record_fingerprint = ?1 AND record_kind = ?2 AND record_blob_id = ?1",
        params![record.fingerprint.to_string(), kind.as_str()],
        |row| row.get(0),
    )?;
    ensure_exact_benchmark_row(exact, record.fingerprint)
}

fn insert_prepared_benchmark_run(
    transaction: &Transaction<'_>,
    run: &PreparedBenchmarkRun,
    completed_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO research_benchmark_runs(
            run_id, seal_fingerprint, run_class, assignment_fingerprint,
            requested_model_fingerprint, observed_model_fingerprint,
            outcome, record_fingerprint, completed_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            run.run_id.to_string(),
            run.seal_fingerprint.to_string(),
            run.run_class,
            run.assignment_fingerprint.to_string(),
            run.requested_model_fingerprint.to_string(),
            run.observed_model_fingerprint.to_string(),
            run.outcome,
            run.record.fingerprint.to_string(),
            completed_at_ms,
        ],
    )?;
    let exact: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_benchmark_runs
         WHERE run_id = ?1 AND seal_fingerprint = ?2 AND run_class = ?3
           AND assignment_fingerprint = ?4 AND requested_model_fingerprint = ?5
           AND observed_model_fingerprint = ?6 AND outcome = ?7
           AND record_fingerprint = ?8",
        params![
            run.run_id.to_string(),
            run.seal_fingerprint.to_string(),
            run.run_class,
            run.assignment_fingerprint.to_string(),
            run.requested_model_fingerprint.to_string(),
            run.observed_model_fingerprint.to_string(),
            run.outcome,
            run.record.fingerprint.to_string(),
        ],
        |row| row.get(0),
    )?;
    ensure_exact_benchmark_row(exact, run.run_id)
}

fn insert_benchmark_membership(
    transaction: &Transaction<'_>,
    prepared: &PreparedBenchmarkJournal,
    namespace: BenchmarkJournalNamespace,
) -> Result<()> {
    for member in &prepared.membership {
        transaction.execute(
            "INSERT OR IGNORE INTO research_benchmark_journal_members(
                journal_fingerprint, journal_namespace, event_sequence,
                event_hash, run_id, run_class, assignment_fingerprint
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                prepared.journal_fingerprint.to_string(),
                namespace.as_str(),
                member.sequence,
                member.event_hash.to_string(),
                member.run_id.to_string(),
                member.run_class,
                member.assignment_fingerprint.to_string(),
            ],
        )?;
    }
    Ok(())
}

fn insert_prepared_benchmark_journal(
    transaction: &Transaction<'_>,
    prepared: &PreparedBenchmarkJournal,
    namespace: BenchmarkJournalNamespace,
    created_at_ms: i64,
) -> Result<()> {
    let run_count = prepared
        .primary_run_count
        .checked_add(prepared.supplemental_run_count)
        .ok_or_else(|| {
            StoreError::InvalidResearchDiagnostic("benchmark journal run count overflow".into())
        })?;
    transaction.execute(
        "INSERT OR IGNORE INTO research_benchmark_journals(
            journal_fingerprint, journal_namespace, seal_fingerprint,
            chain_head, run_count, primary_run_count, supplemental_run_count,
            record_fingerprint, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            prepared.journal_fingerprint.to_string(),
            namespace.as_str(),
            prepared.seal_fingerprint.to_string(),
            prepared.chain_head.to_string(),
            run_count,
            prepared.primary_run_count,
            prepared.supplemental_run_count,
            prepared.record.fingerprint.to_string(),
            created_at_ms,
        ],
    )?;
    let exact: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM research_benchmark_journals
         WHERE journal_fingerprint = ?1 AND journal_namespace = ?2
           AND seal_fingerprint = ?3 AND chain_head = ?4 AND run_count = ?5
           AND primary_run_count = ?6 AND supplemental_run_count = ?7
           AND record_fingerprint = ?8",
        params![
            prepared.journal_fingerprint.to_string(),
            namespace.as_str(),
            prepared.seal_fingerprint.to_string(),
            prepared.chain_head.to_string(),
            run_count,
            prepared.primary_run_count,
            prepared.supplemental_run_count,
            prepared.record.fingerprint.to_string(),
        ],
        |row| row.get(0),
    )?;
    ensure_exact_benchmark_row(exact, prepared.journal_fingerprint)
}

type StoredBenchmarkMembership = (i64, String, String, String, String);

fn verify_exact_benchmark_membership(
    transaction: &Transaction<'_>,
    prepared: &PreparedBenchmarkJournal,
    namespace: BenchmarkJournalNamespace,
) -> Result<()> {
    let mut statement = transaction.prepare(
        "SELECT event_sequence, event_hash, run_id, run_class, assignment_fingerprint
         FROM research_benchmark_journal_members
         WHERE journal_fingerprint = ?1 AND journal_namespace = ?2
         ORDER BY event_sequence",
    )?;
    let stored = statement
        .query_map(
            params![prepared.journal_fingerprint.to_string(), namespace.as_str(),],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<StoredBenchmarkMembership>, _>>()?;
    let expected = prepared
        .membership
        .iter()
        .map(|member| {
            (
                member.sequence,
                member.event_hash.to_string(),
                member.run_id.to_string(),
                member.run_class.to_owned(),
                member.assignment_fingerprint.to_string(),
            )
        })
        .collect::<Vec<_>>();
    if stored != expected {
        return Err(StoreError::ResearchExecutionSubjectConflict {
            subject: prepared.journal_fingerprint,
        });
    }
    Ok(())
}

fn benchmark_run_counts(
    connection: &rusqlite::Connection,
    journal_fingerprint: BlobId,
    namespace: BenchmarkJournalNamespace,
) -> Result<(i64, i64)> {
    connection
        .query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN run_class = 'primary' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN run_class = 'supplemental' THEN 1 ELSE 0 END), 0)
             FROM research_benchmark_journal_members
             WHERE journal_fingerprint = ?1 AND journal_namespace = ?2",
            params![journal_fingerprint.to_string(), namespace.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(StoreError::from)
}

const fn benchmark_run_class(class: BenchmarkRunClass) -> &'static str {
    match class {
        BenchmarkRunClass::Primary => "primary",
        BenchmarkRunClass::Supplemental { .. } => "supplemental",
    }
}

const fn blinded_pair_outcome(outcome: BlindedPairOutcome) -> &'static str {
    match outcome {
        BlindedPairOutcome::LeftWin => "left_win",
        BlindedPairOutcome::RightWin => "right_win",
        BlindedPairOutcome::Tie => "tie",
        BlindedPairOutcome::Abstain => "abstain",
    }
}

const fn human_encryption_algorithm(algorithm: HumanLabelEncryptionAlgorithm) -> &'static str {
    match algorithm {
        HumanLabelEncryptionAlgorithm::XChaCha20Poly1305V1 => "xchacha20poly1305_v1",
        HumanLabelEncryptionAlgorithm::AgeX25519V1 => "age_x25519_v1",
    }
}

fn ensure_exact_benchmark_row(count: i64, subject: BlobId) -> Result<()> {
    if count != 1 {
        return Err(StoreError::ResearchExecutionSubjectConflict { subject });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn rejected_journal_conflict_rolls_back_every_semantic_row() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "benchmark transaction").expect("store");
        let seal = seed_benchmark_seal(&mut store);
        let assignment = BlobId::digest(b"assignment/a");
        let first = prepared_journal(&store, seal, b"journal/a", assignment);
        persist_prepared_benchmark_journal(
            &mut store.connection,
            &first,
            BenchmarkJournalNamespace::Diagnostic,
        )
        .expect("first journal");
        let before = semantic_counts(&store);

        let mut conflict = prepared_journal(&store, seal, b"journal/conflict", assignment);
        conflict.journal_fingerprint = first.journal_fingerprint;
        assert!(
            persist_prepared_benchmark_journal(
                &mut store.connection,
                &conflict,
                BenchmarkJournalNamespace::Diagnostic,
            )
            .is_err()
        );
        assert_eq!(semantic_counts(&store), before);

        let fresh = prepared_journal(
            &store,
            seal,
            b"journal/fresh-after-rejection",
            BlobId::digest(b"assignment/fresh"),
        );
        persist_prepared_benchmark_journal(
            &mut store.connection,
            &fresh,
            BenchmarkJournalNamespace::Diagnostic,
        )
        .expect("fresh journal after rollback");
        let after = semantic_counts(&store);
        assert_eq!(after.1, before.1 + 1, "fresh run must not be reserved");
        assert_eq!(after.2, before.2 + 1, "fresh membership must append");
        assert_eq!(after.3, before.3 + 1, "fresh journal must append");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn diagnostic_alternates_cannot_contaminate_qualification_membership() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "benchmark namespaces").expect("store");
        let seal = seed_benchmark_seal(&mut store);
        let assignment = BlobId::digest(b"assignment/qualified");
        let qualified = prepared_journal(&store, seal, b"journal/qualified", assignment);

        persist_prepared_benchmark_journal(
            &mut store.connection,
            &qualified,
            BenchmarkJournalNamespace::Diagnostic,
        )
        .expect("diagnostic copy");
        persist_prepared_benchmark_journal(
            &mut store.connection,
            &qualified,
            BenchmarkJournalNamespace::Qualification,
        )
        .expect("qualification copy");
        assert_eq!(
            benchmark_run_counts(
                &store.connection,
                qualified.journal_fingerprint,
                BenchmarkJournalNamespace::Qualification,
            )
            .expect("qualified counts"),
            (1, 0)
        );

        let alternate = prepared_journal(&store, seal, b"journal/alternate", assignment);
        persist_prepared_benchmark_journal(
            &mut store.connection,
            &alternate,
            BenchmarkJournalNamespace::Diagnostic,
        )
        .expect("alternate diagnostic");
        assert_eq!(
            benchmark_run_counts(
                &store.connection,
                qualified.journal_fingerprint,
                BenchmarkJournalNamespace::Qualification,
            )
            .expect("qualified counts after diagnostic"),
            (1, 0)
        );

        let before = semantic_counts(&store);
        assert!(
            persist_prepared_benchmark_journal(
                &mut store.connection,
                &alternate,
                BenchmarkJournalNamespace::Qualification,
            )
            .is_err(),
            "one seal cannot acquire an alternate qualification journal"
        );
        assert_eq!(semantic_counts(&store), before);

        let exact_member: (i64, String, String) = store
            .connection
            .query_row(
                "SELECT event_sequence, event_hash, run_id
                 FROM research_benchmark_journal_members
                 WHERE journal_fingerprint = ?1 AND journal_namespace = 'qualification'",
                [qualified.journal_fingerprint.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("exact qualified member");
        assert_eq!(
            exact_member,
            (
                qualified.membership[0].sequence,
                qualified.membership[0].event_hash.to_string(),
                qualified.membership[0].run_id.to_string(),
            )
        );

        let result_record = store
            .persist_research_execution_record(
                ResearchExecutionRecordKind::BenchmarkResult,
                b"qualification membership result record",
            )
            .expect("result record");
        let result_fingerprint = BlobId::digest(b"qualification membership result");
        let wrong_count = store.connection.execute(
            "INSERT INTO research_benchmark_results(
                result_fingerprint, seal_fingerprint, result_status,
                primary_run_count, supplemental_run_count,
                human_label_packet_fingerprint, journal_fingerprint,
                journal_namespace, record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, 'frontier_reviewed_provisional', 2, 0,
                       NULL, ?3, 'qualification', ?4, ?5)",
            params![
                result_fingerprint.to_string(),
                seal.to_string(),
                qualified.journal_fingerprint.to_string(),
                result_record.fingerprint().to_string(),
                now_unix_ms(),
            ],
        );
        assert!(wrong_count.is_err());
        store
            .connection
            .execute(
                "INSERT INTO research_benchmark_results(
                    result_fingerprint, seal_fingerprint, result_status,
                    primary_run_count, supplemental_run_count,
                    human_label_packet_fingerprint, journal_fingerprint,
                    journal_namespace, record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, 'frontier_reviewed_provisional', 1, 0,
                           NULL, ?3, 'qualification', ?4, ?5)",
                params![
                    result_fingerprint.to_string(),
                    seal.to_string(),
                    qualified.journal_fingerprint.to_string(),
                    result_record.fingerprint().to_string(),
                    now_unix_ms(),
                ],
            )
            .expect("result with exact qualified membership counts");
    }

    fn seed_benchmark_seal(store: &mut ProjectStore) -> BlobId {
        let suite = BlobId::digest(b"test benchmark suite");
        let seal = BlobId::digest(b"test benchmark seal");
        let source = b"test benchmark manifest";
        let source_blob = store.put_blob(source).expect("source CAS");
        let suite_record = store
            .persist_research_execution_record(
                ResearchExecutionRecordKind::BenchmarkSuite,
                b"test benchmark suite record",
            )
            .expect("suite record");
        let seal_record = store
            .persist_research_execution_record(
                ResearchExecutionRecordKind::BenchmarkSeal,
                b"test benchmark seal record",
            )
            .expect("seal record");
        let created_at_ms = now_unix_ms();
        let transaction = store.connection.transaction().expect("seed transaction");
        insert_blob_row(&transaction, source_blob, source.len(), created_at_ms)
            .expect("source row");
        transaction
            .execute(
                "INSERT INTO research_benchmark_suites(
                    suite_fingerprint, manifest_source_blob_id, manifest_fingerprint,
                    case_count, genre_function_count, record_fingerprint, created_at_ms
                 ) VALUES (?1, ?2, ?3, 30, 5, ?4, ?5)",
                params![
                    suite.to_string(),
                    source_blob.to_string(),
                    BlobId::digest(b"test manifest artifact").to_string(),
                    suite_record.fingerprint().to_string(),
                    created_at_ms,
                ],
            )
            .expect("suite row");
        transaction
            .execute(
                "INSERT INTO research_benchmark_seals(
                    seal_fingerprint, suite_fingerprint,
                    benchmark_manifest_fingerprint, assignment_matrix_fingerprint,
                    frontier_review_binding_fingerprint, sealed_contender_count,
                    record_fingerprint, sealed_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 2, ?6, ?7)",
                params![
                    seal.to_string(),
                    suite.to_string(),
                    BlobId::digest(b"test benchmark manifest").to_string(),
                    BlobId::digest(b"test assignment matrix").to_string(),
                    BlobId::digest(b"test frontier binding").to_string(),
                    seal_record.fingerprint().to_string(),
                    created_at_ms,
                ],
            )
            .expect("seal row");
        transaction.commit().expect("seed commit");
        seal
    }

    fn prepared_journal(
        store: &ProjectStore,
        seal: BlobId,
        tag: &[u8],
        assignment: BlobId,
    ) -> PreparedBenchmarkJournal {
        let run_id = tagged(tag, b"run");
        let run_record = prepare_execution_record(
            store,
            ResearchExecutionRecordKind::BenchmarkRun,
            [tag, b"/run-record"].concat(),
        )
        .expect("prepare run record");
        let journal_record = prepare_execution_record(
            store,
            ResearchExecutionRecordKind::BenchmarkJournal,
            [tag, b"/journal-record"].concat(),
        )
        .expect("prepare journal record");
        let event_hash = tagged(tag, b"event");
        PreparedBenchmarkJournal {
            seal_fingerprint: seal,
            journal_fingerprint: tagged(tag, b"journal"),
            chain_head: event_hash,
            record: journal_record,
            primary_run_count: 1,
            supplemental_run_count: 0,
            runs: vec![PreparedBenchmarkRun {
                run_id,
                seal_fingerprint: seal,
                run_class: "primary",
                assignment_fingerprint: assignment,
                requested_model_fingerprint: BlobId::digest(b"test model"),
                observed_model_fingerprint: BlobId::digest(b"test model"),
                outcome: "tie",
                record: run_record,
            }],
            membership: vec![PreparedBenchmarkMembership {
                sequence: 0,
                event_hash,
                run_id,
                run_class: "primary",
                assignment_fingerprint: assignment,
            }],
        }
    }

    fn tagged(tag: &[u8], suffix: &[u8]) -> BlobId {
        BlobId::digest(&[tag, b"/", suffix].concat())
    }

    fn semantic_counts(store: &ProjectStore) -> (i64, i64, i64, i64) {
        store
            .connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM research_execution_records),
                    (SELECT COUNT(*) FROM research_benchmark_runs),
                    (SELECT COUNT(*) FROM research_benchmark_journal_members),
                    (SELECT COUNT(*) FROM research_benchmark_journals)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("semantic row counts")
    }
}

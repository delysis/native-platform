use loom_research_types::CampaignId;
use loom_types::{BlobId, now_unix_ms};
use rusqlite::{TransactionBehavior, params};
use serde::Serialize;

use crate::{
    MAX_RESEARCH_EXECUTION_RECORD_BYTES, ProjectStore, ResearchExecutionRecordKind, Result,
    StoreError,
};

/// Caller-supplied archive metadata remains diagnostic. `loom-campaign` owns
/// the move-only evaluated-candidate leases that can produce a live archive;
/// depending on it here would create a store/campaign cycle.
#[derive(Clone, Copy, Debug)]
pub struct DiagnosticArchiveSnapshotPersistence<'a> {
    pub campaign_id: CampaignId,
    pub archive_fingerprint: BlobId,
    pub parent_archive_fingerprint: Option<BlobId>,
    pub generation: u64,
    pub cell_count: u32,
    pub candidate_count: u32,
    pub exact_snapshot_record_bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticArchiveSnapshot {
    archive_fingerprint: BlobId,
    record_fingerprint: BlobId,
}

impl PersistedDiagnosticArchiveSnapshot {
    pub const fn archive_fingerprint(self) -> BlobId {
        self.archive_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }
}

#[derive(Serialize)]
struct ArchiveRecordBinding {
    format: &'static str,
    campaign_id: CampaignId,
    archive_fingerprint: BlobId,
    parent_archive_fingerprint: Option<BlobId>,
    generation: u64,
    cell_count: u32,
    candidate_count: u32,
    exact_snapshot_record_blob_id: BlobId,
}

impl ProjectStore {
    pub fn persist_diagnostic_archive_snapshot(
        &mut self,
        input: DiagnosticArchiveSnapshotPersistence<'_>,
    ) -> Result<PersistedDiagnosticArchiveSnapshot> {
        validate_snapshot_shape(&input)?;
        if (input.generation == 0) != input.parent_archive_fingerprint.is_none() {
            return Err(StoreError::InvalidResearchDiagnostic(
                "archive generation zero alone may omit a parent".into(),
            ));
        }
        let campaign_exists: i64 = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM research_campaigns WHERE campaign_id = ?1)",
            [input.campaign_id.to_string()],
            |row| row.get(0),
        )?;
        if campaign_exists != 1 {
            return Err(StoreError::ResearchCampaignNotPersisted(input.campaign_id));
        }
        let exact_snapshot_record_blob_id = self.put_blob(input.exact_snapshot_record_bytes)?;
        let canonical = serde_json::to_vec(&ArchiveRecordBinding {
            format: "loom.diagnostic-archive-snapshot.v1",
            campaign_id: input.campaign_id,
            archive_fingerprint: input.archive_fingerprint,
            parent_archive_fingerprint: input.parent_archive_fingerprint,
            generation: input.generation,
            cell_count: input.cell_count,
            candidate_count: input.candidate_count,
            exact_snapshot_record_blob_id,
        })?;
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::ArchiveSnapshot,
            &canonical,
        )?;
        let generation = i64::try_from(input.generation).map_err(|_| {
            StoreError::InvalidResearchDiagnostic(
                "archive generation exceeds SQLite's integer domain".into(),
            )
        })?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::provenance::insert_blob_row(
            &transaction,
            exact_snapshot_record_blob_id,
            input.exact_snapshot_record_bytes.len(),
            created_at_ms,
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_archive_snapshots(
                archive_fingerprint, campaign_id, parent_archive_fingerprint,
                generation, cell_count, candidate_count, record_fingerprint,
                created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                input.archive_fingerprint.to_string(),
                input.campaign_id.to_string(),
                input
                    .parent_archive_fingerprint
                    .map(|value| value.to_string()),
                generation,
                i64::from(input.cell_count),
                i64::from(input.candidate_count),
                record.fingerprint().to_string(),
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_archive_snapshots
             WHERE archive_fingerprint = ?1 AND campaign_id = ?2
               AND parent_archive_fingerprint IS ?3 AND generation = ?4
               AND cell_count = ?5 AND candidate_count = ?6
               AND record_fingerprint = ?7",
            params![
                input.archive_fingerprint.to_string(),
                input.campaign_id.to_string(),
                input
                    .parent_archive_fingerprint
                    .map(|value| value.to_string()),
                generation,
                i64::from(input.cell_count),
                i64::from(input.candidate_count),
                record.fingerprint().to_string(),
            ],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Err(StoreError::ResearchExecutionSubjectConflict {
                subject: input.archive_fingerprint,
            });
        }
        transaction.commit()?;
        Ok(PersistedDiagnosticArchiveSnapshot {
            archive_fingerprint: input.archive_fingerprint,
            record_fingerprint: record.fingerprint(),
        })
    }
}

fn validate_snapshot_shape(input: &DiagnosticArchiveSnapshotPersistence<'_>) -> Result<()> {
    if input.exact_snapshot_record_bytes.is_empty()
        || input.exact_snapshot_record_bytes.len() > MAX_RESEARCH_EXECUTION_RECORD_BYTES
    {
        return Err(StoreError::InvalidResearchDiagnostic(
            "archive snapshot record byte length is outside its bounded domain".into(),
        ));
    }
    if input.cell_count > input.candidate_count {
        return Err(StoreError::InvalidResearchDiagnostic(
            "archive cannot contain more non-empty cells than candidates".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use loom_research_types::CampaignId;
    use loom_types::BlobId;

    use super::*;

    #[test]
    fn archive_snapshot_shape_is_bounded_and_internally_consistent() {
        let mut input = DiagnosticArchiveSnapshotPersistence {
            campaign_id: CampaignId::new(),
            archive_fingerprint: BlobId::digest(b"archive"),
            parent_archive_fingerprint: None,
            generation: 0,
            cell_count: 1,
            candidate_count: 0,
            exact_snapshot_record_bytes: b"snapshot",
        };
        assert!(matches!(
            validate_snapshot_shape(&input),
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));
        input.cell_count = 0;
        input.exact_snapshot_record_bytes = &[];
        assert!(matches!(
            validate_snapshot_shape(&input),
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));
    }
}

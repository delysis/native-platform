use super::*;
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs::{File, OpenOptions};

use retention::{MAX_PENDING as MAX_JOBS, Usage};
const MAX_GRANTS: i64 = 64;

pub(super) struct Ledger {
    database: Connection,
    identity: Identity,
    _lease: File,
}

impl Ledger {
    pub fn open(directory: &Path, identity: Identity) -> Result<Self> {
        if directory
            .symlink_metadata()
            .is_ok_and(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
        {
            return Err(Error::Invalid("Compute storage cannot be a symbolic link"));
        }
        std::fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let database_path = directory.join("compute.db");
        let lease_path = directory.join("compute.lock");
        for path in [
            &database_path,
            &lease_path,
            &directory.join("compute.db-wal"),
            &directory.join("compute.db-shm"),
        ] {
            if path.symlink_metadata().is_ok_and(|metadata| {
                !metadata.file_type().is_file() || metadata.len() > retention::MAX_DATABASE_BYTES
            }) {
                return Err(Error::Invalid("Compute storage must use ordinary files"));
            }
        }
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lease_path)?;
        lease
            .try_lock_exclusive()
            .map_err(|_| Error::Invalid("Another process owns this compute ledger"))?;
        let exists = database_path.exists();
        let database = Connection::open(&database_path)?;
        let version: i64 = database.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if exists && version != STORAGE_VERSION {
            return Err(Error::Invalid(
                "Unsupported compute ledger; it was preserved",
            ));
        }
        if exists {
            let owner: String =
                database.query_row("SELECT key FROM owner", [], |row| row.get(0))?;
            if owner != identity.public_key().to_string() {
                return Err(Error::Invalid("Compute ledger belongs to another device"));
            }
        }
        database.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;",
        )?;
        retention::configure(&database)?;
        if !exists {
            database.execute_batch("CREATE TABLE owner (key TEXT NOT NULL);
                CREATE TABLE grants (id TEXT PRIMARY KEY, body TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0, retired INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE jobs (peer TEXT NOT NULL, id TEXT NOT NULL, grant_id TEXT NOT NULL REFERENCES grants(id),
                    input TEXT NOT NULL, settled INTEGER NOT NULL DEFAULT 0 CHECK (settled IN (0, 1)),
                    stored_bytes INTEGER NOT NULL CHECK (stored_bytes >= 0), PRIMARY KEY(peer, id));
                CREATE INDEX jobs_retention ON jobs(settled, stored_bytes);
                CREATE INDEX jobs_by_grant ON jobs(grant_id);
                CREATE TABLE receipts (peer TEXT NOT NULL, job TEXT NOT NULL, revision INTEGER NOT NULL,
                    body TEXT NOT NULL, PRIMARY KEY(peer, job, revision), FOREIGN KEY(peer, job) REFERENCES jobs(peer, id));")?;
            database.execute(
                "INSERT INTO owner(key) VALUES (?)",
                [identity.public_key().to_string()],
            )?;
            database.pragma_update(None, "user_version", STORAGE_VERSION)?;
            #[cfg(unix)]
            File::open(directory)?.sync_all()?;
        }
        let owner: String = database.query_row("SELECT key FROM owner", [], |row| row.get(0))?;
        if owner != identity.public_key().to_string() {
            return Err(Error::Invalid("Compute ledger belongs to another device"));
        }
        let grants: i64 =
            database.query_row("SELECT count(*) FROM grants WHERE retired = 0", [], |row| {
                row.get(0)
            })?;
        if grants > MAX_GRANTS {
            return Err(Error::Invalid("Too many active compute grants"));
        }
        let mut ledger = Self {
            database,
            identity,
            _lease: lease,
        };
        ledger.usage()?.validate()?;
        // No continuation can be owned across a process restart. Append this
        // fact without changing the immutable accepted input or prior receipts.
        let keys = ledger
            .database
            .prepare("SELECT peer, id FROM jobs WHERE settled = 0 LIMIT ?")?
            .query_map([MAX_JOBS + 1], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (peer, job) in keys {
            let peer = peer
                .parse()
                .map_err(|_| Error::Invalid("Invalid compute peer"))?;
            let job = job
                .parse()
                .map_err(|_| Error::Invalid("Invalid compute job"))?;
            let receipt = ledger
                .get(peer, job)?
                .ok_or(Error::Invalid("Compute job has no receipt"))?;
            if !receipt.payload.status.is_terminal() {
                ledger.transition(peer, job, ComputeStatus::Interrupted)?;
            }
        }
        Ok(ledger)
    }

    pub fn grant(&mut self, grant: ComputeGrant) -> Result<()> {
        let body = serde_json::to_string(&grant)?;
        let existing: Option<(String, bool)> = self
            .database
            .query_row(
                "SELECT body, revoked FROM grants WHERE id = ?",
                [grant.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((old, revoked)) = existing {
            return if old == body && !revoked {
                Ok(())
            } else {
                Err(Error::Invalid(
                    "Compute grant identity cannot be replaced or restored",
                ))
            };
        }
        self.ensure_grant_capacity()?;
        self.database.execute(
            "INSERT INTO grants(id, body) VALUES (?, ?)",
            params![grant.id.to_string(), body],
        )?;
        Ok(())
    }

    pub fn revoke(&mut self, id: Uuid) -> Result<()> {
        if self.database.execute(
            "UPDATE grants SET revoked = 1, retired = 1 WHERE id = ?",
            [id.to_string()],
        )? == 0
        {
            // A cancelled, uncertain local grant must reject even a delayed
            // first admission. Its identity remains after leaving the active set.
            self.database.execute(
                "INSERT INTO grants(id, body, revoked, retired) VALUES (?, '', 1, 1)",
                [id.to_string()],
            )?;
        }
        Ok(())
    }

    fn ensure_grant_capacity(&mut self) -> Result<()> {
        // Only exhausted grants with every job settled can leave the active
        // set. Keep the original body and identity: an exact retry cannot
        // restore its budget, and an uncertain revocation cannot be forgotten.
        for grant in self.grants()? {
            if self.remaining_jobs(&grant)? == 0 {
                let unfinished: bool = self.database.query_row(
                    "SELECT EXISTS(SELECT 1 FROM jobs WHERE grant_id = ? AND settled = 0)",
                    [grant.id.to_string()],
                    |row| row.get(0),
                )?;
                if !unfinished {
                    self.database.execute(
                        "UPDATE grants SET retired = 1 WHERE id = ?",
                        [grant.id.to_string()],
                    )?;
                }
            }
        }
        let count: i64 = self.database.query_row(
            "SELECT count(*) FROM grants WHERE retired = 0",
            [],
            |row| row.get(0),
        )?;
        if count >= MAX_GRANTS {
            return Err(Error::Invalid(
                "Too many active compute grants; revoke an unused grant",
            ));
        }
        Ok(())
    }

    pub fn remaining_jobs(&self, grant: &ComputeGrant) -> Result<u32> {
        let used: u32 = self.database.query_row(
            "SELECT count(*) FROM jobs WHERE grant_id = ?",
            [grant.id.to_string()],
            |row| row.get(0),
        )?;
        Ok(grant.jobs.saturating_sub(used))
    }

    pub fn grants(&self) -> Result<Vec<ComputeGrant>> {
        let bodies = self
            .database
            .prepare(
                "SELECT body FROM grants WHERE revoked = 0 AND retired = 0 ORDER BY id LIMIT 65",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if bodies.len() > MAX_GRANTS as usize {
            return Err(Error::Invalid("Too many active compute grants"));
        }
        bodies
            .into_iter()
            .map(|body| {
                let grant: ComputeGrant = serde_json::from_str(&body)?;
                grant.validate()?;
                Ok(grant)
            })
            .collect()
    }

    pub fn find_grant(&self, id: Uuid) -> Result<Option<ComputeGrant>> {
        Ok(self.grants()?.into_iter().find(|grant| grant.id == id))
    }

    pub fn has_capacity(&self, grant: &ComputeGrant, input: Option<&ComputeInput>) -> Result<bool> {
        let input_bytes = input
            .map(serde_json::to_vec)
            .transpose()?
            .map_or(0, |bytes| bytes.len());
        Ok(self.remaining_jobs(grant)? > 0 && self.usage()?.can_accept(input_bytes))
    }

    fn usage(&self) -> Result<Usage> {
        self.database
            .query_row(
                "SELECT coalesce(sum(settled = 0), 0),
            coalesce(sum(CASE WHEN settled = 0 THEN stored_bytes ELSE 0 END), 0),
            coalesce(sum(stored_bytes), 0) FROM jobs",
                [],
                |row| {
                    Ok(Usage {
                        unfinished: row.get(0)?,
                        active_bytes: row.get(1)?,
                        retained_bytes: row.get(2)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    pub fn accept(
        &mut self,
        job: &HostComputeJob,
        fingerprint: String,
    ) -> Result<RemoteJobReceipt> {
        let created_at_ms = now_ms()?;
        let receipt = self.identity.sign(RemoteJobRecord {
            kind: RemoteRecordKind::Execution,
            job: job.id,
            peer: job.peer,
            grant: job.grant.id,
            request_fingerprint: fingerprint,
            model: job.grant.model.clone(),
            revision: 0,
            created_at_ms,
            recorded_at_ms: created_at_ms,
            status: ComputeStatus::Accepted,
        })?;
        let input = serde_json::to_string(&job.input)?;
        let body = serde_json::to_string(&receipt)?;
        let tx = self.database.transaction()?;
        tx.execute(
            "INSERT INTO jobs(peer, id, grant_id, input, stored_bytes) VALUES (?, ?, ?, ?, ?)",
            params![
                job.peer.to_string(),
                job.id.to_string(),
                job.grant.id.to_string(),
                input,
                (input.len() + body.len()) as i64,
            ],
        )?;
        tx.execute(
            "INSERT INTO receipts(peer, job, revision, body) VALUES (?, ?, 0, ?)",
            params![job.peer.to_string(), job.id.to_string(), body,],
        )?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn get(&self, peer: PublicKey, job: Uuid) -> Result<Option<RemoteJobReceipt>> {
        let saved: Option<(String, Option<String>, bool, i64)> = self.database.query_row(
            "SELECT grant_id, CASE WHEN length(CAST(input AS BLOB)) <= ? THEN input END, settled, stored_bytes
             FROM jobs WHERE peer = ? AND id = ?",
            params![MAX_COMPUTE_FRAME_BYTES as i64, peer.to_string(), job.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional()?;
        let Some((grant_id, input, settled, stored_bytes)) = saved else {
            return Ok(None);
        };
        let input = input.ok_or(Error::Invalid("Compute input exceeds its storage limit"))?;
        let grant: String = self.database.query_row(
            "SELECT body FROM grants WHERE id = ?",
            [&grant_id],
            |row| row.get(0),
        )?;
        let request = ClientRequest {
            id: job,
            host: self.identity.public_key(),
            grant: serde_json::from_str(&grant)?,
            input: serde_json::from_str(&input)?,
        };
        if request.grant.peer != peer || request.grant.id.to_string() != grant_id {
            return Err(Error::Invalid("Compute receipt identity mismatch"));
        }
        let mut bytes = input.len() as i64;
        let mut latest = None;
        for body in self.receipt_bodies(peer, job)? {
            bytes += body.len() as i64;
            let receipt: RemoteJobReceipt = serde_json::from_str(&body)?;
            client::validate_receipt(&request, &receipt)?;
            if let Some(previous) = &latest {
                client::validate_advance(previous, &receipt)?;
            }
            latest = Some(receipt);
        }
        let receipt = latest.ok_or(Error::Invalid("Compute job has no receipt"))?;
        if settled != receipt.payload.status.is_terminal() || bytes != stored_bytes {
            return Err(Error::Invalid(
                "Compute retention index does not match its receipts",
            ));
        }
        Ok(Some(receipt))
    }

    fn receipt_bodies(&self, peer: PublicKey, job: Uuid) -> Result<Vec<String>> {
        let mut query = self.database.prepare(
            "SELECT CASE WHEN length(CAST(body AS BLOB)) <= ? THEN body END
             FROM receipts WHERE peer = ? AND job = ? ORDER BY revision LIMIT 5",
        )?;
        let bodies = query
            .query_map(
                params![
                    crate::MAX_FRAME_BYTES as i64,
                    peer.to_string(),
                    job.to_string()
                ],
                |row| row.get(0),
            )?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        if bodies.len() > 4 {
            return Err(Error::Invalid("Too many compute receipts"));
        }
        Ok(bodies)
    }

    pub fn cancel(
        &mut self,
        peer: PublicKey,
        job: Uuid,
        reason: ComputeCancellation,
    ) -> Result<Option<RemoteJobReceipt>> {
        let Some(receipt) = self.get(peer, job)? else {
            return Ok(None);
        };
        if matches!(
            receipt.payload.status,
            ComputeStatus::Accepted | ComputeStatus::Running
        ) {
            return self
                .transition(peer, job, ComputeStatus::Cancelling { reason })
                .map(Some);
        }
        Ok(Some(receipt))
    }

    pub fn transition(
        &mut self,
        peer: PublicKey,
        job: Uuid,
        status: ComputeStatus,
    ) -> Result<RemoteJobReceipt> {
        let old = self
            .get(peer, job)?
            .ok_or(Error::Invalid("Compute job disappeared"))?;
        if old.payload.status.is_terminal() {
            return if old.payload.status == status {
                Ok(old)
            } else {
                Err(Error::Invalid(
                    "A terminal compute receipt cannot be replaced",
                ))
            };
        }
        let valid = matches!(
            (&old.payload.status, &status),
            (
                ComputeStatus::Accepted,
                ComputeStatus::Running
                    | ComputeStatus::Cancelling { .. }
                    | ComputeStatus::Interrupted
            ) | (
                ComputeStatus::Running,
                ComputeStatus::Completed { .. }
                    | ComputeStatus::Failed { .. }
                    | ComputeStatus::Cancelling { .. }
                    | ComputeStatus::Interrupted
            ) | (
                ComputeStatus::Cancelling { .. },
                ComputeStatus::Cancelled { .. } | ComputeStatus::Interrupted
            )
        );
        if !valid {
            return Err(Error::Invalid("Invalid compute job transition"));
        }
        let receipt = self.identity.sign(RemoteJobRecord {
            revision: old
                .payload
                .revision
                .checked_add(1)
                .ok_or(Error::Invalid("Compute revision overflow"))?,
            recorded_at_ms: now_ms()?.max(old.payload.recorded_at_ms),
            status,
            ..old.payload
        })?;
        let body = serde_json::to_string(&receipt)?;
        let tx = self.database.transaction()?;
        tx.execute(
            "INSERT INTO receipts(peer, job, revision, body) VALUES (?, ?, ?, ?)",
            params![
                peer.to_string(),
                job.to_string(),
                receipt.payload.revision,
                body
            ],
        )?;
        tx.execute("UPDATE jobs SET settled = ?, stored_bytes = stored_bytes + ? WHERE peer = ? AND id = ?",
            params![receipt.payload.status.is_terminal(), body.len() as i64, peer.to_string(), job.to_string()])?;
        tx.commit()?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(identity: &Identity) -> HostComputeJob {
        HostComputeJob {
            id: Uuid::new_v4(),
            peer: identity.public_key(),
            grant: ComputeGrant {
                id: Uuid::new_v4(),
                cabal: Uuid::new_v4(),
                epoch: 0,
                peer: identity.public_key(),
                model: ComputeModel {
                    media: Vec::new(),
                    fingerprint: "ab".repeat(32),
                    name: "Model".into(),
                },
                max_output_tokens: 10,
                max_seconds: 1,
                jobs: 1,
            },
            input: ComputeInput {
                format: ComputePromptFormat::Raw,
                media: Vec::new(),
                prompt: "Preserved input".into(),
                max_output_tokens: 10,
                seed: 1,
            },
        }
    }

    #[test]
    fn restart_appends_interrupted_without_rewriting_or_replaying_accepted_work() -> Result<()> {
        for state in [
            ComputeStatus::Accepted,
            ComputeStatus::Running,
            ComputeStatus::Cancelling {
                reason: ComputeCancellation::Requested,
            },
        ] {
            let directory = tempfile::tempdir()?;
            let identity = Identity::generate()?;
            let job = job(&identity);
            let mut ledger = Ledger::open(directory.path(), identity.clone())?;
            ledger.grant(job.grant.clone())?;
            let accepted = ledger.accept(&job, job.input.fingerprint(job.grant.id)?)?;
            if state != ComputeStatus::Accepted {
                ledger.transition(job.peer, job.id, state)?;
            }
            drop(ledger);
            let mut reopened = Ledger::open(directory.path(), identity)?;
            let interrupted = reopened.get(job.peer, job.id)?.expect("durable job");
            assert_eq!(interrupted.payload.status, ComputeStatus::Interrupted);
            assert_eq!(
                interrupted.payload.request_fingerprint,
                accepted.payload.request_fingerprint
            );
            assert!(
                !reopened.has_capacity(&job.grant, Some(&job.input))?,
                "restart cannot restore a consumed budget"
            );
            assert_eq!(
                reopened.receipt_bodies(job.peer, job.id)?[0],
                serde_json::to_string(&accepted)?
            );
            assert!(
                reopened
                    .transition(job.peer, job.id, ComputeStatus::Running)
                    .is_err()
            );
            assert!(
                reopened
                    .transition(
                        job.peer,
                        job.id,
                        ComputeStatus::Completed {
                            text: "invented".into()
                        }
                    )
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn revoking_an_uncertain_grant_before_admission_prevents_a_late_retry() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let job = job(&identity);
        let mut ledger = Ledger::open(directory.path(), identity.clone())?;
        ledger.revoke(job.grant.id)?;
        ledger.revoke(job.grant.id)?;
        assert!(ledger.grants()?.is_empty());
        drop(ledger);
        let mut ledger = Ledger::open(directory.path(), identity)?;
        assert!(
            ledger.grant(job.grant).is_err(),
            "a late grant must stay revoked"
        );
        Ok(())
    }

    #[test]
    fn uncertain_revocations_retain_identity_without_occupying_active_grant_slots() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let mut ledger = Ledger::open(directory.path(), identity)?;
        let mut first = Uuid::nil();
        for index in 0..=MAX_GRANTS {
            let id = Uuid::new_v4();
            if index == 0 {
                first = id;
            }
            ledger.revoke(id)?;
        }
        ledger.revoke(Uuid::new_v4())?;
        ledger.revoke(first)?;
        assert!(ledger.grants()?.is_empty());
        let mut late = job(&Identity::generate()?);
        late.grant.id = first;
        assert!(ledger.grant(late.grant).is_err());
        Ok(())
    }

    #[test]
    fn a_single_device_owner_holds_the_ledger_until_it_is_dropped() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let ledger = Ledger::open(directory.path(), identity.clone())?;
        assert!(Ledger::open(directory.path(), identity.clone()).is_err());
        drop(ledger);
        assert!(Ledger::open(directory.path(), Identity::generate()?).is_err());
        Ledger::open(directory.path(), identity)?;
        Ok(())
    }

    #[test]
    fn incompatible_schema_is_rejected_without_rewriting_existing_data() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("compute.db");
        let database = Connection::open(&path)?;
        database.execute_batch("PRAGMA user_version = 3; CREATE TABLE sentinel (body TEXT); INSERT INTO sentinel VALUES ('keep me');")?;
        drop(database);
        let before = std::fs::read(&path)?;
        assert!(Ledger::open(directory.path(), Identity::generate()?).is_err());
        assert_eq!(std::fs::read(path)?, before);
        Ok(())
    }

    #[test]
    fn settled_jobs_outlive_active_limits_without_restoring_spent_budgets() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let mut ledger = Ledger::open(directory.path(), identity.clone())?;
        let mut job = job(&identity);
        let mut saved = Vec::new();
        // More jobs and grant identities than either old lifetime limit.
        for _ in 0..MAX_JOBS + 4 {
            job.id = Uuid::new_v4();
            job.grant.id = Uuid::new_v4();
            ledger.grant(job.grant.clone())?;
            assert!(ledger.has_capacity(&job.grant, Some(&job.input))?);
            ledger.accept(&job, job.input.fingerprint(job.grant.id)?)?;
            let terminal = ledger.transition(job.peer, job.id, ComputeStatus::Interrupted)?;
            saved.push((job.clone(), terminal.hash()?));
        }
        drop(ledger);
        let mut ledger = Ledger::open(directory.path(), identity)?;
        for (job, hash) in &saved {
            assert_eq!(
                ledger.get(job.peer, job.id)?.expect("archived").hash()?,
                *hash
            );
            assert_eq!(ledger.remaining_jobs(&job.grant)?, 0);
            ledger.grant(job.grant.clone())?;
            assert!(!ledger.has_capacity(&job.grant, Some(&job.input))?);
        }
        assert_eq!(
            ledger.database.query_row(
                "SELECT count(*) FROM jobs WHERE settled = 0",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            ledger.database.query_row(
                "SELECT count(*) FROM jobs WHERE settled = 1",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            MAX_JOBS + 4
        );
        assert_eq!(
            ledger.grants()?.len(),
            1,
            "retrying retired grants cannot reactivate them"
        );
        Ok(())
    }

    #[test]
    fn active_grants_stay_bounded_while_revoked_identities_remain_retired() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let mut ledger = Ledger::open(directory.path(), identity.clone())?;
        let first = job(&identity).grant;
        ledger.grant(first.clone())?;
        for _ in 1..MAX_GRANTS {
            ledger.grant(job(&identity).grant)?;
        }
        let next = job(&identity).grant;
        assert!(ledger.grant(next.clone()).is_err());
        ledger.revoke(first.id)?;
        ledger.grant(next)?;
        assert!(ledger.grant(first).is_err());
        Ok(())
    }

    #[test]
    fn spent_grant_is_not_retired_while_its_job_is_unfinished() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let mut ledger = Ledger::open(directory.path(), identity.clone())?;
        let pending = job(&identity);
        ledger.grant(pending.grant.clone())?;
        ledger.accept(&pending, pending.input.fingerprint(pending.grant.id)?)?;
        for _ in 1..MAX_GRANTS {
            ledger.grant(job(&identity).grant)?;
        }
        let next = job(&identity).grant;
        assert!(ledger.grant(next.clone()).is_err());
        assert!(ledger.find_grant(pending.grant.id)?.is_some());
        ledger.transition(pending.peer, pending.id, ComputeStatus::Interrupted)?;
        ledger.grant(next)?;
        assert!(ledger.find_grant(pending.grant.id)?.is_none());
        ledger.grant(pending.grant.clone())?;
        assert!(ledger.find_grant(pending.grant.id)?.is_none());
        assert_eq!(ledger.remaining_jobs(&pending.grant)?, 0);
        Ok(())
    }

    #[test]
    fn archive_commit_is_atomic_and_missing_history_never_becomes_a_new_job() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let mut ledger = Ledger::open(directory.path(), identity.clone())?;
        let job = job(&identity);
        ledger.grant(job.grant.clone())?;
        let accepted = ledger.accept(&job, job.input.fingerprint(job.grant.id)?)?;
        ledger.database.execute_batch("CREATE TRIGGER reject_retirement BEFORE UPDATE OF settled ON jobs BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;")?;
        assert!(
            ledger
                .transition(job.peer, job.id, ComputeStatus::Interrupted)
                .is_err()
        );
        assert_eq!(
            ledger.get(job.peer, job.id)?.expect("original").hash()?,
            accepted.hash()?
        );
        assert_eq!(ledger.usage()?.unfinished, 1);
        assert_eq!(ledger.receipt_bodies(job.peer, job.id)?.len(), 1);
        ledger
            .database
            .execute_batch("DROP TRIGGER reject_retirement")?;
        ledger.transition(job.peer, job.id, ComputeStatus::Interrupted)?;
        let input: String = ledger.database.query_row(
            "SELECT input FROM jobs WHERE peer = ? AND id = ?",
            params![job.peer.to_string(), job.id.to_string()],
            |row| row.get(0),
        )?;
        assert_eq!(input, serde_json::to_string(&job.input)?);
        assert_eq!(
            ledger.receipt_bodies(job.peer, job.id)?[0],
            serde_json::to_string(&accepted)?
        );
        ledger.database.execute(
            "DELETE FROM receipts WHERE peer = ? AND job = ?",
            params![job.peer.to_string(), job.id.to_string()],
        )?;
        assert!(ledger.get(job.peer, job.id).is_err());
        assert!(
            ledger
                .accept(&job, job.input.fingerprint(job.grant.id)?)
                .is_err()
        );
        assert_eq!(ledger.remaining_jobs(&job.grant)?, 0);
        Ok(())
    }

    #[test]
    fn large_media_reserves_its_full_input_and_the_terminal_before_admission() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let mut ledger = Ledger::open(directory.path(), identity.clone())?;
        let mut job = job(&identity);
        job.grant.jobs = 256;
        job.grant.model.media = vec![ComputeModality::Image];
        job.input.media = vec![ComputeMedia::new(
            ComputeMediaFormat::Png,
            &vec![42; MAX_COMPUTE_MEDIA_BYTES],
        )?];
        ledger.grant(job.grant.clone())?;
        let mut accepted = 0;
        for _ in 0..6 {
            assert!(ledger.has_capacity(&job.grant, Some(&job.input))?);
            job.id = Uuid::new_v4();
            ledger.accept(&job, job.input.fingerprint(job.grant.id)?)?;
            ledger.transition(job.peer, job.id, ComputeStatus::Running)?;
            ledger.transition(
                job.peer,
                job.id,
                ComputeStatus::Completed {
                    text: "\0".repeat(MAX_COMPUTE_TEXT_BYTES),
                },
            )?;
            accepted += 1;
        }
        assert_eq!(
            accepted, 6,
            "settled media leaves the active storage budget"
        );
        let usage = ledger.usage()?;
        assert!(usage.retained_bytes > retention::MAX_ACTIVE_BYTES);
        assert!(
            usage.retained_bytes
                < 6 * (serde_json::to_vec(&job.input)?.len() as i64 + retention::RECEIPT_RESERVE)
        );
        assert_eq!(usage.active_bytes, 0);
        let bytes: i64 = ledger.database.query_row(
            "SELECT (SELECT coalesce(sum(length(CAST(input AS BLOB))), 0) FROM jobs) + (SELECT coalesce(sum(length(CAST(body AS BLOB))), 0) FROM receipts)", [], |row| row.get(0),
        )?;
        assert_eq!(bytes, usage.retained_bytes);
        assert!(
            ledger
                .get(job.peer, job.id)?
                .expect("the last admitted job retains its terminal receipt")
                .payload
                .status
                .is_terminal()
        );
        Ok(())
    }

    #[derive(Debug, Default)]
    struct OwnedUntilReleased {
        started: tokio::sync::Notify,
        cancelled: tokio::sync::Notify,
        released: tokio::sync::Notify,
        joined: std::sync::atomic::AtomicBool,
    }

    #[derive(Debug)]
    struct FaultExecutor(Arc<OwnedUntilReleased>);

    impl ComputeExecutor for FaultExecutor {
        fn execute(
            &self,
            _job: HostComputeJob,
            cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = std::result::Result<String, ComputeFailure>> + Send>>
        {
            let owner = self.0.clone();
            Box::pin(async move {
                owner.started.notify_one();
                cancel.cancelled().await;
                owner.cancelled.notify_one();
                owner.released.notified().await;
                owner
                    .joined
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok("never publish this cancelled output".into())
            })
        }
    }

    #[tokio::test]
    async fn failed_revocation_persistence_stops_and_joins_before_reporting_failure() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let job = job(&identity);
        let owner = Arc::new(OwnedUntilReleased::default());
        let host = ComputeHost::open(
            directory.path(),
            identity,
            Arc::new(|_| true),
            Arc::new(FaultExecutor(owner.clone())),
        )?;
        host.grant(job.grant.clone())?;
        assert!(matches!(
            host.respond(
                job.peer,
                Request::Submit {
                    job: job.id,
                    grant: job.grant.id,
                    input: job.input.clone(),
                }
            )?,
            Response::Receipt { .. }
        ));
        tokio::time::timeout(Duration::from_secs(5), owner.started.notified())
            .await
            .expect("worker started");
        host.lock()?
            .ledger
            .database
            .pragma_update(None, "query_only", true)?;
        assert!(host.revoke(job.grant.id).is_err());
        tokio::time::timeout(Duration::from_secs(5), owner.cancelled.notified())
            .await
            .expect("cancel despite write failure");
        assert!(!owner.joined.load(std::sync::atomic::Ordering::SeqCst));
        host.lock()?
            .ledger
            .database
            .pragma_update(None, "query_only", false)?;
        owner.released.notify_one();
        assert!(
            host.shutdown().await.is_err(),
            "joining must not erase the persistence failure"
        );
        assert!(
            host.shutdown().await.is_err(),
            "every shutdown observer sees the failure"
        );
        assert!(owner.joined.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            host.lock()?
                .ledger
                .get(job.peer, job.id)?
                .expect("preserved receipt")
                .payload
                .status,
            ComputeStatus::Interrupted
        );
        Ok(())
    }
}

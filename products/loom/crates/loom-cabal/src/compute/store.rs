use super::*;
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs::{File, OpenOptions};

const MAX_JOBS: i64 = 256;
const MAX_GRANTS: i64 = 64;
const MAX_STORED_BYTES: i64 = 64 * 1024 * 1024;
// JSON can expand one text byte to six bytes. Reserve space for the accepted
// input, maximum output, and all transition receipts before dispatching work.
const JOB_STORAGE_RESERVE: i64 = 1024 * 1024;

pub(super) struct Ledger {
    database: Connection,
    identity: Identity,
    _lease: File,
}

impl Ledger {
    pub fn open(directory: &Path, identity: Identity) -> Result<Self> {
        if directory
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
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
        for path in [&database_path, &lease_path] {
            if path
                .symlink_metadata()
                .is_ok_and(|metadata| !metadata.file_type().is_file())
            {
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
        if exists && version != 1 {
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
        database.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;
            PRAGMA max_page_count = 32768;
            CREATE TABLE IF NOT EXISTS owner (key TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS grants (id TEXT PRIMARY KEY, body TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS jobs (peer TEXT NOT NULL, id TEXT NOT NULL, grant_id TEXT NOT NULL REFERENCES grants(id),
                input TEXT NOT NULL, PRIMARY KEY(peer, id));
            CREATE TABLE IF NOT EXISTS receipts (peer TEXT NOT NULL, job TEXT NOT NULL, revision INTEGER NOT NULL,
                body TEXT NOT NULL, PRIMARY KEY(peer, job, revision), FOREIGN KEY(peer, job) REFERENCES jobs(peer, id));")?;
        if !exists {
            database.execute(
                "INSERT INTO owner(key) VALUES (?)",
                [identity.public_key().to_string()],
            )?;
            database.pragma_update(None, "user_version", 1)?;
        }
        let owner: String = database.query_row("SELECT key FROM owner", [], |row| row.get(0))?;
        if owner != identity.public_key().to_string() {
            return Err(Error::Invalid("Compute ledger belongs to another device"));
        }
        let jobs: i64 = database.query_row("SELECT count(*) FROM jobs", [], |row| row.get(0))?;
        let grants: i64 =
            database.query_row("SELECT count(*) FROM grants", [], |row| row.get(0))?;
        if jobs > MAX_JOBS || grants > MAX_GRANTS {
            return Err(Error::Invalid("Compute ledger exceeds storage limits"));
        }
        let mut ledger = Self {
            database,
            identity,
            _lease: lease,
        };
        // No continuation can be owned across a process restart. Append this
        // fact without changing the immutable accepted input or prior receipts.
        let keys = ledger
            .database
            .prepare("SELECT peer, id FROM jobs")?
            .query_map([], |row| {
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
        let count: i64 = self
            .database
            .query_row("SELECT count(*) FROM grants", [], |row| row.get(0))?;
        if count >= MAX_GRANTS {
            return Err(Error::Invalid("Compute grant ledger is full"));
        }
        self.database.execute(
            "INSERT INTO grants(id, body) VALUES (?, ?)",
            params![grant.id.to_string(), body],
        )?;
        Ok(())
    }

    pub fn revoke(&mut self, id: Uuid) -> Result<()> {
        self.database.execute(
            "UPDATE grants SET revoked = 1 WHERE id = ?",
            [id.to_string()],
        )?;
        Ok(())
    }

    pub fn grants(&self) -> Result<Vec<ComputeGrant>> {
        let bodies = self
            .database
            .prepare("SELECT body FROM grants WHERE revoked = 0 ORDER BY id")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
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

    pub fn has_capacity(&self, grant: &ComputeGrant) -> Result<bool> {
        let total: i64 = self
            .database
            .query_row("SELECT count(*) FROM jobs", [], |row| row.get(0))?;
        let used: i64 = self.database.query_row(
            "SELECT count(*) FROM jobs WHERE grant_id = ?",
            [grant.id.to_string()],
            |row| row.get(0),
        )?;
        let bytes: i64 = self.database.query_row(
            "SELECT (SELECT coalesce(sum(length(CAST(input AS BLOB))), 0) FROM jobs)
                + (SELECT coalesce(sum(length(CAST(body AS BLOB))), 0) FROM receipts)",
            [],
            |row| row.get(0),
        )?;
        Ok(total < MAX_JOBS
            && used < i64::from(grant.jobs)
            && bytes <= MAX_STORED_BYTES - JOB_STORAGE_RESERVE)
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
        let tx = self.database.transaction()?;
        tx.execute(
            "INSERT INTO jobs(peer, id, grant_id, input) VALUES (?, ?, ?, ?)",
            params![
                job.peer.to_string(),
                job.id.to_string(),
                job.grant.id.to_string(),
                serde_json::to_string(&job.input)?,
            ],
        )?;
        tx.execute(
            "INSERT INTO receipts(peer, job, revision, body) VALUES (?, ?, 0, ?)",
            params![
                job.peer.to_string(),
                job.id.to_string(),
                serde_json::to_string(&receipt)?,
            ],
        )?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn get(&self, peer: PublicKey, job: Uuid) -> Result<Option<RemoteJobReceipt>> {
        let encoded: Option<String> = self.database.query_row(
            "SELECT body FROM receipts WHERE peer = ? AND job = ? ORDER BY revision DESC LIMIT 1",
            params![peer.to_string(), job.to_string()], |row| row.get(0),
        ).optional()?;
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        if encoded.len() > crate::MAX_FRAME_BYTES {
            return Err(Error::Invalid("Compute receipt exceeds limit"));
        }
        let receipt: RemoteJobReceipt = serde_json::from_str(&encoded)?;
        receipt.verify()?;
        if receipt.signer != self.identity.public_key()
            || receipt.payload.peer != peer
            || receipt.payload.job != job
        {
            return Err(Error::Invalid("Compute receipt identity mismatch"));
        }
        let (grant, input): (String, String) = self.database.query_row(
            "SELECT grant_id, input FROM jobs WHERE peer = ? AND id = ?",
            params![peer.to_string(), job.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let grant = grant
            .parse()
            .map_err(|_| Error::Invalid("Invalid compute grant identity"))?;
        let input: ComputeInput = serde_json::from_str(&input)?;
        if receipt.payload.grant != grant
            || receipt.payload.request_fingerprint != input.fingerprint(grant)?
        {
            return Err(Error::Invalid(
                "Compute receipt does not match the saved input",
            ));
        }
        Ok(Some(receipt))
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
        self.database.execute(
            "INSERT INTO receipts(peer, job, revision, body) VALUES (?, ?, ?, ?)",
            params![
                peer.to_string(),
                job.to_string(),
                receipt.payload.revision,
                serde_json::to_string(&receipt)?,
            ],
        )?;
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
                    fingerprint: "ab".repeat(32),
                    name: "Model".into(),
                },
                max_output_tokens: 10,
                max_seconds: 1,
                jobs: 1,
            },
            input: ComputeInput {
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
                !reopened.has_capacity(&job.grant)?,
                "restart cannot restore a consumed budget"
            );
            let original: String = reopened.database.query_row(
                "SELECT body FROM receipts WHERE revision = 0",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(original, serde_json::to_string(&accepted)?);
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
        database.execute_batch("PRAGMA user_version = 77; CREATE TABLE sentinel (body TEXT); INSERT INTO sentinel VALUES ('keep me');")?;
        drop(database);
        let before = std::fs::read(&path)?;
        assert!(Ledger::open(directory.path(), Identity::generate()?).is_err());
        assert_eq!(std::fs::read(path)?, before);
        Ok(())
    }

    #[test]
    fn grant_and_storage_caps_keep_consumed_identities_durable() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let mut ledger = Ledger::open(directory.path(), identity.clone())?;
        let mut job = job(&identity);
        job.grant.jobs = MAX_JOBS as u32;
        ledger.grant(job.grant.clone())?;
        for _ in 0..MAX_JOBS {
            job.id = Uuid::new_v4();
            assert!(ledger.has_capacity(&job.grant)?);
            ledger.accept(&job, job.input.fingerprint(job.grant.id)?)?;
            ledger.transition(job.peer, job.id, ComputeStatus::Interrupted)?;
        }
        assert!(!ledger.has_capacity(&job.grant)?);
        for _ in 1..MAX_GRANTS {
            job.grant.id = Uuid::new_v4();
            ledger.grant(job.grant.clone())?;
            ledger.revoke(job.grant.id)?;
        }
        job.grant.id = Uuid::new_v4();
        assert!(
            ledger.grant(job.grant).is_err(),
            "revocation must not recycle identity tombstones"
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

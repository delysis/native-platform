//! Durable requesting-device intent and immutable, checked host assertions.
//! Opening this store never submits or retries a network request.
use super::*;
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs::{File, OpenOptions};

use retention::{MAX_PENDING as MAX_JOBS, Usage};
const MAX_RECORD_BYTES: usize = MAX_COMPUTE_FRAME_BYTES;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientRequest {
    pub id: Uuid,
    pub host: PublicKey,
    pub grant: ComputeGrant,
    pub input: ComputeInput,
}

impl ClientRequest {
    fn validate(&self, peer: PublicKey) -> Result<()> {
        self.grant.validate()?;
        self.input.validate_for_model(&self.grant.model)?;
        if self.id.is_nil()
            || self.host == peer
            || self.grant.peer != peer
            || self.input.max_output_tokens > self.grant.max_output_tokens
        {
            return Err(Error::Invalid("Invalid requested compute job"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ClientJob {
    pub request: ClientRequest,
    pub cancel_requested: bool,
    /// None means unconfirmed, never proof that the host did not execute.
    pub receipt: Option<RemoteJobReceipt>,
}

pub struct ComputeClient {
    database: Connection,
    peer: PublicKey,
    _lease: File,
}

impl std::fmt::Debug for ComputeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputeClient")
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

impl ComputeClient {
    pub fn open(directory: &Path, peer: PublicKey) -> Result<Self> {
        Self::open_inner(directory, peer, true)
    }

    /// Reopen saved requests without creating a replacement ledger if the
    /// directory or database was removed. This operation never dispatches work.
    pub fn open_existing(directory: &Path, peer: PublicKey) -> Result<Self> {
        Self::open_inner(directory, peer, false)
    }

    fn open_inner(directory: &Path, peer: PublicKey, create: bool) -> Result<Self> {
        if !cfg!(unix) {
            return Err(Error::Invalid(
                "Private compute request storage requires Unix",
            ));
        }
        if directory
            .symlink_metadata()
            .is_ok_and(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
        {
            return Err(Error::Invalid(
                "Compute request storage must use a private directory",
            ));
        }
        if create {
            std::fs::create_dir_all(directory)?;
        } else if !directory.join("requests.db").try_exists()? {
            return Err(Error::Invalid("Saved compute requests are unavailable"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let database_path = directory.join("requests.db");
        for name in [
            "requests.db",
            "requests.db-wal",
            "requests.db-shm",
            "requests.lock",
        ] {
            if directory
                .join(name)
                .symlink_metadata()
                .is_ok_and(|metadata| {
                    !metadata.file_type().is_file()
                        || metadata.len() > retention::MAX_DATABASE_BYTES
                })
            {
                return Err(Error::Invalid(
                    "Compute request storage must use bounded ordinary files",
                ));
            }
        }
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("requests.lock"))?;
        lease
            .try_lock_exclusive()
            .map_err(|_| Error::Invalid("Another process owns these compute requests"))?;
        let exists = database_path.exists();
        let mut flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE;
        if create {
            flags |= rusqlite::OpenFlags::SQLITE_OPEN_CREATE;
        }
        let database = Connection::open_with_flags(&database_path, flags)?;
        let version: i64 = database.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if exists {
            if version != 3 {
                return Err(Error::Invalid(
                    "Unsupported compute requests; they were preserved",
                ));
            }
            let owner: String =
                database.query_row("SELECT key FROM owner", [], |row| row.get(0))?;
            if owner != peer.to_string() {
                return Err(Error::Invalid("Compute requests belong to another device"));
            }
        }
        database.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;",
        )?;
        retention::configure(&database)?;
        if !exists {
            database.execute_batch("CREATE TABLE owner (key TEXT NOT NULL);
                CREATE TABLE requests (id TEXT PRIMARY KEY, cabal TEXT NOT NULL, body TEXT NOT NULL,
                    settled INTEGER NOT NULL DEFAULT 0 CHECK (settled IN (0, 1)), stored_bytes INTEGER NOT NULL CHECK (stored_bytes >= 0));
                CREATE INDEX requests_retention ON requests(settled, stored_bytes);
                CREATE INDEX requests_by_cabal ON requests(cabal);
                CREATE TABLE cancellations (job TEXT PRIMARY KEY REFERENCES requests(id));
                CREATE TABLE receipts (job TEXT NOT NULL REFERENCES requests(id), revision INTEGER NOT NULL,
                    terminal INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY(job, revision));")?;
            database.execute("INSERT INTO owner(key) VALUES (?)", [peer.to_string()])?;
            database.pragma_update(None, "user_version", 3)?;
            File::open(directory)?.sync_all()?;
        }
        let client = Self {
            database,
            peer,
            _lease: lease,
        };
        client.usage()?.validate()?;
        // Read and verify saved assertions without interpreting a restart as a
        // remote terminal event or implicitly resubmitting an unfinished job.
        for id in client.ids(None)? {
            client.get(id)?;
        }
        Ok(client)
    }

    /// Call before sending any bytes. The application must bind this request to
    /// its reviewed model target and exact source revision before preparing it.
    pub fn prepare(&mut self, request: ClientRequest) -> Result<ClientJob> {
        request.validate(self.peer)?;
        if let Some(old) = self.get(request.id)? {
            return if old.request == request {
                Ok(old)
            } else {
                Err(Error::Invalid(
                    "Compute job identity belongs to another request",
                ))
            };
        }
        let body = encode(&request)?;
        if !self.usage()?.can_accept(body.len()) {
            return Err(Error::Invalid(
                "Compute request ledger or archive is full; existing jobs were preserved",
            ));
        }
        self.database.execute(
            "INSERT INTO requests(id, cabal, body, stored_bytes) VALUES (?, ?, ?, ?)",
            params![
                request.id.to_string(),
                request.grant.cabal.to_string(),
                body,
                body.len() as i64,
            ],
        )?;
        self.get(request.id)?
            .ok_or(Error::Invalid("Prepared compute request disappeared"))
    }

    /// Persist cancellation intent before sending it. A later explicit retry
    /// must continue cancelling this same input, never return to submission.
    pub fn request_cancel(&mut self, job: Uuid) -> Result<ClientJob> {
        self.get(job)?
            .ok_or(Error::Invalid("Unknown compute request"))?;
        self.database.execute(
            "INSERT OR IGNORE INTO cancellations(job) VALUES (?)",
            [job.to_string()],
        )?;
        self.get(job)?
            .ok_or(Error::Invalid("Compute request disappeared"))
    }

    pub fn get(&self, job: Uuid) -> Result<Option<ClientJob>> {
        let saved: Option<(String, String)> = self
            .database
            .query_row(
                "SELECT cabal, body FROM requests WHERE id = ? AND length(CAST(body AS BLOB)) <= ?",
                params![
                    job.to_string(),
                    i64::try_from(MAX_RECORD_BYTES).expect("bounded record limit")
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((cabal, body)) = saved else {
            let exists: bool = self.database.query_row(
                "SELECT EXISTS(SELECT 1 FROM requests WHERE id = ?)",
                [job.to_string()],
                |row| row.get(0),
            )?;
            return if exists {
                Err(Error::Invalid("Compute request exceeds its limit"))
            } else {
                Ok(None)
            };
        };
        let request: ClientRequest = serde_json::from_str(&body)?;
        request.validate(self.peer)?;
        if request.id != job || request.grant.cabal.to_string() != cabal {
            return Err(Error::Invalid("Compute request identity mismatch"));
        }
        let mut bytes = body.len() as i64;
        let mut receipt: Option<RemoteJobReceipt> = None;
        let mut query = self.database.prepare("SELECT revision, terminal, length(CAST(body AS BLOB)), CASE WHEN length(CAST(body AS BLOB)) <= ? THEN body END
            FROM receipts WHERE job = ? ORDER BY revision")?;
        let mut rows = query.query(params![
            i64::try_from(MAX_RECORD_BYTES).expect("bounded record limit"),
            job.to_string()
        ])?;
        while let Some(row) = rows.next()? {
            let receipt_bytes: i64 = row.get(2)?;
            if receipt_bytes > i64::try_from(MAX_RECORD_BYTES).expect("bounded record limit") {
                return Err(Error::Invalid("Compute receipt exceeds its limit"));
            }
            bytes += receipt_bytes;
            let body: String = row.get(3)?;
            let next: RemoteJobReceipt = serde_json::from_str(&body)?;
            validate_receipt(&request, &next)?;
            if row.get::<_, u32>(0)? != next.payload.revision
                || row.get::<_, bool>(1)? != next.payload.status.is_terminal()
            {
                return Err(Error::Invalid("Compute receipt index mismatch"));
            }
            if let Some(old) = &receipt {
                validate_advance(old, &next)?;
            }
            receipt = Some(next);
        }
        let (settled, stored_bytes): (bool, i64) = self.database.query_row(
            "SELECT settled, stored_bytes FROM requests WHERE id = ?",
            [job.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if settled
            != receipt
                .as_ref()
                .is_some_and(|receipt| receipt.payload.status.is_terminal())
            || bytes != stored_bytes
        {
            return Err(Error::Invalid(
                "Compute retention index does not match its receipts",
            ));
        }
        let cancel_requested = self.database.query_row(
            "SELECT EXISTS(SELECT 1 FROM cancellations WHERE job = ?)",
            [job.to_string()],
            |row| row.get(0),
        )?;
        Ok(Some(ClientJob {
            request,
            cancel_requested,
            receipt,
        }))
    }

    /// Newest 256 identities only. Read and release each job individually when
    /// constructing previews; never collect a page of full media payloads.
    pub fn job_ids(&self, cabal: Uuid) -> Result<Vec<Uuid>> {
        self.ids(Some(cabal))
    }

    pub fn record(&mut self, receipt: RemoteJobReceipt) -> Result<ClientJob> {
        let job = self
            .get(receipt.payload.job)?
            .ok_or(Error::Invalid("Unsolicited compute receipt"))?;
        validate_receipt(&job.request, &receipt)?;
        if let Some(old) = &job.receipt {
            if receipt.payload.revision <= old.payload.revision {
                let body: Option<String> = self
                    .database
                    .query_row(
                        "SELECT body FROM receipts WHERE job = ? AND revision = ?",
                        params![receipt.payload.job.to_string(), receipt.payload.revision],
                        |row| row.get(0),
                    )
                    .optional()?;
                return if body.as_deref() == Some(encode(&receipt)?.as_str()) {
                    Ok(job)
                } else {
                    Err(Error::Invalid(
                        "A remote compute receipt cannot be rewritten or rolled back",
                    ))
                };
            }
            validate_advance(old, &receipt)?;
        }
        let body = encode(&receipt)?;
        let usage = self.usage()?;
        if usage.active_bytes + body.len() as i64 > retention::MAX_ACTIVE_BYTES
            || usage.retained_bytes + body.len() as i64 > retention::MAX_RETAINED_BYTES
        {
            return Err(Error::Invalid("Compute receipt storage is full"));
        }
        let tx = self.database.transaction()?;
        tx.execute(
            "INSERT INTO receipts(job, revision, terminal, body) VALUES (?, ?, ?, ?)",
            params![
                receipt.payload.job.to_string(),
                receipt.payload.revision,
                receipt.payload.status.is_terminal(),
                body
            ],
        )?;
        tx.execute(
            "UPDATE requests SET settled = ?, stored_bytes = stored_bytes + ? WHERE id = ?",
            params![
                receipt.payload.status.is_terminal(),
                body.len() as i64,
                receipt.payload.job.to_string()
            ],
        )?;
        tx.commit()?;
        self.get(receipt.payload.job)?
            .ok_or(Error::Invalid("Compute request disappeared"))
    }

    fn ids(&self, cabal: Option<Uuid>) -> Result<Vec<Uuid>> {
        // Opening verifies only the bounded unfinished set. Settled history is
        // verified lazily by identity, and UI listing returns the newest page.
        let (sql, value, limit) = if let Some(cabal) = cabal {
            (
                "SELECT id FROM requests WHERE cabal = ? ORDER BY rowid DESC LIMIT ?",
                cabal.to_string(),
                MAX_JOBS,
            )
        } else {
            (
                "SELECT id FROM requests WHERE settled = 0 AND ? = '' ORDER BY rowid DESC LIMIT ?",
                String::new(),
                MAX_JOBS + 1,
            )
        };
        let mut query = self.database.prepare(sql)?;
        let ids = query
            .query_map(params![value, limit], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if ids.len() > MAX_JOBS as usize {
            return Err(Error::Invalid("Too many unfinished compute requests"));
        }
        ids.into_iter()
            .map(|id| {
                id.parse()
                    .map_err(|_| Error::Invalid("Invalid compute request identity"))
            })
            .collect()
    }

    fn usage(&self) -> Result<Usage> {
        self.database
            .query_row(
                "SELECT coalesce(sum(settled = 0), 0),
            coalesce(sum(CASE WHEN settled = 0 THEN stored_bytes ELSE 0 END), 0),
            coalesce(sum(stored_bytes), 0) FROM requests",
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
}

fn encode(value: &impl Serialize) -> Result<String> {
    let body = serde_json::to_string(value)?;
    if body.len() > MAX_RECORD_BYTES {
        return Err(Error::Invalid("Compute record exceeds its limit"));
    }
    Ok(body)
}

pub(super) fn validate_receipt(request: &ClientRequest, receipt: &RemoteJobReceipt) -> Result<()> {
    receipt.verify()?;
    let value = &receipt.payload;
    if receipt.signer != request.host
        || value.peer != request.grant.peer
        || value.job != request.id
        || value.grant != request.grant.id
        || value.model != request.grant.model
        || value.request_fingerprint != request.input.fingerprint(request.grant.id)?
        || value.created_at_ms > value.recorded_at_ms
    {
        return Err(Error::Invalid(
            "Remote compute receipt does not match the saved request",
        ));
    }
    let revision_valid = match value.status {
        ComputeStatus::Accepted => value.revision == 0,
        ComputeStatus::Running => value.revision == 1,
        ComputeStatus::Cancelling { .. } => (1..=2).contains(&value.revision),
        ComputeStatus::Cancelled { .. } => (2..=3).contains(&value.revision),
        ComputeStatus::Completed { .. } | ComputeStatus::Failed { .. } => value.revision == 2,
        ComputeStatus::Interrupted => (1..=3).contains(&value.revision),
    };
    if !revision_valid
        || matches!(&value.status, ComputeStatus::Completed { text } if text.len() > MAX_COMPUTE_TEXT_BYTES)
    {
        return Err(Error::Invalid("Invalid remote compute result"));
    }
    Ok(())
}

pub(super) fn validate_advance(old: &RemoteJobReceipt, next: &RemoteJobReceipt) -> Result<()> {
    let before = &old.payload;
    let after = &next.payload;
    let valid = match (&before.status, &after.status) {
        (ComputeStatus::Accepted, ComputeStatus::Accepted) => false,
        (ComputeStatus::Accepted, _) => true,
        (
            ComputeStatus::Running,
            ComputeStatus::Cancelling { .. }
            | ComputeStatus::Cancelled { .. }
            | ComputeStatus::Completed { .. }
            | ComputeStatus::Failed { .. }
            | ComputeStatus::Interrupted,
        ) => true,
        (ComputeStatus::Cancelling { reason: old }, ComputeStatus::Cancelled { reason: new }) => {
            old == new
        }
        (ComputeStatus::Cancelling { .. }, ComputeStatus::Interrupted) => true,
        _ => false,
    };
    if !valid
        || after.revision <= before.revision
        || after.created_at_ms != before.created_at_ms
        || after.recorded_at_ms < before.recorded_at_ms
    {
        return Err(Error::Invalid(
            "A remote compute receipt cannot be rewritten or rolled back",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

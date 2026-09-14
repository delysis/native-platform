//! Durable requesting-device intent and immutable, checked host assertions.
//! Opening this store never submits or retries a network request.
use super::*;
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs::{File, OpenOptions};

const MAX_JOBS: i64 = 256;
const MAX_BYTES: i64 = 64 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 512 * 1024;
// A job has at most four receipts, with text only in a successful terminal.
// Reserve its terminal before dispatch, even when several peers are offline.
const RECEIPT_RESERVE: i64 = 1024 * 1024;

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
        self.input.validate()?;
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
        std::fs::create_dir_all(directory)?;
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
                    !metadata.file_type().is_file() || metadata.len() > 128 * 1024 * 1024
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
        let database = Connection::open(&database_path)?;
        let version: i64 = database.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if exists {
            if version != 1 {
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
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;
            PRAGMA max_page_count = 32768;",
        )?;
        if !exists {
            database.execute_batch("CREATE TABLE owner (key TEXT NOT NULL);
                CREATE TABLE requests (id TEXT PRIMARY KEY, cabal TEXT NOT NULL, body TEXT NOT NULL);
                CREATE TABLE cancellations (job TEXT PRIMARY KEY REFERENCES requests(id));
                CREATE TABLE receipts (job TEXT NOT NULL REFERENCES requests(id), revision INTEGER NOT NULL,
                    terminal INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY(job, revision));")?;
            database.execute("INSERT INTO owner(key) VALUES (?)", [peer.to_string()])?;
            database.pragma_update(None, "user_version", 1)?;
            File::open(directory)?.sync_all()?;
        }
        let client = Self {
            database,
            peer,
            _lease: lease,
        };
        let (jobs, bytes, _) = client.usage()?;
        if jobs > MAX_JOBS || bytes > MAX_BYTES {
            return Err(Error::Invalid("Compute request ledger exceeds its limits"));
        }
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
        let (jobs, bytes, pending) = self.usage()?;
        if jobs >= MAX_JOBS
            || bytes
                + (pending + 1) * RECEIPT_RESERVE
                + i64::try_from(body.len()).expect("bounded record")
                > MAX_BYTES
        {
            return Err(Error::Invalid(
                "Compute request ledger is full; existing jobs were preserved",
            ));
        }
        self.database.execute(
            "INSERT INTO requests(id, cabal, body) VALUES (?, ?, ?)",
            params![
                request.id.to_string(),
                request.grant.cabal.to_string(),
                body
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
        let mut receipt: Option<RemoteJobReceipt> = None;
        let mut query = self.database.prepare("SELECT revision, terminal, length(CAST(body AS BLOB)), CASE WHEN length(CAST(body AS BLOB)) <= ? THEN body END
            FROM receipts WHERE job = ? ORDER BY revision")?;
        let mut rows = query.query(params![
            i64::try_from(MAX_RECORD_BYTES).expect("bounded record limit"),
            job.to_string()
        ])?;
        while let Some(row) = rows.next()? {
            let bytes: i64 = row.get(2)?;
            if bytes > i64::try_from(MAX_RECORD_BYTES).expect("bounded record limit") {
                return Err(Error::Invalid("Compute receipt exceeds its limit"));
            }
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

    pub fn jobs(&self, cabal: Uuid) -> Result<Vec<ClientJob>> {
        self.ids(Some(cabal))?
            .into_iter()
            .map(|id| {
                self.get(id)?
                    .ok_or(Error::Invalid("Compute request disappeared"))
            })
            .collect()
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
        if self.usage()?.1 + i64::try_from(body.len()).expect("bounded record") > MAX_BYTES {
            return Err(Error::Invalid("Compute receipt storage is full"));
        }
        self.database.execute(
            "INSERT INTO receipts(job, revision, terminal, body) VALUES (?, ?, ?, ?)",
            params![
                receipt.payload.job.to_string(),
                receipt.payload.revision,
                receipt.payload.status.is_terminal(),
                body
            ],
        )?;
        self.get(receipt.payload.job)?
            .ok_or(Error::Invalid("Compute request disappeared"))
    }

    fn ids(&self, cabal: Option<Uuid>) -> Result<Vec<Uuid>> {
        let mut query = self.database.prepare(
            "SELECT id FROM requests WHERE (? IS NULL OR cabal = ?) ORDER BY id LIMIT 257",
        )?;
        let value = cabal.map(|id| id.to_string());
        let ids = query
            .query_map(params![value, value], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if i64::try_from(ids.len()).expect("bounded request count") > MAX_JOBS {
            return Err(Error::Invalid("Too many compute requests"));
        }
        ids.into_iter()
            .map(|id| {
                id.parse()
                    .map_err(|_| Error::Invalid("Invalid compute request identity"))
            })
            .collect()
    }

    fn usage(&self) -> Result<(i64, i64, i64)> {
        self.database.query_row("SELECT (SELECT count(*) FROM requests),
            (SELECT coalesce(sum(length(CAST(body AS BLOB))), 0) FROM requests) + (SELECT coalesce(sum(length(CAST(body AS BLOB))), 0) FROM receipts),
            (SELECT count(*) FROM requests WHERE NOT EXISTS(SELECT 1 FROM receipts WHERE receipts.job = requests.id AND terminal = 1))",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(Into::into)
    }
}

fn encode(value: &impl Serialize) -> Result<String> {
    let body = serde_json::to_string(value)?;
    if body.len() > MAX_RECORD_BYTES {
        return Err(Error::Invalid("Compute record exceeds its limit"));
    }
    Ok(body)
}

fn validate_receipt(request: &ClientRequest, receipt: &RemoteJobReceipt) -> Result<()> {
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

fn validate_advance(old: &RemoteJobReceipt, next: &RemoteJobReceipt) -> Result<()> {
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

//! Exact signed envelopes remain recoverable after local compression. Archiving
//! changes representation only; it never resets the causal graph or its authority.
use std::io::Write;

use automerge::Change;
use flate2::{Compression, Decompress, FlushDecompress, Status, write::ZlibEncoder};
use rusqlite::{Connection, Row, Transaction, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{ChangeEnvelope, validate_envelope};
use crate::{Error, MAX_CHANGE_BYTES, MAX_CHANGES, Result};

pub(super) const MAX_ENVELOPE_BYTES: usize = MAX_CHANGE_BYTES * 4 / 3 + 2048;
const MAX_RETAINED_BYTES: usize = 512 * 1024 * 1024;
const MAX_RAW_BYTES: usize = 384 * 1024 * 1024;
pub(super) const MAX_DATABASE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_RECENT_COUNT: usize = 1024;
const MAX_RECENT_BYTES: usize = 8 * 1024 * 1024;
const RECENT_TARGET_COUNT: usize = 512;
const RECENT_TARGET_BYTES: usize = 4 * 1024 * 1024;
const COLUMNS: &str =
    "hash, body, encoded_bytes, raw_bytes, archived, orphaned, document, causal, compressed";

#[derive(Clone, Copy, Default)]
pub(super) struct Usage {
    pub count: usize,
    pub encoded_bytes: usize,
    pub raw_bytes: usize,
    pub recent_count: usize,
    pub recent_bytes: usize,
}

impl Usage {
    pub(super) fn validate(self) -> Result<()> {
        if self.count > MAX_CHANGES
            || self.encoded_bytes > MAX_RETAINED_BYTES
            || self.raw_bytes > MAX_RAW_BYTES
        {
            return Err(Error::Invalid("Cabal retained history limit reached"));
        }
        Ok(())
    }

    pub(super) fn add(&mut self, encoded_bytes: usize, raw_bytes: usize) -> Result<()> {
        self.count += 1;
        self.encoded_bytes += encoded_bytes;
        self.raw_bytes += raw_bytes;
        self.recent_count += 1;
        self.recent_bytes += encoded_bytes;
        self.validate()
    }
}

pub(super) struct StoredChange {
    pub hash: String,
    pub envelope: ChangeEnvelope,
    pub change: Change,
    pub orphaned: bool,
}

pub(super) fn initialize(database: &Connection) -> Result<()> {
    let page_bytes: i64 = database.pragma_query_value(None, "page_size", |row| row.get(0))?;
    database.pragma_update(
        None,
        "max_page_count",
        MAX_DATABASE_BYTES as i64 / page_bytes,
    )?;
    database.execute_batch(
        "CREATE TABLE IF NOT EXISTS changes(
            hash TEXT PRIMARY KEY,
            body BLOB NOT NULL,
            encoded_bytes INTEGER NOT NULL CHECK(encoded_bytes > 0),
            raw_bytes INTEGER NOT NULL CHECK(raw_bytes > 0),
            archived INTEGER NOT NULL CHECK(archived IN (0, 1)),
            orphaned INTEGER NOT NULL CHECK(orphaned IN (0, 1)),
            compressed INTEGER NOT NULL DEFAULT 0 CHECK(compressed IN (0, 1)),
            document TEXT NOT NULL,
            causal TEXT NOT NULL,
            UNIQUE(document, causal),
            CHECK(length(body) <= encoded_bytes),
            CHECK(compressed = 1 OR length(body) = encoded_bytes),
            CHECK(archived = 1 OR compressed = 0)
        ) STRICT;
        CREATE INDEX IF NOT EXISTS changes_recent ON changes(archived) WHERE archived = 0;
        CREATE INDEX IF NOT EXISTS changes_orphaned ON changes(orphaned) WHERE orphaned = 1;",
    )?;
    Ok(())
}

/// Inspect sizes before decoding a single payload. Quarantine consumes the same
/// retention budget: revocation cannot silently discard writing or reset quotas.
pub(super) fn usage(database: &Connection) -> Result<Usage> {
    let value = database.query_row(
        "SELECT count(*), coalesce(sum(encoded_bytes), 0), coalesce(sum(raw_bytes), 0),
            coalesce(sum(archived = 0), 0),
            coalesce(sum(CASE WHEN archived = 0 THEN encoded_bytes ELSE 0 END), 0)
         FROM changes",
        [],
        |row| {
            Ok(Usage {
                count: row.get::<_, u32>(0)? as usize,
                encoded_bytes: row.get::<_, u32>(1)? as usize,
                raw_bytes: row.get::<_, u32>(2)? as usize,
                recent_count: row.get::<_, u32>(3)? as usize,
                recent_bytes: row.get::<_, u32>(4)? as usize,
            })
        },
    )?;
    value.validate()?;
    if value.recent_count > MAX_RECENT_COUNT || value.recent_bytes > MAX_RECENT_BYTES {
        return Err(Error::Invalid("Cabal recent history exceeds limit"));
    }
    Ok(value)
}

fn decode(row: &Row<'_>, cabal: Uuid) -> Result<StoredChange> {
    let hash: String = row.get(0)?;
    let body = row.get_ref(1)?.as_blob().map_err(rusqlite::Error::from)?;
    let encoded_bytes = row.get::<_, u32>(2)? as usize;
    let raw_bytes = row.get::<_, u32>(3)? as usize;
    let archived: bool = row.get(4)?;
    let orphaned: bool = row.get(5)?;
    let compressed: bool = row.get(8)?;
    if encoded_bytes == 0
        || encoded_bytes > MAX_ENVELOPE_BYTES
        || raw_bytes == 0
        || raw_bytes > MAX_CHANGE_BYTES
        || body.len() > encoded_bytes
        || (compressed && !archived)
    {
        return Err(Error::Invalid("Invalid retained change size"));
    }
    let mut expanded = Vec::new();
    let bytes = if compressed {
        // This API never grows the output. A full buffer without StreamEnd,
        // a missing checksum, or trailing bytes is a corrupt archive.
        expanded.reserve_exact(encoded_bytes + 1);
        let mut decoder = Decompress::new(true);
        let status = decoder
            .decompress_vec(body, &mut expanded, FlushDecompress::Finish)
            .map_err(|_| Error::Invalid("Invalid compressed history"))?;
        if status != Status::StreamEnd || decoder.total_in() != body.len() as u64 {
            return Err(Error::Invalid(
                "Incomplete or trailing data in archived change",
            ));
        }
        expanded.as_slice()
    } else {
        body
    };
    if bytes.len() != encoded_bytes || hex::encode(Sha256::digest(bytes)) != hash {
        return Err(Error::Invalid(
            "Cabal change index does not match its signed history",
        ));
    }
    let envelope: ChangeEnvelope = serde_json::from_slice(bytes)?;
    if envelope.hash()? != hash {
        return Err(Error::Invalid("Stored change is not canonical"));
    }
    let change = validate_envelope(&envelope, cabal)?;
    if change.raw_bytes().len() != raw_bytes
        || envelope.payload.document.to_string() != row.get::<_, String>(6)?
        || change.hash().to_string() != row.get::<_, String>(7)?
    {
        return Err(Error::Invalid("Invalid retained change index"));
    }
    Ok(StoredChange {
        hash,
        envelope,
        change,
        orphaned,
    })
}

pub(super) fn read(database: &Connection, cabal: Uuid, hash: &str) -> Result<StoredChange> {
    let mut statement =
        database.prepare(&format!("SELECT {COLUMNS} FROM changes WHERE hash = ?"))?;
    let mut rows = statement.query([hash])?;
    decode(
        rows.next()?
            .ok_or(Error::Invalid("Missing retained change"))?,
        cabal,
    )
}

/// The callback owns only this change; neither the serialized history nor its
/// expanded archive is collected in a lifetime-sized Vec.
pub(super) fn visit(
    database: &Connection,
    cabal: Uuid,
    mut visitor: impl FnMut(StoredChange) -> Result<()>,
) -> Result<()> {
    let mut statement =
        database.prepare(&format!("SELECT {COLUMNS} FROM changes ORDER BY rowid"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        visitor(decode(row, cabal)?)?;
    }
    Ok(())
}

pub(super) fn compact(transaction: &Transaction<'_>, usage: &mut Usage, cabal: Uuid) -> Result<()> {
    if usage.recent_count <= MAX_RECENT_COUNT && usage.recent_bytes <= MAX_RECENT_BYTES {
        return Ok(());
    }
    while usage.recent_count > RECENT_TARGET_COUNT || usage.recent_bytes > RECENT_TARGET_BYTES {
        let hash: String = transaction.query_row(
            "SELECT hash FROM changes WHERE archived = 0 ORDER BY rowid LIMIT 1",
            [],
            |row| row.get(0),
        )?;
        let stored = read(transaction, cabal, &hash)?;
        let bytes = serde_json::to_vec(&stored.envelope)?;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&bytes)?;
        let archive = encoder.finish()?;
        // Incompressible small changes remain byte-for-byte JSON in cold
        // storage; codec overhead must not consume extra retention budget.
        let compressed = archive.len() < bytes.len();
        let body = if compressed { archive } else { bytes.clone() };
        if transaction.execute(
            "UPDATE changes SET body = ?, compressed = ?, archived = 1 WHERE hash = ? AND archived = 0",
            params![body, compressed, hash],
        )? != 1
        {
            return Err(Error::Invalid(
                "Retained change disappeared during archival",
            ));
        }
        usage.recent_count -= 1;
        usage.recent_bytes -= bytes.len();
    }
    Ok(())
}

#[cfg(test)]
mod tests;

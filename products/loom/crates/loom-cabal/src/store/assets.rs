//! Only bytes explicitly published into this cabal are available to peers.
//! A Markdown link never imports bytes from a private workspace or filesystem.
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Cabal;
use crate::{Error, Result};

pub const ASSET_CHUNK_BYTES: usize = 1024 * 1024;
pub const MAX_ASSET_BYTES: usize = 128 * 1024 * 1024;
const MAX_ASSETS: usize = 256;
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetDescriptor {
    pub sha256: String,
    /// A display label only. Never interpreted as a destination path.
    pub name: String,
    pub byte_count: u64,
}

impl AssetDescriptor {
    fn validate(&self) -> Result<()> {
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.name.trim().is_empty()
            || self.name.len() > 512
            || self.name.chars().any(char::is_control)
            || self.byte_count == 0
            || self.byte_count > MAX_ASSET_BYTES as u64
        {
            return Err(Error::Invalid("Invalid shared attachment"));
        }
        Ok(())
    }
}

pub(super) fn initialize(database: &rusqlite::Connection) -> Result<()> {
    database.execute_batch("CREATE TABLE IF NOT EXISTS assets(sha256 TEXT PRIMARY KEY, name TEXT NOT NULL, byte_count INTEGER NOT NULL, complete INTEGER NOT NULL CHECK(complete IN (0, 1))); CREATE TABLE IF NOT EXISTS asset_chunks(sha256 TEXT NOT NULL REFERENCES assets(sha256) ON DELETE CASCADE, position INTEGER NOT NULL, bytes BLOB NOT NULL, PRIMARY KEY(sha256, position));")?;
    Ok(())
}

impl Cabal {
    /// Native publication is the only way a private local file enters this
    /// namespace. No network request can ask the provider to publish a file.
    pub fn publish_asset(&mut self, name: &str, bytes: &[u8]) -> Result<AssetDescriptor> {
        self.require_member()?;
        if bytes.is_empty() || bytes.len() > MAX_ASSET_BYTES {
            return Err(Error::Invalid("Shared attachment exceeds its size limit"));
        }
        let descriptor = AssetDescriptor {
            sha256: hex::encode(Sha256::digest(bytes)),
            name: name.into(),
            byte_count: bytes.len() as u64,
        };
        descriptor.validate()?;
        let tx = self.database.transaction()?;
        let (known, complete) = reserve(&tx, &descriptor)?;
        if !complete {
            tx.execute("DELETE FROM asset_chunks WHERE sha256 = ?", [&known.sha256])?;
            for (index, bytes) in bytes.chunks(ASSET_CHUNK_BYTES).enumerate() {
                tx.execute(
                    "INSERT INTO asset_chunks(sha256, position, bytes) VALUES (?, ?, ?)",
                    params![known.sha256, index as i64, bytes],
                )?;
            }
            tx.execute(
                "UPDATE assets SET complete = 1 WHERE sha256 = ?",
                [&known.sha256],
            )?;
        }
        tx.commit()?;
        Ok(known)
    }

    /// Complete objects only. A partially downloaded object cannot be relayed
    /// until its full SHA-256 has been verified.
    pub fn assets(&self) -> Result<Vec<AssetDescriptor>> {
        let mut statement = self.database.prepare("SELECT sha256, name, byte_count FROM assets WHERE complete = 1 ORDER BY sha256 LIMIT 257")?;
        let rows = statement.query_map([], |row| {
            Ok(AssetDescriptor {
                sha256: row.get(0)?,
                name: row.get(1)?,
                byte_count: unsigned(row, 2)?,
            })
        })?;
        let assets: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
        if assets.len() > MAX_ASSETS {
            return Err(Error::Invalid(
                "Shared attachment catalog exceeds its limit",
            ));
        }
        for item in &assets {
            item.validate()?;
        }
        Ok(assets)
    }

    pub fn asset_chunk_for(
        &self,
        peer: iroh::PublicKey,
        sha256: &str,
        offset: u64,
    ) -> Result<Vec<u8>> {
        self.require_member()?;
        if !self.is_member(peer) || !offset.is_multiple_of(ASSET_CHUNK_BYTES as u64) {
            return Err(Error::Invalid(
                "Attachment request is outside cabal authority",
            ));
        }
        let bytes = self.database.query_row(
            "SELECT c.bytes FROM asset_chunks c JOIN assets a ON a.sha256 = c.sha256 WHERE c.sha256 = ? AND c.position = ? AND a.complete = 1",
            params![sha256, (offset / ASSET_CHUNK_BYTES as u64) as i64], |row| row.get::<_, Vec<u8>>(0),
        ).optional()?.ok_or(Error::Invalid("Shared attachment chunk unavailable"))?;
        if bytes.is_empty() || bytes.len() > ASSET_CHUNK_BYTES {
            return Err(Error::Invalid("Invalid attachment chunk"));
        }
        Ok(bytes)
    }

    /// Reserve the entire advertised size before receiving any chunk. Pending
    /// transfers count against the same durable quota as completed objects.
    pub fn begin_asset(&mut self, descriptor: &AssetDescriptor) -> Result<Option<u64>> {
        self.require_member()?;
        let tx = self.database.transaction()?;
        let (_, complete) = reserve(&tx, descriptor)?;
        let offset = if complete {
            None
        } else {
            Some(received(&tx, &descriptor.sha256)?)
        };
        tx.commit()?;
        Ok(offset)
    }

    /// A duplicate acknowledgement is harmless. New bytes can only extend the
    /// durable contiguous prefix; they cannot overwrite an accepted prefix.
    pub fn accept_asset_chunk(
        &mut self,
        descriptor: &AssetDescriptor,
        offset: u64,
        bytes: &[u8],
    ) -> Result<bool> {
        self.require_member()?;
        descriptor.validate()?;
        if !offset.is_multiple_of(ASSET_CHUNK_BYTES as u64)
            || offset >= descriptor.byte_count
            || bytes.len() as u64 != (descriptor.byte_count - offset).min(ASSET_CHUNK_BYTES as u64)
        {
            return Err(Error::Invalid("Invalid attachment chunk range"));
        }
        let tx = self.database.transaction()?;
        let (_, complete) = reserve(&tx, descriptor)?;
        if complete {
            return Ok(false);
        }
        let next = received(&tx, &descriptor.sha256)?;
        if offset < next {
            let known: Vec<u8> = tx.query_row(
                "SELECT bytes FROM asset_chunks WHERE sha256 = ? AND position = ?",
                params![
                    descriptor.sha256,
                    (offset / ASSET_CHUNK_BYTES as u64) as i64
                ],
                |row| row.get(0),
            )?;
            if known != bytes {
                return Err(Error::Invalid("Attachment retry changed its bytes"));
            }
            return Ok(false);
        }
        if offset != next {
            return Err(Error::Invalid(
                "Attachment chunks must extend the saved prefix",
            ));
        }
        tx.execute(
            "INSERT INTO asset_chunks(sha256, position, bytes) VALUES (?, ?, ?)",
            params![
                descriptor.sha256,
                (offset / ASSET_CHUNK_BYTES as u64) as i64,
                bytes
            ],
        )?;
        if next + bytes.len() as u64 == descriptor.byte_count {
            let mut digest = Sha256::new();
            let mut statement =
                tx.prepare("SELECT bytes FROM asset_chunks WHERE sha256 = ? ORDER BY position")?;
            let chunks =
                statement.query_map([&descriptor.sha256], |row| row.get::<_, Vec<u8>>(0))?;
            for chunk in chunks {
                digest.update(chunk?);
            }
            drop(statement);
            if hex::encode(digest.finalize()) != descriptor.sha256 {
                // A bad prefix cannot be resumed. Remove only this unverified
                // object, so the next peer can supply a fresh verified copy.
                tx.execute(
                    "DELETE FROM asset_chunks WHERE sha256 = ?",
                    [&descriptor.sha256],
                )?;
                tx.execute("DELETE FROM assets WHERE sha256 = ?", [&descriptor.sha256])?;
                tx.commit()?;
                return Err(Error::Invalid("Shared attachment digest mismatch"));
            }
            tx.execute(
                "UPDATE assets SET complete = 1 WHERE sha256 = ?",
                [&descriptor.sha256],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Read a verified retained object for local inspection. This grants no
    /// filesystem path and deliberately rechecks the stored bytes' digest.
    pub fn asset_bytes(&self, sha256: &str) -> Result<Vec<u8>> {
        let size: u64 = self
            .database
            .query_row(
                "SELECT byte_count FROM assets WHERE sha256 = ? AND complete = 1",
                [sha256],
                |row| unsigned(row, 0),
            )
            .optional()?
            .ok_or(Error::Invalid("Shared attachment is still downloading"))?;
        if size == 0 || size > MAX_ASSET_BYTES as u64 {
            return Err(Error::Invalid("Invalid shared attachment size"));
        }
        let mut result = Vec::with_capacity(size as usize);
        let mut statement = self
            .database
            .prepare("SELECT bytes FROM asset_chunks WHERE sha256 = ? ORDER BY position")?;
        let chunks = statement.query_map([sha256], |row| row.get::<_, Vec<u8>>(0))?;
        for chunk in chunks {
            let chunk = chunk?;
            if result.len() as u64 + chunk.len() as u64 > size {
                return Err(Error::Invalid(
                    "Shared attachment exceeds its declared size",
                ));
            }
            result.extend_from_slice(&chunk);
        }
        if result.len() as u64 != size || hex::encode(Sha256::digest(&result)) != sha256 {
            return Err(Error::Invalid("Shared attachment digest mismatch"));
        }
        Ok(result)
    }
}

fn received(tx: &Transaction<'_>, sha256: &str) -> Result<u64> {
    Ok(tx.query_row(
        "SELECT coalesce(sum(length(bytes)), 0) FROM asset_chunks WHERE sha256 = ?",
        [sha256],
        |row| unsigned(row, 0),
    )?)
}

fn reserve(tx: &Transaction<'_>, descriptor: &AssetDescriptor) -> Result<(AssetDescriptor, bool)> {
    descriptor.validate()?;
    let old: Option<(String, u64, bool)> = tx
        .query_row(
            "SELECT name, byte_count, complete FROM assets WHERE sha256 = ?",
            [&descriptor.sha256],
            |row| Ok((row.get(0)?, unsigned(row, 1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((name, size, complete)) = old {
        if size != descriptor.byte_count {
            return Err(Error::Invalid("Attachment identity changed its size"));
        }
        return Ok((
            AssetDescriptor {
                name,
                ..descriptor.clone()
            },
            complete,
        ));
    }
    let (count, total): (u64, u64) = tx.query_row(
        "SELECT count(*), coalesce(sum(byte_count), 0) FROM assets",
        [],
        |row| Ok((unsigned(row, 0)?, unsigned(row, 1)?)),
    )?;
    if count >= MAX_ASSETS as u64
        || total.saturating_add(descriptor.byte_count) > MAX_TOTAL_BYTES as u64
    {
        return Err(Error::Invalid("Shared attachment storage is full"));
    }
    tx.execute(
        "INSERT INTO assets(sha256, name, byte_count, complete) VALUES (?, ?, ?, 0)",
        params![
            descriptor.sha256,
            descriptor.name,
            descriptor.byte_count as i64
        ],
    )?;
    Ok((descriptor.clone(), false))
}

fn unsigned(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(column)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}

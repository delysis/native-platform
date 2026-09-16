//! SQLite keeps settled payloads on disk. Only unfinished jobs occupy the
//! active budget; immutable input and receipt rows never move or disappear.
use super::*;
use rusqlite::Connection;

pub(super) const MAX_PENDING: i64 = 256;
pub(super) const MAX_ACTIVE_BYTES: i64 = 64 * 1024 * 1024;
pub(super) const MAX_RETAINED_BYTES: i64 = 4 * 1024 * 1024 * 1024;
pub(super) const MAX_DATABASE_BYTES: u64 = MAX_RETAINED_BYTES as u64 + 128 * 1024 * 1024;
pub(super) const RECEIPT_RESERVE: i64 = 1024 * 1024;

pub(super) struct Usage {
    pub unfinished: i64,
    pub active_bytes: i64,
    pub retained_bytes: i64,
}

impl Usage {
    pub fn validate(&self) -> Result<()> {
        if self.unfinished > MAX_PENDING
            || self.active_bytes > MAX_ACTIVE_BYTES
            || self.retained_bytes > MAX_RETAINED_BYTES
        {
            return Err(Error::Invalid("Compute ledger exceeds its storage limits"));
        }
        Ok(())
    }

    pub fn can_accept(&self, input_bytes: usize) -> bool {
        let Ok(input_bytes) = i64::try_from(input_bytes) else {
            return false;
        };
        // Actual encoded bytes plus a full MiB for each pending result. This
        // covers all four signed receipts and worst-case escaped output text.
        let reserved = (self.unfinished + 1) * RECEIPT_RESERVE;
        self.unfinished < MAX_PENDING
            && self.active_bytes + input_bytes + reserved <= MAX_ACTIVE_BYTES
            && self.retained_bytes + input_bytes + reserved <= MAX_RETAINED_BYTES
    }
}

pub(super) fn configure(database: &Connection) -> Result<()> {
    let page_bytes: i64 = database.pragma_query_value(None, "page_size", |row| row.get(0))?;
    database.pragma_update(
        None,
        "max_page_count",
        MAX_DATABASE_BYTES as i64 / page_bytes,
    )?;
    Ok(())
}

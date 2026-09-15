//! Short-lived, device-bound admission. Membership outlives its invitation.
use super::{Cabal, Error, Member, Result, Roster, validate_name, validate_roster};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use iroh::{EndpointAddr, PublicKey};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use uuid::Uuid;

const INVITATION_LIFETIME_SECONDS: i64 = 24 * 60 * 60;
const MAX_INVITATIONS: i64 = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub schema: u32,
    pub cabal: Uuid,
    pub owner: EndpointAddr,
    pub token: String,
}

impl Invitation {
    pub fn encode(&self) -> Result<String> {
        Ok(format!(
            "loom://cabal/{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }

    pub fn decode(value: &str) -> Result<Self> {
        if value.len() > 8192 {
            return Err(Error::Invalid("Cabal invitation is too long"));
        }
        let encoded = value
            .trim()
            .strip_prefix("loom://cabal/")
            .ok_or(Error::Invalid("Invalid cabal invitation"))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| Error::Invalid("Invalid cabal invitation"))?;
        let invitation: Self = serde_json::from_slice(&bytes)?;
        if invitation.schema != 1 || !valid_token(&invitation.token) {
            return Err(Error::Invalid("Unsupported cabal invitation"));
        }
        Ok(invitation)
    }
}

fn valid_token(token: &str) -> bool {
    token.len() == 43
        && URL_SAFE_NO_PAD
            .decode(token)
            .is_ok_and(|bytes| bytes.len() == 32)
}

fn unix_seconds() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .ok_or(Error::Invalid("The invitation clock is unavailable"))
}

impl Cabal {
    pub fn invite(&mut self, address: EndpointAddr) -> Result<Invitation> {
        self.invite_at(address, unix_seconds()?)
    }

    fn invite_at(&mut self, address: EndpointAddr, now: i64) -> Result<Invitation> {
        self.require_owner()?;
        if address.id != self.identity.public_key() {
            return Err(Error::Invalid("Invitation endpoint does not match owner"));
        }
        let now = self.expire_invitations(now)?;
        let count: i64 =
            self.database
                .query_row("SELECT count(*) FROM invitations", [], |row| row.get(0))?;
        if count >= MAX_INVITATIONS {
            return Err(Error::Invalid(
                "Invitation limit reached. Existing links expire after 24 hours.",
            ));
        }
        let expires_at = now
            .checked_add(INVITATION_LIFETIME_SECONDS)
            .ok_or(Error::Invalid("The invitation clock exceeds its limit"))?;
        let mut token = [0_u8; 32];
        getrandom::fill(&mut token).map_err(|_| Error::Invalid("OS randomness unavailable"))?;
        let token = URL_SAFE_NO_PAD.encode(token);
        let hash = hex::encode(Sha256::digest(token.as_bytes()));
        self.database.execute(
            "INSERT INTO invitations(hash, member, expires_at) VALUES (?, NULL, ?)",
            params![hash, expires_at],
        )?;
        Ok(Invitation {
            schema: 1,
            cabal: self.id(),
            owner: address,
            token,
        })
    }

    /// Bind the invitation to its first authenticated device. Exact retry is
    /// allowed until expiry, including after a lost reply or owner restart.
    pub fn admit(&mut self, token: &str, key: PublicKey, name: &str) -> Result<Roster> {
        self.admit_at(token, key, name, unix_seconds()?)
    }

    fn admit_at(&mut self, token: &str, key: PublicKey, name: &str, now: i64) -> Result<Roster> {
        self.require_owner()?;
        validate_name(name)?;
        if !valid_token(token) {
            return Err(Error::Invalid("Invalid cabal invitation"));
        }
        let hash = hex::encode(Sha256::digest(token.as_bytes()));
        let mut statement = self
            .database
            .prepare("SELECT hash, member, expires_at FROM invitations")?;
        let candidates = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        let Some((_, member, expires_at)) = candidates
            .into_iter()
            .find(|(candidate, _, _)| bool::from(candidate.as_bytes().ct_eq(hash.as_bytes())))
        else {
            return Err(Error::Invalid(
                "Invitation is unavailable or expired. Ask the owner for a new one.",
            ));
        };
        // An unknown caller cannot turn arbitrary guesses into database writes.
        // Only an actual bearer capability advances the saved clock/cleanup.
        if expires_at <= self.expire_invitations(now)? {
            return Err(Error::Invalid(
                "Invitation is unavailable or expired. Ask the owner for a new one.",
            ));
        }
        if let Some(member) = member {
            if member == key.to_string() && self.is_member(key) {
                return Ok(self.roster.clone());
            }
            return Err(Error::Invalid("Invitation was already used"));
        }
        let mut membership = self.roster.payload.clone();
        if !self.is_member(key) {
            membership.members.push(Member {
                key,
                name: name.into(),
            });
        }
        membership.revision += 1;
        let roster = self.identity.sign(membership)?;
        validate_roster(&roster)?;
        let transaction = self.database.transaction()?;
        transaction.execute(
            "UPDATE invitations SET member = ? WHERE hash = ?",
            params![key.to_string(), hash],
        )?;
        transaction.execute(
            "UPDATE metadata SET value = ? WHERE key = 'roster'",
            [serde_json::to_string(&roster)?],
        )?;
        transaction.commit()?;
        self.roster = roster;
        Ok(self.roster.clone())
    }

    /// Reclaim only expired capabilities, never membership or writing. Persist
    /// the latest observed wall time so rollback cannot revive an expired link
    /// or mint a new link whose expiry is behind the last observed time.
    fn expire_invitations(&mut self, observed: i64) -> Result<i64> {
        if !(0..=i64::MAX - INVITATION_LIFETIME_SECONDS).contains(&observed) {
            return Err(Error::Invalid("The invitation clock exceeds its limit"));
        }
        let transaction = self.database.transaction()?;
        let last: Option<String> = transaction
            .query_row(
                "SELECT value FROM metadata WHERE key = 'invitation_clock'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let last = last
            .map(|value| value.parse::<i64>())
            .transpose()
            .map_err(|_| Error::Invalid("The saved invitation clock is invalid"))?
            .unwrap_or(0);
        if !(0..=i64::MAX - INVITATION_LIFETIME_SECONDS).contains(&last) {
            return Err(Error::Invalid("The saved invitation clock is invalid"));
        }
        let now = observed.max(last);
        transaction.execute("DELETE FROM invitations WHERE expires_at <= ?", [now])?;
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES ('invitation_clock', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [now.to_string()],
        )?;
        transaction.commit()?;
        Ok(now)
    }
}

#[cfg(test)]
mod tests;

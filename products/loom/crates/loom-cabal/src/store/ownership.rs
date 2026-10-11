//! Ownership follows an explicit chain of signed delegations. A former owner
//! cannot regain authority by replaying a roster with a larger revision number.
use iroh::PublicKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{Cabal, Membership, Roster};
use crate::{Error, Result, Signed};

const MAX_TRANSFERS: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipGrant {
    pub schema: u32,
    pub cabal: Uuid,
    /// Hash of the immediately preceding delegation, or None at the root.
    pub previous_authority: Option<String>,
    /// The exact roster the transferring owner reviewed.
    pub previous_roster: String,
    pub owner: PublicKey,
    pub revision: u64,
    pub epoch: u64,
    /// Initial membership state, excluding the delegation chain itself.
    pub state_hash: String,
}

pub(super) fn origin(roster: &Roster) -> PublicKey {
    roster
        .payload
        .authority
        .first()
        .map_or(roster.payload.owner, |grant| grant.signer)
}

fn state_hash(membership: &Membership) -> Result<String> {
    let mut state = membership.clone();
    state.authority.clear();
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&state)?)))
}

fn canonical_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn validate(roster: &Roster) -> Result<()> {
    if roster.payload.authority.len() > MAX_TRANSFERS {
        return Err(Error::Invalid("Cabal ownership chain exceeds limit"));
    }
    let mut owner = origin(roster);
    let mut previous = None;
    let mut revision = 0;
    let mut epoch = 0;
    for grant in &roster.payload.authority {
        grant.verify()?;
        let value = &grant.payload;
        if grant.signer != owner
            || value.schema != 1
            || value.cabal != roster.payload.cabal
            || value.previous_authority != previous
            || value.owner == owner
            || value.revision <= revision
            || value.epoch < epoch
            || !canonical_hash(&value.previous_roster)
            || !canonical_hash(&value.state_hash)
        {
            return Err(Error::Invalid("Invalid cabal ownership chain"));
        }
        owner = value.owner;
        revision = value.revision;
        epoch = value.epoch;
        previous = Some(grant.hash()?);
    }
    if roster.payload.owner != owner
        || roster.payload.revision < revision
        || roster.payload.epoch < epoch
    {
        return Err(Error::Invalid(
            "Membership predates its ownership authority",
        ));
    }
    if roster.signer == owner {
        return Ok(());
    }
    // The old owner may publish exactly the handoff it signed. Every later
    // membership decision requires the new owner's key.
    if let Some(grant) = roster.payload.authority.last()
        && roster.signer == grant.signer
        && roster.payload.revision == grant.payload.revision
        && roster.payload.epoch == grant.payload.epoch
        && state_hash(&roster.payload)? == grant.payload.state_hash
    {
        return Ok(());
    }
    Err(Error::Invalid(
        "Membership was not signed by its current owner",
    ))
}

/// A known chain is a monotonic trust anchor. Reject forks, ignore old prefixes,
/// and bind a directly observed handoff to its exact predecessor roster.
pub(super) fn advances(current: &Roster, next: &Roster) -> Result<bool> {
    if origin(current) != origin(next) || current.payload.cabal != next.payload.cabal {
        return Err(Error::Invalid("Membership belongs to another cabal"));
    }
    for (known, offered) in current
        .payload
        .authority
        .iter()
        .zip(&next.payload.authority)
    {
        if known.hash()? != offered.hash()? {
            return Err(Error::Invalid("Conflicting cabal ownership decisions"));
        }
    }
    if next.payload.authority.len() < current.payload.authority.len() {
        return Ok(false);
    }
    if let Some(handoff) = next.payload.authority.get(current.payload.authority.len()) {
        let value = &handoff.payload;
        if value.revision <= current.payload.revision
            || value.epoch < current.payload.epoch
            || (current.payload.revision.checked_add(1) == Some(value.revision)
                && value.previous_roster != current.hash()?)
        {
            return Err(Error::Invalid(
                "Ownership handoff conflicts with known membership",
            ));
        }
    }
    Ok(true)
}

impl Cabal {
    /// Hand admission/removal authority to an existing member. Writing membership
    /// and its epoch are unchanged, so offline edits do not need a new invitation.
    pub fn transfer_owner(&mut self, recipient: PublicKey, expected_roster: &str) -> Result<()> {
        self.require_owner()?;
        if self.roster.hash()? != expected_roster {
            return Err(Error::Invalid(
                "Membership changed; review the ownership handoff again",
            ));
        }
        if recipient == self.roster.payload.owner || !self.is_member(recipient) {
            return Err(Error::Invalid("Choose another current cabal member"));
        }
        if self.roster.payload.authority.len() >= MAX_TRANSFERS {
            return Err(Error::Invalid("Cabal ownership chain exceeds limit"));
        }
        let mut membership = self.roster.payload.clone();
        membership.owner = recipient;
        membership.revision = membership
            .revision
            .checked_add(1)
            .ok_or(Error::Invalid("Cabal revision exceeds limit"))?;
        let grant = self.identity.sign(OwnershipGrant {
            schema: 1,
            cabal: self.id(),
            previous_authority: membership.authority.last().map(Signed::hash).transpose()?,
            previous_roster: expected_roster.to_owned(),
            owner: recipient,
            revision: membership.revision,
            epoch: membership.epoch,
            state_hash: state_hash(&membership)?,
        })?;
        membership.authority.push(grant);
        self.accept_roster(self.identity.sign(membership)?)?;
        Ok(())
    }
}

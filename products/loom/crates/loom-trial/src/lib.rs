#![forbid(unsafe_code)]

//! Frozen execution for one treatment/case pair.
//!
//! This crate is deliberately narrower than a campaign scheduler. It has no
//! candidate selection, adaptive search, prose payload, or manuscript write
//! surface. A caller freezes trusted research artifacts into a
//! [`FrozenTrialSpec`] and advances the fixed twelve-stage graph through a
//! live [`TrialJournal`]. Durable events are claims: strict replay returns only
//! a [`CheckedTrialReplay`] diagnostic and can never recreate a live command or
//! completion lease.
//!
//! Starting a live journal consumes and retains the transactional store's
//! exclusive lease over the exact persisted trial. Inference and evaluation
//! adapters must likewise mint the private terminal and completion leases;
//! fingerprints, booleans, and deserialized event records are never authority.

mod authority;
mod budget;
mod journal;
mod runtime;
mod spec;

pub use authority::*;
pub use budget::*;
pub use journal::*;
pub use runtime::*;
pub use spec::*;

#[cfg(test)]
mod tests;

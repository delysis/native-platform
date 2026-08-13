#![forbid(unsafe_code)]

//! Bounded exploratory scheduling over immutable frozen trials.
//!
//! This crate does not load models, write stores, inspect prose, or mutate a
//! manuscript. Campaign events are durable claims. [`CampaignJournal::replay`]
//! validates those claims into a diagnostic snapshot, but cannot create a
//! trial command or any other execution authority.
//!
//! Starting a live journal consumes and retains the transactional store's
//! exclusive lease over the exact persisted campaign. Live `loom-trial` and
//! `loom-eval` adapters must mint the remaining private evidence leases;
//! weakening authority to a serialized hash, boolean, or caller assertion is
//! not a compatible substitute.

mod archive;
mod authority;
mod budget;
mod decision;
mod factorial;
mod halving;
mod journal;
mod ncurve;
mod pressure;
mod spec;

pub use archive::*;
pub use authority::*;
pub use budget::*;
pub use decision::*;
pub use factorial::*;
pub use halving::*;
pub use journal::*;
pub use ncurve::*;
pub use pressure::*;
pub use spec::*;

#[cfg(test)]
mod tests;

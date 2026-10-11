#![forbid(unsafe_code)]

//! Process-local run control shared by chat and writing operations.
//! Controls never cancel native work by themselves, publish results, or certify
//! worker joins. Product owners perform those effects at their existing edges.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// In-memory owner identity. Equality means the same allocation, never merely
/// equal public request strings or equal per-supervisor sequence numbers.
#[derive(Clone, Debug)]
pub struct InstanceId(Arc<()>);

impl Default for InstanceId {
    fn default() -> Self {
        Self(Arc::new(()))
    }
}

impl PartialEq for InstanceId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for InstanceId {}

impl PartialOrd for InstanceId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for InstanceId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Arc::as_ptr(&self.0).cmp(&Arc::as_ptr(&other.0))
    }
}

const RUNNING: u8 = 0;
const CANCEL_REQUESTED: u8 = 1;
const TERMINAL: u8 = 2;
const TERMINAL_CANCELLED: u8 = 3;

/// Cancellation and terminal claiming share one atomic linearization point.
/// Each branch owns a distinct control. Keep it alive with that run's lease;
/// never look up a replacement control using a reusable public request ID.
#[derive(Debug)]
pub struct RunControl(AtomicU8);

/// A single successful claim to an operation's terminal boundary.
/// Cancellation is an observation, not the result class: a panic may still be
/// classified as failed after cancellation was requested.
#[derive(Debug, Eq, PartialEq)]
pub struct TerminalClaim {
    pub cancellation_requested: bool,
}

impl RunControl {
    #[must_use]
    pub const fn new(cancelled: bool) -> Self {
        Self(AtomicU8::new(if cancelled {
            CANCEL_REQUESTED
        } else {
            RUNNING
        }))
    }

    /// Returns true only for the first cancellation request before terminality.
    pub fn request_cancel(&self) -> bool {
        self.0
            .compare_exchange(
                RUNNING,
                CANCEL_REQUESTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Retains cancellation history after terminal claiming for diagnostics.
    #[must_use]
    pub fn cancellation_requested(&self) -> bool {
        matches!(
            self.0.load(Ordering::Acquire),
            CANCEL_REQUESTED | TERMINAL_CANCELLED
        )
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.0.load(Ordering::Acquire),
            TERMINAL | TERMINAL_CANCELLED
        )
    }

    /// Only the executor that receives `Some` may claim a new terminal result.
    pub fn claim_terminal(&self) -> Option<TerminalClaim> {
        loop {
            let state = self.0.load(Ordering::Acquire);
            let terminal = match state {
                RUNNING => TERMINAL,
                CANCEL_REQUESTED => TERMINAL_CANCELLED,
                TERMINAL | TERMINAL_CANCELLED => return None,
                _ => unreachable!("run control contains an invalid state"),
            };
            if self
                .0
                .compare_exchange(state, terminal, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(TerminalClaim {
                    cancellation_requested: state == CANCEL_REQUESTED,
                });
            }
        }
    }
}

/// Bounded diagnostics for workers whose owners retain the actual join handles.
/// Outstanding identities are never evicted. `note_joined` must be called only
/// after consuming the corresponding handle; this ledger is not join evidence.
#[derive(Debug)]
pub struct WorkerLedger<K> {
    outstanding: BTreeSet<K>,
    joined: BTreeSet<K>,
    joined_order: VecDeque<K>,
    history_capacity: usize,
}

impl<K: Ord + Clone> WorkerLedger<K> {
    #[must_use]
    pub fn new(history_capacity: usize) -> Self {
        Self {
            outstanding: BTreeSet::new(),
            joined: BTreeSet::new(),
            joined_order: VecDeque::new(),
            history_capacity,
        }
    }

    /// A worker identity must be unique within its owner's lifetime.
    pub fn note_started(&mut self, identity: K) -> bool {
        !self.joined.contains(&identity) && self.outstanding.insert(identity)
    }

    pub fn note_joined(&mut self, identity: &K) -> bool {
        if !self.outstanding.remove(identity) {
            return false;
        }
        self.joined.insert(identity.clone());
        self.joined_order.push_back(identity.clone());
        while self.joined_order.len() > self.history_capacity {
            if let Some(oldest) = self.joined_order.pop_front() {
                self.joined.remove(&oldest);
            }
        }
        true
    }

    #[must_use]
    pub fn outstanding_count(&self) -> usize {
        self.outstanding.len()
    }

    #[must_use]
    pub fn was_joined(&self, identity: &K) -> bool {
        self.joined.contains(identity)
    }

    #[must_use]
    pub fn expected(&self) -> BTreeSet<K> {
        self.outstanding.union(&self.joined).cloned().collect()
    }

    #[must_use]
    pub fn joined(&self) -> &BTreeSet<K> {
        &self.joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn cancellation_and_terminal_have_exactly_one_winner() {
        for _ in 0..128 {
            let control = Arc::new(RunControl::new(false));
            let start = Arc::new(Barrier::new(2));
            let canceller = {
                let control = Arc::clone(&control);
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    control.request_cancel()
                })
            };
            start.wait();
            let claim = control.claim_terminal().expect("sole terminal claimant");
            assert_eq!(
                claim.cancellation_requested,
                canceller.join().expect("cancel worker")
            );
            assert!(control.is_terminal());
            assert!(!control.request_cancel());
            assert!(control.claim_terminal().is_none());
            assert_eq!(
                control.cancellation_requested(),
                claim.cancellation_requested
            );
        }
    }

    #[test]
    fn terminal_claim_is_not_replayed_and_branches_are_independent() {
        let left = RunControl::new(true);
        let right = RunControl::new(false);
        assert!(!left.request_cancel());
        assert!(
            left.claim_terminal()
                .expect("left terminal")
                .cancellation_requested
        );
        assert!(left.claim_terminal().is_none());
        assert!(
            !right
                .claim_terminal()
                .expect("right terminal")
                .cancellation_requested
        );
    }

    #[test]
    fn worker_history_never_evicts_unjoined_owners() {
        let mut ledger = WorkerLedger::new(4);
        assert!(ledger.note_started(0));
        for id in 1..1024 {
            assert!(ledger.note_started(id));
            assert!(!ledger.note_started(id));
            assert!(ledger.note_joined(&id));
            assert!(!ledger.note_joined(&id));
            assert_eq!(ledger.outstanding_count(), 1);
            assert!(ledger.expected().contains(&0));
            assert!(ledger.joined().len() <= 4);
        }
        assert!(ledger.note_joined(&0));
        assert_eq!(ledger.outstanding_count(), 0);
        assert_eq!(ledger.expected(), *ledger.joined());
    }

    #[test]
    fn zero_history_capacity_still_tracks_outstanding_workers() {
        let mut ledger = WorkerLedger::new(0);
        assert!(ledger.note_started("worker"));
        assert_eq!(ledger.outstanding_count(), 1);
        assert!(ledger.note_joined(&"worker"));
        assert!(ledger.expected().is_empty());
        assert!(ledger.joined().is_empty());
    }
}

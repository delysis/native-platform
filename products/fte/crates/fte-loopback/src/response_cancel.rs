//! Reconcile a cancellation request with authoritative terminal persistence.
//!
//! A cancellation count is an admission observation, never completion evidence.
//! Keep this helper independent of the HTTP edge so store failures and the
//! persist-during-cancel race can be tested without a live listener or provider.

use fte_types::{RequestId, TerminalStatus};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CancelResolution {
    Missing,
    IdentityConflict,
    InProgress,
    Cancelling,
    Terminal(TerminalStatus),
}

impl CancelResolution {
    pub(super) const fn status(self) -> Option<&'static str> {
        match self {
            Self::Missing | Self::IdentityConflict => None,
            Self::InProgress => Some("in_progress"),
            Self::Cancelling => Some("cancelling"),
            Self::Terminal(TerminalStatus::Completed) => Some("completed"),
            Self::Terminal(TerminalStatus::Incomplete) => Some("incomplete"),
            Self::Terminal(TerminalStatus::Cancelled) => Some("cancelled"),
            Self::Terminal(TerminalStatus::Failed) => Some("failed"),
        }
    }
}

/// The caller snapshots the active registration, releasing its mutex before
/// entering this function. Reads propagate errors; they are not absence.
/// Neither registry entries nor stored responses are removed or rewritten.
///
/// The second read lets a terminal persisted during cancellation win. A terminal
/// published after that read will be observed by the next GET: until then the
/// reply remains explicitly nonterminal. This does not assert a worker join.
pub(super) fn reconcile<E>(
    active: Option<RequestId>,
    mut read_terminal: impl FnMut() -> Result<Option<(RequestId, TerminalStatus)>, E>,
    cancel: impl FnOnce(&RequestId) -> usize,
) -> Result<CancelResolution, E> {
    if let Some((owner, status)) = read_terminal()? {
        return Ok(if active.as_ref().is_some_and(|active| active != &owner) {
            CancelResolution::IdentityConflict
        } else {
            CancelResolution::Terminal(status)
        });
    }
    let Some(request_id) = active else {
        return Ok(CancelResolution::Missing);
    };
    let count = cancel(&request_id);
    if let Some((owner, status)) = read_terminal()? {
        return Ok(if owner == request_id {
            CancelResolution::Terminal(status)
        } else {
            CancelResolution::IdentityConflict
        });
    }
    Ok(if count > 0 {
        CancelResolution::Cancelling
    } else {
        CancelResolution::InProgress
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;

    fn request() -> RequestId {
        RequestId("request-identity".to_owned())
    }

    #[test]
    fn every_stored_terminal_is_authoritative_without_cancellation() {
        for (terminal, expected) in [
            (TerminalStatus::Completed, "completed"),
            (TerminalStatus::Incomplete, "incomplete"),
            (TerminalStatus::Cancelled, "cancelled"),
            (TerminalStatus::Failed, "failed"),
        ] {
            for active in [None, Some(request())] {
                let result = reconcile::<()>(
                    active,
                    || Ok(Some((request(), terminal))),
                    |_| panic!("a terminal response must not cancel another operation"),
                )
                .expect("fixture store succeeds");
                assert_eq!(result, CancelResolution::Terminal(terminal));
                assert_eq!(result.status(), Some(expected));
            }
        }
    }

    #[test]
    fn unknown_id_is_absent_not_completed() {
        assert_eq!(
            reconcile::<()>(None, || Ok(None), |_| panic!("no request to cancel")),
            Ok(CancelResolution::Missing)
        );
    }

    #[test]
    fn zero_cancellations_never_proves_completion() {
        let calls = Cell::new(0);
        let result = reconcile::<()>(
            Some(request()),
            || Ok(None),
            |id| {
                assert_eq!(id, &request());
                calls.set(calls.get() + 1);
                0
            },
        )
        .expect("fixture store succeeds");
        assert_eq!(calls.get(), 1);
        assert_eq!(result.status(), Some("in_progress"));
    }

    #[test]
    fn positive_cancellations_are_nonterminal() {
        assert_eq!(
            reconcile::<()>(Some(request()), || Ok(None), |_| 1),
            Ok(CancelResolution::Cancelling)
        );
    }

    #[test]
    fn all_terminals_persisted_during_cancel_win_over_both_counts() {
        for terminal in [
            TerminalStatus::Completed,
            TerminalStatus::Incomplete,
            TerminalStatus::Cancelled,
            TerminalStatus::Failed,
        ] {
            for count in [0, 1] {
                let mut reads = VecDeque::from([None, Some((request(), terminal))]);
                assert_eq!(
                    reconcile::<()>(
                        Some(request()),
                        || Ok(reads.pop_front().expect("exactly two store reads")),
                        |_| count,
                    ),
                    Ok(CancelResolution::Terminal(terminal))
                );
                assert!(reads.is_empty());
            }
        }
    }

    #[test]
    fn first_store_error_is_not_absence_and_does_not_cancel() {
        assert_eq!(
            reconcile(
                Some(request()),
                || Err("store unavailable"),
                |_| panic!("no admission")
            ),
            Err("store unavailable")
        );
    }

    #[test]
    fn second_store_error_is_not_success() {
        let mut reads = VecDeque::from([Ok(None), Err("store unavailable")]);
        assert_eq!(
            reconcile(
                Some(request()),
                || reads.pop_front().expect("exactly two reads"),
                |_| 1,
            ),
            Err("store unavailable")
        );
    }

    #[test]
    fn conflicting_registration_cannot_cancel_a_replacement() {
        assert_eq!(
            reconcile::<()>(
                Some(request()),
                || Ok(Some((
                    RequestId("other".to_owned()),
                    TerminalStatus::Failed
                ))),
                |_| panic!("identity mismatch must fail closed"),
            ),
            Ok(CancelResolution::IdentityConflict)
        );
    }

    #[test]
    fn conflicting_late_terminal_cannot_supply_success_evidence() {
        let mut reads = VecDeque::from([
            None,
            Some((RequestId("other".to_owned()), TerminalStatus::Completed)),
        ]);
        assert_eq!(
            reconcile::<()>(
                Some(request()),
                || Ok(reads.pop_front().expect("two reads")),
                |_| 0,
            ),
            Ok(CancelResolution::IdentityConflict)
        );
    }
}

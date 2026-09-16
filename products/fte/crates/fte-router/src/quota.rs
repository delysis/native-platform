//! Per-route reservations for configured limits and authoritative usage.
//!
//! Request ceilings are reserved atomically before dispatch. Token ceilings
//! account for reported input/output usage and reserve the caller's output
//! ceiling across concurrent calls. Unknown input tokenization is not guessed;
//! a remote provider remains authoritative about its own token limits. Missing
//! final accounting closes a token-limited window rather than refunding an
//! unknowable charge. No counter survives beyond its minute/day window.

use fte_types::{
    ErrorClass, GatewayError, GatewayResponse, QuotaLimits, RequestId, UsageProvenance,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) struct Quota {
    state: Mutex<State>,
}

struct State {
    limits: QuotaLimits,
    minute: Window,
    day: Window,
}

struct Window {
    start: Instant,
    requests: u64,
    tokens: u64,
    reserved: u64,
    unknown: bool,
}

impl Window {
    fn new(start: Instant) -> Self {
        Self {
            start,
            requests: 0,
            tokens: 0,
            reserved: 0,
            unknown: false,
        }
    }

    fn roll(&mut self, now: Instant, duration: Duration) {
        if now.saturating_duration_since(self.start) >= duration {
            *self = Self::new(now);
        }
    }

    fn headroom(&self, requests: Option<u64>, tokens: Option<u64>) -> Option<f64> {
        let request_ratio = requests.map(|limit| ratio(limit, self.requests));
        let token_ratio = tokens.map(|limit| {
            if self.unknown {
                0.0
            } else {
                ratio(limit, self.tokens.saturating_add(self.reserved))
            }
        });
        match (request_ratio, token_ratio) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

fn ratio(limit: u64, used: u64) -> f64 {
    if limit == 0 {
        0.0
    } else {
        limit.saturating_sub(used) as f64 / limit as f64
    }
}

impl State {
    fn roll(&mut self, now: Instant) {
        self.minute.roll(now, Duration::from_secs(60));
        self.day.roll(now, Duration::from_secs(86_400));
    }
    fn headroom(&self) -> Option<f64> {
        let minute = self.minute.headroom(
            self.limits.requests_per_minute,
            self.limits.tokens_per_minute,
        );
        let day = self
            .day
            .headroom(self.limits.requests_per_day, self.limits.tokens_per_day);
        match (minute, day) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

impl Quota {
    pub(crate) fn new(limits: QuotaLimits) -> Arc<Self> {
        let now = Instant::now();
        Arc::new(Self {
            state: Mutex::new(State {
                limits,
                minute: Window::new(now),
                day: Window::new(now),
            }),
        })
    }

    pub(crate) fn headroom(&self) -> Option<f64> {
        let Ok(mut state) = self.state.lock() else {
            return Some(0.0);
        };
        state.roll(Instant::now());
        state.headroom()
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        id: &RequestId,
        max_output: Option<u32>,
    ) -> Result<Reservation, GatewayError> {
        self.reserve_at(id, max_output, Instant::now())
    }

    fn reserve_at(
        self: &Arc<Self>,
        id: &RequestId,
        max_output: Option<u32>,
        now: Instant,
    ) -> Result<Reservation, GatewayError> {
        let mut state = self.state.lock().map_err(|_| exhausted(id))?;
        state.roll(now);
        if state.headroom().is_some_and(|value| value <= 0.0) {
            return Err(exhausted(id));
        }
        let remaining = [
            state.limits.tokens_per_minute.map(|limit| {
                limit.saturating_sub(state.minute.tokens.saturating_add(state.minute.reserved))
            }),
            state.limits.tokens_per_day.map(|limit| {
                limit.saturating_sub(state.day.tokens.saturating_add(state.day.reserved))
            }),
        ]
        .into_iter()
        .flatten()
        .min();
        let reserved = remaining.map_or(0, |remaining| max_output.map_or(remaining, u64::from));
        if remaining.is_some_and(|remaining| reserved > remaining) {
            return Err(exhausted(id));
        }
        state.minute.requests = state.minute.requests.saturating_add(1);
        state.day.requests = state.day.requests.saturating_add(1);
        state.minute.reserved = state.minute.reserved.saturating_add(reserved);
        state.day.reserved = state.day.reserved.saturating_add(reserved);
        Ok(Reservation {
            quota: Arc::clone(self),
            starts: [state.minute.start, state.day.start],
            reserved,
            finished: AtomicBool::new(false),
        })
    }
}

pub(crate) struct Reservation {
    quota: Arc<Quota>,
    starts: [Instant; 2],
    reserved: u64,
    finished: AtomicBool,
}

impl Reservation {
    pub(crate) fn finish(&self, result: &Result<GatewayResponse, GatewayError>) {
        let tokens = result.as_ref().ok().and_then(|response| {
            (response.usage.provenance == UsageProvenance::Exact)
                .then(|| {
                    response
                        .usage
                        .input_tokens?
                        .checked_add(response.usage.output_tokens?)
                })
                .flatten()
        });
        self.settle(tokens);
    }

    fn settle(&self, tokens: Option<u64>) {
        if self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut state) = self.quota.state.lock() {
            let State { minute, day, .. } = &mut *state;
            for (window, start) in [minute, day].into_iter().zip(self.starts) {
                // Completion of an old request cannot refund a newer window.
                if window.start != start {
                    continue;
                }
                window.reserved = window.reserved.saturating_sub(self.reserved);
                if let Some(tokens) = tokens {
                    window.tokens = window.tokens.saturating_add(tokens);
                } else {
                    window.unknown = true;
                }
            }
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.settle(None);
    }
}

fn exhausted(id: &RequestId) -> GatewayError {
    GatewayError {
        code: "route_quota_exhausted".into(),
        class: ErrorClass::Unavailable,
        retryable: true,
        http_status: 429,
        request_id: id.clone(),
        provider: None,
        safe_detail: "the route has no available configured quota in its current window".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_reservations_are_atomic_and_unknown_limits_remain_unknown() {
        let quota = Quota::new(QuotaLimits {
            requests_per_minute: Some(1),
            ..Default::default()
        });
        let first = quota
            .reserve(&RequestId::new(), None)
            .expect("first request");
        assert!(quota.reserve(&RequestId::new(), None).is_err());
        drop(first);
        assert_eq!(quota.headroom(), Some(0.0));
        assert_eq!(Quota::new(QuotaLimits::default()).headroom(), None);
    }

    #[test]
    fn reported_tokens_reconcile_reservations_without_refunding_new_windows() {
        let quota = Quota::new(QuotaLimits {
            tokens_per_minute: Some(20),
            ..Default::default()
        });
        let first = quota.reserve(&RequestId::new(), Some(10)).expect("first");
        let second = quota.reserve(&RequestId::new(), Some(10)).expect("second");
        assert!(quota.reserve(&RequestId::new(), Some(1)).is_err());
        first.settle(Some(4));
        assert_eq!(quota.headroom(), Some(0.3));
        let next = quota
            .reserve_at(
                &RequestId::new(),
                Some(20),
                Instant::now() + Duration::from_secs(61),
            )
            .expect("new window");
        second.settle(Some(4));
        assert_eq!(quota.headroom(), Some(0.0));
        next.settle(Some(8));
        assert_eq!(quota.headroom(), Some(0.6));
    }

    #[test]
    fn unknown_usage_is_never_refunded_as_zero() {
        let quota = Quota::new(QuotaLimits {
            tokens_per_day: Some(20),
            ..Default::default()
        });
        let reservation = quota.reserve(&RequestId::new(), Some(1)).expect("first");
        drop(reservation);
        assert_eq!(quota.headroom(), Some(0.0));
    }
}

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

type ProviderId = String;
type ModelId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlidingWindow {
    limit: u32,
    window_duration_secs: i64,
    events: VecDeque<(DateTime<Utc>, u32)>,
}

impl SlidingWindow {
    pub fn new(limit: u32, window_duration_secs: i64) -> Self {
        Self {
            limit,
            window_duration_secs: window_duration_secs.max(1),
            events: VecDeque::new(),
        }
    }

    pub fn add_event(&mut self, now: DateTime<Utc>, amount: u32) {
        if amount > 0 {
            self.events.push_back((now, amount));
        }
        self.cleanup(now);
    }

    pub fn cleanup(&mut self, now: DateTime<Utc>) {
        let cutoff = now - Duration::seconds(self.window_duration_secs);
        while let Some(&(timestamp, _)) = self.events.front() {
            if timestamp <= cutoff {
                self.events.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn current_usage(&self) -> u32 {
        self.events
            .iter()
            .fold(0_u32, |total, &(_, amount)| total.saturating_add(amount))
    }

    pub fn headroom(&mut self, now: DateTime<Utc>) -> f64 {
        if self.limit == 0 {
            return 0.0;
        }

        self.cleanup(now);
        let usage = self.current_usage();
        if usage >= self.limit {
            0.0
        } else {
            1.0 - (f64::from(usage) / f64::from(self.limit))
        }
    }

    fn can_add(&mut self, now: DateTime<Utc>, amount: u32) -> bool {
        self.cleanup(now);
        self.limit > 0 && self.current_usage().saturating_add(amount) <= self.limit
    }
}

pub struct QuotaWindows {
    rpm: SlidingWindow,
    rpd: SlidingWindow,
    tpm: SlidingWindow,
    tpd: SlidingWindow,
}

impl QuotaWindows {
    pub fn new(rpm: u32, rpd: u32, tpm: u32, tpd: u32) -> Self {
        Self {
            rpm: SlidingWindow::new(rpm, 60),
            rpd: SlidingWindow::new(rpd, 86_400),
            tpm: SlidingWindow::new(tpm, 60),
            tpd: SlidingWindow::new(tpd, 86_400),
        }
    }

    pub fn headroom(&mut self, now: DateTime<Utc>) -> f64 {
        self.rpm
            .headroom(now)
            .min(self.rpd.headroom(now))
            .min(self.tpm.headroom(now))
            .min(self.tpd.headroom(now))
    }

    fn try_record_request(&mut self, now: DateTime<Utc>) -> bool {
        if !self.rpm.can_add(now, 1) || !self.rpd.can_add(now, 1) {
            return false;
        }
        self.rpm.add_event(now, 1);
        self.rpd.add_event(now, 1);
        true
    }

    fn record_tokens(&mut self, now: DateTime<Utc>, tokens: u32) {
        self.tpm.add_event(now, tokens);
        self.tpd.add_event(now, tokens);
    }

    fn restore(&mut self, now: DateTime<Utc>, request_count: u32, tokens: u32) {
        self.rpm.add_event(now, request_count);
        self.rpd.add_event(now, request_count);
        self.tpm.add_event(now, tokens);
        self.tpd.add_event(now, tokens);
    }
}

#[derive(Default)]
pub struct QuotaTracker {
    windows: Arc<Mutex<HashMap<(ProviderId, ModelId), QuotaWindows>>>,
}

impl QuotaTracker {
    pub fn new() -> Self {
        Self::default()
    }

    fn windows(&self) -> MutexGuard<'_, HashMap<(ProviderId, ModelId), QuotaWindows>> {
        self.windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn register_model(&self, provider: ProviderId, model: ModelId, windows: QuotaWindows) {
        self.windows().insert((provider, model), windows);
    }

    pub fn headroom(&self, provider: &str, model: &str) -> f64 {
        self.windows()
            .get_mut(&(provider.to_string(), model.to_string()))
            .map(|windows| windows.headroom(Utc::now()))
            .unwrap_or(0.0)
    }

    pub fn try_record_request(&self, provider: &str, model: &str, now: DateTime<Utc>) -> bool {
        self.windows()
            .get_mut(&(provider.to_string(), model.to_string()))
            .map(|windows| windows.try_record_request(now))
            .unwrap_or(false)
    }

    pub fn record_tokens(&self, provider: &str, model: &str, tokens: u32, now: DateTime<Utc>) {
        if let Some(windows) = self
            .windows()
            .get_mut(&(provider.to_string(), model.to_string()))
        {
            windows.record_tokens(now, tokens);
        }
    }

    pub fn restore_event(
        &self,
        provider: &str,
        model: &str,
        occurred_at: DateTime<Utc>,
        request_count: u32,
        tokens: u32,
    ) {
        if let Some(windows) = self
            .windows()
            .get_mut(&(provider.to_string(), model.to_string()))
        {
            windows.restore(occurred_at, request_count, tokens);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_limit_has_no_headroom() {
        let now = Utc::now();
        assert_eq!(SlidingWindow::new(0, 60).headroom(now), 0.0);
    }

    #[test]
    fn events_at_window_boundary_expire() {
        let now = Utc::now();
        let mut window = SlidingWindow::new(2, 60);
        window.add_event(now - Duration::seconds(60), 1);
        assert_eq!(window.headroom(now), 1.0);
    }

    #[test]
    fn request_reservation_is_atomic() {
        let tracker = QuotaTracker::new();
        tracker.register_model(
            "provider".to_string(),
            "model".to_string(),
            QuotaWindows::new(1, 10, 100, 1_000),
        );
        let now = Utc::now();

        assert!(tracker.try_record_request("provider", "model", now));
        assert!(!tracker.try_record_request("provider", "model", now));
    }
}

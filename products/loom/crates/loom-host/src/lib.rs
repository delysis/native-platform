#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};

use thiserror::Error;

pub const MAX_QUEUE_CAPACITY: usize = 65_536;

#[derive(Clone, Debug, Default)]
pub struct AgencyGate {
    focus_mode: Arc<AtomicBool>,
    automation_enabled: Arc<AtomicBool>,
}

impl AgencyGate {
    pub fn set_focus_mode(&self, enabled: bool) {
        self.focus_mode.store(enabled, Ordering::Release);
    }

    pub fn set_automation_enabled(&self, enabled: bool) {
        self.automation_enabled.store(enabled, Ordering::Release);
    }

    #[must_use]
    pub fn focus_mode(&self) -> bool {
        self.focus_mode.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn automation_enabled(&self) -> bool {
        self.automation_enabled.load(Ordering::Acquire)
    }

    pub fn admit_manual_generation(&self) -> Result<(), AgencyAdmissionError> {
        if self.focus_mode() {
            return Err(AgencyAdmissionError::FocusMode);
        }
        Ok(())
    }

    pub fn admit_automation(&self) -> Result<(), AgencyAdmissionError> {
        self.admit_manual_generation()?;
        if !self.automation_enabled() {
            return Err(AgencyAdmissionError::AutomationDisabled);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AgencyAdmissionError {
    #[error("focus mode blocks model generation")]
    FocusMode,
    #[error("project automation has not been explicitly enabled")]
    AutomationDisabled,
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub struct JobSender<T> {
    sender: SyncSender<T>,
}

impl<T> Clone for JobSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

impl<T> JobSender<T> {
    pub fn try_submit(&self, job: T) -> Result<(), SubmitError<T>> {
        self.sender.try_send(job).map_err(|error| match error {
            TrySendError::Full(job) => SubmitError::Full(job),
            TrySendError::Disconnected(job) => SubmitError::Disconnected(job),
        })
    }
}

#[derive(Debug)]
pub struct JobReceiver<T> {
    receiver: Receiver<T>,
}

impl<T> JobReceiver<T> {
    pub fn try_receive(&self) -> Result<Option<T>, QueueDisconnected> {
        match self.receiver.try_recv() {
            Ok(job) => Ok(Some(job)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(QueueDisconnected),
        }
    }
}

pub fn bounded_job_queue<T>(
    capacity: usize,
) -> Result<(JobSender<T>, JobReceiver<T>), QueueConfigError> {
    if capacity == 0 || capacity > MAX_QUEUE_CAPACITY {
        return Err(QueueConfigError { capacity });
    }
    let (sender, receiver) = sync_channel(capacity);
    Ok((JobSender { sender }, JobReceiver { receiver }))
}

#[derive(Debug, Error)]
pub enum SubmitError<T> {
    #[error("bounded job queue is full")]
    Full(T),
    #[error("bounded job queue is disconnected")]
    Disconnected(T),
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("queue capacity {capacity} is outside 1..={MAX_QUEUE_CAPACITY}")]
pub struct QueueConfigError {
    capacity: usize,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("bounded job queue is disconnected")]
pub struct QueueDisconnected;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_applies_backpressure() {
        let (sender, receiver) = bounded_job_queue(1).expect("queue");
        sender.try_submit(1).expect("first job");
        assert!(matches!(sender.try_submit(2), Err(SubmitError::Full(2))));
        assert_eq!(receiver.try_receive().expect("receive"), Some(1));
    }

    #[test]
    fn cancellation_is_shared() {
        let first = CancellationToken::default();
        let second = first.clone();
        first.cancel();
        assert!(second.is_cancelled());
    }

    #[test]
    fn focus_mode_atomically_blocks_manual_and_automatic_generation() {
        let gate = AgencyGate::default();
        gate.set_automation_enabled(true);
        assert!(gate.admit_manual_generation().is_ok());
        assert!(gate.admit_automation().is_ok());
        gate.set_focus_mode(true);
        assert_eq!(
            gate.admit_manual_generation(),
            Err(AgencyAdmissionError::FocusMode)
        );
        assert_eq!(
            gate.admit_automation(),
            Err(AgencyAdmissionError::FocusMode)
        );
    }

    #[test]
    fn automation_is_opt_in_even_when_manual_generation_is_available() {
        let gate = AgencyGate::default();
        assert!(gate.admit_manual_generation().is_ok());
        assert_eq!(
            gate.admit_automation(),
            Err(AgencyAdmissionError::AutomationDisabled)
        );
    }
}

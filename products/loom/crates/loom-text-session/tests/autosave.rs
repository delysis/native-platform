use loom_text_session::{
    autosave::{CHECKPOINT_IDLE, DRAFT_INTERVAL, Schedule},
    worker::WriteMode,
};
use std::time::{Duration, Instant};

#[test]
fn composition_defers_checkpoints_without_spinning_or_delaying_draft_protection() {
    let now = Instant::now();
    let mut schedule = Schedule::default();
    schedule.observe(now, [1, 0], true, true);
    let later = now + Duration::from_secs(10);
    assert_eq!(schedule.deadline(false), Some(now + DRAFT_INTERVAL));
    assert_eq!(schedule.due(later, false), Some(WriteMode::Journal));
    schedule.observe(later, [1, 0], true, false);
    assert!(schedule.deadline(false).is_none());
    assert!(schedule.due(later, false).is_none());
    assert_eq!(schedule.deadline(true), Some(now + CHECKPOINT_IDLE));
    assert_eq!(schedule.due(later, true), Some(WriteMode::Checkpoint));
}

#[test]
fn typing_does_not_postpone_journal_protection_but_restarts_the_idle_checkpoint() {
    let now = Instant::now();
    let mut schedule = Schedule::default();
    schedule.observe(now, [1, 0], true, true);
    for revision in 2..=5 {
        schedule.observe(
            now + Duration::from_millis(revision * 100),
            [revision, 0],
            true,
            true,
        );
    }
    assert_eq!(
        schedule.due(now + DRAFT_INTERVAL, true),
        Some(WriteMode::Journal)
    );
    schedule.observe(now + DRAFT_INTERVAL, [5, 0], true, false);
    assert_eq!(schedule.due(now + CHECKPOINT_IDLE, true), None);
    assert_eq!(
        schedule.due(now + Duration::from_millis(500) + CHECKPOINT_IDLE, true),
        Some(WriteMode::Checkpoint)
    );
}

#[test]
fn navigation_does_not_delay_save_and_failures_pause_until_explicit_retry() {
    let now = Instant::now();
    let mut schedule = Schedule::default();
    schedule.observe(now, [1, 0], true, false);
    for millis in [100, 200, 500, 800] {
        schedule.observe(now + Duration::from_millis(millis), [1, 0], true, false);
    }
    assert_eq!(
        schedule.due(now + CHECKPOINT_IDLE, true),
        Some(WriteMode::Checkpoint)
    );
    schedule.pause();
    schedule.observe(now + Duration::from_secs(10), [2, 0], true, true);
    assert!(schedule.deadline(true).is_none());
    assert!(schedule.due(now + Duration::from_secs(100), true).is_none());
    schedule.resume();
    schedule.observe(now + Duration::from_secs(100), [2, 0], true, true);
    assert_eq!(
        schedule.deadline(true),
        Some(now + Duration::from_secs(100) + DRAFT_INTERVAL)
    );
    schedule.observe(now + Duration::from_secs(100), [2, 0], false, false);
    assert!(schedule.deadline(true).is_none());
}

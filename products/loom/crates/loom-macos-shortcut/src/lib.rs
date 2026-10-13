#![cfg(target_os = "macos")]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::RcBlock;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

const LEFT_ARROW_KEY_CODE: u16 = 123;
const RIGHT_ARROW_KEY_CODE: u16 = 124;
const RETURN_KEY_CODE: u16 = 36;
const KEYPAD_ENTER_KEY_CODE: u16 = 76;
const TAB_KEY_CODE: u16 = 48;
static REVERSAL_ARMED: AtomicBool = AtomicBool::new(false);
static CANDIDATE_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_armed(armed: bool) {
    REVERSAL_ARMED.store(armed, Ordering::Release);
    acceptance_log(&format!("completion reversal armed={armed}"));
}

pub fn set_candidate_active(active: bool) {
    CANDIDATE_ACTIVE.store(active, Ordering::Release);
}

/// Install Loom's process-local Option-Left observation before `WebKit` handles
/// the event. An armed reversal consumes exactly one matching key; ordinary
/// Option-Left events continue through `AppKit`.
pub fn install(notify: impl Fn() + Send + Sync + 'static) {
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit supplies a valid NSEvent for the duration of this
        // callback. The event is only inspected and its original pointer is
        // returned so ordinary AppKit/WebKit dispatch continues.
        let event_ref = unsafe { event.as_ref() };
        let modifiers = event_ref.modifierFlags();
        let unmodified = !modifiers.intersects(
            NSEventModifierFlags::Command
                | NSEventModifierFlags::Control
                | NSEventModifierFlags::Shift,
        );
        // A fan acceptance keeps Option held while Return/Tab is dispatched.
        // That same native event is stronger than the renderer's eventually
        // delivered candidate-active update and closes the immediate reversal
        // race without affecting an ordinary acceptance key.
        let fan_acceptance = modifiers.contains(NSEventModifierFlags::Option);
        if (CANDIDATE_ACTIVE.load(Ordering::Acquire) || fan_acceptance)
            && unmodified
            && matches!(
                event_ref.keyCode(),
                RETURN_KEY_CODE | KEYPAD_ENTER_KEY_CODE | TAB_KEY_CODE | RIGHT_ARROW_KEY_CODE
            )
        {
            REVERSAL_ARMED.store(true, Ordering::Release);
            acceptance_log("completion acceptance key armed reversal");
        }
        if event_ref.keyCode() == LEFT_ARROW_KEY_CODE
            && modifiers.contains(NSEventModifierFlags::Option)
            && unmodified
            && REVERSAL_ARMED.swap(false, Ordering::AcqRel)
        {
            acceptance_log("completion reversal key consumed");
            notify();
            return std::ptr::null_mut();
        }
        event.as_ptr()
    });
    // SAFETY: The block always returns the same valid pointer received from
    // AppKit. AppKit copies the block and owns the process-lifetime monitor.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
    };
    if let Some(monitor) = monitor {
        std::mem::forget(monitor);
    } else {
        eprintln!("Loom could not install completion reversal monitor");
    }
}

fn acceptance_log(message: &str) {
    if std::env::var_os("DELYSIS_LOOM_ACCEPTANCE_DIR").is_some() {
        eprintln!("loom-macos-shortcut: {message}");
    }
}

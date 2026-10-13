#![cfg(target_os = "macos")]

use std::ptr::NonNull;

use block2::RcBlock;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

const LEFT_ARROW_KEY_CODE: u16 = 123;

/// Install Loom's process-local Option-Left observation before WebKit handles
/// the event. The original event always continues through AppKit.
pub fn install(notify: impl Fn() + Send + Sync + 'static) {
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit supplies a valid NSEvent for the duration of this
        // callback. The event is only inspected and its original pointer is
        // returned so ordinary AppKit/WebKit dispatch continues.
        let event_ref = unsafe { event.as_ref() };
        let modifiers = event_ref.modifierFlags();
        if event_ref.keyCode() == LEFT_ARROW_KEY_CODE
            && modifiers.contains(NSEventModifierFlags::Option)
            && !modifiers.intersects(NSEventModifierFlags::Command | NSEventModifierFlags::Control)
        {
            notify();
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

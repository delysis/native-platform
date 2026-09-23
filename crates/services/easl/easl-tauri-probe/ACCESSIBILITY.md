# Native accessibility qualification

The macOS surface attaches `easl-native-accessibility::Bridge` before its
managed window is shown. AccessKit exposes the two existing `TextEditor`
buffers, their actual shaped text runs, labels, selection, logical bounds and
scale transform. No second mutable text model is introduced. The bridge remains
attached across renderer suspend/reattach. A close intention first retires its
native child geometry to an empty, noninteractive root and drains the retirement
notifications before native close processing. A veto republishes the retained
editor tree. Actual destruction takes the editor out of its terminal slot and
drops the adapter outside the host borrow; final host cleanup follows the same
rule. Destructors can themselves generate native notifications.

## Editing and redraw

Native callbacks enqueue bounded requests and wake the real Tauri event loop
through its captured proxy. A main-thread convenience function is insufficient
for waking an idle loop because it may execute immediately. Actions are drained
at the event-batch boundary. The view's owner/revision and exact field/run IDs
must still match; stale actions do nothing. A focus request selects a field and
asks the OS to focus the window outside the host borrow. It never invents an OS
focus observation. Value and selection mutations require observed window focus
and that field to be active. One SetValue replaces the actual buffer in one
undo operation; an identical value is a no-op.

Text edits, undo/redo, focus-owner changes and geometry changes invalidate the
input revision. Caret and scroll updates advance a separate presentation counter:
consecutive absolute selections over unchanged text remain valid, while a queued
value cannot overwrite subsequent typing. Multiline parent values are explicit
immutable copies of the real buffer, because the pinned consumer does not derive
them from child text runs. No independent mutable text owner is created.
Unchanged native repaints do not rebuild the AX tree. Without AX activation,
no accessibility tree is constructed. Changed actions share the existing
coalesced repaint path, and notifications are raised only outside the host
borrow. Performance gains are not quantified until measured.

Tao's unresolved composition path remains guarded. An unpaired text/IME event
freezes the surface rather than allowing a queued AX write to bypass the input
failure. This is not an IME implementation or a substitute for one.

## Regular component checks

From the exact checkout, with the pinned toolchain:

```sh
rustup run 1.92.0 cargo test --offline --locked --profile native-view -p easl-native-accessibility
rustup run 1.92.0 cargo clippy --offline --locked --profile native-view -p easl-native-accessibility --all-targets -- -D warnings
rustup run 1.92.0 cargo test --offline --locked --profile native-view -p easl-tauri-probe --all-targets --features native-probe
rustup run 1.92.0 cargo clippy --offline --locked --profile native-view -p easl-tauri-probe --all-targets --features native-probe -- -D warnings
rustup run 1.92.0 cargo run --offline --locked --profile native-view -p xtask -- macos-smoke-support target/macos-smoke-support
```

The full macOS workflow runs the new crate's tests/Clippy alongside the existing
probe. The smoke-helper compiler runs the new Swift helper's `--self-test`
(argument and exact-byte comparison checks only) without creating an application.
Keep the existing `--check-native-lifecycle` run, including close/exit vetoes,
peer preservation, and actual Manager removal, on the new binary.

## Real AX exercise, explicit foreground operation

After foreground testing is authorized, open a fresh example with `--open-probe`.
This creates only ephemeral buffers, not a Loom project. Obtain its exact PID
and SHA-256 from the launch. Run the helper built above:

```sh
target/macos-smoke-support/exercise_easl_native_accessibility \
  "$EXACT_PID" "$EXACT_EXECUTABLE_SHA256" --exercise-ephemeral
```

The helper verifies the process/executable and refuses nonempty buffers. It
finds exactly two native AX text areas, requests and observes field focus,
sets exact Unicode text, changes a UTF-16 selection, replaces nonempty text,
and drives Cmd-Z/Cmd-Shift-Z through the actual native key path. It checks exact
UTF-8 values and independent histories, not Unicode canonical-equivalent String
comparison. It reads no clipboard. It rejects an observed AXWebArea and unreadable
or unbounded trees. Process replacement, ambiguous fields and missing permission
are failures, never synthetic success. After the 61 text/focus/history assertions,
the helper presses the exact process's sole native window close button and observes
that captured process terminate (63 assertions total). It does not kill processes.
The launching agent must independently wait for that exact child, require exit
status 0, and retain stdout/stderr as well as the helper receipt and source/binary
identities. Observed termination alone is not a clean-exit or resource-join proof.

This proves only the exercised two-field AX path when it actually passes.
VoiceOver navigation/announcements, rich document semantics, genuine IME
preedit/commit/cancel, candidate rectangles, arbitrary reentrant/modal callbacks,
Loom services, source parity and signed packaging still need independent
acceptance. Wry dependencies remain linked and the Loom App/CSS guard is
unchanged. Do not mark PR #73 ready or promote it from component results.

# EASL / Tauri native surface probe

A managed-window integration experiment: each pair of ephemeral editors uses one
EASL edit-policy instance and one native font/layout context. A small embedded
EASL program lays out the fields. The same buffers edited by the native input
adapter are shaped and painted through `easl-native-text` into Softbuffer.

There are no Loom dependencies, documents, projects, generation, network
requests, IPC commands, or WebView creation calls. Tauri owns the event loop and
managed native windows. **Wry/WebView dependencies remain linked.** This is not a new
Tauri runtime or a production frontend API.

## Boundaries

The pinned APIs are Tauri 2.11.5, tauri-runtime 2.11.3, tauri-runtime-wry 2.11.4
and Tao 0.35.3. The existing `App::wry_plugin` hook receives raw Tao events;
`WindowBuilder` creates managed native windows without creating WebViews. The
`unstable` feature is required for this reviewed native API. Manager registration,
window dispatch and close/exit callbacks now participate in the same runtime.

The hook owns no native state and is Send. One interactive surface or two hidden
check surfaces live in bounded thread-local slots on the event-loop thread,
avoiding unsafe Send/Sync wrappers. Exact runtime IDs own routing. Close/exit
intentions suspend OS render targets but retain editor text, selections, history
and fonts. Only actual destruction removes a logical owner. The hook returns
false, preserving Tauri callbacks and vetoes. `run_return` permits failure
reporting after the event loop; post-run verification calls no Tauri getters.

See [managed lifecycle](MANAGED-LIFECYCLE.md) for the explicit hidden-window
exercise and the non-reentrant callback restriction. This is not yet attachable
to arbitrary applications, plugins or modal event loops.

Tao's public event enum includes `ReceivedImeText`, and its macOS implementation
can emit it before an ordinary key's identical text. The adapter retains one
bounded echo and accepts that ordinary key exactly once, never editing from the
echo itself. An unmatched echo, a second unpaired echo, or an intervening input/
focus/event-batch boundary emits a fixed diagnostic and latches keyboard input
closed until the probe restarts. It ignores synthetic focus-replay key presses.
Preedit/cancellation ownership is still missing; this is not qualified IME input.
Tao does expose `set_ime_position`; the probe has not bound it to a complete
composition/candidate-geometry contract. macOS now has an AccessKit attachment
over the real text buffers; native qualification is described in
[accessibility](ACCESSIBILITY.md). **IME/candidate rectangles, non-macOS OS
accessibility, multiclick selection, timed drag autoscroll, caret blinking, touch,
and mobile are not implemented here.** The underlying text library's facilities do not qualify a
missing OS binding. Do not enter valuable text: accepted destruction discards
the ephemeral buffers.

Clipboard access is explicit Copy/Cut/Paste. Cut writes the clipboard before
changing text; failed writes do not delete. Text admission is bounded, but
Arboard's initial OS clipboard read allocates before text admission; no bounded
OS clipboard-allocation claim is made. Errors report fixed stage names, not text.

Native pointer motion is coalesced while dragging; release flushes the last
position before clearing capture. Resizing/DPI changes revoke the old pointer
capture and defer geometry reads until after the runtime processes its event.
A shared viewport cache skips unchanged EASL layout calls. Fonts, raster storage
and text layouts are reused. No speedup or latency threshold is claimed without
measurements.

## Component and compile checks (no application launch)

From the repository root, using the pinned toolchain and an existing dependency
cache:

```sh
rustup run 1.92.0 cargo test --offline --locked --profile native-view -p easl-tauri-probe
rustup run 1.92.0 cargo clippy --offline --locked --profile native-view -p easl-tauri-probe --all-targets -- -D warnings
```

On macOS also compile the actual hook/example and run its component tests:

```sh
rustup run 1.92.0 cargo test --offline --locked --profile native-view -p easl-tauri-probe --all-targets --features native-probe
rustup run 1.92.0 cargo clippy --offline --locked --profile native-view -p easl-tauri-probe --all-targets --features native-probe -- -D warnings
```

The example accepts exactly `--open-probe` for the foreground probe or
`--check-native-lifecycle` for the two-hidden-window runtime check. `--build-info`
prints source-level metadata without constructing a context, VM, window or buffer.
None of these modes is invoked by component tests. The full macOS workflow
compiles the native feature and runs its tests, not its application main.

`build.rs` supplies Cargo's OUT_DIR and target triple for the ordinary
`generate_context!` macro. This no-IPC probe uses codegen's empty-ACL defaults,
not tauri-build's source-tree schema generation. Adding commands or production
capabilities requires revisiting that boundary, not importing a dummy manifest.

## Separately authorized native checks

After compilation, explicitly invoke the same identified example binary with
`--check-native-lifecycle` in a working GUI session. This executes actual close
and exit veto callbacks using hidden windows and fixed in-memory text. It is not
a foreground test, but still initializes a native application. Retain command,
exit status, executable/source identities and the emitted receipt. Hidden native
lifecycle evidence does not qualify visible pixels or native text input.

Only after foreground testing is separately authorized:

```sh
rustup run 1.92.0 cargo run --offline --locked --profile native-view -p easl-tauri-probe --features native-probe --example easl-tauri-two-fields -- --open-probe
```

Confirm two actual editable surfaces; Unicode key text and clipboard insertion;
independent undo/redo; Tab/Shift-Tab; click/drag selection; wheel/pixel scrolling;
resize/Retina changes; focus loss; native window close and Cmd/Ctrl-Q. Record PID,
executable hash and source/dirty-patch identity. Retain a native failure as a
failure. A shutdown frame count is diagnostic, never a parity certificate.

The next upstream-facing work is an actual per-window composition/candidate
rectangle bridge. Qualify the attached macOS accessibility path and preserve the
managed lifecycle check before attempting the full two-field qualification and
a real Loom service-bound writing pane. Component passes alone do not close either
the OS-accessibility or IME boundary.
The existing Loom App/CSS reference guard is neither changed nor bypassed by this
separate experiment.

# Managed native-window lifecycle

This remains a bounded native-surface experiment, not a production editor or a
universal Tauri plugin. It uses actual Tauri `WindowBuilder` windows instead of
raw `create_tao_window` windows. Tauri Manager registration, native dispatch and
close/exit callbacks therefore participate in the same run.

## Ownership

`native.rs` creates the application and installs the pinned runtime hook.
`native/host.rs` routes by the exact runtime window ID captured at attachment.
Labels are used to resolve that ID once, not to authorize later input. The
inventory is deliberately bounded to one interactive window or two check windows.
No view is rebound after destruction. No second event loop, hidden WebView,
JavaScript command bridge, product document store or model is introduced.

`SurfaceSlot` has three meaningful states: attached, suspended, and destroyed.
A close or exit request is an intention. Before Tauri processes it, the host
suspends the affected native render target, retaining the text buffers,
selections, undo histories, font context and EASL policy. A rejected intention
can resume that same logical owner after the runtime dispatch. An accepted
intention stays suspended until destruction takes the owner exactly once.

`RenderTarget` owns only the OS-dependent softbuffer surface/context. It uses
Tauri dispatcher handles, not an extra strong `Arc<TaoWindow>` owner. Resources
are released before normal runtime close processing; finding them still attached
at `Destroyed` records a failure rather than claiming a successful teardown.

The hook never consumes Tauri events. Close requests are sent after releasing
the host borrow. Exit requests use the runtime proxy's fallible send rather than
`AppHandle::exit`'s process-exiting fallback. Manager removal is recorded during
the final exit-request callback; the post-`run_return` verifier invokes no Tauri
getters after framework cleanup.

## Explicit native check

Compile with `native-probe`, then invoke the example with
`--check-native-lifecycle`. This is real native application execution, not an
ordinary unit test. It requests two hidden, initially unfocused windows. It does
not request foreground window display or accessibility permission, but it still
requires a working GUI session and may initialize normal OS application state.
Neither Cargo tests nor the default CI commands invoke it.

The check uses fixed ephemeral text, never user files or the clipboard. It:

1. Creates two managed windows and paints actual native buffers.
2. Requests close on the primary window. The real window listener vetoes once.
   The host must reattach the renderer, repaint, and preserve both fields' exact
   bytes, selections and undo/redo. The peer renderer must not be released.
3. Requests application exit. The real application callback vetoes once. Both
   windows must reattach, repaint, and retain their independent edit histories.
4. Allows primary destruction, checks Manager removal and the surviving peer,
   then allows peer destruction and normal event-loop return.

A successful receipt requires the corresponding real callbacks, exact native
attachment/release counts, a positive presentation for each attachment, and
actual editor undo/redo checks. A timeout, missing callback, early exit, foreign
release, stale owner, retained native resource, or missing Manager removal is a
failure. The fifteen-second test deadline starts after editor initialization;
it is not a production I/O deadline and must not be increased to hide a failure.

The receipt remains `qualified: false`. Hidden-buffer presentation is not proof
of visible pixels, keyboard input, IME behavior, screen-reader interaction,
signed packaging, Loom parity, or measured application performance.

## Work and resource bounds

Input invalidates one pending repaint. The host flushes at `MainEventsCleared`;
a real OS exposure can independently request painting. There is no idle redraw
poller in interactive mode. Geometry getters cross the managed dispatcher, so
size/scale are cached and refreshed only after geometry invalidation or native
reattachment. The last geometry, view/model, and font context remain separately
owned. Close-veto reattachment never recompiles the editor or replaces its text.

Frame, attachment, release and geometry-resolution counters in the native check
are observations, not a speed claim. Measure latency and allocations in the
actual artifact before selecting CPU/GPU composition or broader caching policy.

## Deliberate qualification limits

This launcher installs only its own non-reentrant check callbacks. Batch-end
liveness is not an exported general-purpose close-decision API. Before allowing
arbitrary application/plugins, qualify modal/nested callback delivery and add an
explicit post-dispatch lifecycle coordinator if nested native dispatch can run
before the original close decision finishes. Never turn a surviving label into
permission to resume an old native handle.

Accepted application exit while native windows remain open is not covered by
this checker: it tests an exit veto and subsequent ordinary window destruction.
Forced external native destruction, other plugins consuming lifecycle events,
late events across runtime replacement, creation of further windows, and mobile
lifecycle variants remain unqualified. The application is intentionally not
advertised as attachable to arbitrary existing Tauri instances yet.

Tao's incomplete composition stream remains behind the existing text-echo
rejection policy. Full preedit/cancellation/owner/candidate-rectangle integration
and the OS accessibility adapter remain separate required work. The code does
not emulate either with labels or pretend success on unsupported events.

Pinned APIs: Tauri 2.11.5 (`unstable` window API), tauri-runtime-wry 2.11.4,
tauri-runtime 2.11.3, Tao 0.35.3. Wry/WebKit dependencies remain in the graph even
though this launcher requests no WebViews. Do not claim a WebKit-free binary.

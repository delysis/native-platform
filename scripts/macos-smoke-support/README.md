# macOS smoke support

These platform programs implement operations used by `scripts/smoke-macos-app.sh`.
Keep Swift platform bindings here; shell owns launch, artifact identity, isolated
state and cleanup. Compile the settled source with the pinned toolchain:

```sh
rustup run 1.92.0 cargo run --offline --locked -p xtask -- \
  macos-smoke-support target/macos-smoke-support
```

This compiles and links every helper and runs the completion driver's portable
`--self-test`, without launching an application or requesting Accessibility
access. The existing full macOS CI and selected release-tooling lane run it.
The smoke script compiles its helpers once in its isolated temporary directory
and reuses them throughout both launch/use/quit cycles. Compilation and portable
contract checks do not establish a successful UI interaction.

## Current completion controls

`set_loom_completion_toggle` is the single matcher and transition driver.
`exercise_loom_completion_controls` and `loom_completion_control_state` invoke
the sibling compiled driver rather than defining another AX traversal.
Keep the three executables together.

- `--state PID` observes only. It neither activates a window nor posts input.
- `--exercise PID` exercises policy off/on, both mode changes, then policy off,
  preserving the initially observed Ghost text/Loompad mode. It certifies only
  those observed control transitions, never inference, generation or rendering.
- `PID enable enabled` / `PID disable disabled` change policy with Cmd+Shift+G.
  Optional `require-press` rejects a state already present without activation.
  The two old argument pairs remain for the current shell caller; they are not
  labels searched for in the UI.

Policy changes use Cmd+Shift+G; mode changes use AXPress on the one current
Ghost text/Loompad button. Each admitted transition sends at most one input and
waits for its exact settled acknowledgement. Busy controls, ambiguous matches,
incomplete traversal, changed PID/launch identity and lost foreground ownership
cannot admit another input. A failed acknowledgement is not retried by toggling
again. Run mutation-capable commands only within an authorized isolated smoke
session with the exact intended process frontmost. They can enable suggestions;
there is no claim that the helper itself establishes a model-free environment.

## Observation-only failure diagnostics

`--state` also locates the exact `Completion session witness` AX note. It emits a
bounded, whitelisted summary under `completion_witness`: family phase/counts,
selected/rendered/visible-key agreement and stream-versus-immutable-key shape.
It does not copy manuscript text, paths, IDs, opaque keys or unknown fields into
that summary. Missing, ambiguous, oversized and malformed notes are distinct
outcomes; none are silently interpreted as an empty or successful state.

The note is renderer-owned metadata, not independent visual or durable proof.
Every summary states `visible_ghost_proven: false`. A terminal-key projection
report is a diagnostic lead, not permission to replace the strict pre-terminal
streaming witness with a status label. `start_loom_live_streaming_monitor` and
all artifact, generation, persistence, cancellation and relaunch gates retain
their existing acceptance conditions.

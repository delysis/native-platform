# macOS smoke support

These are the platform programs used by `scripts/smoke-macos-app.sh`, extracted
unchanged from its former Swift heredocs. Each filename matches its shell
operation. Keep the Swift platform bindings here; shell owns launch and cleanup.

From the repository root:

```sh
cargo run --locked -p xtask -- macos-smoke-support target/macos-smoke-support
```

This compiles and links every helper without launching applications or requesting
Accessibility access. Full macOS CI and the selected release-tooling lane run it.
The smoke script builds its helpers once in its isolated temporary directory and
reuses the executables throughout both launch/use/quit cycles. Compilation alone
does not establish a successful UI interaction.

Loom acceptance uses an actual cached writer admitted by the embedded model
policy. Set `LOOM_SMOKE_GGUF_MODEL_PATH` to that GGUF. The default policy admits
`gemma-4-E2B-base-Q8_0.gguf`; native admission still verifies its exact digest,
byte length and completion capabilities. The script hard-links it into isolated
state under its existing filename and checks the file identity. It neither
renames a different model as the catalog writer nor downloads or modifies weights.
A text-only writer needs no projector. Missing weights fail before any window
opens; there is no model-free Loom acceptance mode.

Run this against the exact bundle being handed over, after focused development
checks have passed. The real four-candidate generation, visible ghost, cycling,
acceptance, reversal, and persistence checks are required even for changes
outside the editor. Browser fixtures and helper compilation remain useful fast
checks, but cannot replace this native handoff gate.

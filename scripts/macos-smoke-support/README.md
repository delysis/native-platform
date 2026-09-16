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

Loom acceptance always uses the actual local Gemma model. Set
`LOOM_SMOKE_GGUF_MODEL_PATH` to the cached `gemma-4-12b-it-qat-q4_0.gguf` and
keep its matching `mmproj-gemma-4-12b-it-qat-q4_0.gguf` alongside it (or set
`LOOM_SMOKE_PROJECTOR_PATH`). The script hard-links both into isolated state;
it neither downloads weights nor modifies the originals. Missing assets fail
before any window opens. There is no model-free Loom acceptance mode.

Run this against the exact bundle being handed over, after focused development
checks have passed. The real four-candidate generation, visible ghost, cycling,
acceptance, reversal, and persistence checks are required even for changes
outside the editor. Browser fixtures and helper compilation remain useful fast
checks, but cannot replace this native handoff gate.

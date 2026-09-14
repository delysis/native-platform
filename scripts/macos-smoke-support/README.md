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

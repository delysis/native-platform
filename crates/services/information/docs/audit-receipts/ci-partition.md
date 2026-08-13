# CI partition receipt

- The portable package list exactly covers every non-Tauri workspace crate and
  is checked before both test and Clippy commands.
- Linux, macOS, and Windows run the same portable package tests and strict
  Clippy gate on Rust 1.92.0.
- The Tauri plugin is isolated in a Linux job that installs the GTK, GLib,
  WebKitGTK, AppIndicator, and packaging dependencies it actually needs.
- All third-party actions use reviewed immutable revisions. Repository-owned
  workflow policy rejects mutable action references and live `ssh-keyscan`.

Local macOS success is not evidence for the Linux or Windows jobs. Those
portability claims require the public GitHub-hosted matrix to pass.

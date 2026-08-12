# Workflow hardening receipt

- Removed the deploy-key, SSH rewrite, and live `ssh-keyscan` path. Both native
  dependencies are public HTTPS repositories pinned by exact Git revision.
- Pinned every third-party action to a reviewed 40-hex revision.
- Corrected the workspace minimum Rust version from 1.85 to 1.88, matching the
  repaired native dependency and stable let-chain use.
- Split portable core feedback across Linux, macOS, and Windows from the Linux
  Tauri/GTK/WebKit shell job.
- Added a first-class policy job covering workflow pins and the coherent native
  dependency pair.

Local macOS gates do not substitute for GitHub-hosted Linux and Windows runs.

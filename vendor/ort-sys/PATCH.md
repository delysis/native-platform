# ort-sys 2.0.0-rc.13 Windows DLL placement

Source: published crates.io `ort-sys` 2.0.0-rc.13 archive, SHA-256
`cf211e3776eea6aec988552fa118dd746d70e1b1e5e244058d1c98015f3e5872`.
The original MIT and Apache-2.0 licenses and upstream source metadata are retained.

Only `build/dynamic_link.rs` differs from the published source. The DLL installer
creates all three destination directories and continues through `examples` and
`deps` after Windows falls back from symlinking to copying. Previously it stopped
after the profile directory, leaving tests and examples without adjacent DLLs.
Windows searches system directories before PATH, so adding the profile directory
to PATH cannot ensure the executable loads its matching runtime.

Dependency versions, native distribution URLs and checksums, linking policy,
and Unix symlink behavior are unchanged. This fixes a demonstrated installation
defect; the Windows loader diagnostics must still identify any specific missing
entry point. Remove the patch when a compatible upstream release fixes this
fallback. Review by 2026-12-14.

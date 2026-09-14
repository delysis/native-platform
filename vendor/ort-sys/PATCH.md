# ort-sys 2.0.0-rc.13 Windows DLL placement

Source: published crates.io `ort-sys` 2.0.0-rc.13 archive, SHA-256
`cf211e3776eea6aec988552fa118dd746d70e1b1e5e244058d1c98015f3e5872`.
The original MIT and Apache-2.0 licenses and upstream source metadata are retained.

Only `build/dynamic_link.rs` differs from the published source. On Windows, the
DLL installer copies verified native-library bytes into the profile, `examples`,
and `deps` directories. It creates missing directories, removes existing symlinks,
and refreshes regular copies when the build script runs. All destination files
are tracked so missing or changed outputs trigger repair.

Windows CI restored broken `DirectML.dll` symlinks in the profile and `deps`
directories: their targets were in an absent external `ort.pyke.io` download
cache. Upstream also stopped after the profile directory when symlinking failed
and it fell back to copying. Always copying on Windows fixes both cases without
requiring Developer Mode or external-cache symlinks. Windows searches system
directories before PATH, so PATH changes cannot ensure matching runtime DLLs.

Dependency versions, native distribution URLs and checksums, linking policy,
and Unix symlink behavior are unchanged. Loader diagnostics identify broken links;
they do not establish the exact symbol behind an earlier missing-entry-point
failure. Remove the patch when a compatible upstream release installs self-contained
Windows runtime DLLs in all executable directories. Review by 2026-12-14.

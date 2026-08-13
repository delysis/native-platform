# Unpublished fresh-staging receipt

## Identity

- Repository: `delysis/information-native-kit`
- Branch: `codex/unpublished-fresh-staging`
- Base: `7f27d06c6f522abd77561741bfb8154906fa6171`
- Status: steward successor candidate

## Ownership correction

Fresh transfers no longer create the caller-visible destination before the
bytes are validated. File paths, granted `file:` URIs, and HTTP transfers with
`ResumePolicy::Disabled` use a library-generated `NamedTempFile` in the
destination's parent, enforce private regular-file properties, validate exact
length and SHA-256, expose a cancellable `Publishing` observation, sync the
file, and publish with `persist_noclobber`.

Durable Unix resume remains on the existing `StagingFile`/sidecar/lease path.
No fresh transfer creates a resume sidecar or preserves a partial destination.

On Unix, tempfile's path-only automatic cleanup is disabled and cleanup is
bound to the opened inode. A replaced temporary pathname is therefore not
deleted. On non-Unix, `NamedTempFile` retains its native owned cleanup behavior;
the caller-provided destination is never used as the temporary pathname.

## Deterministic evidence

New tests cover digest and length mismatch, source read failure, cancellation
at start, cancellation after validation but before publication, destination
creation during publication, exact successful publication, sibling-temp
cleanup, private Unix permissions, replacement-safe temp cleanup, HTTP digest
failure, and an HTTP publication race. The two historically failing Windows
regressions remain enabled and pass on this macOS tree.

```text
Rust 1.92.0: cargo test -p information-native-acquire --lib
  PASS: 39 passed
Rust 1.92.0: cargo test --workspace --all-targets --locked
  PASS: 188 passed, 4 explicitly ignored real-library tests
Rust 1.92.0: cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
  PASS
cargo fmt --all -- --check
  PASS
./scripts/check-boundaries.sh
  PASS
git diff --check
  PASS
```

## Verification boundary

No Windows Rust target is installed on this host, so this is not a Windows
runtime claim. The public workflow now has an exact non-Tauri package list and
runs it on Linux, macOS, and Windows; the Tauri plugin has a distinct Linux job
with its GTK/GLib/WebKitGTK dependencies. The historical Windows tests and new
portable regressions must pass on that GitHub-hosted runner before promotion.

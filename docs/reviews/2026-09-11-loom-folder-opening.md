# Loom folder opening repair

The reported native failure had two causes: the default writing folder contained
the previous development database, and choosing an ordinary folder required a
pre-existing Loom manifest. The earlier isolated-store acceptance did not cover
the normal application store.

## Product changes

Loom opens ordinary folders, discovers Markdown and text files recursively, and
registers them in place without rewriting their bytes. New notes live at the
folder root. The private history sidecar is created when absent; manuscript,
source, and asset directories are no longer created gratuitously. Image ingestion
creates its asset directory only when needed.

Discovery is bounded, skips hidden/generated directories and symlinks, and keeps
existing document identities, revisions, and deletion tombstones. Filesystem hints
discover new files and cover directory renames, including dotted directory names.
Root-level files use the same source-bound save and no-clobber rename/delete paths.
An unsupported text encoding or oversized unregistered file is left untouched
and listed in compact folder details, without blocking readable writing or later
filesystem refreshes. Replacing that file externally with supported UTF-8 lets
the next refresh discover it normally.

Folder selection validates and holds one candidate store while the current
writing remains open. Cancellation, same-folder selection, and invalid targets
retain that session. Only a validated selection begins the existing save/close
protocol. Opaque single-use tokens bind the handoff; shutdown revokes admission,
and no mutex is held over the native picker. Failed startup offers Retry and
Open folder.

## Existing data

The local writing database contained 23 document records and 400 revisions.
Seven documents had visible files; the other histories were retained as well.
There were no transient drafts, unfinished visible projections, active
generations, or pending rename/delete operations.

A temporary safe-Rust development repair rebuilt the inspected database using
the single current schema. Every retained table matched its original typed-value
hash and row count. All 1,023 weave bindings passed the stronger current trigger;
SQLite integrity and foreign-key checks passed. The product's strict opener then
opened the installed database and all retained table values were checked again.

Both a complete logical backup and a byte-exact original remain in the writing
folder's `.loom/backups/development-store-2026-09-11/`, with preparation and
installation receipts. The complete backup also retains the retired research
tables (749 candidate records and 1,498 review events). No writing files or model
assets were deleted. The temporary repair program is excluded from the product;
there is no shipped migration chain or weakened schema validation.

## Validation

- Store: 127 unit tests and three active integration tests passed; one pre-existing
  integration test remains explicitly ignored.
- CLI: 10 tests passed.
- Plugin: 176 tests passed; two pre-existing native tests remain explicitly ignored.
- Frontend: 448 tests passed; Svelte reports zero errors and warnings.
- Strict Clippy passed for the changed Rust crates and CLI; formatting, workspace
  policy, and current-doc validation passed.
- The debug bundle built using the shared target and passed strict ad-hoc signature
  verification. Executable SHA-256:
  `65b3645eea0668177a0c0a20e49336ae4f9b9075320ae336b4f6bcc023d12708`.
- Native interaction remains unverified: after the repaired store passed the real
  storage opener, Computer Use reported that the Mac was locked. The rebuilt app
  has not yet been visibly opened. A plain-folder native test copy is prepared;
  no launch or static check is being treated as visible product acceptance.

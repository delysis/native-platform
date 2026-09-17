# Loom text session

Renderer-independent Rust operations for Loom frontends. No dependency on EASL,
text rendering, windows, webviews or Tauri. Current `loom-store`, `loom-config`,
`loom-document` and `loom-types` remain the application authorities.

Shared Tauri/native consumers use:

- `configuration::read` for the current settings snapshot, authored `.mine.toml`
  precedence, existing `.loom.md` handling and external-edit conflicts.
- `lifecycle::create_untitled` for root-level `Untitled.md` naming, reserved-path
  checks and no-clobber creation.
- `completion::capture` for a store-read source snapshot bound to document ID,
  revision, visible blob and a UTF-8 cursor boundary. The captured prefix retains
  literal Markdown and line endings. Reading it creates no generation artifacts
  and grants neither model authority nor permission to accept a continuation.

The native worker uses `persistence` for checkpoints and draft writes/clears bound
to exact document, revision, visible blob, command and draft version. Tauri and
native both use current `loom-store` operations; their adapter orchestration is
not yet consolidated in this checkout.

The Tauri adapter retains its existing project/session admission guard. The
standalone native `worker` owns one store and its exclusive project lease;
bounded request/reply queues keep subsequent disk operations off the UI thread.
Shutdown stops admission, drains accepted work and joins the worker. Requests
use stable document identities, including explicitly opened auxiliary editors.
There is no automatically created or hidden context manuscript.

`worker::Command::CaptureCompletion` uses the same source capture as Tauri's
Weave admission. Open and successful write replies include the acknowledged
`SourceIdentity`; a native caller must carry the matching identity when requesting
a prefix. The worker refuses drafts/pending checkpoints, stale identities,
external edits and wrong-project requests. It does not checkpoint implicitly or
start inference. Frontends must still bind their current selection, document and
visible bytes to the request and invalidate stale asynchronous replies.

Views own their unsaved text widgets. `TextProject::read` captures the source and
restores a draft only when document, kind and revision match. `baseline` returns
saved bytes separately from recovered text. Stale drafts remain intact and need
reconciliation. Saves journal committed text and checkpoint that exact draft.
Both prose and verse preserve exact bytes, including mixed line endings. Saving
does not rewrite editor text or create an undo transaction.

Only an applied visible projection advances the saved baseline. Pending commands
retain their original requests for retry. Reverting to the saved source clears
only the captured draft version without adding a semantic revision. Writes to
two documents are separate operations; a later failure preserves an earlier
successful acknowledgement. Automatic retry pauses after a failure.

New macOS native projects use the same encrypted-private-history store entrypoint
as Tauri. Existing plaintext projects remain unchanged; manuscripts and authored
configuration remain ordinary readable files. Tests use synthetic plaintext
fixtures rather than implicitly authorizing Keychain prompts.

This is not yet the complete shared application session. The native prototype
still limits admission to 16 documents and 1 MiB per text. Full lifecycle,
external-change UI, settings refresh/error presentation, stale-draft reconciliation,
generation, pane orchestration and fatal-worker recovery remain incomplete.

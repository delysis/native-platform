Use safe, idiomatic Rust with explicit ownership and narrow authority.
Read the owning product contract before changing behavior. Preserve source,
user data, and provenance; fixture output never establishes runtime acceptance.

These products are unreleased. Remove unused compatibility and migration code;
accept current formats and reject incompatible data without silently rewriting it.
Avoid speculative abstractions and duplicate sources of truth.

Use `docs/browser-development.md` for the browser-chat and GitHub CI workflow.
Keep substantial research and implementation in browser chats; reserve unattended
Codex calls for small evidenced CI repairs. Preserve the distinction between
model/specification, production binding, component checks and real product acceptance.

Prioritize working content in every product layout. Avoid oversized chrome,
permanent shortcut hints, and empty padding; retain comfortable click targets,
accessible controls, and native window resizing.

Reproduce consequential defects at the real boundary. Run focused checks while
editing, then one consolidated gate on the final revision. Reuse build outputs
and prune stale generated caches proactively; do not repeat expensive builds
without a changed input or unresolved failure. See CONTRIBUTING.md for commands.

Batch related edits after focused local checks before pushing a CI revision.
Dispatch full cross-platform qualification once the revision is settled; reuse
the existing PR and main workflows instead of launching overlapping full runs.

macOS is the development acceptance platform. Gate progress on macOS and fast
platform-independent checks. Keep Linux and Windows tests running, but repair
their failures asynchronously without holding up macOS development.

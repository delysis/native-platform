# Loom's Presage extension

This is the `presage` crate from the immutable revision in UPSTREAM.json.
Its AGPL-3.0-only license is retained in LICENSE.md. The file manifest records
unmodified upstream hashes so a future update can compare this small patch.

Changes:

- Add `Manager::retrieve_identity_key(Aci)`, using Presage's authenticated
  connection and libsignal-service's profile endpoint. It returns only the
  decoded public key. It does not replace trusted keys or establish a session.
- Add `Manager::retrieve_group_details` and `publish_group_description`, using
  the existing authenticated group client. The description PATCH contains only
  a revision and already encrypted description. It returns a bounded signed
  response; Loom verifies its server signature, group, editor, revision, and
  exact actions before recording publication. It does not notify members.
- Forbid unsafe Rust in this vendored crate.

This crate remains inside the isolated AGPL Signal worker. Its public extension
exists because the pinned upstream Manager keeps its authenticated connection
private. Do not extract account credentials or add a second HTTP client to work
around that boundary. Keep future modifications narrow and documented here.

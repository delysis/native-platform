# Foreground command authority

## Claim

`VerifiedForegroundCommand` means only:

> The trusted application host accepted one focused, one-use command in this
> process for the exact pending promotion named by the receipt.

It does not claim physical user presence, OS-authenticated input, biometric
identity, or protection from a renderer that is already compromised while the
confirmation is displayed. A future native menu, native dialog, or platform
credential path must use a separately named evidence class.

## Trusted boundary

The Tauri host owns the challenge registry, process-session fingerprint,
native window label, native focus observations, focus epoch, monotonic event
index, expiry clock, and atomic nonce consumption. Renderer IPC may carry the
opaque nonce and the requested identities, but cannot create a registry entry
or a `VerifiedForegroundCommand` value.

The production import controller is also host-owned. Renderer IPC can ask it to
open the native file picker but cannot supply a packet path or packet bytes.
The host reads a bounded regular file, admits its exact mixed-authorship record
into the live store, derives the current source and fresh command identity, and
only then creates a registry entry. The imported packet is not authority.

Before confirmation is rendered, the host records the exact pending promotion
and issues a bounded challenge tied to:

- process and application session;
- native window and its current focus epoch;
- document and candidate occurrence;
- command occurrence and canonical promotion fingerprint;
- one random, one-use nonce;
- issue and expiry times.

At the command edge the host derives the window label and focus from the native
window and immediately binds that reading into a move-only, registry/window-
scoped sample. The production constructor accepts the Tauri window itself and
queries focus internally, so callers cannot supply a Boolean focus claim. The
registry rejects a sample from another registry or window, a sample older than
one second, or a sample that predates the challenge. It checks the active
application session, removes the nonce under the registry mutex, validates
every binding, and only then mints the move-only authority. A failed use also
spends the presented nonce.

## Persistence and restart

The store consumes the authority by value in the same immediate transaction
that validates the durable pending request and its live subject lease and
commits the selected manuscript revision, provenance, ordinary command
receipt, and pending visible-file outbox row. SQLite persists a
content-addressed derived receipt, never the nonce or the authority. The
receipt records the narrow claim, identities, focus epoch, event index, and
bounded timeline. It is immutable audit evidence and has no constructor path
back to live authority.

Restart creates a new process-session registry with no pending nonces. Copying
or reopening a project creates a new store capability domain. Database rows,
receipt JSON, and replayed IPC payloads therefore cannot recreate promotion
authority.

## Residual risks

- A renderer compromised while a valid confirmation is visible can act as the
  renderer. Focus and one-use binding limit replay and substitution; they do
  not authenticate a human.
- Host-process compromise is outside this authority boundary.
- Clock rollback fails closed because both monotonic and wall-clock bounds are
  checked.
- Ordinary Tab acceptance remains product promotion evidence. Research
  promotion uses this separate foreground-command contract.
- Local manuscript inference remains in-process and local-only; this authority
  path adds no subprocess, loopback, hosted provider, or model-policy fallback.

# Native accessibility attachment

A small macOS AccessKit attachment plus a platform-independent deferred-action
mailbox. It has no Tauri, EASL, Loom, document, filesystem, network or renderer
owner. A frontend supplies a real AccessKit tree and its monotonically changing
input and presentation revisions. The existing buffers remain the only editable text owners.

## Event-loop contract

Attach on the macOS main thread, before showing or focusing the NSView. The
`HasWindowHandle` borrow must describe that same live view. The bridge retains
its NSView through AccessKit; it is not an additional owner of a Tauri window.
Do not install a second accessibility adapter on the same view.

Activation returns no fabricated tree. It records the request and wakes the
application; the next event batch creates a full tree from the real editor.
Actions capture the *published input revision at callback enqueue time*. The frontend
must check that revision against its current owner, revision, field and text-run
IDs before mutation. This rejects changes queued before subsequent typing,
focus changes, resize or other local edits. The presentation generation is
separate: caret/scroll notifications can change without discarding consecutive
absolute selections against unchanged text. It cannot recover an originating
revision that an OS accessibility request does not carry.

The callback queue admits at most 32 requests and at most 1 MiB per SetValue
payload. The initial OS/AccessKit string allocation precedes that admission;
this is not a claim of a bounded OS allocation. Queue overflow and failed wake
are recorded failures, not silent success. Wake requests coalesce until the
next batch. No callback borrows a document or performs an edit.

`publish` returns `Notifications`. Raise them **after** releasing every editor,
host or adapter borrow. AccessKit's pinned implementation explicitly warns
that raising notifications can synchronously call back into accessibility.
A suspended/destroyed owner invalidates queued actions and suppresses stale
notifications. Closing intent suspends input and replaces the native tree with
one noninteractive window root with no child geometry. Deliver those retirement
notifications outside the host borrow, before Tauri processes the close/exit.
This avoids leaving text nodes with coordinates tied to a detached native window.
A veto retains the subclass and editor; resume republishes the full real tree.
The subclass itself is not detached/recreated on a veto, because AccessKit requires
initial attachment before the view's first show/focus. Final adapter destruction
also runs outside the host borrow.

## Narrow platform boundary

The existing safe-Rust frontends retain `forbid(unsafe_code)`. Only `macos.rs`
in this new platform crate uses unsafe, at exactly two documented operations:

1. Query `pthread_main_np`, with no pointer arguments, before touching AppKit.
2. Give AccessKit 0.26.3 the live borrowed AppKit NSView pointer. Its constructor
   synchronously retains the view; its destructor restores the original class.

No unsafe Send/Sync implementation exists. A main-thread marker based on `Rc`
prevents moving the bridge to another thread. A thread-local registration
prevents duplicate attachment through this crate and survives constructor
failure through RAII. The adapter is dropped before its registration or
mailbox. This boundary needs the ordinary code/policy review; no existing
unsafe prohibition is removed or weakened.

The attachment is macOS-only. The mailbox's non-macOS tests do not prove an OS
binding. Run the dependent probe's real AX exercise and its managed-window
lifecycle check before calling the integration qualified. Neither a valid
TreeUpdate nor a compiled adapter is VoiceOver or IME acceptance.

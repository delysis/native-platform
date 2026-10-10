# Archive friends

Read-only preparation of cited Community Archive context for a native editor.
This workspace crate extracts the native preparation path from the source
snapshot identified in `SOURCE.json`. It imports no ingestion, migration,
network, archive-writing or model-experiment services.

The caller supplies an explicit dotfile, immutable SQLite snapshot, source
basis and project/session/document scope. Relative archive paths resolve beside
the dotfile. Preparation returns an immutable prompt pack and an untrusted-data
preamble; it never edits a manuscript or calls a model. The host must revalidate
its document/session authority before using the result, reserve model context
space and preserve the pack's provenance.

Two archive operations may be in flight. Scope replacement cancels its predecessor;
project shutdown must cancel and drain the provider. Queued jobs capture an
epoch before dispatch. Cache entries are bounded and scoped to source/config
identity. Archive size/mtime and WAL checks assume a frozen SQLite snapshot;
they do not establish a content hash of the entire archive.

Component checks use synthetic SQLite data. They establish retrieval, pack
integrity, scope/cache separation, cancellation admission, retained WAL refusal
and refusal of unsupported archive record kinds. Unix dotfile checks cover
regular-file/size bounds and rejection of symbolic links, multiple hard links
and FIFOs. They do not qualify Windows filesystem behavior, concurrent hostile
parent replacement or an integrated native editor journey.

Invitations use the shared workspace reference grammar. Only bare configured
handles invite archive context; quoted/scoped/path names and retained links
remain workspace references. Code and historical quotes cannot invite a voice.
The topic window retains the full document lexical state.

Loom now prepares this context outside its admission locks and rechecks source,
workspace, model and cancellation identity at final admission. Archive evidence
reserves space before ordinary material planning and its pack is stored as
provenance. Exact command replay precedes preparation. Project close, Focus,
suggestion opt-out and runtime exit cancel preparation; close/exit drain it.
Configuration and frozen-snapshot validation occurs inside the provider lease.

The integrated host has component tests; packaged native/model acceptance and
native Help presentation remain pending. Component success
does not authorize promoting the completion integration.

The native Friends Help entry reads the actual configured circle through a
bounded provider lease. It captures and revalidates the leased workspace owner,
shares cancellation/draining with preparation, and never opens the archive or
runs a model. Closed or replaced sessions cannot publish its result. The old
standalone suggestion IPC proposal had no frontend caller; it is not imported.

Transient draft writes cancel active preparation in that document's scope.
Final admission also compares the captured draft version and content claim,
so an unsaved edit cannot enter an already-prepared request. These archive
checks apply to opted-in context; projects without it retain their saved-source
command semantics.

# Threat model

## Boundary

The core accepts an immutable byte slice already granted by the embedding
application. A filename and declared media type are untrusted hints. Core
crates have no filesystem traversal, network, process, UI, speech, or model
authority.

The adversary controls every input byte and may construct polyglots, recursive
containers, forged lengths, invalid Unicode names, duplicate objects,
compression bombs, parser worst cases, prompt-injection text, and media that is
valid for one decoder but hostile to another.

## Security properties

- Content signatures and successful structural parsing outrank extensions and
  declared MIME types.
- Container traversal is iterative. One monotonic job budget covers every
  branch, including skipped, encrypted, malformed, duplicate, and rejected
  entries.
- Archive member names are provenance only. The core never joins them to a
  filesystem path and never materializes links, devices, pipes, or sockets.
- Objects are SHA-256 addressed. Duplicate bytes are analyzed once while all
  parent edges remain visible.
- Partial, truncated, encrypted, malformed, unsupported, and budget-exhausted
  coverage remains explicit. None can become a clean or complete result.
- Canonical text remains labeled `untrusted_attachment_data`; a downstream
  adapter must preserve that data boundary when constructing a prompt.
- Model and speech capabilities are resolved at dispatch time. Import success
  never implies that a selected model can consume the attachment directly.
- Optional transforms are requests, not ambient authority. OCR, transcription,
  video demux, frame decoding, and PDF rasterization execute only through an
  injected adapter with its own limits and receipts.

## Resource accounting

Before retaining or deriving content, implementations use checked arithmetic
and enforce absolute per-object and cumulative byte limits. Expansion ratios
are an additional signal, not a substitute for byte limits. Entry, edge,
object, name, depth, text, media, transform, and deadline limits are global to
one job and are never reset between nested containers. ZIP directory metadata
and XZ indexes are preflighted without entry-table allocation. Structured
parser input, XZ dictionaries, Zstandard windows, and image pixels have
independent ceilings because tiny inputs can otherwise request large internal
allocations before producing output.

A synchronous parser deadline is cooperative: it can stop between reads or
events but cannot preempt a parser stuck inside one call. Any decoder that
needs a hard CPU or wall-clock deadline belongs in a killable worker process,
not the core library.

The bounded 7z lane depends on `libarchive_oxide`'s native limits and typed
decoder errors. Its current public reader does not expose per-member compressed
sizes or parsed CRC values, so the adapter does not claim independent 7z ratio
or CRC verification. Absolute decoded/member/metadata/codec/in-flight limits
remain mandatory. RAR is not decoded in process because the audited candidates
did not expose adequate dictionary-memory and step controls.

## Non-goals

The first core does not claim malware-free verdicts, execute active content,
crack encrypted archives, repair malformed documents, follow document links,
fetch remote resources, or silently invoke a host executable. Malware scanning
can consume the same content-addressed graph through a separate, explicitly
authorized analysis adapter.

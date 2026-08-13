# Architecture

```text
caller-granted bytes
  -> content detector
  -> iterative object-graph inspector
  -> format processor registry
  -> canonical artifacts
  -> target capability planner
  -> direct media and/or explicit transform requests
```

## Deliberate decomposition

The attachment kit describes and prepares data. It does not execute inference,
speech, OCR, video decoding, remote fetches, or operating-system UI. This keeps
it reusable by Mom Llama, Free Token Energy, command-line tools, and other
Tauri applications without creating dependency cycles.

Applications grant bytes and own persistence. `llama-native-kit` reports exact
model/media capabilities and consumes approved image/audio inputs.
`speech-native-kit` may satisfy transcription requests. A future media worker
may satisfy video-frame, audio-demux, OCR, or PDF-rasterization requests.

## Object graph

Objects are content-addressed by SHA-256. Derivation edges retain every parent,
logical member name, depth, transform, and source range. Identical content is
analyzed once but may have many edges. The graph is deterministic and partial
coverage is explicit.

## Budget semantics

One monotonic ledger covers roots and every derived branch. It charges actual
bytes, declared bytes, objects, edges, enumerated entries, depth, retained
bytes, canonical text, media, and transformation attempts. Separate gates cap
input handed to structured parsers, container directory/index metadata, image
pixels, and codec history/dictionary windows. Budget is reserved before
allocation and reconciled against streamed output. It is never refunded in a
way an attacker can exploit.

Root depth is zero. An object at `max_depth` is analyzed but cannot derive more
children. This avoids both stack growth and the common off-by-one ambiguity.

## Trust boundary

Canonical text is always labeled untrusted attachment data. Filenames are
escaped display metadata. Provider/model adapters must encode content as data
parts or a strong system-delimited attachment section; raw attachment text is
never concatenated into the instruction channel without that boundary.

## Media dispatch

The planner consumes a concrete capability snapshot, not a generic
"multimodal" boolean. Exact accepted media types and families, direct PDF/video
support, and byte/object limits determine whether original bytes can be sent.
Otherwise the plan contains an explicit dependency DAG: for example, video
audio extraction precedes transcription, and frame sampling precedes OCR.
The attachment kit does not execute the DAG.

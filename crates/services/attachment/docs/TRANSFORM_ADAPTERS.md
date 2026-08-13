# Transform adapter contract

`attachment-native-plan` may request work that the authority-free core cannot
safely perform itself:

- image OCR;
- audio transcription;
- video audio demux and bounded frame sampling;
- PDF page rasterization;
- document extraction unavailable in the safe in-process processor set.

An embedding application may satisfy a request only after matching the exact
source object and artifact hashes. The adapter result must record its name,
version, policy fingerprint, output hashes, elapsed time, and whether process,
network, model, or hardware acceleration authority was used.

Adapters must enforce every `TransformLimits` field independently. Output is
untrusted attachment data. A failed, timed-out, cancelled, or truncated
transform returns a typed terminal result; it must not substitute an empty
artifact.

Recommended ownership:

- `speech-native-kit` satisfies local transcription requests.
- A sandboxed media worker satisfies video demux and frame sampling. General
  video decoding is intentionally not in the first-party safe core.
- A bounded OCR adapter may use a selected local vision model or a dedicated
  OCR engine. The receipt must distinguish those paths.
- Remote transforms require an explicit application privacy policy. They are
  never an automatic fallback from a local-only profile.

The attachment kit never depends on those implementations. This avoids making
format ingestion depend on provider routing, a GUI runtime, or one model
family.

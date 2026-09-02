# Mom Llama product contract

Mom Llama is a native, local-first llama.cpp chat application for private
personal use. It ports upstream `llama.cpp/tools/ui` intent into a Rust-owned
Tauri product and adds one general extension: any frozen conversation template
or live chat can be invited into an ordinary chat by its unique `@handle`.

## Product behavior

- Chat, generation, streaming and cancellation use the exact model profile of
  the active conversation.
- A Persona is a versioned conversation branch plus an execution profile:
  model, optional system message/projector, sampler, template policy, context
  budgets and allowlisted tools.
- Persona templates remain editable. Sending from one instantiates a normal
  chat; the template never silently accumulates ordinary traffic.
- Consult groups are ordered Settings records containing one to four Persona
  references. Selecting a group inserts its handle into the normal composer.
- Mention responses are ordinary attributed messages in the host conversation.
  They never write back to their source Persona/chat.
- User and assistant edits preserve message branches.
- Attachment previews are local and content-addressed: canonical text/PDF is
  escaped and bounded, admitted image/audio/video blobs use revocable object
  URLs, and stale root/artifact/policy identities fail closed.
- Alexandria is the one compiled external Information profile. A native picker
  yields only an opaque, one-time grant; registration preflights the exact
  schema, records immutable-read-only identity, remounts durably, and exposes
  only bounded local evidence and exact citation anchors to the renderer.
- Model use requires a separate process-local grant bound to one conversation,
  resource, release, representation, rights policy, and source SHA-256. Native
  chat receives a length- and SHA-bound untrusted evidence packet; a UI search
  alone is not described as model integration.
- “Add to Library” re-presents the exact Attachment root, graph, canonical
  artifact, processor policy, text bytes/hash, title, and explicit private-use
  rights through the Attachment-to-Information bridge. Information's active
  receipt and manifest are the sole discoverability authority; the managed copy
  can be searched, cited, and removed independently without changing its source.
- Product state, receipts and cache policy are Rust-owned.

The exact user-supplied therapeutic Persona templates are documented in
`PERSONA_LIBRARY.md`. Names and prompts are not rewritten into abstract labels,
and no consult group is seeded on the user's behalf.

## Dependency boundary

Mom Llama consumes the accepted imported monorepo packages for:

- `llama-native-kit` for in-process model execution and cache-safe native state;
- `speech-native-kit` only for the deliberate Apple complete-WAV Read Aloud and
  verified Parakeet attachment-transcription surfaces; microphone capture is
  not composed;
- `information-native-kit` for the exact Alexandria read-only profile and the
  promoted Attachment canonical-text materializer. Mom does not gain generic
  SQL, URL, directory, glob, archive-registration, or network authority.

Free Token Energy remains a separate product for protocol routing, hosted
providers, and optional loopback. Mom does not depend on its crates, install its
Tauri plugin, grant its renderer permission, or drain its gateway. It also does
not copy these implementations or retain their retired Git sources. The root
workspace and lock establish one Native/Attachment/Speech/contracts identity
for Mom.

## Frontend boundary

Rust owns authoritative state and rendered projections. A small local
JavaScript bridge performs token insertion, targeted DOM swaps, focus,
keyboard, clipboard, paste and drag/drop behavior. No frontend framework or
browser networking is required for core behavior.

The ordinary product surface follows the pinned upstream llama.cpp chat UI.
Mom's deliberate additions are the Persona menu and contextual actions attached
to the chat objects they operate on, such as Read Aloud, attachment
transcription, preview and Add to Library. Engine diagnostics, resident model
slots, legacy Skills and KV-cache policy/status/mutation remain typed backend
and CLI capabilities; they do not authorize settings cards, dashboards or
navigation in the ordinary UI. Model discovery and selection remain the single
user-facing model setup path.

## Release meaning

The app is not released until:

- `scripts/check-persona-product-ux.sh` passes from an empty store;
- the pinned upstream ledger has no unacknowledged required gaps;
- desktop/compact same-state visual comparisons pass;
- a real supported GGUF completes chat, Persona mention, four-Persona group,
  independent cancellation, synthesis, cache restart and persistence proofs;
- the signed native bundle passes human inspection;
- existing encrypted data opens through the retained compatibility identifiers;
- Mom accepts the product.

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
- Product state, receipts and cache policy are Rust-owned.

The exact user-supplied therapeutic Persona templates are documented in
`PERSONA_LIBRARY.md`. Names and prompts are not rewritten into abstract labels,
and no consult group is seeded on the user's behalf.

## Dependency boundary

Mom Llama consumes exact immutable Git revisions of:

- `llama-native-kit` for in-process model execution and cache-safe native state;
- Free Token Energy for protocol-neutral routing and the optional text gateway.

It does not copy either implementation. It currently installs only the FTE text
plugin. Speech backends remain in FTE until Mom Llama has a deliberate,
human-reviewed microphone/read-aloud product surface.

## Frontend boundary

Rust owns authoritative state and rendered projections. A small local
JavaScript bridge performs token insertion, targeted DOM swaps, focus,
keyboard, clipboard, paste and drag/drop behavior. No frontend framework or
browser networking is required for core behavior.

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

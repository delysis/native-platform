# Shared generation profiles and model execution limits

Compared source: native-platform `1cee70d53605ba3bfba6a479e15eda308066ad21`.
This implementation replaces duplicated product sampling construction with
`desktop-generation-policy`, consumed by Mom chat/Persona/composer/tool paths and
Loom Weave/terminal paths. It is a code integration, not model-quality tuning or
native GUI acceptance.

## What each implementation contributed

| Concern | Mom implementation | Loom implementation | Shared decision |
|---|---|---|---|
| Persistent choices | Encrypted settings, frozen per-conversation sampling, explicit custom override precedence in `mom-llama-runtime/src/config.rs` and `conversation_store.rs` | Immutable generation records bind exact sampling fingerprints and deterministic branch seeds in `tauri-plugin-loom/src/lib.rs` | Keep product storage/identity ownership. Resolve one typed sampling configuration; validate frozen profiles too. |
| Defaults | Native defaults with chat output limit 512; a second hand-written field list repeated native defaults | Explicit prose/verse/manual cases; prose min-p 0.05 and disabled prompt-history penalties following a concrete degeneration repair | One `GenerationTask` default projection from native defaults. Preserve all existing Loom V2 exact-bit fingerprints and Mom chat values. No claim that these are empirically optimal. |
| Override parsing | UI values and `customJson`; unsupported keys already have explicit authority blockers, but numeric parsing and sampler names could silently disappear | No equivalent global inherited string map; requests have typed policies | Typed `SamplingOverrides`; UI numeric text accepted; invalid types, range errors, unknown stages, duplicate stages, and integer overflow reject the entire request. Product allowlists still authorize exposed controls. |
| Validation | Settings writes could persist malformed controls that silently fell back at generation | Weave validates its policy, while native controlled-generation already has comprehensive bounded sampling validation | Expose the existing native sampling validator and reuse it. Validate the baseline and every override layer, not just the final layer. Reject before settings persistence. |
| Prompt semantics | Typed role conversation and frozen chat-template policy | Raw and instruct writing adapters, plus raw terminal chat stop strings | Remain separate from sampling and workspace templates. Shared sampling does not flatten prompts or grant tools. |
| Context request | Explicit fixed context, batch, and parallel count; invalid values could be clamped without a receipt | Memory-aware power-of-two default context; actual native descriptor used for prompt packing | Shared `ModelExecutionLimits` validates explicit requests without rewriting them. Native resident descriptor remains actual-allocation authority. |
| Automatic context | No automatic context resizing of a user's explicit setting | Model/projector bytes, physical headroom and immutable host budget; conservative Gemma KV estimate | Extract authority-free `ContextMemoryBudget` and `ContextPlan`. Keep the model-family estimate explicit, and reject an unaffordable minimum instead of returning 512 as if it fitted. |
| Host admission | Explicit manual-vs-auto memory budget, half physical RAM bounded to 2–64 GiB with 8 GiB fallback | Immutable host budget; final resident admission and exact worker identity | Extract the bounded desktop auto-budget policy. Both still use the existing native host for actual admission; no second allocator or model residency registry. |

## Removed/replaced code

- Mom's full copied `SamplingConfig` field constructor, permissive sampler-name
  filter, and independent custom-JSON numeric mutation block.
- Loom's copied sampler-chain/default-field constructor.
- Mom's duplicate bounded physical-memory-budget function.
- Loom's independent automatic-context arithmetic; a small adapter now supplies
  its explicit model-family estimate to the shared plan.
- Silent context/batch/parallel clamps at the product configuration boundary.

The new shared crate adds typed parsing, validation, explicit estimate records,
and boundary tests. This first slice is a net code addition rather than an honest
claim of immediate line-count reduction. It establishes one implementation for
future callers instead of extracting two adapters that retain the old logic.

## Behavioral boundaries and deliberate changes

A valid existing task profile remains bit-for-bit unchanged. An invalid supplied
sampler is no longer silently replaced by a default. The update returns a typed
blocker, and the previous encrypted settings remain unchanged. Direct settings
saves and Persona sampling updates also validate before opening/creating the
store or attempting model access. Invalid persisted profiles
remain readable so the settings screen and reset command can repair them, but
generation resolution rejects them. Reading does not rewrite stored data or
invent a migration.

Native ordinary sampling and controlled generation now share the same published
validation entry point. This does not change native sampler execution or claim
that requested sampler stages execute in greedy mode; native receipts retain
that distinction. No new UI knobs, empirical profiles, control vectors, or power
sampling algorithms are added.

Loom automatic context planning can now fail before model load when its own
conservative estimate cannot fit 512 cells. That failure occurs before taking
the previous model out of the registry. The shared estimate is not a measured
allocation, universal KV formula, or guarantee against memory pressure. The
native host still checks its metadata-aware admission estimate and other resident
models. Mom's explicit context is not silently resized using Loom's Gemma
heuristic.

Loom context planning still considers the current physical memory snapshot; it
does not yet subtract only marginal allocation on replacement or derive all KV
layouts from GGUF metadata. Native model descriptors can cap requested context.
The `ContextPlan` makes requested maximum, selected size, and heuristic cost
inspectable for callers; a new persistent UI receipt was not invented here.

## Validation

Focused verification is recorded with the implementation commit. Boundary tests
cover malformed settings, custom-layer precedence, hidden invalid earlier layers,
unknown and duplicate sampler stages, frozen invalid profiles, no store creation
on invalid input, preservation of stored settings after rejected updates,
context-budget exhaustion/overflow, exact requested-limit preservation, and
existing Loom V2 fingerprints. No real-model generation or GUI behavior has been
accepted by these tests.

Verification before integration: shared policy 8/8; Mom runtime unit suite 175/175;
Loom context tests 4/4; Loom sampling/terminal replay tests 3/3; three product
packages compile with `--tests`; CI metadata selection 9/9. The final Persona
preflight regression is additionally included for the integration gate. No
real-model or GUI acceptance was run.

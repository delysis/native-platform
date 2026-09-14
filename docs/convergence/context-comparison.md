# Context and attachment convergence

Baseline: `1cee70d53605ba3bfba6a479e15eda308066ad21`. This change is implemented in both product call paths. It does not claim that the frontends or model prompt compilers have converged.

## Paired implementations and judgment

| Concern | Mama Llama | Loom | Replacement decision |
| --- | --- | --- | --- |
| Source ownership | Encrypted content-addressed blob/manifest store, conversation ownership and staged/committed states; active-branch selection; aggregate admission limits | Project-local immutable attachment bytes, editable context document, selection references and generation receipts | Keep storage authorization at each store boundary; use the existing shared attachment service for parsing |
| Context selection | Whole canonical extracted text; explicit untrusted-source framing; limited total bytes | Opening/query/end selection, UTF-8-safe chunking, source-text and slice digests, exact byte offsets | Shared `desktop-context`: Loom selection/evidence with Mama Llama framing and aggregate bounds |
| Partial extraction | Previously rejected all partial extraction even when useful canonical text existed | Retained useful text with source coverage | Both can use validated partial text; label coverage; partial extraction never authorizes native media |
| Source validation | Strong preview validation, weaker generation validation | Content-addressed persisted artifacts and exact excerpt receipts | Mama Llama generation now uses the same graph, source, artifact, coverage, identity and accounting checks as preview |
| Small context budgets | Whole source or reject | Chunks could all be larger than the budget, yielding no excerpts | Preserve a final exact UTF-8 prefix of the highest-priority deferred chunk when it fits; account for every framing byte |
| Authored instructions | Explicit system/persona fields | Deliberately edited co-writer context | Keep separate from untrusted extracted data; text markers alone are not an injection-security boundary |
| Direct media | Validated image input; audio requires transcription; video requires a configured pipeline | Selected-model native image/audio capabilities, explicit projector binding | Existing native and attachment service contracts remain the shared basis. This slice does not make Mom's media path equivalent to Loom's |

## Actual consumers

- `products/mom/crates/mom-llama-runtime/src/attachments.rs`: `prepare_chat_attachments` selects a query from the current user turn, apportions its aggregate canonical-text allowance across references, validates the retained manifest, and calls shared `render_source`.
- `products/loom/crates/tauri-plugin-loom/src/context_attachments.rs`: `assemble_generation_context` calls that same renderer and preserves its returned excerpt evidence in Loom's receipt path. Local copies of excerpt selection, ranking, chunking, hashing and trailing UTF-8 selection were removed.
- `crates/desktop-context/src/lib.rs` owns no files, stores, network clients, tokenizer, model, or capability grants. Inputs are already authorized borrowed sources; outputs are bounded derived text and exact evidence.

The source-text digest names the canonical extracted text. The root digest names original input. They are intentionally different when extraction or editing changes the text. Selected ranges refer to canonical text, and markers/separators are derived presentation bytes, never attributed to the original source.

## Verification and remaining work

Core tests exercise exact Unicode and CRLF slices, chunk reconstruction, tiny budgets, escaped metadata, and framing-inclusive limits. Store-boundary regressions prove that validated partial text enters chat with explicit coverage and a mismatched source identity is rejected. Existing media, attachment ownership, aggregate-limit, branch, cancellation, transaction rollback and garbage-collection tests remain relevant and are run alongside these additions. A pre-existing test relabelled PNG artifacts as audio/video; stronger validation now correctly rejects those malformed artifacts before format admission.

Remaining: one tokenizer-backed context planner must budget rendered chat templates, writer prefixes/suffixes, reserved output, selected sequences and media tokens against the actual allocated context. Mom still has a 2 MiB aggregate text safety limit; Loom still uses a conservative byte estimate before native admission. These are resource ceilings, not exact tokenizer measurements. A selected source that cannot fit needs explicit omission presentation. Native-media admission, context receipts and source-navigation UI still need one shared product path. This change does not establish an optimal retrieval policy or model-quality result.

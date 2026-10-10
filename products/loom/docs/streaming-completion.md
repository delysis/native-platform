# Streaming completion contract

Preview length and acceptance length are separate. Ghost mode renders the entire
currently authorized, editor-safe prefix, in both Visual and Source. Later
native deltas grow that preview. A word acceptance inserts one exact prefix;
it must neither hide the remaining preview nor stop its stream. Visual's existing
canonical-prose projection still applies; Source owns multiline Markdown.

Loompad suppresses inline Ghost but renders four next-word choices with their
streaming continuations. Emphasize the exact prefix that a W/A/S/D action accepts;
keep the tail visible, preserve whitespace, and preserve run-to-key mappings as
tails grow. Never reorder a published choice merely because a different stream
advanced first. The overlay must not resize or write to the manuscript.

Acceptance freezes the already-authorized bytes and identities, not the length
of future output. A retained run may append under a new presentation identity
only if its entire old text remains an exact prefix. It may not rewrite, truncate,
reuse an old presentation identity, revive a retired policy, or update an obsolete
session. Do not change authority while an insertion is pending. Unconsume removes
only the exact last accepted chunk; ordinary editor undo remains a different action.

A matching presentation key is insufficient evidence of a rendered preview.
The actual connected, visible DOM text must equal the authorized prefix before
inline acceptance. Offscreen-caret, focus, composition, canonical Markdown,
grapheme, model, document, revision and family checks remain in force.

## Executable regressions

The normal frontend unit suite includes `completionStreaming.test.ts`. It exercises
production controller transitions for append-after-acceptance, immutable prior
bytes, exact unconsume, stale sessions, pending insertions and retired policies.

The normal WebKit suite includes `completionController.browser.test.ts`,
`editorInteractions.browser.test.ts`, and `loompadStreaming.browser.test.ts`.
They must assert actual DOM text from multiple successive multiword frames,
not a status label or the length of a hidden candidate. Controller/editor fixtures
are component evidence, never native inference evidence. Harness counters count
regeneration intents, not backend generation runs.

Mount the real editor and establish its caret before admitting a fixture family.
Assert that the displayed stream is bound to a controller session and that each
transport update actually changes that session. Revoking a scope must retire
both its session and its fixture source; fallback text must not mask a missing
session or resurrect a revoked run. Keep DOM locators aligned with the complete
preview, and include missing-text, stale-key and offscreen negative witnesses.

Before changing these expectations, reproduce the intended defect. A one-word
render cap, a blocked accepted stream, and acceptance of truncated DOM text must
each fail their corresponding browser/controller regression. Do not replace the
full-prefix expectation with a first-word expectation to make a build green.

## Automatic candidate admission

`automatic_v3` opts into the native `distinct_plain_text_v3` first-word policy.
The native worker withholds each proposal until it has both a complete lexical
first word and proof that its leading non-whitespace bytes are not a completed
angle-bracket markup construct. A completed markup-led proposal is recorded as
`disallowed_prefix`, charged to the immutable attempt ledger, discarded before
emission, and retried from the exact prompt with the policy's deterministic retry
seed. Incomplete markup remains withheld. The existing token and attempt bounds
still fail closed; exhaustion never releases rejected bytes.

`automatic_v2` remains unchanged for receipt compatibility. Automatic V3 cannot
fall back to a server path that cannot return the native admission evidence.

## Native promotion boundary

A clean build, four completed runs, events, Metal initialization and green
component tests do not satisfy writing acceptance. Run the pinned artifact and
approved model through both Visual and Source journeys, including a real
pre-terminal rendered prefix, further streaming growth, unchanged-manuscript
cycling, stable Loompad mappings, exact acceptance/unconsume/undo, stale-scope
invalidation, joined active-work shutdown and same-project relaunch.

Do not waive a failed pre-terminal witness by renaming it an overly strict smoke.
A failed or unavailable native journey blocks product acceptance and promotion.
Preserve its raw observations. The receipt must distinguish generation, frontend
projection, rendered visibility and byte correlation, and identify the exact
source tree, executable and model actually exercised.

## Speculation budget

Four current continuation streams cost four live paths, not an exhaustive tree.
Precomputing four choices at every possible prefix through depth `d` requires
`4^d` leaves (`4 + 16 + ... + 4^d` non-root nodes). Those are distinct guarantees.
Use bounded lookahead, shared-prefix reuse, and pruning after the author's choice;
never postpone the current preview until an exponential subtree is complete.
A four-run refill is not evidence of four immediately available children at every
deeper prefix. No such exhaustive scheduler is supplied by the preview repair.

Visual automatic V4 withholds leading line breaks and markup before native emission.
Rejected proposals remain in the bounded attempt ledger; numeric prose and literal
`<3 forever` remain eligible. Source uses its existing automatic V2 policy and
retains multiline Markdown. V3 records keep their original policy semantics.
The captured editor mode fixes the request policy across an uncertain-command retry.

# Chat Pro handoff prompt — September 17, 2026

**Submission blocked before Send.** Job
`loom-easl-handoff-ec2fa72c-20260917-01` is `needs_attention`:
`Model selector was not found; page layout needs an adapter update`.
No Chat Pro response or completed delegation is claimed. Chat mode remains
mandatory; Work mode and a model downgrade are forbidden. Do not create a new
job ID or assume a failed acknowledgement is permission to resend. Preserve
any staged draft/attachments. The installed adapter needs a source-grounded
compatibility repair and a fresh safe submission procedure.

The native messaging host and Chrome extension were installed and connected.
Chrome initially rejected top-level await in the service worker; the installed
worker now starts that read through a Promise callback. The Chat/Work/Pro gates
were not changed. This installation fix is local to the plugin cache, not a
Loom code change or an upstream plugin release.

Source implementation checkpoint: `ec2fa72c59c667f85e83543f7bf00b22332e042f`.
Tested experimental checkpoint: `78392f82762193b3c3f107d981c280f15d727b5d`.
The source archive is a curated 1,639-file UTF-8 source subset (3,184,517 bytes),
SHA-256 `e759491cf516a359eacba00807562a96ba64b2fda9d82d152d08b4515549f08c`.
Local packet and machine-readable attempt receipt are under
`target/easl-integration/chat-pro-handoff/`; no private manuscripts, credentials,
Git history or removed Studio source are included. The packet is a review subset,
not a complete buildable monorepo.

## Prepared prompt

You are taking over George's native EASL Loom frontend experiment. Use ordinary ChatGPT Pro in CHAT MODE ONLY throughout. Never switch to Work mode, launch a Codex/cloud task, use a paid API fallback, create a recurring scheduler, or silently delegate. George explicitly stopped the local coding agent after roughly a day and a half and asked for the work to be pushed and handed to you. Do not restart that agent or assume authority over his desktop.

Your objective is a usable native EASL replacement view for Loom, with original/current interface fidelity and reusable excellent EASL text rendering/editing. Ultimately it should coexist with a Tauri/platform webview without being that webview, preserve all existing native-kit service authority, and meet or exceed the specified Pretext layout behavior and robustness. George wants beautiful flexible typography and a real benefit to the EASL ecosystem, not only a product-specific Rust editor. The project is not finished. Be candid, prioritize a working current-interface vertical slice, and avoid another architecture-only detour.

SOURCE OF TRUTH
Repository: private https://github.com/delysis/native-platform . Do not assume your chat can access it; the attached curated source archive is authoritative for this handoff and has a manifest of exact Git blob IDs.
- Tested experimental publication branch codex/loom-easl-native: 78392f82762193b3c3f107d981c280f15d727b5d. This is usable for experimental text editing, NOT a dependable/full Loom replacement.
- Latest work, pushed separately as deliberately unfinished integration: codex/loom-easl-main-integration at ec2fa72c59c667f85e83543f7bf00b22332e042f. It merges the tested checkpoint with main c0bcc42d8e274c9f428d3e19508709d7fa6e23ae. It has known test-build failures and stale native UI reference pins. Do not call it runnable/green. Both remote SHAs were verified; working tree clean on handoff.
- Tested publication parents: reviewed main 90349a54061790954cf8a88160e4e29a8a325d4d and Mine PR47 a43231626f5deb4ad8f6f08beb36dca40a236270.
- CRITICAL: Never push codex/loom-easl-current, historical local refs, or all refs. That unpublished development ancestry contained Studio. The clean tested publication deliberately excluded it. No Studio source is in either published checkpoint. No force push or remote deletion was done. Do not treat metadata naming a removed import as permission to reintroduce it.

READ FIRST (archive paths relative to repo)
1. AGENTS.md; products/loom/AGENTS.md; docs/browser-development.md.
2. products/loom/experiments/easl-interface/HANDOFF-MAIN-INTEGRATION-2026-09-17.md: exact stopped state, four errors, merge semantics, next steps.
3. products/loom/experiments/easl-interface/HANDOFF-2026-09-17.md: full 23-item prioritized TODO inventory and architecture map. All unfinished items remain open.
4. Same folder PARITY.md, REFERENCE-AUDIT.md, PERFORMANCE.md, VERIFICATION-2026-09-17.json, then historical integration/focus reports with their explicit evidence limits.
5. crates/services/easl/TEXT-LIBRARY.md and easl-text/easl-native-text READMEs, including bounded APIs and limitations.
Do not conflate different source revisions or repeat old progress claims as present acceptance.

NONNEGOTIABLE REQUIREMENTS
- NO STUDIO CODE anywhere in the stack. No Studio application, Cast, Hollow, or easl-native-host import, dependency, copied snippet, or published ancestry. Retain only the allowed compiler/runtime and independent text components. The old build target directory happens to contain 'easl-studio' in its name; it is only a cache.
- Current Loom UI is the spec. The user repeatedly rejected the invented/old titlebar and missing buttons. Read actual App.svelte/CSS and SVGs from the reviewed integration. Do not reproduce old screenshots as current spec or just update hash pins to silence the guard.
- Native authority in safe idiomatic Rust, narrow typed interfaces, explicit state machines, bounded work and failure atomicity. App/config/storage/model/acquisition/credentials/provenance decisions stay in modular native-kit/Rust services; EASL owns reusable text/edit/layout policy and view behavior.
- Ella's high-priority direction: an EASL library is desirable; narrow compiler/native font-loading and rasterization primitives can load a TTF, rasterize glyphs with Swash at chosen resolution, expose immutable Texture2D data; additional generic input primitives are legitimate. Complex shaping/Unicode may use narrowly scoped native primitives, but the deliverable must remain an ergonomic reusable EASL text/editor library. Explain this boundary in concrete code, not rhetoric.
- Preserve ordinary authored UTF-8/CRLF bytes, source/revision identity, immutable history/receipts, author control, explicit promotion/undo, shared project/config ownership and encryption. Invalid config must reject, not silently fall back or rewrite. Never restore a plaintext file escape to satisfy an obsolete test.
- No foreground app/window fixtures, unrelated desktop automation, or editing real manuscripts. Native acceptance needs explicit later foreground authorization. No subagents. No vendor workspace membership/lockfiles. One Cargo at a time; optimized native-view; focused changed-boundary checks, then a single consolidated gate. No broad local rebuild merely for appearances.

WHAT EXISTS
The standalone Rust native host executes ui/loom.easl display-list geometry/controls. Source-accurate loom-markdown transactions support headings, emphasis, links, quotes, nested lists, code, rules and undo. loom-text-session owns async project/session/source admission/config capture. loom-config/store/document/types and desktop vault own configuration and persistence. AccessKit, native focus/selection, pane-local caret/scroll/layout and optional Wry child coexistence are present but not fully OS-qualified.
The EASL library has paragraph composition, edit/history/preedit, navigation/hit testing, rich style, bidi/run planning, fallback, glyph placement, page selection and viewport policies, with native shaping/font/atlas primitives. Loom already uses several EASL policies BUT STILL uses Parley line/caret layout and Vello CPU rasterization. The standalone atlas widget is not yet integrated into the production Loom paint/line/caret path. Do not claim that goal already achieved.
The tested checkpoint finished the exact-source formatting/link palette including independent single-line destination input, hscroll, undo, pointer routing and IME ownership. Width-only reflow retains shapes. Earlier synthetic 175KB resize median improved 62.672ms to 1.993ms, EXCLUDING paint/input. The initial extreme slowdown involved unoptimized CPU rasterization/unnecessary repaint. No current end-to-end p95 typing claim exists.
279 tests in 29 suites passed on the tested checkpoint, with 2 manual tests excluded; owned strict Clippy, doctests, policy, formatting and six offscreen appearance checks passed. Those are component receipts, not packaged OS acceptance, not full current Loom parity, and not the integration's results.

EXACT CURRENT FAILURE / FIRST PATCH
The main-integration merge resolves ten textual conflict files; auto-merged code also needs semantic review. Current main adds materials/import/inference/function recipes; the experiment contains Mine shared config, frozen persona/sampling and protected storage. The merge attempts to preserve both.
New FunctionFormat/FunctionSettings are in loom-config. loom-text-session::configuration now captures immutable exact config source and FunctionRecipe; unregistered Mine config retains bytes/digest without invented document identity. Tauri delegates. Generation evidence attempts to retain both frozen profiles and material plans. Terminal receipts retain function recipe + persona/profile/context. Three function_configuration tests were added but have NOT RUN.
The existing check finished exit101 (Rust1.92, macOS arm64):
cargo check --offline --locked --profile native-view -p loom-config -p loom-text-session -p tauri-plugin-loom --tests
Four E0425s for missing context_attachments::original_path:
- tauri-plugin-loom/src/import_batch.rs:512 and :581
- tauri-plugin-loom/src/materials.rs:1214
- tauri-plugin-loom/src/context_attachments.rs:2303
The implementation has original_bytes after protected-storage changes. Determine the correct boundary for assertions; do NOT merely expose decrypted originals through a plaintext pathname. Repair the fixtures/service contract narrowly and include accepting/rejecting encrypted-storage evidence. More failures may surface. cargo fmt and whitespace checks passed. No frontend check, new tests, Clippy, native-view gate or runtime tests ran on this merge.
The native view's build guard still pins the OLD reviewed App/CSS and will intentionally reject new files. Main now has Add menu (New document/Add files/Open library/Connect sources), main pane collapse/show, and one conditional Ghost/Loompad button. The EASL view and SVG extraction still need those changes. The current main reference is not the old EASL-labelled titlebar. Preserve all Mine additions while porting.

REMAINING WORK (full details in 23-item handoff; do not silently drop any)
P0: reconcile latest source/interface and service merge; identified signed macOS bundle; real keyboard/pointer/focus/splitter/palette and late-IME sequences; signed encrypted save/quit/relaunch/recovery/leases; native/Wry focus/accessibility; reusable native-surface/Tauri API and platform qualification.
P1 application: complete document lifecycle/export/watch/recovery/large folders; outline navigation; chat/terminal/page-preview panes; shared settings/themes/errors/session orchestration; model loading/download/cancel; real Ghost/Loompad streaming/promotion/undo; materials/import/media; speech controls; unsupported Markdown representability/mark continuity and exact-source roundtrips.
P1 library: integrate EASL contextual/rich/atlas widget into actual Loom line/caret/painter; real font discovery/fallback/color emoji/multipage eviction; tabs/hyphens/bidi/ligature caret edge cases; retained incremental paragraph caches/clear resource limits; generic input/clipboard/IME/accessibility; module/API/compiler startup ergonomics. Current paragraph/document capacities are bounded and must not be described as unlimited.
P2: differential browser Pretext at cc8619ad5d856190925951545965911458abe29d (existing corpus is native invariants, not geometry equivalence); dictionary hyphenation/full Knuth-Plass/optical margins/microtypography before TeX-quality claims; startup/layout/typing/selection/scroll/resize p50/p95 and cache/allocation/boundary costs; fuzz/property/failure-atomicity; final independently qualified library + actual current Loom acceptance.
Pretext reference: https://github.com/chenglou/pretext . Source may be unavailable in the packet: state that and inspect the pinned source if accessible, do not invent its behavior.

REQUESTED OUTPUT / HOW TO BEGIN
Begin by checking packet provenance and reading the two handoffs plus changed source. State succinctly what is usable, what is broken, and the narrow first acceptance milestone. Then perform substantial source-grounded review and produce a bounded concrete first patch (unified diff against ec2fa72c, with exact paths) addressing the observed build boundary and preserving shared config/provenance/encryption. Include meaningful tests and a current-UI port plan derived from the actual App/CSS, with a clearly prioritized continuation checklist covering all remaining obligations. If runtime/repository tools are available and authorized, validate; otherwise label checks as proposed/not run. Return patch artifacts rather than pretending local files or GitHub were modified. No huge speculative rewrite or token-consuming architecture survey. Give an ergonomic EASL library path, ownership table, risks and exact next commands. If no repository access exists, use the attached code instead of blocking at that fact; identify precisely which missing files block further proof.
The source zip is a curated subset, not a buildable full monorepo; its manifest explains omitted binary fonts/assets/generated data/unrelated services. Root Cargo.lock and product/renderer/compiler sources are included for source review. Consult GitHub at the exact commit for missing transitive dependencies if available. Do not report 'all tests pass' without executing them on a full matching checkout.

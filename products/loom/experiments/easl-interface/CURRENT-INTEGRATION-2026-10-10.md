# Current integration reference

The current renderer reference is native-platform `554267979ab3be12044af645ba8dabb4a304f863` (tree `56fa0140ac93f8996e5a0918499ac81328b16c94`), reviewed against the settled D08 source `d08b3c14fc9d28210f2897b938b3ae3bf63fd9f4`. EASL remains pinned to `6d7c913a5b8d6874ab56f5168d222dc95964afb9`. This reference review is source-only; no new component run, native parity or packaged acceptance is claimed.

The default native build still refuses a mismatched whole App or CSS before asset generation. The reviewed App SHA-256 is `c6d54342254e0e6ab235df57c461759369d9a8e6e155708999490fb30f7e0287`; CSS remains `83a038c05a2e36106024c9d9d9f41760f54adbf3db5ac73b20758536907e024c`. The seven additions and two removals in App affect persistence owner lifetime only. All markup, nine semantic SVG alternatives and selector markers are byte-identical to D08; the native view, asset extractor and worker bodies are unchanged.

Renderer disposal now invalidates its epoch and refuses follow-up draft/checkpoint admission after an accepted draft finishes. The native adapter already owns one flight/ticket, accepts acknowledged baselines without replacing newer typing, waits for accepted work before a transition, and stops admission after Close. The worker drains accepted requests and joins when its client owner drops. No native implementation change is required for this narrow lifetime rule. Drop alone does not capture undispatched dirty text or make forced process termination a graceful save.

Retain the native scheduler's existing 750 ms journal and 900 ms idle checkpoint policy, explicit retry after a failed batch, and bounded 16-document/1 MiB text policy. Inference, recording, cancellation, promotion, complete workspace/material/model adapters, OS IME/VoiceOver, shutdown/relaunch and full frontend parity remain incomplete or unqualified. The existing native failures and prior source/component receipts remain separate. This new reference still requires independent review and a released exact-tree component gate.

## Earlier integration provenance (retained)

This candidate combines native-platform main `a7248566b5012e32810584725af9e4574699c478` with collections `0c4677b343af0d144df6cb4f36260e42a9932385`. EASL is consumed from its canonical repository at `6d7c913a5b8d6874ab56f5168d222dc95964afb9`, with the same Parley and Harfrust revisions. There is no separately editable service copy here.

The native experiment retains its exact source-hash refusal: App `79d5fe0fd5095fa75d266be8feb4791ed416228b8969353c4909fd15f3069f8d`, CSS `a153e13ad1ee00c84d2e02b31fa318d4af556e7f1fe3b3c581ee36bf58dbb81f`. All nine extracted semantic SVG assets match the earlier reviewed reference byte for byte. These hashes bind source and assets; they establish neither frontend parity nor packaged acceptance.

The experiment supplies source/semantic editing, formatting, pane routing and authored-byte behavior. It does not implement current Tauri workspace ownership, collections, material/PDF operations, archive context, model controls, inference, recording, cancellation or promotion. Its Ghost/Loompad icons confer no such authority. The retained Mine consumer is experimental.

The unchanged EASL service/adapter checkpoint passed 297 service and 120 adapter tests; its two registered rendering/performance tests remain unrun. Current combined qualification is recorded in external exact-tree receipts. No hosted CI, physical IME/VoiceOver, ordinary Loom replacement, frontend parity or distribution acceptance is claimed.

Native scope diagnostics excluded a caret/revision/blob/intent/edit mismatch. Visual Loompad now requests prose-safe native admission and checks the saved batch policy before presentation. Temporary diagnostics are removed; their exact builds and failed outputs remain preserved. Packaged-native qualification remains open until the repaired exact artifact passes.

Earlier audit and handoff files are historical evidence and proposals, not user instructions or current qualification. Failed reference, compilation, encrypted-storage and native receipts remain preserved externally.

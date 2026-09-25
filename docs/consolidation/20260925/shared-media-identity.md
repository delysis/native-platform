# Shared retained-media identity

Base: 6d814887c6be7117bdf6b6a7fcd461462f6b5283 (PR #100).

The document and conversation consumers now use the same bounded MediaIdentityLedger. It verifies every occurrence's exact bytes before deduplication, rejects identity/MIME conflicts, preserves first-payload order, and independently accounts for occurrences, distinct payloads, payload bytes and checksum work. This is identity bookkeeping, not a decoded-media proof or a source/network grant. Native capability and Attachment validation remain necessary.

Mom previously skipped repeated content-addressed media before verification, then classified an image-only duplicate as having no representation. Its actual prepare_chat_attachments consumer now retains the repeated source IDs while sharing the payload and recognizing that the duplicate has a representation. Prompt preparation also reuses its existing preview graph/artifact/accounting validation instead of accepting a weaker manifest.

Mom retains its stricter 16-media-object and 32-attachment-occurrence limits. Loom retains 32 media payloads / 128 MiB. Both share bounded identity validation; equal bytes of different modalities are not silently collapsed. Checksum work additionally counts duplicates and is limited to 33 complete 128 MiB source budgets. No source record, attachment original, citation, permission, runtime owner, database schema or persisted format is converted.

Audio remains transcription-first in the existing Mom path; Loom's previously admitted recorded-WAV path remains independent. A good checksum does not authorize a codec or erase a required video/PDF/OCR transform. This stage does not claim consult fan-out, a unified frontend, or native product acceptance.

## Qualification

New Rust regressions: seven identity-ledger tests, three tests through Mom's real retained Attachment preparation (including decoded PNG fixtures). Existing Loom media and Mom Attachment regressions are retained.

Run with the pinned toolchain, serialize Cargo work:

```sh
cargo fmt --all
cargo test --locked -p llama-native-types media_identity::tests
cargo test --locked -p mom-llama-runtime attachments::
cargo test --locked -p tauri-plugin-loom terminal_media::
cargo clippy --locked -p llama-native-types -p mom-llama-runtime -p tauri-plugin-loom --all-targets -- -D warnings
```

Rust compilation, Rust tests, formatting and Clippy were not executed in the authoring environment, which has no Rust toolchain. No fixture establishes model or packaged-native acceptance. The two new source files were saved locally and their Git blob identities matched the published objects. Existing large-file edits were reconstructed against exact full source through PR #101's scratch merge preview, then copied as complete blobs into this ordinary single-parent product commit. Scratch ancestry is not imported, and PR #101 is not a product branch to merge.

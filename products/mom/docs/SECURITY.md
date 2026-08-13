# Local data security boundary

Mom Llama encrypts conversations, Skills, attachments, receipts, settings, and
persistent llama.cpp sequence state with XChaCha20-Poly1305 before SQLite sees
them. Each record has a fresh random nonce and binds its namespace as
authenticated associated data. The confidentiality boundary depends on how the
encryption key is provisioned.

Secure and release builds use a random 32-byte installation key stored in the
user's macOS Keychain. In that mode, copied database files and file-only backups
are unreadable without the installation key, and authenticated encryption
detects record substitution.

Debug builds default to a separate `mom-llama-development` data directory and a
predictable path-derived key so rapid iteration does not trigger Keychain
authorization prompts. This preserves the production record format and detects
accidental corruption, but it is **not a confidentiality boundary**: anyone who
has the source and knows the data path can derive the development key. Set
`LLAMA_NATIVE_KIT_SECURE_STORAGE=1` to exercise Keychain-backed storage in a
debug build.

In secure mode, the installation key is stored in the user's Keychain.
The runtime attempts to resolve it at most once per data directory during a
process launch and keeps either the key or the denial/error only in memory. This
avoids repeated Keychain access dialogs when a development-signed app opens the
store many times or access is denied. A rebuilt unsigned or ad-hoc-signed
development app can still receive one macOS authorization prompt; after denial,
the user must relaunch to retry. A stable signed release is the final
distribution boundary for that behavior.

Automated tests and isolated CLI proofs can set
`LLAMA_NATIVE_KIT_STORE_KEY_HEX`. That explicit key always takes precedence.
The in-process test data-directory override also selects a deterministic test
key. `LLAMA_NATIVE_KIT_DATA_DIR` changes the storage location but does not, by
itself, select the test-only key; a debug build still uses its documented
development-key policy unless secure storage is explicitly enabled.

## What this does not claim

Secure-mode encryption does not protect content from someone who controls the
running app, the unlocked macOS account, or the model output itself. Debug-mode
encryption additionally does not protect copied files from someone who can
derive the development key. Neither mode is a HIPAA compliance claim or a
substitute for full-disk encryption, account security, signed distribution, or
careful exports. Secure mode's narrow purpose is to keep app-owned private
records and model-state caches authenticated and unreadable when SQLite files
are copied or inspected without the installation key.

## Native prefix-cache boundary

The cache manager owns three cache tiers:

- a bounded 256 MiB in-process LRU;
- encrypted per-conversation session checkpoints;
- encrypted persistent persona/Skill prompt packs.

The llama.cpp worker also retains live sequence state while a model is resident,
but that engine context is not advertised as a fourth managed cache tier.
Persistent cache storage is globally bounded to 64 entries and 2 GiB. Reuse
requires exact token-prefix agreement plus model, binding/build, tokenizer,
chat-template, projector, LoRA, context, batch, sequence-count, device, RoPE,
and KV-layout fingerprints. Missing, malformed, unauthenticated, or rejected
state is invalidated and ordinary uncached generation continues.

The runtime cache mode is authoritative across all three tiers:

- **Automatic** (the new-install default) permits reusable Persona/Skill prefix
  packs and writes a bounded checkpoint for the active conversation after a
  successful response.
- **Prefixes only** permits reusable Persona/Skill prefix packs but does not
  write conversation checkpoints.
- **Off** performs no cache lookup, creation, memory promotion, manual restore,
  or checkpoint write. It does not delete existing encrypted entries; clearing
  them remains a separate explicit action.

A Skill's own cache policy can further prevent creation of a stable Skill
prefix pack, but it cannot override the global mode. Edits change the Skill
owner fingerprint, so stale packs cannot satisfy the new prompt. The fixed
256 MiB memory ceiling, 64-entry/2 GiB persistent ceiling, fingerprint set, and
uncached fallback are compiled safety policy rather than user-tunable settings.

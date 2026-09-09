# Mom Llama

Mom Llama is the canonical native, local-first chat product in the
`delysis/native-platform` monorepo. The `products/mom` boundary owns its Rust
product runtime, CLI, command/effect contracts, evidence receipts and
Tauri/Maud interface.

Mom composes the shared [native runtime](../../crates/native),
[Speech](../../crates/services/speech), and
[Attachment](../../crates/services/attachment) crates through their typed public
APIs. [FTE](../fte) is a separate product; Mom has no provider gateway or loopback.
The product owner passes one explicit operation scope through CLI and desktop
operations, and dropping it joins its native workers.

See [`docs/MODULE_BOUNDARIES.md`](docs/MODULE_BOUNDARIES.md) for the complete
dependency graph and the exact present status of speech.

## Workspace

- `crates/mom-llama-runtime`: conversations, editable/versioned Personas,
  `@mention` dispatch, attachment lifecycle, storage, tools, compatibility
  Skill records and product cache policy. Skills and cache operations are
  backend-only compatibility capabilities, not ordinary UI sections.
- `crates/mom-llama-cli`: the complete machine-exercisable product boundary.
- `apps/mom-llama`: the thin Maud/Tauri application.
- `contracts`: command-surface, effect, settings and upstream-parity ledgers.
- `receipts`: preserved historical product evidence. A receipt counts as
  current proof only when it is explicitly source-bound; older path/date-only
  receipts remain informative.

Native, Attachment, Speech, and the shared platform contracts are resolved from their
imported monorepo paths and one root lock. FTE remains in the root workspace for
its standalone product, but no FTE crate or permission is in Mom's dependency
graph. No retired first-party Git source remains in Mom's dependency graph.

## Gates

```sh
cargo fmt --all -- --check
cargo test --locked -p mom-llama-runtime -p mom-llama-cli -p mom-llama-app --all-targets
cargo clippy --locked -p mom-llama-runtime -p mom-llama-cli -p mom-llama-app --all-targets -- -D warnings
node --check products/mom/apps/mom-llama/ui/coop-hx.js
products/mom/scripts/check-architecture.sh
products/mom/scripts/check-contracts.sh
products/mom/scripts/check-persona-product-ux.sh
```

## Run the app

Fresh setup selects Google's Gemma 4 12B instruction model, first-party QAT
Q4_0, when its pinned artifact is already in the Hugging Face cache. The matching
cached projector is paired automatically. Nothing is downloaded on startup.
Choose another GGUF from the composer dropdown (or **Choose GGUF file…**) at any
time. Run:

```sh
cargo run --locked -p mom-llama-app
```

`MOM_LLAMA_MODEL_PATH` remains an explicit runtime override. For a
set-and-forget product build, set `MOM_LLAMA_DEFAULT_MODEL_PATH` while compiling
and, only when automatic same-directory pairing is not unique, optionally set
`MOM_LLAMA_DEFAULT_MMPROJ_PATH`. A persisted user choice wins over the compiled
default; the explicit runtime override wins over both. Compile defaults must be
absolute and all selected model/projector files are still validated at runtime.
When none of those choices is set, Mom checks the shared pinned Gemma cache
entry using `HF_HUB_CACHE`, `HUGGINGFACE_HUB_CACHE`, `HF_HOME`, `XDG_CACHE_HOME`,
then the platform home cache. Mom embeds no developer model path and does not
fall back to the build directory or an arbitrary smaller model.

Mom uses the current Tauri identifier, data paths, environment variables, and
Keychain service. These are the product configuration, not a second compatibility
layer. Unused migration paths are retired; incompatible data is refused intact.

Debug builds use the prompt-free development store unless
`LLAMA_NATIVE_KIT_SECURE_STORAGE=1` is set. Release builds retain the existing
Keychain-backed store.

Prompt caching remains a product runtime preference: `automatic` (conversation
checkpoints plus stable Persona/Skill prefixes), `prefixes-only`, or `off`.
Compatibility fingerprints and safety ceilings are enforced by native-kit.
This policy is exercised through the typed backend and CLI; Mom does not expose
cache internals, cache mutation or cache-policy controls in the ordinary UI.

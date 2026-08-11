# Native executor-owned request leases

## Scope

- Repository: `delysis/llama-native-kit`
- Branch: `codex/native-executor-leases`
- Base: `6a82439ee449599f7a7e477e1150ae29efdb23d6`
- Binding pin: `delysis/llama-cpp-rs@152dabbd3492d8e35fdf7112e556685c6c75ec9a`
- Status: successor candidate pending pull-request review

## Invariant

An admitted public request ID remains reserved until the owner worker has
attempted final-result publication and drops its private, non-cloneable
executor lease. Dropping or timing out a caller ticket can request
cancellation, but cannot release or replace that identity.

Generation, controlled generation, and embeddings share one registry. The
admission mutex linearizes shutdown, reservation, and bounded queue insertion.
Queue insertion failure drops the unstarted command and its lease before the
typed error is returned. Joined shutdown refuses to mint its model receipt if
any executor reservation remains.

## Red evidence

The initial focused test command was:

```text
cargo test -p llama-native-engine operation_registry --lib
```

It failed to compile with 19 errors because the new tests referenced the
absent `RequestRegistry`, `RequestControls`, `RequestClass`, and
`DuplicateActiveRequest` contract. The base implementation instead used
three ticket-cleaned maps, and every ticket `Drop` removed its entries.

## Deterministic evidence

The repaired tree proves:

- ticket interest and executor lease ownership are distinct;
- a stale entry/lease identity cannot remove a newer reservation;
- shutdown quiescence rejects admission and cancels current controls;
- a closed registry requires zero active executor leases and cannot reopen;
- queue-full rejection releases an unstarted reservation;
- queued rejection publishes its terminal before releasing identity;
- generation, controlled-generation, and embedding ticket drops retain IDs;
- timeout returns a live ticket and a later wait receives the real terminal;
- queued shutdown produces one final result and one terminal per case;
- registry release accounting is exactly once for each matching lease.

Final deterministic gates on this patch:

```text
cargo test --workspace --all-targets
  PASS: 170 passed, 8 intentionally ignored real-GGUF tests
cargo clippy --workspace --all-targets -- -D warnings
  PASS
rustup run 1.88.0 cargo check --workspace --all-targets
  PASS
cargo fmt --all -- --check
  PASS
./scripts/check-architecture.sh
  PASS
git diff --check
  PASS
```

The binding repin initially made
`reported_binding_identity_matches_the_private_recipe_and_lock_pin` fail
(`left: 0`, `right: 2`) because the independently maintained compile-time
binding-revision constants still named `01e48b7`. Updating both the public
runtime constant and the private build-evidence recipe to `152dabbd...`
restored the exact manifest/lock/build identity test. The full gates above
were then rerun against the successor pin.

## Real-model evidence

Standalone model:

```text
path: /Users/george/Documents/llama-native-kit/target/test-models/Qwen_Qwen3-0.6B-Q4_K_M.gguf
bytes: 484220320
sha256: 9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14
device observed: Metal on Apple M4 Max
```

Exact ignored tests run with `MOM_LLAMA_MODEL_PATH` set to that artifact:

```text
controlled_runtime::tests::real_small_gguf_controlled_generation_proves_baseline_cfg_constraints_and_samplers
  PASS 1/1
tests::real_in_process_prompt_smoke
  PASS 1/1
tests::real_per_token_embeddings_preserve_generation_context
  PASS 1/1
```

Negative evidence retained: the 97,797,120-byte Gemma 4 assistant auxiliary
artifact at SHA-256
`9eba8199d64637ab5b8936f205651450bb063c1c6f21fa7a58470659313d1c95`
loaded but correctly failed context creation because it requires a companion
`ctx_other`; it is not a standalone generation fixture and is not counted as a
successful runtime proof.

## Deferred acceptance

This patch does not claim the packet's macOS loaded-model quit/relaunch proof,
Windows capability authority, downstream FTE/Loom compatibility, or binding
branch reconciliation. Those remain separate merge-train gates.

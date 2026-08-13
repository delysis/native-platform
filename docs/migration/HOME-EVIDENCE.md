# W8 HOME: local acceptance evidence

Status: **locally accepted; GitHub promotion pending**.

The W8 candidate commit is
`66e6bf091f16ddad0fcbab194db40b2f8b7c3457`, tree
`009927f7cf255daa3f05c8386ffc1a53b5ef0252`, with accepted W7 merge
`99c49908ef8ccdb39fbbb1f710331e8a4161bc43` in its ancestry. It converts the
imported repository into one root build graph: 65 first-party Cargo packages,
one root `Cargo.lock`, one root pnpm workspace and lock, and one bundled SQLite
native link. The sole auxiliary Cargo workspace/lock is the explicitly allowed
Attachment fuzz target.

## One graph

The root Cargo lock is 216,767 bytes, SHA-256
`2210d33fbd3359281637c4a90cd96eeb9ca6eed0161f483486ba82cd143bd43d`.
The root pnpm lock is 41,294 bytes, SHA-256
`1c2d7d73d3c24ab2a8d1ecce65efb30b65683dcd975212d5c62cb45273ba2714`.
All 65 packages occur exactly once across the 12 primary groups in
`ci/package-groups.json`. Cargo metadata, package-group coverage, workspace
root/lock uniqueness, frontend workspace ownership, and absence of nested
active workflows are enforced by `xtask policy`.

No first-party Git dependency remains. The one external unsafe boundary is
`llama-cpp-rs` at exact revision
`a3cf95eb1d4fa748480eb780e6fcbfc1a5c1c391`. All stores resolve through
`rusqlite 0.39.0` and `libsqlite3-sys 0.37.0`, one `links = sqlite3` target.
The bundled SQLite runtime is 3.51.3; its complete sorted compile-option set
has SHA-256
`237dbc028deb283af23c96fd82473d36055b11b437c9b282be65e50b1a2acd36`
and includes `THREADSAFE=1` and `ENABLE_FTS5`.

## Local gates

On macOS 15.6 build 24G84, arm64 Apple M4 Max, the declared Rust 1.92.0
toolchain passed the complete locked workspace all-target test suite, strict
all-target Clippy with warnings denied, formatting, and migration policy. Node
CI-planner/policy tests passed 23 tests. Exact pnpm 11.16.0 accepted the frozen
lock. FTE's two frontend tests passed; Loom passed 32 files / 182 tests, Svelte
check with zero errors and warnings, and the Vite build with only the existing
greater-than-500-kB chunk advisory.

The full store suites include current/prior FTE, Mom, Information, and Loom
fixture/migration/reopen paths. The consolidated SQLite identity test executes
the exact runtime version and compile-option fingerprint above. No schema was
changed merely to flatten the graph.

## Designated Mac real-hardware gate

The exact Qwen3 0.6B Q4_K_M model is 484,220,320 bytes, SHA-256
`9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14`.
Through the consolidated graph:

- Native passed the exact W1 model prerequisite, real generation, release,
  and worker join;
- FTE passed real chat, stable-prefix miss/hit, exact completion, adapter
  drain, and owned-host joined shutdown;
- Mom passed a real base completion with positive generated tokens and
  explicit `real_engine_invoked = true`, `fake_fixture = false`.

The exact Gemma 4 E2B base Q8 model is 4,954,576,032 bytes, SHA-256
`aa0a9a03993440f45176f19f8189a2e84c210ff8628ec13dc6edf42d017f7670`.
Its exact vertical passed real raw completion, release, and worker join.

The fresh product bundle was `Loom W8 66e6bf0.app`, identifier
`app.delysis.loom.w8.66e6bf0`. Its 21,127,840-byte executable has SHA-256
`fa2c36b6a3610f9d2177852e1309da00cd0bafcfea7d423bf87c86a2f477c527`
and an ad-hoc signature. It loaded all 36/36 Gemma layers on Apple M4 Max
Metal. In the visible native editor, the exact prefix `At dawn, the locked
observatory began to breathe.` produced a real caret-local multiline
suggestion. Accessibility reported `Suggestion available. Tab accepts; Escape
dismisses.` One ordinary Tab accepted it and reported `Suggestion accepted`.

The resulting four-paragraph manuscript is 262 bytes, SHA-256
`ef15713f9fc9a00b337e70b146a9b5c9d8e1fb269a8c375362618651c89bbc4d`.
SQLite records three source candidates and binds selected candidate
`01KZYB8R1FFHCTG44EHNHWA2W7`, source revision
`01KZYB8MFWN50MXB6HBR7VE0A0`, resulting revision
`01KZYBA06KQABR25NBPFE6TQ65`, selection
`01KZYBA06K4P63JWNRZ0CAJXQ3`, promotion command
`01KZYBA062ZKHYPQ616S64424A`, authorship attestation, and exact model
environment `7385407900474d07ec4ebde1012bccd065bdff4074aa07720563fb7bc1a8812e`.

Visible Cmd-Q with the model loaded exited zero and logged Metal deallocation
within an observed upper bound of 9,219 ms. The same bundle immediately
relaunched, visibly reopened the exact 262-byte manuscript, reloaded the exact
model, and a second visible Cmd-Q exited zero with Metal deallocation within
9,864 ms. Those are polling upper bounds, not internal latency claims.

Speech also passed fresh consolidated evidence. The exact 480,708,981-byte
Parakeet bundle (combined SHA-256
`c710ae82b52aa969f89874e7e7b35ad570fec50cc3d943a4fdde0bb874948756`)
processed the exact 305,580-byte WAV (SHA-256
`326d6723b8bcd7ae63cdff4a2c3e536a29a9d3a44e30f9dca7b65e58a9b4aa34`)
through complete and streaming real inference, cancelled exactly one peer,
preserved the other, and joined shutdown. Its known incorrect opening
transcription, `hardly beneath`, is retained as negative evidence; no accuracy
claim is made. The checked-in Apple Tauri consumer launched and returned
`APPLE_W1_OK` for installed voice `com.apple.eloquence.en-US.Eddy`, 153,540
WAV bytes, one terminal, `network=never`, and `real_local_inference=true`.

## Promotion boundary

W6 and W7 are merged, protected, and their source repositories frozen. W8 is
locally accepted but does not yet claim GitHub publication, required policy
and macOS checks, main merge, or a protected HOME tag. Linux and Windows are
informational under the Mac-first policy and do not block local engineering.
Distribution signing/notarization, W9 line/package reduction, and W10 release
and retirement are not claimed by this receipt.

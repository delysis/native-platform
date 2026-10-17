# Voice reliability qualification

## Current diagnosis

The last available app log is `/tmp/mom-live-parakeet-app.log` (Oct 16,
17:54). It reports Gemma and Parakeet warm-up, Parakeet taking 5.236 s, and
prepared speech becoming ready after 7.263 s. Metal identifies an M4 Max and a
Gemma 4 12B Q4_0 model. The log names the data identity
`mom-llama-development-mom1-v1`, but does not bind the executable or installed
bundle to a source commit or digest. There is no Mom process running at the
time of inspection. Treat this as diagnostic context, not acceptance evidence.

The browser capture requests native `echoCancellation`, `noiseSuppression`, and
`autoGainControl`. Current source compares mic frames with the actual rendered
playback reference. It suppresses low-level input only after a stable echo
match; unmatched caller audio passes after its 48 ms onset debounce, and
double-talk leaves an independent residual that can open the gate. This removes
the previous blanket 250 ms suppression. Deterministic matcher and double-talk
tests pass; physical speaker/microphone loopback remains unqualified.

The old app log reports Parakeet taking 5.236 s and speech readiness at 7.263 s.
Current source removes the unused startup Parakeet transcription probe while
retaining backend discovery and on-demand transcription. A packaged launch
trace has not yet verified this source change.

Live transcription queues at most 40,960 samples (2.56 s at 16 kHz), includes
in-flight audio in that bound, and fails the parallel transcription explicitly
on overflow or a stalled native acknowledgement. The saved recording remains
the fallback source. Existing Node tests cover this boundary, deadline,
cancellation, PCM order, ring overflow, rebuffering, scheduler runway, and
telemetry. They do not establish microphone-device behavior or slow-machine
resilience.

## Regression comparison

Use `97e6e459` (`Make Mom voice conversations native and responsive`) as the
pre-live-Parakeet-fanout candidate baseline; live Parakeet fan-out begins later
at `660afc42` and `3b3be988`. Confirm the exact two worktree commits and retain
the baseline source and measurements before comparing. The baseline is a
comparison point, not a release qualification. Run both versions with the same
Mac, OS, model/projector, voice reference, audio corpus, cache state, and
thermal starting condition. Report warm and cold first response, first token,
first PCM, playback start, underruns, queue high-water, and quit duration.
Do not compare results across cache states or devices as if they were paired.

## Acceptance run

One integrator owns the checkout, app state, and Cargo target at a time. Keep
model loads serialized. Preserve source logs and failed traces. Begin with the
affected Node suites; then run the standard Mom native tests and app build with
the same configured target. Do not run ignored real-model diagnostics until
the owner has stopped other model work and recorded the intended model and
artifact.

For real-device qualification, record the exact app bundle and executable
paths, executable SHA-256, `Info.plist` bundle identity/version, source commit,
OS/build, Mac model/RAM, model/projector and Parakeet identities/digests,
voice reference digest, and whether each process was cold or resident. Capture
the final trace and process/resource sample with that identity.

Exercise at least one sustained multi-minute conversation, then 20 rapid
cancel/resume cycles with varied speech timing, plus a longer 30-minute soak.
For every accepted input, verify source ordering, no silent drop or retarget,
bounded queues, one terminal result per operation, no late speech/transcript
publication, and successful immediate restart. Include playback plus physical
microphone loopback to assess echo and double-talk, and repeat the sustained
run on a lower-memory M2-class Mac under normal thermal load. Report startup,
first-token/PCM/playback percentiles, underrun and cancellation distributions,
peak process memory, queue high-water, CPU, thermal state, and any failed
receipts. Deterministic tests and a synthetic loopback are component evidence;
they do not replace these packaged-device runs.

## Current local evidence

The comparison source is `97e6e459`; the qualification worktree is based on
`a8d85989` on branch `fix/voice-reliability`. No paired baseline run or ABBA
comparison has been performed. The source and build receipts are retained under
`outputs/qualification`; the final commit and packaged-app identity must be
recorded in the external candidate manifest after the settled-tree gate. The old
`/tmp/mom-live-parakeet-app.log` is diagnostic evidence only: it names the
development data identity but not the app executable, bundle digest, or source
commit. It cannot identify the current installed/running artifact. No release
app has been launched for this qualification.

On an M4 Max, an ignored real Gemma 4 12B Q4_0 + matching mmproj test passed
with 24 generated chronological audio-history items, 20 cancel/resume cycles,
and deterministic synthetic live PCM. The final test seals audio, waits for a
nonempty decode token, cancels the exact generation ticket, and waits for its
terminal cancellation event in all 20 cycles. It reported 19/19 resumed-prefix
hits; decode-cancel-to-terminal latency was p50 1.899 ms, p95 1.962 ms, and max
2.032 ms in this uncontented component run. Final generated token IDs and text
matched the cold baseline exactly. The earlier 17.678 ms measurement was the
separate prefill-cancellation path, retained at
`outputs/qualification/real-audio-20-cancel-final-core-executed.log`. The
signed-state worker-restart test also passed on the real GGUF with CPU
execution. Neither is packaged-call acceptance.

The high-scale M4 Metal test initially found authenticated imported KV diverging
from resident KV, compact token replay, and cold generation. Inspection isolated
SWA export pruning: the 8,192-cell context allocated the full SWA cache, but the
export included only 1,024 cells. The pinned llama.cpp binding now exports all
SWA cells. The same four-path test then passed exact token and text parity at a
5,270-token prompt with 5,262 shared tokens. `state_read_meta` reported all
5,270 cells on import; restored=5,262, compact replay=5,262, and resident=5,262.
Timings were 175 ms resident, 6,487 ms imported, 10,877 ms compact replay,
11,486 ms cold, and 18,082 ms to warm. The import path is correct but still
slower than a resident hit; performance needs separate qualification. The exact
serialized snapshot byte length was not logged. Preserve the earlier failure
receipts at `outputs/qualification/real-edited-prompt-imported-lcp-long-persona.log`
and `outputs/qualification/real-edited-prompt-four-path-diagnostic.log`, and
the passing receipt at
`outputs/qualification/real-edited-prompt-four-path-swa-full.log`.

The complete Mom frontend suite passes 234 Node tests. It covers progress
deadlines, complete buffering for sustained slow synthesis, reference-correlated
echo, independent caller speech, double-talk residual, playback lifecycle, and
first-call buffering. These are deterministic component tests; they do not
qualify physical echo or the M2-class machine. Rust 1.95.0 focused native gates
pass: cache 21, engine 225 with 15 ignored, types 72, and runtime all-targets
344 with 17 ignored. The app build and full local-ci gate are pending. The
intended lower-memory target is the mother's M2 MacBook Pro with 32 GB RAM; it
is unavailable for this local run.

The new ignored encrypted-chunk integration test exercised a real 5,556-token
voice persona on an M4 Max using the Gemma 4 12B Q4_0 model with SHA-256
`93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b`. The
resolved context was 19,456 tokens under the 21,845 MiB manual resident budget.
Native state was 1,911,754,200 bytes and persisted in 114 encrypted chunks;
setup and persistence took 165,813 ms in this debug test run. CPU sampling from
the earlier failed attempt attributed 668 of 671 samples to unoptimized
ChaCha20 decryption, so this is correctness evidence, not an application latency
measurement. After restarting the native worker, the authenticated state
imported all 5,556 cells. This exposed a live-promotion bug: restore incorrectly
required the huge state to fit the separate RAM LRU. The lease now accepts a
disk-backed authenticated state when its owner and memory generation still
validate. The rerun passed after worker restart, reported
`prompt_smoke_verified`, and took 255,980 ms for reload/verification in the
debug test. Its full test took 421.05 s. The current receipt reports native
state import for all 5,556 cells; the test also asserts the receipt names
authenticated native state and does not report token replay. The
original single-branch attempt failed configuration because persona preparation
requires two probe branches; both failed receipts remain. These debug ChaCha20
times are correctness diagnostics, not optimized application latency. Receipts:
`outputs/qualification/real-runtime-encrypted-5k-cache-latest.log` (branch
configuration failure),
`outputs/qualification/real-runtime-encrypted-5k-cache-retry-maxseq2.log`
(pre-fix restore promotion failure), and
`outputs/qualification/real-runtime-encrypted-5k-cache-promotion-fixed.log`
(successful restart restore), and
`outputs/qualification/real-kv-cache-maxseq1-metal-guard-fixed.log`
(one-sequence real save/restore). The preceding one-sequence attempt exposed a
spurious two-probe minimum in shared-prefix prefill; its failure receipt is
`outputs/qualification/real-kv-cache-maxseq1-metal.log`.

Run the standard focused non-model native gate from the repository root with
the pinned binaries required by `CONTRIBUTING.md`:

```sh
export RUSTUP_TOOLCHAIN=1.95.0
export CARGO="$(rustup which --toolchain "$RUSTUP_TOOLCHAIN" cargo)"
export RUSTC="$(rustup which --toolchain "$RUSTUP_TOOLCHAIN" rustc)"
export RUSTDOC="$(rustup which --toolchain "$RUSTUP_TOOLCHAIN" rustdoc)"
export PATH="$(dirname "$CARGO"):$PATH"
cargo test --locked -p mom-llama-runtime -p mom-llama-cli -p mom-llama-app --all-targets
```

After the complete local-ci gate passes on the exact clean source tree, create
one reviewable candidate with the checked builder. It requires the JSON summary
for that exact commit/tree, verifies the source before and after building, and
writes a per-file SHA-256 manifest beside the bundle. It does not install or
launch the app:

```sh
node products/mom/scripts/build-voice-candidate.mjs \
  "$HOME/.codex/evidence/local-ci-<commit>-attempt-01/summary.json" \
  "/Users/george/Documents/Codex/2026-10-16/gemma-first-token-152-154-ms/outputs/qualification/candidate-attempt-01"
```

It creates `Blessenger Voice Candidate.app`, `candidate.json`, and `build.log`
inside the new output directory. Verify the exact candidate and manifest before
launching it for device qualification.

The current real-model audio command is opt-in and uses generated test PCM:

```sh
MOM_LLAMA_MULTIMODAL_MODEL_PATH="$HOME/.cache/huggingface/hub/models--google--gemma-4-12B-it-qat-q4_0-gguf/snapshots/29d097773436b69ff9feafd636ab4cf873786537/gemma-4-12b-it-qat-q4_0.gguf" \
MOM_LLAMA_MM_PROJ_PATH="$HOME/.cache/huggingface/hub/models--google--gemma-4-12B-it-qat-q4_0-gguf/snapshots/29d097773436b69ff9feafd636ab4cf873786537/mmproj-gemma-4-12b-it-qat-q4_0.gguf" \
cargo test --locked -p llama-native-engine tests::real_incremental_audio_accepts_earlier_recording -- --ignored --exact --nocapture
```

Record an artifact identity manifest immediately after packaging. At minimum
capture `git rev-parse HEAD`, dirty-tree status, the absolute `.app` path,
`/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' Info.plist`,
`/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' Info.plist`,
`shasum -a 256` of the actual `Contents/MacOS/*` executable, `sw_vers`,
`system_profiler SPHardwareDataType`, model/projector/Parakeet file identities
and SHA-256 values, voice-reference digest, and each run's cold/resident state.
Retain this manifest beside the trace, capture log, and process/resource sample;
keep logs and failed receipts unchanged. The Tauri source identity currently
declares Blessenger `0.1.0`, bundle identifier
`com.delysis.llama-native-kit.mom-llama`; this is source configuration, not
proof that a particular installed app has that identity.

The full-device acceptance matrix remains open: exact packaged app, one
integrator and serialized model slot, sustained conversation, 20 rapid
cancel/resume cycles, 30-minute soak, microphone/speaker physical loopback with
double-talk, and repeat on a lower-memory M2-class Mac under ordinary thermal
load. Preserve startup/first-token/first-PCM/playback percentiles, underruns,
cancellation latency, peak memory, queue high-water, CPU/thermal readings, and
failed operation receipts. Synthetic audio and unbundled native tests are not
packaged-product acceptance.

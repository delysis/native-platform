# Native Platform: one integration owner, three qualification tracks

Prepared September 22, 2026 (America/New_York); repository observations and portable rechecks continued on September 23 UTC.

## What this handoff resolves

No new subagent report was present in the latest request: its attachment was the original 360-degree audit plan. GitHub issue inventory and PR #78 comments did not supply another report. This handoff therefore distinguishes retrieved, concrete local-agent failures from unidentified additional issues. Do not mark an unseen report resolved.

One actual integration omission is now repaired. PR #77's branch was still at `f0490c463f75f76714520bcaead099ac2216f7be`, while the local agent had pushed two descendants to the separate Pro-handoff branch. The authenticated comparison showed two commits ahead, zero behind, and the exact former PR head as merge base. PR #77 was fast-forwarded without force to `050672127b735df0fba6c0a55dd09de23dad79e8` and read back at that head. This includes `034f8f94474d66aa09c027a526c96653b0b75403`, which supplies the required `seed` and `model_id` in `completionHydration.test.ts`, plus the local agent's subsequent failure handoff. No main merge or new native pass is implied.

Two later implementation packets were recovered and their portable tests rerun. Their changes are NOT yet repository source changes. Their remaining publication is a bounded, exact-base application task for the local agents, not permission to redesign the features.

## Observed heads and ownership

| Track | Exact observed source | Disposition |
| --- | --- | --- |
| Main | `87939f3873977ef2670ddab9e869ca9d5d7d1739` | Unchanged by this continuation. |
| Audit PR #78, `audit/360-evidence-20260922` | Code head `6c6d07738cf98c3a4ca91adccbc79971375e3a06`, tree `05150d45c78a4b4412ce9e8a096640d6a52e4368` | All nine previously reported findings have source fixes; Rust and native qualification remain open. The commit publishing this handoff is documentation-only on top of that code head. |
| Completion PR #77, `codex/pr75-astra-handoff-20260922` | `050672127b735df0fba6c0a55dd09de23dad79e8` | Now includes the local agent's fixture correction and latest zero-admission failure. Apply the enable/isolation packet here through an isolated descendant worktree. |
| EASL PR #73, `codex/loom-easl-main-integration` | `53e7b7ba08c43286f85d6b0c780f333c6ce8469d` | Separate draft native-interface track. Its accessibility packet needs full-checkout application and actual target validation. Not a prerequisite to the scoped audit repair and not an accepted replacement frontend. |

These are parallel tracks, not a pre-existing linear stack. Refresh the actual remote references before any work or push. A newer head requires reviewing its changes and reconciling the packet; never overwrite it or substitute an old checkout.

## Coordinator rules

One coordinator owns the integration branch and main promotion. Workers own isolated worktrees and return bounded results; they do not independently merge, rebase the shared stack, clear stores, or reinterpret acceptance requirements. Use one Cargo process per shared target directory. Independent workers may run concurrently only with separate ownership of files and build outputs. Stop the failing job at its first meaningful failure; independent work need not be discarded.

Before each run record HEAD, tree, working diff, target/features, exact command, and tool identity. Preserve raw failures and prior native-smoke directories. A failed infrastructure run with zero steps is neither a compiler failure nor a product pass. The last checked PR #78 run `35794071883` had zero planner steps and skipped downstream work; its cause was not established. Use the existing authorized local equivalent, not repeated unchanged dispatches or a new paid-model service.

Select the complete pinned Rust toolchain, not merely an environment variable preceding a standalone Homebrew Cargo. Resolve Cargo, rustc and rustdoc from Rust 1.92.0, put that Cargo directory first on PATH, and verify nested pnpm/Tauri uses the same tools. Preserve lockfiles and actual default/release/platform feature sets. Missing dependencies or native permissions are precise blockers, not authority to edit dependencies, bypass checks or fabricate results.

## Worker A: finish the nine-finding audit qualification

Use the current PR #78 descendant. Read `docs/reviews/360-remediation-local-ci.md` and `2026-09-22-360-remediation.md` in that worktree. Do not apply the historical `360-candidates.patch`: both test repairs are already source changes.

Run the existing focused sequence before the expensive gate: 26 Node gate cases, six installation-key coordinator tests, one paired-document transaction regression, one Speech duplicate-completion regression, and seven Unix/four non-Unix FTE storage cases at the supplied code revision. If a later legitimate edit changes discovery, report why; never count zero or ignored execution as a pass. Exercise the actual Mom release selections and the exact release helper against real pinned Cargo/libtest: ordinary test passes; ignored and nonexistent tests are rejected.

Run the specified behavioral negative controls one at a time after the unmodified test passes. An unrelated compile error is not successful falsification. Then run owning packages and strict Clippy. Mechanical compile/format/lint repairs may be committed with renewed checks; semantic failures require a minimal source-grounded diagnosis, not a weaker assertion.

Keychain qualification is explicit and disposable. Use the existing opt-in fixture only for its fresh `audit-360-<UUID>` account. Never touch the installation account, change Keychain configuration, rotate keys or reset a database. Restore the transient test append byte-for-byte before the final build. Keep issues #79/#80 open until their actual target checks pass. Test real Mom/FTE startup, protected storage, save/reopen and owned-worker shutdown on the identified artifacts. Portable coordinator/store tests cannot replace this.

## Worker B: unblock the current completion path, then publish to #77

Consume `native-platform-enable-isolation-repair-05067212.zip` from the consolidated delivery. Its SHA-256 is:

`9691bf95bb77aae85d7077fa593b3ef904168d1e175474b2c6c6db1605d21153`

Its exact base is `050672127b735df0fba6c0a55dd09de23dad79e8`, tree `f244612be52e12cd3b31dae126cd4dfbc2182769`. The archive contains the complete changed-file postimages, five-file patch, before/after hashes, regression replay, and exact application helper. It is not a complete repository checkout.

Set `PACKET` to its extracted directory, `REPO` to an authorized complete checkout and `DEST` to a new worktree location. Run the helper's tests, then `node "$PACKET/apply.mjs" "$REPO" "$DEST"`. The helper requires the exact base, checks the patch and every pre/postimage, creates an isolated branch/worktree, and stages the result. It does not commit or promote. An existing destination/branch is not permission to delete it; diagnose the collision and preserve existing work.

The supplied correction addresses three distinct mechanisms:

- After enabling suggestions, wait for the reactive update and recheck mounted/application/project/session scope before scheduling. Repeated already-enabled policy must not create work. The new real-App browser case requires one admission with the same warm writer after disable/re-enable.
- Explicitly apply the persistent acceptance-store identifier to the WebView builder; retain ordinary-launch behavior and isolated persistent acceptance roots. The configuration-only assertion did not establish the assembled native store.
- Collect bounded pre-admission/editor diagnostics before the monitor's `family_pending` branch. Empty diagnostics from an early continue are not evidence that the editor was inspected and absent. Zero generations still fail acceptance; this is not a synthetic success witness.

Preserve the raw latest zero-run incident before testing. Do not conflate it with the earlier eight-run incident, and do not assert these source mechanisms caused either recorded Mac run without its actual trace. Follow the packet's `LOCAL-AGENT.md` and the resulting `docs/reviews/2026-09-22-zero-admission.md`: pinned frontend check, real App WebKit tests, loom-app tests/Clippy, native helper compilation, then the settled gate and one clean identified candidate.

On the Mac require an A/B/A test of separate persistent acceptance roots without clearing user histories; warm-writer disable/re-enable; actual admitted family identities; increasing full pre-terminal Ghost text; stable four-choice Loompad/WASD; unchanged bytes and generation count during pure cycling; exact insertion; separate unconsume and ordinary undo; stale-scope invalidation; joined quit; and same-artifact exact-content reopen in both Visual and Source. Do not increase the four-run guard, extend deadlines to hide a defect, cap previews to one word, inject completions or add hosted fallback.

After focused checks and factual handoff, commit the reviewed descendant and push to the existing PR #77 branch using a normal fast-forward. Read back both branch and PR head. A draft source push may retain explicitly recorded native blockers; readiness and main promotion may not. Do not create another competing completion PR or merge #75/#76 wholesale.

## Worker C: keep EASL's next slice separate and honest

Consume `easl-accessibility-next-53e7b7ba.zip`, SHA-256:

`c2b1180b261b7cc8f395e8d43e2670b20f02904b2daff67bd8278e1f3c2b3b3f`

Apply only to a complete isolated checkout at `53e7b7ba08c43286f85d6b0c780f333c6ce8469d`. Run `node --test "$PACKET"/tools/*.test.mjs`, then `node "$PACKET/tools/preflight.mjs" "$CHECKOUT"`, then its `--apply` form. Full Cargo.toml/Cargo.lock preimages are required; recovered excerpts must not be used as the original files or an excuse to change expected hashes.

Follow `LOCAL-AGENT-PROMPT.md` for pinned `native-view` tests, both probe feature modes, strict Clippy, package-group policy, helper compilation, retained reusable/review-only suites, same-binary hidden lifecycle, and actual native AX. The packet adds 33 Rust tests; that authored count is not execution evidence. The 44 packet/helper checks rerun here do not compile those Rust tests.

For native AX retain the exact spawned child PID and independent executable hash, use only the empty ephemeral probe buffers, require all actual helper assertions, then independently wait for the child and require exit code zero. A process disappearing can be a crash. The earlier hidden-lifecycle receipt cannot qualify a binary with a new accessibility adapter. Preserve the full IME/preedit, VoiceOver, pixels, product-owner/service integration and source-parity boundaries; do not relax the App/CSS guard or turn qualification flags true merely to compile.

Publish a tested/component-qualified continuation to existing draft PR #73, without force. This packet is not authorization to mark the native replacement ready, retire the existing frontend or merge #73 to main.

## Assembly and elevation

First qualify the audit and completion lanes independently; then assemble their reviewed current heads in a fresh integration branch from refreshed main. Inspect actual conflicts and changed shared inputs. Do not assume two green branch runs certify their merge. Re-run affected regressions and one consolidated pinned workspace/doctest/Clippy/policy/frontend gate on the settled integration tree, then build the exact native artifacts from that clean tree and run their required journeys.

Keep EASL out of that integration until its own production requirements qualify or an explicitly reviewed narrower experimental scope is chosen. Its workspace/lock/package inventory and App/CSS reference guard will need deliberate reconciliation later. Do not resolve the reference guard by copying the new App hash without semantic parity review.

Promotion remains conditional on the user's existing authorization: required checks and affected functionality must actually pass. A missing required native observation is not an advisory failure. Retain genuinely advisory Linux/Windows reporting without claiming unsupported private storage. Do not bypass red gates, silently waive the known completion failure, substitute an old binary, close an issue using only a source diff, or call an entire audit complete from these nine repairs.

Before merging, record the exact tested integration tree and source/artifact/model identities. After merging, verify main's tree equals that tested tree. Any intervening code change requires renewed relevant qualification. Only the coordinator closes qualified issues and updates PR readiness.

## Fresh execution evidence in this delivery

The current host has Node 22.16.0 and Swift, but no Rust toolchain or macOS runtime. Rechecked results:

| Check | Actual result | Boundary |
| --- | --- | --- |
| Completion source-function replay plus application-helper tests | 17 passed, zero failed/skipped | Extracted production functions with simulated reactive flush, plus temporary Git fixture operations; not Svelte/WebKit or an actual full repository application. |
| Same replay against the original App function | 5 passed, 3 failed as expected | Fresh negative baseline; not reproduction of the particular native smoke incident. |
| Completion targeted mutations | All three rejected by their intended assertions | Missing flush, missing scope recheck, repeated-enable rearm. |
| EASL packet/helper suite | 44 passed, zero failed/skipped | Packet/receipt tests and compiled portable Swift checks, including negative controls; not Rust, macOS AX, IME or native product acceptance. |

Raw logs and unchanged original archives accompany this document. Earlier 26/26 audit gate results remain historical for their stated code; they were not rerun in this continuation. No local-agent receipt, actual native acceptance, or main merge is claimed here.

When reporting a blocker, return its precise command, source/tree, first actionable diagnostic, actual counts/status, changed files and raw log path/hash. Keep private logs local and report only the minimum necessary excerpt. This lets the next owner fix the real failure rather than infer it from a green counter or a vague "CI failed".

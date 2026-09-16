# Maintaining the OMP² integration

FTE/native-kit maintainers own this integration. OMP² contributes provider data,
wire compatibility and credential-handling knowledge. FTE retains routing,
network permission, finite quotas, full-stream admission, cancellation, deadlines
and exact raw completion. Loom retains manuscript, model and promotion authority.
The MIT notice is retained with every imported source snapshot.

`third-party/omp2/import.json` is the sole upstream pin and source inventory.
`upstream/` contains byte-exact reference inputs, not a second Cargo workspace.
Every input names its downstream consumers and purpose. Adaptations live in the
owning Rust crate, never inside the reference snapshot. Upstream version numbers
and catalog entries do not establish provider or local-model qualification.

The root `xtask policy` gate checks the complete inventory and SHA-256 values.
Snapshots are never refreshed at application startup, build time or from Loom.
An upstream update cannot silently add a provider, discover credentials, enable
network access or alter the author's configured route.

## Update cadence and responsibility

- The FTE maintainer checks `omp2` weekly and reviews meaningful changes within
  two working weeks. Transport/auth security fixes are triaged on the next
  working day. Record the date, candidate SHA and disposition in the update PR,
  including a reason when deferring it. No requirement to take an unqualified
  revision simply to keep the pin recent.
- `.github/workflows/omp2-upstream.yml` prepares the review bundle every Monday
  and on manual dispatch. It has read-only repository permission, retains the
  artifact for 30 days, and never opens a PR or changes the pin. The workflow
  runs on the default branch once merged; it is not active in an unmerged branch.
- Before each native-kit/FTE release, review the outstanding upstream diff and
  the known exclusions below. A second maintainer reviews changes to credential
  destinations, endpoint defaults, route permissions or accounting semantics.
- Keep one atomic commit for each accepted source pin plus its consumer changes
  and regression fixtures. Ordinary `git revert` restores the previous pin,
  adapters and fixtures together. No user-store migration is part of an import.

## Reproducible import procedure

Use an upstream checkout with history from the old pin through the candidate.
The tool reads Git objects, so a dirty upstream working tree is not imported.
Resolve and review a full immutable commit; branch names are rejected by the
importer. No upstream Rust, build script or executable is run.

```sh
git clone --filter=blob:none --single-branch --branch omp2 https://github.com/can1357/oh-my-pi.git /tmp/omp2-upstream
git -C /tmp/omp2-upstream fetch origin omp2
git -C /tmp/omp2-upstream rev-parse origin/omp2
cargo run --locked -p xtask -- omp2 verify
cargo run --locked -p xtask -- omp2 review /tmp/omp2-upstream FULL_COMMIT_SHA /tmp/omp2-review-UNIQUE
```

`review` verifies that the checkout contains the exact pinned source, requires
the candidate to descend from that pin, and creates a new directory containing:

- `upstream.diff`: changes across the watched catalog, codecs, auth, transport,
  lifecycle and fixture paths, including files not currently imported;
- `baseline/` and `candidate/`: original and proposed byte-exact inputs;
- `import.patch`: the proposed source snapshot and manifest update;
- `REVIEW.txt`: the concrete apply and validation commands.

Review the surrounding upstream diff as well as imported files. Port relevant
behavior into the consumers named in the manifest. Review new files, renamed
files and removed APIs explicitly; the tool rejects deletion/rename of an
imported source. An upstream history rewrite also requires a separate provenance
review instead of silently resetting the ancestor requirement.

From the downstream checkout, after reviewing and adapting consumers:

```sh
git apply --check -p2 /tmp/omp2-review-UNIQUE/import.patch
git apply -p2 /tmp/omp2-review-UNIQUE/import.patch
cargo run --locked -p xtask -- omp2 verify
cargo test --locked -p xtask -p fte-providers
cargo test --locked -p fte-providers --doc
cargo clippy --locked -p xtask -p fte-providers --all-targets -- -D warnings
cargo run --locked -p xtask -- policy
cargo fmt --all -- --check
node scripts/ci/cargo-group.mjs test gateway
```

Run affected consumer gates selected by dependency-aware CI. A raw/native adapter
change additionally needs the contracted real Gemma 4 native proof; a Loom
configuration change needs the real configuration-to-runtime boundary and normal
editor behavior checked. Keep these claims separate from offline provider
fixtures. Hosted live checks are explicit, credential-scoped operator actions,
never automatic use of a developer's credentials.

Changes under `third-party/omp2` select full CI because external `include_str!`
inputs have no Cargo reverse-dependency edge. Source-only pin changes must still
exercise all consumers, even when no downstream `.rs` file changed.

Current local adaptations are explicit: OpenAI input totals remain inclusive of
cache reads, absent usage remains unknown, split usage frames retain previous
dimensions, and known zero is preserved. Provider auth uses injected credential
handles; OMP environment-variable discovery and OAuth login are excluded. Gemini
API keys use a sensitive header instead of a query parameter. OMP branding headers
are excluded. Existing raw-completion endpoints are local contracts and are not
inferred from upstream chat support. The reviewed profile allowlist does not grow
when the upstream registry adds providers.

Selected Chat compatibility is applied only by `from_omp2`, per explicitly
registered model. Provider defaults select the output-token field and streaming
usage option; unsupported penalties or tool strictness fail before dispatch.
The pinned model profiles contribute streaming-usage and tool-choice overrides.
Other upstream fields (reasoning dialects, replay healing, tool-ID rewriting,
provider cache annotations and model discovery) remain review inputs, not support
claims. Adding one requires an adapter regression and a documented disposition.
Custom endpoints do not inherit these policies from their caller-chosen IDs.

The imported Messages stream exercises thinking text plus its opaque signature,
split tool arguments and inclusive input/cache accounting through the actual HTTP
adapter. Partial usage frames retain the raw dimensions before normalization,
so a later frame cannot double count caches. The OpenAI parity fixture currently
qualifies usage-row merging only: its terminal EOF is not accepted as a complete
strictly delimited SSE stream. The other two imported streams exercise full HTTP
transport. No fixture establishes live provider availability.

Loom reads its optional global `.loom.toml` once per launch. Only named suggestion
or manual-weave scopes authorize server execution. The gateway is in process;
it starts no listener, dashboard or extra response database. Missing/invalid
configuration leaves ordinary native operation available. See
[`inference-dotfile.md`](../../loom/docs/inference-dotfile.md) for the versioned
contract and keychain references. Server output retains `server_response`
evidence, with unknown weights/tokenizer identity; native receipts remain native.

The import tool's integration tests create real Git histories, generate and
apply an update, verify it, reverse it, and verify rollback. They also reject
source tampering, unlisted files, deleted inputs and malformed paths/revisions.
The source snapshot patch alone does not prove semantic synchronization: the PR
must explain each relevant upstream change and its downstream disposition.

## Deliberate exclusions and reassessment

Do not import OMP's agent loop, terminal UI, configuration command system,
automatic environment credential discovery, default native audio, stream
admission or aggregate budget ledger. The audited pin releases admission at
handshake, charges usage before stream completion and checks decompression bounds
after allocation. Watch these areas for upstream fixes, then evaluate them
against our existing full-lifetime tests before replacing local authority.

Loom's exact-prefix native generation does not become chat message generation.
OMP model/provider metadata supplies no invented free quota, live availability
or Gemma 4 qualification. Local route/model selection remains explicit.

## Adoption completion requirements

The integration is complete only after all of the following have current evidence:

1. Pinned, licensed catalog/compatibility inputs have real runtime consumers.
2. Selected codec and auth behavior is adapted behind FTE's existing boundaries,
   with imported fixtures exercised through the real hosted adapter.
3. Loom's configuration integration selects only authorized routes and preserves
   ordinary editing, native exact-prefix generation and provenance.
4. Update detection, candidate review, import, validation and rollback have been
   exercised; CI verifies the same source inventory.
5. A final affected-package/consumer gate passes, with explicit evidence for any
   real-model/UI requirement and an honest list of excluded live-provider claims.

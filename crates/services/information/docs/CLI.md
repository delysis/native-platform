# Operator CLI

The CLI prints machine-readable JSON to standard output and diagnostics to
standard error. It is an operator surface for proving the same contracts an app
will embed; it does not invoke a model or write an external source database.

```sh
# Inspect a provider without installing anything.
cargo run -p information-native-cli -- discover kiwix-opds --max-pages 1
cargo run -p information-native-cli -- discover overture-stac --traversal latest-chain

# Validate or list an explicit v1 source registry without contacting providers.
cargo run -p information-native-cli -- sources validate \
  --registry catalogues/default-sources.json
cargo run -p information-native-cli -- sources list \
  --registry catalogues/default-sources.json

# Validate and search a normalized catalogue, then resolve exact full bytes.
cargo run -p information-native-cli -- catalog validate --catalog catalog.json
cargo run -p information-native-cli -- catalog search --catalog catalog.json --text theology
cargo run -p information-native-cli -- catalog plan --catalog catalog.json \
  --resource-id org.example.corpus --release-id 2026-08 \
  --representation-id sqlite --available-bytes 100000000000

# A file: plan is denied unless its containing directory is granted explicitly.
cargo run -p information-native-cli -- install --plan plan.json --root ./information-store \
  --allow-file-root /absolute/path/to/sources

# Cheap listing is separate from an explicit whole-byte integrity pass.
cargo run -p information-native-cli -- installed --root ./information-store
cargo run -p information-native-cli -- verify --root ./information-store \
  --installation-id chosen-installation-id
cargo run -p information-native-cli -- receipt-history --root ./information-store \
  --installation-id chosen-installation-id
```

`sources` requires an explicit `--registry` path. A repository-relative default
would depend on the process working directory and would fail after ordinary
binary installation. The parser accepts only the exact
`information_native.catalog_sources.v1` schema, known v1 enum values, unique
source IDs, and absolute credential-, query-, and fragment-free HTTPS URLs. It also rejects a source
that claims either shipped support level without matching the corresponding
compiled Kiwix OPDS or Overture STAC adapter identity. Both source commands are
local file operations and report that they performed no network access.

The support values are deliberately narrower than “executable”:

- `shipped_discovery_and_exact_metalink` means the bounded Kiwix OPDS discovery
  and exact Metalink resolver ship; it does not mean ZIM content reading ships.
- `shipped_bounded_discovery` means bounded provider discovery ships; it does
  not mean materialization or querying ships.
- `registry_only` is descriptive metadata with no shipped provider adapter.

Registry membership never authorizes acquisition or installation. Exact bytes,
digests, rights, selection, and host-granted transport policy remain separate
requirements.

`install --plan` is intentionally a detached-plan boundary. The CLI validates
the plan as self-contained input, retains its incoming fingerprint as
`source_plan_sha256`, changes the effective catalogue authority to
`DetachedUnverified`, and recomputes the plan fingerprint before installation.
The authority claimed by a plan file is therefore audit evidence, not authority
granted by the executing host.

Private, loopback, and link-local destinations are rejected by default. The
`--allow-private-network` flag is an explicit broad capability intended for a
controlled LAN or local test server. Redirect destinations are re-authorized on
every hop.

Restarting the same plan against the same root can reuse fully verified staged
artifacts. Partial byte-range continuation is limited to HTTP(S) artifacts with
a matching durable resume sidecar and server validator on platforms with strong
file identity. On Unix, sidecar and staging file must share one canonical
owner-private directory and are protected by an exclusive transfer lease;
elsewhere the HTTP artifact restarts from zero. Receipts retain every source
attempt and half-open byte range. Partial `file:` copies always restart. If a
crash occurs after same-filesystem activation but before the ready receipt, the next
exact-plan attempt recovers that
`activated_unregistered` state. No failed or cancelled attempt is reported
ready. The command is synchronous; this release has no background install-job
or cross-process cancel command.

## Compiled SQLite archive profiles

The query surface contains four schema-checked profiles. The command names are
part of the operator contract; table and column identifiers are not supplied at
runtime.

| Profile | Search command | Stable lookup command |
|---|---|---|
| `alexandria.blocks.v1` | `alexandria search --text TEXT` | `alexandria block --block-id ID` |
| `community-archive.messages.v28` | `community-archive search --text TEXT` | `community-archive message --message-key KEY` |
| `encyclopedia.articles.v1` | `encyclopedia search --text TEXT` | `encyclopedia article --article-id ID` |
| `alexandria.scripture-references.v1` | `scripture search --reference REF` | `scripture occurrence --occurrence-id ID`; `scripture passage --passage-id ID` |

Community message keys use `rowid:`, `tweet_id:`, `note_id:`, or the compound
key returned by search. Encyclopedia and Scripture lookup IDs are positive
numeric values. Scripture search performs literal prefix or exact matching over
normalized references; it is not an FTS query.

Every command also requires `--database`, `--resource-id`, `--release-id`,
`--representation-id`, and `--publisher`. Replace the example paths and
identities with operator-owned values:

```sh
cargo run -p information-native-cli -- alexandria search \
  --database /absolute/path/alexandria.db \
  --resource-id local.alexandria --release-id current \
  --representation-id sqlite --publisher "Local custodian" \
  --text "contemplative prayer"

cargo run -p information-native-cli -- community-archive search \
  --database /absolute/path/community_archive.sqlite \
  --resource-id local.community-archive --release-id current \
  --representation-id sqlite --publisher "Local custodian" \
  --text "shared attention"

cargo run -p information-native-cli -- encyclopedia search \
  --database /absolute/path/encyclopedia.db \
  --resource-id local.encyclopedia --release-id current \
  --representation-id sqlite --publisher "Local custodian" \
  --text "natural philosophy"

cargo run -p information-native-cli -- scripture search \
  --database /absolute/path/bible_refs.db \
  --resource-id local.scripture-references --release-id current \
  --representation-id sqlite --publisher "Local custodian" \
  --reference "John 3:16" --syntax exact
```

All four backends open every operation with `mode=ro&immutable=1`; neither
access policy permits SQLite to create WAL/SHM files beside the source. Both
reject a non-empty sibling WAL or rollback journal and require the database and
sidecar identities to remain stable for the operation. `live-read-only` is for
a quiescent source: it rebinds identity between operations and rejects
`--verified-sha256`. `immutable-read-only` pins the initial file identity and
hashes the complete source, optionally checking `--verified-sha256`; that cost
is intentional for large archives.

`register-sqlite` writes an external registration under the managed root, not
to the canonical database. The profile-specific query commands above can mount
their database path directly and do not require registration first. Optional
`--rights-json` and `--use-policy-json` inputs travel with results. Community
Archive model-context retrieval also requires the policy to allow that purpose
and the explicit `--allow-private-model-context` flag before private records can
be returned.

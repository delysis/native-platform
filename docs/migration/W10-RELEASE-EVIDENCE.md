# W10 macOS release-candidate evidence

Status: **local macOS candidates accepted; none is stable; publication remains pending**.

W10 now has one deliberately small local release path. From a clean commit,
`scripts/release-macos.sh {mom|loom|fte}` runs the product's focused
source-level migration and shutdown tests, builds an app-only macOS bundle with
the workspace-local Tauri CLI, rejects embedded model files, ad-hoc signs the
bundle by default, verifies the signature, and writes a digest receipt. It does
not require GitHub.

`scripts/smoke-macos-app.sh {mom|loom|fte}` launches the exact packaged child
twice against one isolated product-state root, observes an on-screen window and
product readiness, invokes the application's real Quit menu item (the item
bound to Cmd-Q), requires exit status zero, and verifies that the executable
did not change between launches. The smoke deliberately does not override
`HOME`.

## Accepted archive candidates

| Product | Source | Executable SHA-256 | Archive SHA-256 | Archive bytes |
| --- | --- | --- | --- | ---: |
| Mom Llama 0.1.0 | `171d312763ccf89cf8bd0cc5820493fccafa6e9a` | `e7958448b4526a0a5390dafe7903dc55a31d614887d925177eddde3e5572b2bb` | `145ee6aba135f81598ee13a516cc87885b6ce0d284ee6ec5b6cecb4ab74b7132` | 10,896,750 |
| Loom 0.1.0 | `f8ee159de31369c29dc12b24a40a01886a9a02c5` | `f22887eef6f3b21712baf5e6a7ebc079fe57aa42ddfc3b3e7fff99b5f708377f` | `5e030ace600c82c0cebfd017b91bb489ec28d297a12c62dffd9cc2ad04184383` | 8,688,521 |
| Free Token Energy 0.1.0 | `f8ee159de31369c29dc12b24a40a01886a9a02c5` | `d24a63f48d32f6b427aa22cbddf92ecadc7fa64b4db50bb3e5623a531e787e14` | `ca5c1d8779f26a45b43aff82334e872d228d999dfdfb14fd38b53f703531e1a4` | 8,349,209 |

All three bundles declare macOS 13.0 as their floor, contain no GGUF, ONNX, or
safetensors file, and passed `codesign --verify --deep --strict`. The local
artifacts and their separate release/smoke receipts are under `dist/macos/`;
that generated directory is intentionally not committed.

The accepted ZIP for each product was also extracted into a fresh smoke root
and the extracted application passed the same two-launch Quit/relaunch check.
Each archive input SHA-256 matched the digest recorded above. This closes the
identity gap between the build-tree bundle and the emitted archive without
turning the remote packaging workflow into a development gate.

The Mom smoke opened the same encrypted `runtime.sqlite3` file identity from
the isolated root on both launches. Each quit emitted positive
application-drain and native-host join evidence. The release gate also passed prior-store import/reopen,
persistent-cache corruption/reopen, and an active native-operation drain at
source-test level. It did not exercise backup/restore rollback or an active
operation through the packaged application.

The Loom smoke created and reopened one ordinary writing project. Its database
contains two `open_project` receipts after the second launch. The package is
bound to `writer-gemma4-base-v2`, policy file SHA-256
`744fa860ffc979f6c1c9e4e1a96680d31c26b558317755548464df5300b9b791`.
The release gate also passed prior-v10 project migration/reopen, suggestion
promotion/reopen, active-family cancellation/drain, and 178 frontend tests at
source-test level. It did not exercise backup/restore rollback or an active
operation through the packaged application.

The FTE smoke opened the same `gateway.db` and `gateway-v2.db` file identities
from the isolated root on each launch and used process-local credentials instead of Keychain.
The release gate also passed fresh/current-schema reopen, native-runtime join,
active-router cancellation/drain, and both frontend tests. FTE has no legacy
database migration path: it accepts a fresh store or the exact current schema
and rejects legacy, unversioned populated, foreign, and altered-schema
databases before mutation.
Separately, the ignored current-source macOS credential acceptance test wrote,
independently read, replaced, deleted, and confirmed cleanup of one disposable
synthetic Keychain item at `a4161c76075111c462dbe5fef03ddcbd7b2ea193`.
No real provider credential or hosted request was used.

Current-source local inference was rerun at the exact tree promoted as
`0632c16b070ed220b40f07af380891162970961a`, using the existing Hugging Face
cache in place. SmolLM2 135M Q4_K_M (SHA-256
`2e8040ceae7815abe0dcb3540b9995eaa1fa0d2ca9e797d0a635ae4433c68c2d`)
loaded through the real Native host; live-client revocation, joined shutdown,
slot removal, and stopped admission passed. The FTE llama adapter then passed
real chat, cold-to-warm stable-prefix cache reuse, completion, and
`real_local_inference` receipts. No model bytes were copied into the checkout
or application artifacts. This is current-source runtime evidence, not a claim
that the packaged UI exercised a live model.

## Current packaged UX evidence

These checks supplement, but do not replace, the archive candidates above.
They intentionally use local debug bundles with ad-hoc signatures so product
work can advance without waiting for remote packaging or release credentials.

Mom Llama was rebuilt from product-code commit `fc8de70` as bundle
`com.delysis.mom-llama.ux.fc8de70`; its executable SHA-256 is
`fe67432804e05811ff4fadd941659198ebc4a194dfa17b9ba0ec832ff4ed321a`.
The 86-MiB bundle contains no model weights and used the cached SmolLM2 GGUF.
The packaged UI completed a real local chat. Its inspector then reported one
encrypted 30.5-MiB checkpoint and one warm entry. After clean Quit and exact
bundle relaunch against the same isolated root, the conversation and stored
checkpoint were present while the process-local warm count correctly returned
to zero. A separate real-engine restart proof confirmed fresh-process cache
reuse. The revised sidebar visibly exposed 14 Personas and Consult groups,
transferred a landing draft into a persona chat, seeded a group chat with its
`@` handle, retained persona and group `@` autocomplete, and respected the
collapsed Conversations state during search. A follow-up exact-bundle run sent
a real local request and visibly exposed the Stop control before Quit. The
native abort callback ran; the shutdown receipt recorded the Gateway drained,
the operation supervisor closed, one operation worker and one native worker
joined, and all application work drained. The process exited zero in 1,825 ms.
Exact-root relaunch recovered the interrupted prompt as a composer draft rather
than a partial assistant message, and final Quit exited zero in 44 ms.

Loom was rebuilt from product-code commit `fc8de70` as bundle
`app.delysis.loom.ux.fc8de70`; its executable SHA-256 is
`9ff6b164d56fbfb84a57650995aa1c77068b2670cfa5123c58aef85f6e0b18cd`.
The 60-MiB bundle contains no model weights. Its cached Gemma GGUF was
hard-linked into the isolated acceptance root: both paths were inode
`69825204`, 4,954,576,032 bytes, with link count six, so the run allocated no
duplicate model bytes. The packaged UI reached Ready and accepted Quit while
reporting `Suggestions are growing privately`; the exact process exited zero
in 102 ms and deallocated its Metal context. Relaunch recovered the manuscript.
A real suggestion subsequently became visible and was accepted with Tab.
Final Quit exited zero in 106 ms.

Free Token Energy was rebuilt from commit `164fb97` as bundle
`com.delysis.free-token-energy.ux.164fb97`; its executable SHA-256 is
`92f1340f03f137e42f47d68ff800e7e909c865fb021e966bce74ef353959f168`.
The 62-MiB bundle contains no model weights. The native picker selected the
cached SmolLM2 snapshot alias, macOS resolved it to the extensionless
content-addressed Hugging Face blob, and the application verified the file by
its GGUF header instead of trusting a suffix. The packaged Playground completed
a real local request. During a second request the UI visibly reported
`Routing...`; Quit exited zero in 36 ms, invoked the native abort callback, and
deallocated Metal. Exact-bundle relaunch against the same isolated root showed
the local provider Ready with both local requests and token metrics recovered.
Final Quit exited zero in 42 ms. The cache blob remained at its original inode
and no model bytes were copied into the bundle or acceptance root.

## Packaged state backup and rollback

`scripts/product-state-backup.mjs` is the deliberately small offline rollback
tool for all three products. It requires the applicable application to be
stopped, copies state into a new destination, excludes named model formats and
extensionless files with GGUF magic, rejects symbolic links and other special
files, and binds the payload to per-file and tree SHA-256 manifests. Restore
accepts only an absent or empty destination and emits its own digest-bound
receipt. The source state root is never overwritten.

The packaged rollback acceptance used prior build `be641b1` to create one
visible marker in each isolated state root: a Mom draft, a Loom manuscript, and
an FTE local profile name. With all prior applications stopped, all three
backups and independent restores verified. The current Mom `fc8de70`, Loom
`fc8de70`, and FTE `164fb97` bundles then opened the original roots, visibly
recovered the prior markers, replaced them with current-build mutations, and
exited zero. Finally, the exact prior bundles opened fresh restored roots and
visibly recovered the original markers rather than the current mutations.

The preserved local receipt root is
`/var/folders/t0/4s921_v11fv9vlymtx6g5qgm0000gn/T/delysis-state-rollback.XXXXXX.u93jDRmze1`.
The verified tree SHA-256 values are
`f0f19627eafb4a75eb3a576d60063ebd79ff667141ae8ee620bdf44834a2a47f`
for Mom, `4ac641606ff5dc586dc27025a8c7b9d9c6dda00bdd9e3f1ddbcb2aa1171b78df`
for Loom, and
`193af5401084c0a5fe4e4d4cbc688afeeee622414eec8074a286413808abe64e`
for FTE. The restored Mom, Loom, and both FTE SQLite databases returned `ok`
from `PRAGMA quick_check`; every receipt recorded zero model payload files.

## Negative evidence retained

The first current-source Loom packaged smoke did not exit within the original
30-second bound. Sampling proved that Quit had been accepted and was waiting
for an admitted blocking model inspection: acceptance discovery had found the
real 4.60-GiB Gemma GGUF in the user's Hugging Face cache. Commit `61c3b11`
removed Hugging Face roots from explicit acceptance discovery. A retained
WebView preference could still pass the remembered path directly, so commit
`7d39e13` also rejected model paths outside `<acceptance-root>/models` at the
backend boundary. Normal Loom discovery remains unchanged.

The first current FTE packaged UX check selected the intended Hugging Face
snapshot alias, but macOS returned its canonical extensionless blob path. The
pre-fix validator rejected that path solely because it lacked a `.gguf`
suffix. Commit `164fb97` replaced filename trust with a four-byte GGUF header
check in both the application and native engine. Focused suites, strict Clippy,
a real extensionless-blob Metal inference, and the packaged UI run above all
passed. The stored display name is the content hash because macOS discarded the
friendly alias; this is a cosmetic limitation, not a copied-weight fallback.

The final accepted Loom smoke completed both launches and quits in about two
seconds total. Its logs contain no `llama_model_loader` activity. The failed
candidate and smoke logs remain locally under the corresponding generated
`dist/macos/loom-v0.1.0-171d312763cc-20260814T061103Z/` directory.

## Evidence boundary

These are local release candidates and supplemental UX builds, not stable
public releases:

- all three current UX bundles exited during UI-reported active local work.
  Mom additionally emitted a complete joined-shutdown receipt and recovered
  the interrupted prompt as a draft. These runs prove responsive packaged
  shutdown; they do not claim that a cancelled-operation terminal was retained;
- packaged backup/restore rollback passed for all three current-schema state
  roots, including visible reopen by both the current and prior binaries. This
  does not invent a legacy FTE migration path: FTE remains current-schema-only;
- signing is ad-hoc, not Developer ID;
- notarization was not requested;
- no component tag or public artifact has been created yet;
- the older archive smokes prove lifecycle, isolated state reopen, and bundle
  identity only. The current UX checks add exact-bundle real-model behavior for
  all three products, but have not been promoted into new release archives or
  digest receipts;
- prior W6-W8 real-model receipts remain useful regression evidence, but they
  are not relabeled as evidence for these exact binaries;
- macOS is the current supported release platform. Linux CI is informational,
  and Windows is not claimed.

The optional `release-macos.yml` workflow runs only for exact component-version
tags or manual dispatch and rebuilds a distinct remote candidate with its own
receipt. That artifact does not inherit the local package digest or smoke
evidence. The workflow never runs on pull requests and is not a
local-development gate. W9 run
`31774222096` passed policy, macOS, root Linux, Mom Linux, Loom Linux, fuzz, and
the remaining service/dependency lanes at `aca1f85`.

## Retirement boundary

No old source repository is archived by this work. The three candidates above
do not count as either of the two required stable consolidated releases.
Retirement remains blocked by elapsed product evidence, not by W1 history:
after two real stable releases, confirm migration/rollback and issue/security
routing, preserve tags and histories, archive the frozen first-party sources,
and keep `llama-cpp-rs` active and separate.

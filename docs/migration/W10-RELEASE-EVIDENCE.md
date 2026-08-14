# W10 macOS release-candidate evidence

Status: **local macOS candidates accepted; publication and stable releases remain pending**.

W10 now has one deliberately small local release path. From a clean commit,
`scripts/release-macos.sh {mom|loom|fte}` runs the product's migration and
shutdown gates, builds an app-only macOS bundle with the workspace-local Tauri
CLI, rejects embedded model files, ad-hoc signs the bundle by default, verifies
the signature, and writes a digest receipt. It does not require GitHub.

`scripts/smoke-macos-app.sh {mom|loom|fte}` launches the exact packaged child
twice against one isolated product-state root, observes an on-screen window and
product readiness, invokes the application's real Quit menu item (the item
bound to Cmd-Q), requires exit status zero, and verifies that the executable
did not change between launches. The smoke deliberately does not override
`HOME`.

## Accepted local candidates

| Product | Source | Executable SHA-256 | Archive SHA-256 | Archive bytes |
| --- | --- | --- | --- | ---: |
| Mom Llama 0.1.0 | `171d312763ccf89cf8bd0cc5820493fccafa6e9a` | `e7958448b4526a0a5390dafe7903dc55a31d614887d925177eddde3e5572b2bb` | `145ee6aba135f81598ee13a516cc87885b6ce0d284ee6ec5b6cecb4ab74b7132` | 10,896,750 |
| Loom 0.1.0 | `f8ee159de31369c29dc12b24a40a01886a9a02c5` | `f22887eef6f3b21712baf5e6a7ebc079fe57aa42ddfc3b3e7fff99b5f708377f` | `5e030ace600c82c0cebfd017b91bb489ec28d297a12c62dffd9cc2ad04184383` | 8,688,521 |
| Free Token Energy 0.1.0 | `f8ee159de31369c29dc12b24a40a01886a9a02c5` | `d24a63f48d32f6b427aa22cbddf92ecadc7fa64b4db50bb3e5623a531e787e14` | `ca5c1d8779f26a45b43aff82334e872d228d999dfdfb14fd38b53f703531e1a4` | 8,349,209 |

All three bundles declare macOS 13.0 as their floor, contain no GGUF, ONNX, or
safetensors file, and passed `codesign --verify --deep --strict`. The local
artifacts and their separate release/smoke receipts are under `dist/macos/`;
that generated directory is intentionally not committed.

The Mom smoke opened the same encrypted `runtime.sqlite3` file identity from
the isolated root on both launches. Each quit emitted positive
application-drain and native-host join evidence. The release gate also passed prior-store import/reopen,
persistent-cache corruption/reopen, and an active native-operation drain.

The Loom smoke created and reopened one ordinary writing project. Its database
contains two `open_project` receipts after the second launch. The package is
bound to `writer-gemma4-base-v2`, policy file SHA-256
`744fa860ffc979f6c1c9e4e1a96680d31c26b558317755548464df5300b9b791`.
The release gate also passed prior-v10 project migration/reopen, suggestion
promotion/reopen, active-family cancellation/drain, and 178 frontend tests.

The FTE smoke opened the same `gateway.db` and `gateway-v2.db` file identities
from the isolated root on each launch and used process-local credentials instead of Keychain.
The release gate also passed local-model configuration reopen, schema reopen,
native-runtime join, active-router cancellation/drain, and both frontend tests.
Separately, the ignored current-source macOS credential acceptance test wrote,
independently read, replaced, deleted, and confirmed cleanup of one disposable
synthetic Keychain item at `a4161c76075111c462dbe5fef03ddcbd7b2ea193`.
No real provider credential or hosted request was used.

## Negative evidence retained

The first current-source Loom packaged smoke did not exit within the original
30-second bound. Sampling proved that Quit had been accepted and was waiting
for an admitted blocking model inspection: acceptance discovery had found the
real 4.60-GiB Gemma GGUF in the user's Hugging Face cache. Commit `61c3b11`
removed Hugging Face roots from explicit acceptance discovery. A retained
WebView preference could still pass the remembered path directly, so commit
`7d39e13` also rejected model paths outside `<acceptance-root>/models` at the
backend boundary. Normal Loom discovery remains unchanged.

The final accepted Loom smoke completed both launches and quits in about two
seconds total. Its logs contain no `llama_model_loader` activity. The failed
candidate and smoke logs remain locally under the corresponding generated
`dist/macos/loom-v0.1.0-171d312763cc-20260814T061103Z/` directory.

## Evidence boundary

These are local release candidates, not stable public releases:

- signing is ad-hoc, not Developer ID;
- notarization was not requested;
- no component tag or public artifact has been created yet;
- the packaged smoke proves lifecycle, isolated state reopen, and bundle
  identity, but does not claim a live model generation or visible suggestion;
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

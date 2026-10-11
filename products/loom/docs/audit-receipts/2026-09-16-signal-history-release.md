# Packaged Signal history worker

The optimized macOS candidate at `9cad13bebdf4fb551af2c523bc94c42ad615bb11`
built successfully. Its full release gate passed 492 frontend assertions and
the exact promotion and native shutdown checks. This candidate predates the
materials integration from main and does not qualify that later source tree.

An independent safe-Rust harness extracted the ZIP, checked bundle identity,
strict deep code signing, executable and worker hashes, all nine bundled
upstream notice files, source archive hashes and the offline-resolution receipt.
It then ran the exact packaged worker in a fresh isolated profile:

- Protocol 5 reported unlinked; reading conversations failed as unlinked.
- A second vault owner was rejected, while the original owner remained responsive.
- Shutdown exited while the parent's stdin remained open.
- Reopening preserved the encrypted vault; closing stdin shut down the worker.
- The database was ciphertext, and every owned child exited.

| Artifact | SHA-256 |
| --- | --- |
| App ZIP | `2467191cf1cde34e9bad11dd73ceeaf7ec83aff657e47d8265ce70cbf4ce2a11` |
| Native app | `aa7c8526cb51138e981bd393fce992f0720bfa87fc503835971bf76da2006a9d` |
| Signal worker | `96ebaa814f7883e0e5816deb7f20303d6694895d7c972306121f8289ec25b0d6` |
| Signal source archive | `c02c58e1fab2268cf9309b3a223eb07f21cdaf8954c2d427a9e81e77f03c6be2` |
| Independent JSON receipt | `2a99e31e97d7dca95709f7600da59b112b3736ea8d2da721781386d602c31850` |

Local evidence: `/tmp/loom-release-history-selection-final.log`,
`/tmp/loom-signal-history-release.rs`, and `/tmp/loom-signal-history-release.json`.
The candidate is under `dist/macos/loom-v0.1.0-9cad13bebdf4-20260916T120359Z`.
No native UI was exercised in this check, no phone was linked, and no message
was sent. The app is ad-hoc signed and is not notarized.

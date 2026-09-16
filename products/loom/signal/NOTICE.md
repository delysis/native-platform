# Loom Signal worker

The `loom-signal` executable links Presage and Signal's libsignal implementation.
Loom's worker and its modified Presage crates are licensed AGPL-3.0-only. Their
license texts and the Presage upstream revisions, original file hashes, and
modification notes accompany this notice in the app's `licenses/loom-signal`
resources. Other dependencies retain their own licenses and notices.

Each Loom release produced by `scripts/release-macos.sh` includes an adjacent
`loom-signal-source-<Git revision>.tar.gz` and `loom-signal-source.json` receipt.
Keep them with the application when distributing it. The receipt identifies the
exact source revision, archive hash, worker lockfile hash, and dependency inventory.
The archive includes tracked project sources, build scripts, licenses, and all
Cargo sources needed to resolve the worker's locked dependencies offline. Each
vendored dependency retains its own license files. See
`products/loom/docs/networking.md` in that archive for build prerequisites.

From the extracted archive, use the pinned Rust toolchain and run:

```sh
cd products/loom/signal
cargo build --frozen --release
```

This notice identifies included software and source availability. Process
isolation does not determine the legal scope of the combined distribution.

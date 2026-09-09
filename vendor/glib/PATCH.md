# GLib 0.18.5 security backport

Source: crates.io `glib` 0.18.5, checksum
`233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`.
The original `.cargo_vcs_info.json` identifies upstream source commit
`42b9caf98e03ded086362d9653ca58fe94dc8658`.

Only `src/variant_iter.rs` differs from that release: the C output pointer
is mutable and passed by mutable reference. This backports
[upstream PR 1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343), addressing
RUSTSEC-2024-0429. GTK3 in the current Tauri dependency graph requires the 0.18
API; replacing it with 0.20 would not satisfy that dependency.

The workspace `xtask/tests/glib_iterator.rs` regression exercises the real GLib
string iterator with optimization enabled on Linux. Remove this patch when the GTK3 dependency is retired or
a compatible fixed upstream release is available. Review by 2026-12-09.
The original MIT license is retained.

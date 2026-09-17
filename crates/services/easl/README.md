# Native EASL text and rendering

This service contains the EASL compiler/runtime and a reusable text library
written in EASL. **EASL Studio, Cast, Hollow and the Studio host adapter are
excluded from this stack.** Application services, manuscript transactions and
configuration remain in their owning native-kit and Loom Rust crates.

[`easl-text`](easl-text/README.md) provides importable paragraph composition,
font fallback/page selection, glyph placement, editor state/history, selection,
caret geometry and input policy. Narrow native primitives load fonts, shape and
rasterize glyphs, retain immutable UTF-8, expose Unicode/input facts and execute
GPU work. Standalone EASL programs can use the library without a product adapter.
See [the text-library contract](TEXT-LIBRARY.md) for current capabilities and gaps.

[`easl-native-text`](easl-native-text/README.md) supplies the retained Parley/Vello
native path. Loom already consumes shared EASL editing, navigation, style and
final glyph-placement policies. Full EASL atlas-widget adoption is unfinished.
The current UI reference remains
[`PARITY.md`](../../../products/loom/experiments/easl-interface/PARITY.md).

## Source and dependency boundary

The retained vendor packages are EASL, fsexp, HarfRust and Parley. Existing license
notices remain with their sources. EASL and fsexp originated in the local import
recorded in `migration/easl-studio-import.json`; that historical record does not
authorize restoring the removed editor sources. Vendor packages remain excluded
from workspace membership. `xtask policy` rejects the removed Studio dependency
packages and their former source directories.

The Studio-specific preview, source editor, audio host and tests have been
removed. Native EASL runtime regressions live beside the text library and run
without Studio. Historical measurements and import receipts describe their
original revisions; they are not evidence for current application acceptance.

## Verification

From the workspace root with the repository's pinned Rust toolchain:

```sh
cargo test --offline --locked --profile native-view -p easl-text -p easl-native-text -p loom-easl-interface -p fsexp --features easl-text/gpu
cargo clippy --offline --locked --profile native-view -p easl-text -p easl-native-text -p loom-easl-interface --all-targets --features easl-text/gpu -- -D warnings
cargo run --offline --locked -p xtask -- policy
```

The `gpu` checks use offscreen native GPU execution. They do not open an OS
window and do not establish native OS input, accessibility or full Loom parity.

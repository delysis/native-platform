# HarfRust 0.12.0 for EASL font primitives

Imported from the checksum-verified crates.io archive at upstream commit
`60b28ea22b5261710018d69c168a762bcb28794c`. MIT license retained. Archive checksum
and all original file hashes are in `migration/easl-harfrust-import.json` at the
workspace root. This import replaces the same locked registry version.

Local changes:

- Add `Shaper::shape_bounded`, `ShapeLimits`, and `ShapeError`. A caller can cap
  glyph-buffer growth and the existing instrumented operation counter, and detect
  internal exhaustion without receiving partial glyphs. These are resource
  ceilings, not a wall-clock deadline or a bound on every font-parsing operation.
- Enforce the glyph ceiling even when a reused buffer already has enough storage.
- Stop dotted-circle, Thai/Lao, Hangul, vowel-constraint and variation-selector
  loops when output growth fails. Check failure before indexing newly emitted
  glyphs. A real Kawi insertion at a one-glyph cap reproduced a nonadvancing loop;
  the regression sweep exercises small glyph/work ceilings with fresh and reused
  storage across the related script preprocessing paths. The vowel-constraint
  source is generated upstream; retain these failure checks if regenerating it.
- Remove benchmark and Apple platform dev dependencies from the consumer manifest;
  preserve `Cargo.toml.orig` and original manifest hashes.

The archive's `Cargo.lock` is retained as upstream provenance, like the other
registered published dependency snapshot. Only the workspace root lock resolves
this patch for native-kit builds; no dependency version was upgraded.

The existing `shape` API remains available to Parley. EASL's font primitive uses
the bounded API; Swash still rasterizes atlas glyphs. Script selection, line
composition, placement and editor interaction remain EASL library responsibilities.

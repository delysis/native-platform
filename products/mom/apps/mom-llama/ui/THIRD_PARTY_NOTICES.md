# Third-Party Assets

## llama.cpp UI logo

- Source: `ggml-org/llama.cpp`, `tools/ui/src/lib/assets/logo.svg`
- Upstream revision: `3018a11e79e489b657dbb77c95694889ccff92df`
- Source SHA-256: `0a4955422e6affde4811e0c0915f506305d46d084283484970e337bb1282429a`
- Local source: `logo.svg`
- Derived files: `favicon.svg`, `../src-tauri/icons/icon.png`

`logo.svg` is copied verbatim. The favicon adds a white background for legibility,
and the PNG is the deterministic upstream-derived macOS bundle asset.

## Lucide icons

The interface uses the same Lucide icon family as the upstream llama.cpp UI.
Path data is rendered by Rust in `src-tauri/src/view.rs`; no Lucide JavaScript
or Svelte runtime is included. The inspected upstream UI pins
`@lucide/svelte` 1.25.0.

The historical Lucide ISC notice is retained in `LUCIDE-LICENSE.txt`.

### Pinned geometry additions

`chevron-down` and `git-branch` inner SVG geometry is copied from
`lucide-icons/lucide` commit `70562c1ee1c4fdcf736fe97bc893fb8511927934`,
`icons/chevron-down.svg` and `icons/git-branch.svg`. The existing Rust SVG
wrapper retains its size, stroke, CSS and accessibility contract.

The complete pinned upstream `LICENSE`, including its Lucide ISC notice and
Feather MIT terms, is appended verbatim to `LUCIDE-LICENSE.txt`. That license
identifies `chevron-down` as Feather-derived. This bounded addition does not
establish exact originating revisions for the older embedded glyphs.

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

The Lucide ISC license is copied verbatim in `LUCIDE-LICENSE.txt`.

# Third-Party Assets

## llama.cpp UI favicon

- Source: `ggml-org/llama.cpp`, `tools/ui/static/favicon.svg`
- Upstream revision: `4f13cb742476d81a6b42a2aa5996e82a478c2481`
- Source SHA-256: `6438dff1b4d1674838d17ec7f7c764e70d2a7ad84f99e40c1ca4c66cb1f79f0a`
- Local files: `favicon.svg`, `../src-tauri/icons/icon.png`

The SVG is copied verbatim. The PNG is a deterministic rasterization used by
the macOS bundle.

## Lucide icons

The interface uses the same Lucide icon family as the upstream llama.cpp UI.
Path data is rendered by Rust in `src-tauri/src/view.rs`; no Lucide JavaScript
or Svelte runtime is included. The upstream UI pins `@lucide/svelte` 0.515.0.

The Lucide ISC license is copied verbatim in `LUCIDE-LICENSE.txt`.

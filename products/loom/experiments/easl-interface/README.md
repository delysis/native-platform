# Retired Loom EASL experiment

The standalone EASL editor, the `loom-markdown` SDK and the
`loom-text-session` SDK were retired on 2026-10-12 by an explicit product-scope
decision. Their code, Cargo targets and public APIs are no longer maintained
in this repository. External API breaks are accepted; no compatibility layer
or replacement dispatcher is provided.

This withdraws the independent winit/Softbuffer editor, native AccessKit/input
integration, EASL view and optional Wry companion, plus the lossless Markdown
transaction and bounded project-worker SDK surfaces. The supported Loom editor
remains the Tauri application in `products/loom/apps/loom`. Its Store, Document,
Host, Types, preferences, native model, import, speech and peer owners remain.
Keeping that product does not claim native EASL parity or packaged acceptance.

The dated handoffs, parity/performance notes and verification records here
remain unchanged historical evidence. Their old launch commands and source
references do not describe a current build target. Original implementation and
public interfaces remain recoverable in Git at
`979a7cc9d8ec206acad5f86e3839c840faae8d5a`.

See [the current retirement decision](../../../../docs/architecture/LOOM-EASL-RETIREMENT-2026-10-12.md).
Canonical EASL research remains in the separate `delysis/easl` repository.

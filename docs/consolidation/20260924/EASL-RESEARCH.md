# EASL preservation, not product takeover

The primary inspected head is `54b955336590e97c04f40f2548c8a271d9c2b2ba`
(PR #73). The accessibility lock-delta head
`3fd6d84f4e99fa340d3b0b345e45712ded917374` (PR #81) and its companion
`f9cff570edc1ac60f4965fc96c59f46d5d043e62` must be preserved, not blindly
resolved with an "ours" lockfile. The older native head
`78392f82762193b3c3f107d981c280f15d727b5d` is separately inventoried.

Keep reusable `crates/services/easl` text/input/accessibility/layout work and
its transitive path/vendor dependencies, original notices, tests and raw
acceptance receipts. Park `products/loom/experiments/easl-interface` as product
research. Absence from main is not permission to delete its only remote ref.

`tools/preserve-easl.mjs` exports all requested exact heads into an independent
bare repository and self-contained Git bundle. The source archive contains
service code, vendor material and root manifest/lock/license context but omits
the Loom-specific experiment. This is a preserved source slice, not a claimed
standalone-buildable replacement workspace. Dependency closure, platform
shaping/input/AX acceptance, generic Tauri embedding API and licensing review
remain required before promotion. A complete branch bundle retains everything
excluded from the focused archive, including old product experiments.

Promotion boundary: isolate a host-neutral text/editing API, inject platform
input/accessibility adapters, retain grapheme/UTF-16/UTF-8 coordinate contracts,
composition/caret/selection/undo behavior and render evidence. Qualify native
input and accessibility independently of synthetic widget tests. The normal
Loom renderer and its acceptance suite remain unchanged by this stack.

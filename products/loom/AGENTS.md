# Loom Native development rules

- All project-owned Rust is safe Rust and must retain `#![forbid(unsafe_code)]`.
- Prefer explicit state machines, bounded collections and typed failures.
- Manuscript files must remain ordinary readable UTF-8 files.
- Never mutate immutable artifacts, operations, revisions, or receipts in place.
- Never let automation modify the active manuscript without an explicit promotion.
- Keep local inference and editing paths free of subprocess and loopback dependencies.
- Establish the real writing baseline before an audit/refactor batch. Integrate
  small changes and recheck the core journey before moving to another feature;
  do not let a long batch accumulate behind component-only results.
- Before handing over any Loom build, exercise that exact native bundle with the
  cached Gemma writer: type ordinary prose, observe real ghost text and four
  alternatives, cycle with Option-Up/Down, accept, undo to the original bytes,
  and confirm writing survives reopening. Keep the writer warm and reuse one
  build; do not rerun a workspace test suite for each interaction.
- Component tests with supplied candidates prove editor behavior, not inference.
  A model-free fixture or a built bundle must never be described as a working
  writing app. Report missing native acceptance explicitly. When this core path
  fails, repair it before expanding scope or handing over another build.
- Keep one verified native app available for the user. Do not launch a second
  model-free editor for unrelated UI checks on their desktop. Reuse the loaded
  writer and preserve the current writing; isolated data is not a reason to
  disable the product's headline feature.
- Keep the macOS smoke helpers aligned with actual shortcuts and controls in
  the same change that modifies those controls. Do not add visible product
  controls just to satisfy an obsolete test harness.

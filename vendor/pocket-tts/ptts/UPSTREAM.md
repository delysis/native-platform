# PocketTTS native source provenance

Vendored from [gradium-ai/xn-ptts](https://github.com/gradium-ai/xn-ptts), qualified local commit `3a43c2769c44ebb1ba268d43dc6ce4e3ca613dce` (including the English BOS correction and cancellable stream polling), package `ptts` version `0.4.0-rc.3`. The upstream checkout contains MIT and Apache-2.0 license texts; both are retained here. The Rust package source retains the qualified implementation, with mechanical formatting applied using the workspace-pinned Rust 1.95 formatter.

This directory intentionally excludes the upstream CLI, Python bindings, Core ML graph builder, server, WebAssembly frontend, examples, and package-specific workspace. The dependency manifest below pins inference dependencies and app-appropriate features, while common utility dependencies retain the workspace lockfile versions; transitive resolution is recorded by the product Cargo.lock.

The model weights and persona references are separate artifacts and are not covered by the Rust code licenses.

# Mine project configuration

Mama Llama is deprecated; Loom is the implementation destination. The `loom-config` crate owns the readable `.mine.toml` schema. Reading settings never creates a file, downloads a model, grants a tool, or changes a manuscript. Missing settings inherit the quiet writing defaults. Invalid fields, samplers, values, references, paths, or versions fail with a typed error; the author's file remains unchanged.

```toml
version = 1

[workspace.panes.chat]
visible = true

[assistance]
suggestions = true

[generation]
chat = "editor"
automatic_prose = "editor"
manual_writing = "editor"

[profiles.editor]
context_file = ".mine/personas/editor.md"

[profiles.editor.sampling]
temperature = 0.7
min_p = 0.05
sampler_order = ["penalties", "top_k", "top_p", "min_p", "temperature"]

# Explicit model loading remains a separate application action.
# [model]
# path = "../models/writer.gguf"
# context_tokens = 8192
# batch_tokens = 512
# max_sequences = 4
```

There are at most 64 profiles, each named with 1–64 ASCII letters, digits, hyphens, or underscores. Profile context names one Markdown file under `.mine/personas/`; a selected file is read as exact UTF-8 bytes with a 256 KiB limit. Settings have a 64 KiB limit. Reads reject observed symlinks and nonregular files and bound allocation even when file size changes. This is ordinary local filesystem validation, not a claim of race-free protection against a process concurrently replacing project directories.

The schema exposes all sampling controls implemented by `desktop-generation-policy::SamplingOverrides`, with native validation for every supplied value. Defaults are task-specific, not claimed optimal. A frozen generation profile records the configuration source hash, named sampling-definition hash, and a separately hashed exact context copy. Document revision and native applied-generation evidence remain separate authorities. A profile is not a prompt-format or permission selector.

`workspace.rs` preserves Loom's existing @document reference parser and bounded pane rules, makes chat/terminal/browser opt-in, and retains the one-main-editor constraint until a main-chat consumer exists. Model fields are requested settings for an explicit load; model identity and machine admission remain the loader's responsibility. Assistance omission inherits existing per-project behavior.

This commit provides the parser, validation, frozen-input API, and boundary tests. Application consumers for workspace setup, model loading, and profile generation are tracked in subsequent integration commits; the mere presence of a schema field does not establish that a UI action or native generation has been accepted.

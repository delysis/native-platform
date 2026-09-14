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

The production Weave and terminal/chat paths now freeze the selected profile once before consuming automatic budget, writing generation artifacts, or starting a worker. Profile sampling values override request defaults. A configured seed is a family seed: each branch adds its index, so a fixed seed does not create four identical branches. Automatic suggestions still admit at most 48 output tokens; manual writing and chat admit at most 2048. Requests above those bounds fail rather than silently resetting the preference.

Selected persona bytes are included as authored context, separately framed from retrieved attachment excerpts. Weave reserves their exact byte cost before attachment retrieval using the existing conservative one-byte-per-token allowance; the native tokenizer remains the final admission authority. Terminal prompts include the exact persona preamble in the persisted prompt blob and enforce both the prompt byte limit and resident budget. Required chat speaker stops are appended to, rather than substituted for, configured stop strings.

New Weave context evidence records the frozen profile and original request defaults. A replay reads those immutable records: later edits to the dotfile or removal of the source persona file cannot change a recorded run. Changing the request defaults under the same command ID remains a conflict even when an explicit profile value would override them. Earlier generation evidence without profiles continues to describe its original built-in defaults; it is neither rewritten nor reinterpreted using new settings. Terminal receipts similarly own the original frozen profile across every step.

## Setup during model download

Starting the recommended model download opens an optional, two-step setup card while the verified model/projector transfers continue. The first choice is the writing page alone or chat beside it; the second is inline suggestions or assistance only when requested. The default is the page with suggestions. The card leaves the manuscript editable, reports the transfers started by this flow, and offers Back, Skip, and download details. Skip neither creates settings nor cancels a transfer. This is a fixed configuration flow, not an arbitrary script execution facility. Restarting the application does not resume the card.

Finishing creates a commented `.mine.toml` from those choices. An existing file is adopted byte for byte, even when invalid, rather than overwritten. Missing settings keep the quiet writing layout. Invalid settings preserve the last valid layout and expose a repair action. The settings source always uses the plain source editor, including in configured panes, with automatic suggestions suppressed for that document.

Command/Control-comma opens the actual file. Settings updates wait for active composition and draft flushes, and a stale asynchronous read cannot replace a newer snapshot. Explicit `[assistance]` values override the remembered per-project choice; omission inherits it.

`[model]` settings are consumed by explicit loading through the existing native model owner. Command/Control-Shift-R rereads settings and reloads the selected verified writer with the requested configuration. A configured path is not silently replaced with another discovered model. Invalid candidates leave the previous resident intact; changing settings for the same GGUF retires only the exact old native configuration. Supported fields and admission limits are documented in `model-configuration-comparison.md`.

These source and component-browser checks do not establish installed native UI acceptance, a real model download/reload, or measured generation quality.

## Reusable co-writers

The existing co-writer commands now use a frozen `workspace-document::DocumentSnapshot<ContextSources, ()>` containing exact context bytes, a revision ID and parent revision, source lineage, attachment identities, and text-import receipts. Its generation profile is a separate typed value. Applying copies both into the target's existing context record in one atomic write. Later library edits or deletion cannot modify the target's applied copy. Generation evidence also owns that source snapshot. Ordinary edits to the target's context retain the original preset provenance and use the newly edited context through the normal retrieval receipt.

Named `.mine.toml` profiles with a `context_file` participate in the same co-writer list/apply command path. They are marked `configured`; deletion returns an instruction to edit the dotfile, never rewrites the author's config. Their context is copied into editable document context and marked as already present, so it is not also injected as a second persona preamble. Applied sampling remains frozen even if the original profile or file is edited later. Saving a context without an already applied profile freezes the project's manual-writing profile; it does not pretend to recover the profile of an arbitrary earlier run.

Weave consumes the applied context through its normal attachment/context budget. Terminal and chat capture the applied context's bounded rendering and retrieval receipts once before their worker begins, preserving source snapshots and native media identities. Applying changes neither the manuscript nor model identity, prompt grammar, or tool authority.

The named library remains a bounded local JSON projection (64 names, 40 MiB serialized limit). Its current schema is `loom.co-writer-profiles.v2`. Earlier unreleased v1 files are rejected and preserved; this change contains no silent conversion or deletion. Updates create a new source revision linked to the prior revision. Applied copies retain their old payload, rather than looking up whichever library entry currently has the same name. There is no speculative history UI or separate binding database.

Access boundary: `.mine.toml` task-to-profile selection is a live production control. Co-writer save/list/apply/delete remain existing IPC capabilities; the current quiet application has no rendered co-writer picker. This work does not claim a visible co-writer menu or completed native interaction acceptance.

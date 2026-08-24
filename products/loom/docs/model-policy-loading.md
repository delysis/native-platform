# Build-policy model loading

Automatic writer selection is stricter than the manual `model_load` command. The desktop build embeds a closed, typed policy containing ordered model identities, never filesystem paths. A policy load must name one profile in that policy and one GGUF found through bounded runtime discovery.

The default desktop policy is `writer-gemma4-base-v2`. Its `quiet_default` activation means the quiet editor may offer local suggestions without first exposing model-management controls. It does not weaken project focus, privacy, budget, promotion, or capability gates. The immutable `writer-gemma4-base-v1` policy remains available with its original `project_opt_in` activation; Loom rejects a v1 document whose activation has been silently changed.

`LOOM_BUILD_MODEL_POLICY` selects only an allow-listed checked-in policy at compile time. The build script validates every policy, canonicalizes the selected bytes, and binds the embedded policy to its name and SHA-256. It cannot accept a model path. Runtime model discovery remains separate and considers bounded Hugging Face caches, Loom's app-local model library, files selected by the user, and the development/test-only `LOOM_GGUF_MODEL_PATH` process environment variable. None of those runtime paths are compiled into the application.

The renderer can read policy identity through the read-only `build_model_policy_get` command. Rust constructs its name, activation, and canonical digest as one closed value. The frontend decoder admits only the exact checked-in name/activation/digest triples, and preference derivation remains disabled until it receives one; a missing command, unknown field, new name, activation mismatch, or digest mismatch therefore fails closed.

Discovery and loading alone do not grant automatic-generation authority. Rust can create the private, move-only automatic-writer witness only after matching the resident model's exact digest and byte length to the closed build policy and proving raw completion plus generated-token support. Budget reservation requires a borrow of that witness, request construction consumes the authorized model, and native submission consumes the resulting opaque request. An arbitrary manually loaded completion model remains available to explicit advanced/manual workflows but cannot become the automatic writer. The renderer independently follows the same hierarchy: only the exact policy-verified profile is eligible to schedule quiet suggestions.

Before invoking llama.cpp, Loom:

1. resolves the policy profile before touching the supplied path;
2. requires one canonical regular file, with no final symlink;
3. opens an identity handle, checks the exact byte length, and hashes exactly those bytes with SHA-256;
4. verifies that the canonical path still names the same file with the same length and modification stamp.

After native inspection, Loom repeats the path binding check and requires the native descriptor to report the same canonical path, byte length, and SHA-256. The descriptor must also prove raw text completion and generated token IDs for the current writer policy. A failed check releases any staged native state and restores the previously selected model; it never commits the candidate to Loom's model registry. Size matches shown during discovery are explicitly unverified hints and disappear after inspection.

Branch text crosses an unavoidable runtime IPC boundary. Before that text can enter ghost-suggestion selection, the renderer hashes its UTF-8 bytes and requires an exact match with the canonical lowercase SHA-256, byte length, and run identity recorded by the store. Only the branded verification result is accepted downstream; same-length substituted text and cross-run bodies fail closed.

## Embedded download catalog

The desktop exposes one read-only, versioned catalog snapshot. Its single entry
pins Google's `gemma-4-12B-it-qat-q4_0-gguf` repository at revision
`29d097773436b69ff9feafd636ab4cf873786537`, artifact
`gemma-4-12b-it-qat-q4_0.gguf`, exact byte length, SHA-256, Apache-2.0 license
information, 262,144-token context metadata, and a 16 GiB system-memory
recommendation. The download URL names that immutable revision; the hard byte
ceiling equals the expected byte length.

Catalog display grants no network or model authority. The renderer validates
the bounded snapshot before constructing the existing verified-download
command, and only an explicit button press starts HTTPS access. Installation
still requires the exact cold SHA-256 and GGUF checks, and use still requires
native inspection through the catalog identity. Native admission re-hashes the
canonical file against the embedded SHA-256 and byte length, rechecks the path
binding around inspection, and requires the returned native descriptor to
carry the same identity. The renderer compares that returned digest again
before selecting the model. An existing local file with the same artifact name
and byte length remains a legacy discovery hint only: it can offer the exact
catalog-verification action but never enters generic admission or inherits the
catalog checksum. The older exact build-policy writer remains an independent
fallback.

The autocomplete control remains a direct on/off switch. Its native context
gesture (secondary click or touch/pen hold) and keyboard context gesture (Menu
or Shift-F10) open the same local-model dialog exposed through Settings. The
Settings gear remains present because it also owns the independent appearance
choices.

Appearance is a renderer preference with exactly three persisted values:
`system`, `light`, and `dark`. Missing, invalid, or unavailable preference
storage resolves to `system`; explicit light or dark remains selected across
launches until the author explicitly chooses `system` again. It is not project
or manuscript state. A same-origin synchronous bootstrap applies the stored or
system-resolved theme in the document head before application code and the
first paint.

## Residual mutation limit

The native loader currently accepts a filesystem path, not Loom's already-open file handle. Another process with write access can therefore race in the small interval between Loom's last pre-native binding check and llama.cpp opening that path. The post-native identity checks prevent that result from being committed under the policy identity, but they cannot guarantee that native code never parsed bytes from a raced replacement.

Closing that final gap requires either an upstream native API that loads from the verified OS file handle, or copying the verified bytes into an app-owned immutable staging file and loading only that file. Until one of those contracts exists, policy models should live in an app-owned directory that untrusted processes cannot mutate. Pre-hashing is mandatory in every case.

# Build-policy model loading

Automatic writer selection is stricter than the manual `model_load` command. The desktop build embeds a closed, typed policy. A policy load must name one profile in that policy and one discovered local GGUF file.

Before invoking llama.cpp, Loom:

1. resolves the policy profile before touching the supplied path;
2. requires one canonical regular file, with no final symlink;
3. opens an identity handle, checks the exact byte length, and hashes exactly those bytes with SHA-256;
4. verifies that the canonical path still names the same file with the same length and modification stamp.

After native inspection, Loom repeats the path binding check and requires the native descriptor to report the same canonical path, byte length, and SHA-256. The descriptor must also prove raw text completion and generated token IDs for the current writer policy. A failed check releases any staged native state and restores the previously selected model; it never commits the candidate to Loom's model registry. Size matches shown during discovery are explicitly unverified hints and disappear after inspection.

## Residual mutation limit

The native loader currently accepts a filesystem path, not Loom's already-open file handle. Another process with write access can therefore race in the small interval between Loom's last pre-native binding check and llama.cpp opening that path. The post-native identity checks prevent that result from being committed under the policy identity, but they cannot guarantee that native code never parsed bytes from a raced replacement.

Closing that final gap requires either an upstream native API that loads from the verified OS file handle, or copying the verified bytes into an app-owned immutable staging file and loading only that file. Until one of those contracts exists, policy models should live in an app-owned directory that untrusted processes cannot mutate. Pre-hashing is mandatory in every case; the optional build-time writer path only extends bounded discovery and never bypasses verification.

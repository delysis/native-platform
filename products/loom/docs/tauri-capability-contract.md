# Tauri capability contract

Loom's frontend command surface and its packaged ACL are one build contract. A
frontend call that reaches a Tauri core command must be authorized by the
capability assigned to the `main` window.

`scripts/check-tauri-capabilities.mjs` extracts production calls through the
installed `@tauri-apps/api` package, derives their real `plugin:*` command names,
and resolves `src-tauri/capabilities/default.json` through Tauri's generated
`acl-manifests.json`. It fails on either an unclassified API call or a command
that the packaged window cannot invoke. The normal Loom frontend suite and the
macOS release script both run this check.

The packaged completion smoke remains the runtime backstop. Its Option-Right
journey rejects any visible ACL/transport failure after accepting a word. This
separates the two useful proofs: the static gate finds capability drift before
packaging, while the native journey proves the shipped window can execute the
interaction without surfacing a transport error.

When adding a Tauri API call, change the capability in the same commit. Do not
catch and hide ACL failures: they indicate that source and package authority
have diverged.

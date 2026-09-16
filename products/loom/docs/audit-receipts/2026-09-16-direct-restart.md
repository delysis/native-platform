# Native ownership and direct reconnect

Fresh upstream fetches confirmed that main at
`fb3a1b2d5d64b85d148d064cda908912b689347c` is already incorporated in
`codex/loom-cabals-signal`.

## Native reproduction at ca034b7

The optimized release archive at `ca034b734f2b02651cb1c1b7bd085f4141ff88f8`
was independently checked for exact executable, worker, resources, corresponding
source and ad-hoc signature. Its ZIP hash is
`c5941b98211a93db9569d1dcdfe8ffba61e45095e4676065a02cd7df9a48d787`.
The artifact receipt is `/tmp/loom-release-ca034b7-artifact-check.json`.

Two isolated copies used bundle IDs `app.delysis.loom.releaseca034b7.owner`
and `app.delysis.loom.releaseca034b7.peer`, with separate synthetic profiles.
Exact executable paths, hashes and PIDs are recorded in
`/tmp/loom-native-pair-ca034b7.json`. Current native accessibility controls
included **Hand over the keys**. No model was loaded.

Sage created Garden and invited Fern over direct connections. Both editors
displayed the same two paragraphs. Sage reviewed Fern's exact device key and
confirmed the handoff. Fern then displayed **Owner · You** and gained invitation
and removal controls. Sage displayed **You**, without administrative controls.
After restart, Fern retained ownership and Sage retained ordinary membership.

Independent offline edits remained in ordinary Markdown: Sage added lilies to
the pond; Fern added lanterns to the willow. Restarting both direct endpoints
did not reconnect them: their local UDP ports changed, and neither saved peer
address reached the new listener. This is failing-before native evidence, not a
successful convergence receipt. Both applications were quit, their divergent
writing preserved, and no owned app process remained.

## Repair

The native profile now starts direct transport with atomically persisted local
ports under its existing exclusive lease. State is committed before serving any
cabal. Internet and custom-relay modes retain their existing discovery behavior.
The record is bounded, versioned and bound to the device key. Corrupt, symbolic,
unsupported and wrong-device records are refused without replacement. A busy
saved IPv4 port fails rather than silently stranding offline peers. IPv6 is
optional and retains its saved port while unavailable. A failed state write
closes the endpoint before returning the error.

This repairs process restart on a still-reachable network. It does not discover
changed IP addresses or NAT mappings, operate a relay, or expand membership.

The focused test `direct_profiles_merge_offline_edits_after_both_processes_restart`
creates a real QUIC pairing, shuts down both endpoints, reopens both stores,
edits each paragraph independently and starts both endpoints again. Their
supervisors converge using only persisted addresses, without a new invitation,
manual sync or refreshed peer hint. Two further tests cover occupied-port retry
and preservation of invalid records. All three passed in
`/tmp/loom-direct-restart-focused.log`.

The consolidated `cargo test --locked -p loom-cabal -p tauri-plugin-loom`
passed 378 tests, with five existing ignored plugin cases. This includes the
large archived-history, peer compute, causal sync and ownership suites. The log
is `/tmp/loom-direct-restart-consolidated.log`. Strict Clippy for both packages
and all targets passed in `/tmp/loom-direct-restart-clippy.log`. Formatting,
`git diff --check`, current-document validation and its four tests also passed.

## Windows ownership test race

Required macOS CI and dependency audit passed at `ca034b7`. The advisory Windows
lane in run `35062655944` failed in the ownership-delivery test. Reproduction in
`/tmp/loom-owner-sync-race-before.log` showed that automatic sync had already
delivered revocation before a second manual sync checked membership. Correct
production refusal then failed the test. The test now observes the supervisor's
signed-roster delivery directly, shuts down both endpoints and asserts exact
authority and removal. No production membership check was weakened. All nine
ownership tests passed in `/tmp/loom-owner-sync-race-after.log`; strict Clippy
for that target also passed.

## Boundaries

Post-repair packaged native convergence, native archived-history acceptance,
phone-linked Signal, physical Internet peers, physical simultaneous typing/IME
and lost-owner-key recovery remain separate acceptance work. This run did not
link a Signal account, send a message or edit a real group.

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

The following native receipt closes same-Mac archived-history, direct restart,
ownership handoff and successor revocation acceptance for this revision.
Phone-linked Signal, physical Internet peers, physical simultaneous typing/IME
and lost-owner-key recovery remain separate work. This run did not link a Signal
account, send a message or edit a real group. These local checks do not establish
connectivity across separate physical networks or complete feature acceptance.

## Optimized release and native archived late join

The complete candidate script passed at
`dc08fcd21c0d2806d2440fe4bd14582e604c3d46`, including the exact promotion and
shutdown gates, 491 frontend tests, optimized native build, worker/source
packaging, frozen offline dependency resolution and ad-hoc signing. The log is
`/tmp/loom-release-dc08fcd.log`. Independent extraction verified executable,
resource, corresponding-source and signature identities in
`/tmp/loom-release-restart-artifact-check.json`. The packaged worker bytes are
unchanged from the previously exercised encrypted-store/shutdown worker; that
exact component receipt is reused, without claiming another worker run.

Artifacts are under
`dist/macos/loom-v0.1.0-dc08fcd21c0d-20260916T103524Z/`:

* ZIP: `d730aa6c2b6e23061f9fe71f24440f217a095a1ef63f28ff6376727bcdf09587`.
* Signed app executable:
  `e5e80840215c016f7642c5304202f7c9fb5d92e209a36f79938209bafe1763dd`.
* Worker: `748c26f4f0f4bff9de5bd4378ccfff1fea22d090acb44c84805ed1171a260722`.
* Signal source archive:
  `ce2785d6c55213b424e6741b02bc47164f4934b48ddb9fa272bc9df66fd12206`.

Two isolated copies use `app.delysis.loom.restartdc08fcd.owner` and
`app.delysis.loom.restartdc08fcd.peer`. The native owner created Garden and an
invitation, then quit. An isolated Rust helper, holding the profile lease, used
the production cabal library to add 1,100 synthetic edits. This prepares deep
history; it is not a claim that a person typed those edits in the application.
There were 1,101 signed envelopes, including 513 archived rows.

The native owner reopened that history and projected **Archive entry 1100** into
the editor. The peer then joined using the invitation created before the owner
quit. Both ordinary Markdown files matched, and both databases retained the
same 1,101 exact envelope hashes. Their archive layouts differed normally (513
and 589 archived rows); each compressed or uncompressed row decoded to its exact
original envelope hash. No replacement invitation or refreshed address was
provided. Exact profile, bundle, executable, PID and state evidence is in
`/tmp/loom-native-direct-restart.json`.

## Native offline restart and successor revocation

Fern opened the caught-up Garden document in the native editor. Sage reviewed
Fern's exact device key and confirmed **Give Fern the keys**. Sage's invitation
and ownership controls disappeared; Fern displayed **Owner · You** and gained
those controls. Both copies retained the archived document.

Fern quit. Sage changed the first paragraph to **Pond with lilies**, saved, and
quit. Fern restarted and changed the second paragraph to **Willow with
lanterns**, while Sage remained stopped. Read-only file and database inspection
confirmed genuinely separate versions, each holding 1,102 changes. Fern's
ownership remained intact after restart.

Sage restarted and reopened Garden. Without a new invitation, manual sync,
changed connection setting or injected address, both native editors displayed
both changed paragraphs and **Archive entry 1100**. Their ordinary Markdown
files were byte-identical (SHA-256
`8bb787725615cf0d34de909dfd8ce4ee03c75246033a4f4438d74d2409c12f05`).
All 1,101 prepared envelopes remained present. Both stores held the same 1,103
exact envelope hashes, whose sorted-list digest was
`3550391a8d5a2b59899a3457a74aa813fc6961caae112d0d1b98099491b80457`.
Their IPv4 and IPv6 listen-port records were unchanged from initial pairing.

Fern then reviewed and confirmed **Remove Sage**. Sage received the successor's
signed revocation and displayed **This device is no longer a member. Your
existing text remains here.** The native editor became read-only, document
creation and recording were disabled, and **Recover my copies** remained
available. Both stores contained the same signed roster, with one delegation,
Fern as owner and Sage absent. The Markdown and complete signed history were
unchanged by removal. This verifies preservation of accepted writing; it does
not claim a new native orphan-recovery interaction.

Both owned apps quit, and their exact executable paths had no remaining process.
No model was loaded. The completed machine-readable native receipt is
`/tmp/loom-native-direct-restart.json`, SHA-256
`7b9f2ba62ba542145f03ff972819e6904a93f348c79e6dce8998460748f0bea3`.
Native UI access was coordinated with the audit task; its Materials Check app
was left untouched.

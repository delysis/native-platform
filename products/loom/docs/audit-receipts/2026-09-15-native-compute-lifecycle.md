# Native peer-compute lifecycle after upstream alignment

This receipt covers two actual macOS applications built from clean revision
`53a32bbcd4c53a5753c2a451442f271919e8226b`, incorporating main at
`fb3a1b2d5d64b85d148d064cda908912b689347c`. The applications ran on one Mac;
this is not physical Internet, phone-linked Signal, or release certification.

## Identity and setup

Both ad-hoc-signed debug bundles came from the same Tauri build. They used
separate existing synthetic profiles, saved membership, and the same shared
Garden workspace. Neither device needed another invitation. The requester had
no resident model. The host loaded Gemma 4 12B It with its verified projector
and advertised text, images and audio. These jobs used text input.

| Identity | Owner | Peer |
| --- | --- | --- |
| Bundle ID | `app.delysis.loom.lifecycle53a32bb.owner` | `app.delysis.loom.lifecycle53a32bb.peer` |
| Executable SHA-256 | `650bf4050594f4b13ab5d46bf06d651ace45d36ffc609bc56e788b7ff45f9566` | `cfe92ec15a21a7c59b2b774e7727901d263fd45724fcc4804e7a8fd54af074b8` |
| Initial PID | 44557 | 44825 |
| Restarted PID | 47558 | 50402 |

The frontend asset `index-ZoUUGaSI.js` had SHA-256
`f0a5411fd6e4b6c3647c822f990fbcae7a99d26871cc750aa9bdacf77c5abd04`.
The native accessibility tree showed the merged single Ghost text control and
shared-recording notice. Cmd+Shift+C exposed Materials and its existing
Share context as a document action; the sidebar started hidden.

The reviewed grant `b78eac8d-f967-4699-9ae2-99eb036a1f40` admitted Fern to four
jobs, each limited to 512 output tokens and 120 seconds. Its model claim was
`a5743813ea34327288d8eca46af87e949887b8723b1b1e74b858cd9b1c456612`.
Native evidence records Metal execution of model SHA-256
`93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b`
and projector SHA-256
`cb018338a7538a9814d994bfe54644c71eb7ed54e31eae2f721e45fd3c260da7`.

## Exercised behavior

Each job was observed in signed Running state before the interrupting UI action.
The host's retained native evidence records a cancelled continuation in all four
cases; the protocol result separately describes why the remote job stopped.

| Job | Native UI action while running | Final signed host status | Retained native tokens |
| --- | --- | --- | --- |
| `d7f29315-5fbf-f3c8-ec9e-9f1d918ebb50` | Start local terminal generation | Failed / HostBusy | 413 |
| `a2ec74cd-4bcb-1908-edac-abb0c1d268f0` | Requester Ctrl+C, then Check | Cancelling / Requested, then Cancelled / Requested | 168 |
| `ae5b2c2f-32c3-fcfc-1b0a-998c3671bec5` | Start a local prompt with the GGUF's chat framing | Failed / HostBusy | 310 |
| `d61fe8f2-52e5-9af3-be7e-7cf2ea1840e0` | Host Cmd+Q | Failed / HostBusy | 310 |

The first local prompt hit the existing numeric-degeneration guard. A setup
attempt that typed multiline framing also started a partial local prompt; it
was cancelled and its record was preserved. Pasting the complete framing for
the third peer job then produced the local response: “The lantern's warm glow
reflected softly on the still surface of the pond.” Its retained output is
`Runs/01M2M33J2Q2BXHMNXW90YBTA6F/1/turnuser.md`. This establishes actual model
takeover and a successful local response, without treating the earlier failures
as success or changing the active manuscript.

Explicit cancellation first showed an uncertain delivery notice. Check recovered
the saved terminal receipt and displayed “The peer experiment was cancelled.”
The shutdown case drained the adapter before the network service stopped, so its
actual protocol result was HostBusy; it was not a HostStopping receipt. Host
PID 44557 and its unlinked Signal child 44560 exited.

Reopening both exact bundles restored the saved cabal. The owner showed Fern
connected and “0 of 4 jobs left · Budget used”; the peer showed Moss connected.
Its terminal restored all four outcomes, including the cancellation. Read-only
inspection found exactly four host jobs and four requester records before and
after restart. The requester's signed receipts were unchanged. Reopening history
did not rerun work or replenish the grant. The exhausted grant was then explicitly
revoked through the host UI. Final Quit closed owner 47558, Signal child 47559
and peer 50402; process inspection confirmed all three exited.

`Context Check.md` retained SHA-256
`f9e69b25a90b6627aa7eaac5428f710de294c8d3028d376ea248251031c49703`.
The separately user-edited `Garden.md` retained SHA-256
`97c5e4b43f3f6eae99209f9e0a1d2b2ef178fff5544749917a7e84b094139b28`.
Existing writing, recordings, shared context and local test outcomes were kept.

## Evidence and follow-up

The detailed local receipt `/tmp/loom-native-compute-lifecycle.json` has SHA-256
`4f5d0f360dce122b67e55adecc62de196f1c7c5147385be3b1f49fb79a8ab167`.
It records the bundle paths, exact signed receipts, native execution evidence,
local run receipts, process identities, restart counts and manuscript hashes.
The four native evidence blobs, in the job order above, have SHA-256:

- `4ee19457d0328662061c0d34da85243d2ddc0aac4e23cd631b0fe5c47b20434c`
- `dd7b89c66743fab678e937b127f5cf713d2b2bb4df9b6fa0242c5645c2b3776a`
- `91bd540ab9ab130959eadf1328c4589ef88342fabba7a59700737b8b18ad6b68`
- `55fc813382f828017af1d7ba0799e4504dce89f745ace4cea9b93c9016571dbb`

The observed peer UI exposed the internal `Failed { failure: HostBusy }` text.
The follow-up replaces debug formatting with exhaustive plain-language failure
messages; it preserves immutable old run receipts. Six focused terminal/QUIC
tests, strict Clippy for all plugin targets, formatting and documentation checks
passed. Those tests use a labelled fixture executor; they do not establish native
model behavior or visually certify the later wording.

Required macOS CI and the dependency audit passed at `53a32bb`. The preceding
integration receipt records the broader local gate. Phone-linked Signal,
physical Internet peers, long-lived history/compute archival, owner-device
recovery/transfer and final release acceptance remain open.

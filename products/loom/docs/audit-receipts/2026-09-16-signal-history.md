# Signal history pagination

The previous projection selected the requested number of stored records before
filtering out group updates, reactions and other protocol events. In an actual
SQLCipher-backed Presage store, a text message, an attachment and a newer group
update reproduced the problem: requesting two visible messages returned one.
The failing run is `/tmp/loom-signal-history-before.log`. A full page of updates
could also present an empty conversation with no path to its older writing.

The worker now scans at most 100 stored records per read, counts visible messages
separately and returns an exclusive timestamp cursor whenever more records remain.
An empty page of metadata retains that cursor. The encoded-message byte budget
includes JSON escaping and metadata; a message that does not fit remains eligible
on the next page. The Signal IPC contract advances to version 5 so a stale worker
cannot be mistaken for the matching client. The stored account format is unchanged.

The pane provides Earlier messages and Latest messages, keeps the selected page
during live refreshes, and discards delayed pages after conversation changes.
Each page replaces the previous view rather than growing an unbounded transcript
in memory. Sending an explicitly composed message returns to the latest page.
Drafting uses the displayed messages and still requires a separate Send action.

The corrected encrypted-store reproduction passes. Three additional store cases
exercise a metadata-only page followed by older text, JSON-escaped large messages
across byte-limited pages without loss or duplication, and expiry with exclusive
cursor ordering. All 23 worker tests passed. Seven WebKit Signal-pane tests passed,
including empty-page navigation, refresh while reading older messages, and a
delayed reply after switching conversations. Svelte checking reported zero errors
and zero warnings.

Strict worker Clippy passed for all targets. The native plugin rebuilt against
the version-5 protocol and its Signal supervisor regression passed. Formatting,
documentation validation and whitespace checks passed. Required macOS CI and
the dependency audit had also passed at the preceding `2802061` checkpoint;
that earlier CI run does not qualify this subsequent change.

These are encrypted-store and browser component results. No phone was linked,
real message sent or real group edited. Phone-linked history, drafting and send
acceptance remain open; the prior native provisioning receipt applies to its
recorded version-4 worker.

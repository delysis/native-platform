# Importing sources into native-kit and Loom

Loom's **Import sources** panel sits beneath the document's context editor.
Choose files or a folder, paste one or more sources, import a public HTTPS
URL, or explicitly sync a connected account. Imported files are retained in
the project. Select results and choose **Add selected to context** to use them
in the current document. Importing never changes the active manuscript.

All deterministic conversion belongs to `attachment-native-*`. Google OAuth
and read-only downloads belong to `information-native-acquire`. Loom owns
file selection, account consent, credentials, persistence, and context
promotion. Other native-kit consumers receive the new parser capabilities
through the existing `AttachmentHost` API; no second ingestion engine exists.

## Source inventory and disposition

Examined on 2026-09-14:

- Local `delysis/turbo-danielle` at `f63283194d4d9b69031847e1647031fa5999870c`, including its uncommitted import-related changes; left untouched.
- Fork main `ca68c72f770a54ecf1e869c632a1317ebb13e70f`.
- Upstream `daniellekantor/turbo-danielle` main `b8c3ad0687dfa92830567b386b61f878a8a0eb0a`.
- Destination base `native-platform` main `924ac64ef0a7512327406ec6490c5e017b5112a8`.

This is a capability port, implemented against native-kit contracts. No donor
source files, Git history, databases, credentials, private exports, or
personal directory allowlists were copied into the destination.

| Donor entry point / capability | Native destination and behavior |
| --- | --- |
| `src/routes/ingest.rs::document`, `corpus_bulk_files`; `routes/opportunities.rs::{create,upload_rfp}`: uploads of reference material, RFPs, and past proposals | Existing Attachment PDF/Office/text/HTML canonicalizers; new bounded RTF text projection; native multi-file results. No binary-to-lossy-UTF-8 fallback. Product-specific grant/opportunity tables are not imported into Loom. |
| `src/bin/ingest.rs::{ingest_drive,ingest_dir,extract_text}`: recursive local Drive exports | Loom's explicit folder chooser, deterministic traversal, 4096-entry / 256-result / 128 MB source-grant bounds, ordinary files only, hidden entries skipped. No `textutil`, hardcoded personal paths, or subprocess conversion. |
| `src/bin/ingest.rs::ingest_claude`: Claude export JSON | Attachment JSON schema recognition renders conversation titles, sender roles, all text blocks, and remaining metadata. Non-text blocks remain inert JSON. Short conversations and non-grant topics are retained. JSON-pointer segments and original bytes preserve provenance. |
| `src/routes/ingest.rs::{slack,parse_slack_zip}`: Slack ZIP exports | Existing bounded ZIP inspection expands members once; Slack message JSON receives readable projection with message/thread/user identifiers and file metadata. Short/system/files-only rows are retained. Local folders permit selecting individual channels; ZIP ingestion preserves the archive rather than applying hidden topic filters. |
| `src/routes/ingest.rs::paste_slack`, `corpus_bulk_paste` | Native pasted-source import with an explicit optional separator, at most 1 MB and 16 source chunks. No inferred separator, minimum prose length, AI assessment, or automatic voice-corpus classification. Ordinary context paste remains available. |
| `src/routes/ingest.rs::linkedin`: connections and posts CSV | Existing CSV tables preserve every column; LinkedIn's Notes preamble is recognized. Share commentary and URLs remain separate fields. Imported connections do not become invented staffing signals. |
| `src/routes/ingest.rs::{gmail,parse_mbox}`: Gmail/Takeout mailbox exports | New content-detected MBOX container derives separate MIME messages with the same global archive budget. Existing email processing decodes quoted-printable/base64, charsets, HTML, and attachments; labels and Message-ID remain readable. No 4000-byte body slicing or message concatenation bug. |
| `src/gmail.rs`, `routes/gmail_oauth.rs`: accounts and Gmail retrieval | Desktop OAuth with fresh PKCE/state, a bounded loopback callback, cancellation/expiry, read-only Gmail scope, OS credential storage, up to eight accounts per service per project, explicit search and next-page actions. Raw MIME snapshots pass through Attachment. |
| `src/gmail.rs::{fetch_google_alert_results,fetch_linkedin_digest_jobs,fetch_linkedin_message_notifications}` | Google Alerts and LinkedIn notification search presets on an explicitly selected Gmail account. Full source emails, links, and attachments are retained; no model-generated relevance assessment or implied fact verification. |
| `src/drive.rs`, `routes/{drive_oauth,drive_sync}.rs`: Drive sync | Read-only account/folder listing; Google Docs/Sheets/Slides export to DOCX/XLSX/PPTX and ordinary files download as bytes. Changes produce new content-addressed snapshots. Unsupported Workspace formats and individual failures are explicit. |
| `src/routes/signals.rs::fetch_page_text`, older `src/linkedin.rs` HTTP/Spider fallback; upstream `linkedin_provider.rs` | Explicit public HTTPS URL import through Information's existing address-pinning/redirect policy. HTML becomes untrusted source text. No authenticated-page scraping, Googlebot impersonation, hosted Spider fallback, provider credentials, or bulk job crawler. LinkedIn account data uses exports/authorized email notifications. |
| `src/bin/ingest.rs::{simple_hash,extract_spans,chunk_into_spans}`; source snapshots and watchlist provenance | SHA-256 raw objects, immutable processing/acquisition receipts, semantic segments, and Loom's existing deterministic excerpt retrieval. Grant-specific evidence tables, automatic classification, watchlist scheduling, signal ranking, and hosted LLM extraction stay in the donor domain. |

## Account setup

Use a Google Cloud OAuth client of type **Desktop app**, with Gmail API and/or
Drive API enabled. Enter its client ID and client secret in Loom's connection
form, then complete authorization in the system browser. Google may require
consent-screen configuration, test-user enrollment, or verification for these
read-only scopes. Each service requests only its own scope.

Credentials are stored in the operating system's credential store, keyed by
project identity, service, and one of eight account slots. Each account has its
own entry so multiple accounts do not exceed Windows' per-entry password limit.
There is no project-file or environment-variable
fallback. Disconnect removes the selected account's local credential; existing
imports remain. Google-side revocation is available in the account's connected
apps settings. The browser's completion page only acknowledges receipt of the
callback; Loom reports whether token exchange and storage succeeded.

References: [Google desktop OAuth](https://developers.google.com/identity/protocols/oauth2/native-app),
[Gmail raw messages](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.messages/get),
[Drive downloads and exports](https://developers.google.com/workspace/drive/api/guides/manage-downloads).

## Evidence, bounds, and privacy

- Importing and context promotion are separate explicit actions. Neither marks source claims as verified, human-authored, or approved.
- `.loom/attachments/objects/<sha256>` retains original bytes and canonical text. Immutable `source-*.json` receipts in `manifests/` bind local paths or remote account/file identities to those bytes; OAuth secrets are excluded.
- A Drive `listed_modified_time` is listing metadata, not a transactional claim about the download. The SHA-256 is authoritative for the exact imported snapshot.
- Each account page lists at most 16 files, bounds each HTTP response to 32 MB, retains at most 64 MB of downloaded source bytes, and bounds download work to three minutes. Failures do not suppress earlier successful rows. Pagination is explicit; an unrequested next page is not a complete-account scan.
- Folder batches are bounded; hidden entries are skipped and symlinks/special files are rejected. Parser failures consume their source-byte grant. Each file's nested expansion and MIME members use Attachment's existing monotonic graph budget.
- RTF currently supports ANSI Windows-1252 and Unicode text. Unsupported encodings, malformed groups, or malformed Unicode fail explicitly. Hidden destinations, field instructions, and embedded objects are omitted with partial coverage, never executed.
- Slack/Claude files and URLs referenced inside imported material are not fetched automatically. A non-text export block is retained as inert metadata, not treated as a successfully imported media object.
- Large sources retain their full bounded canonical text on disk. Loom's existing context budget selects a bounded middle-out projection and records whether insertion was complete; promotion fails when no context space remains.

## Verification

Automated fixtures cover mailbox boundaries/MIME decoding/budgets, conversation
roles and metadata, Unicode RTF/embedded-content rejection, bounded HTTP bodies,
OAuth state/PKCE, endpoint/query confinement, folder dedupe and symlink refusal.
The [validation receipt](turbo-danielle-validation.md) records the commands,
outcomes, exact native bundle, and exercised interactions.
Live Google consent, OS credential access, and account downloads require an
actual configured OAuth client and account; fixture tests cannot prove them.

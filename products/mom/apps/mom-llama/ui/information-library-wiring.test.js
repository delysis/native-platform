"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const ui = fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");
const app = fs.readFileSync(
  path.join(__dirname, "..", "src-tauri", "src", "information.rs"),
  "utf8",
);

const block = (source, start, end) => {
  const begin = source.indexOf(start);
  assert.notEqual(begin, -1, `missing ${start}`);
  const finish = source.indexOf(end, begin + start.length);
  assert.notEqual(finish, -1, `missing ${end}`);
  return source.slice(begin, finish);
};

test("Alexandria picker exposes only an opaque single-use grant", () => {
  const handler = block(
    ui,
    '"information-alexandria-pick": async',
    '"information-alexandria-register": async',
  );
  assert.match(handler, /grant\.grant_id/);
  assert.match(handler, /renderer has no source path/);
  assert.doesNotMatch(handler, /grant\.path|sourcePath|absolutePath/);
  assert.match(app, /paths: BTreeMap<String, PathGrant>/);
  assert.match(app, /\.paths\s*\.remove\(grant_id\)/s);
});

test("model grant reaches real chat only with the exact conversation and evidence packet", () => {
  const chat = block(
    ui,
    '"information-chat-send": async',
    '"information-managed-refresh": async',
  );
  assert.match(chat, /modelGrantConversation !== conversationId/);
  assert.match(chat, /grantId: section\.dataset\.modelGrantId/);
  assert.match(chat, /mom_llama_information_chat_send/);
  assert.match(app, /RetrievalPurpose::ModelContext/);
  assert.match(app, /BEGIN UNTRUSTED LOCAL INFORMATION EVIDENCE bytes=/);
  assert.match(app, /packet_sha256/);
});

test("Add to Library re-presents the exact canonical anchor and explicit rights", () => {
  const open = block(
    ui,
    "const openAttachmentLibrary =",
    "const previewAttachmentLibrary =",
  );
  const preview = block(
    ui,
    "const previewAttachmentLibrary =",
    "const commitAttachmentLibrary =",
  );
  assert.match(open, /rootSha256/);
  assert.match(open, /policyFingerprint/);
  assert.match(preview, /attachment_id:/);
  assert.match(preview, /root_sha256:/);
  assert.match(preview, /artifact_id:/);
  assert.match(preview, /policy_fingerprint:/);
  assert.match(preview, /confirmedPrivateUse: true/);
  assert.match(app, /materialize_attachment_text/);
  assert.match(app, /active_managed_document/);
});

test("managed discovery is projection-only and destructive authority stays server-held", () => {
  assert.match(app, /list_active_managed_documents\(MAX_ACTIVE_MANAGED_PROJECTION\)/);
  assert.match(app, /project_active_managed_document/);
  assert.match(app, /active_managed_document/);
  assert.match(app, /removals: BTreeMap<String, PendingRemovalPreview>/);
  assert.match(app, /No live server-side preview authorizes this managed removal/);
  assert.match(ui, /external source removed: \$\{preview\.external_source_bytes_removed\}/);
});


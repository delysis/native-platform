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


// Component witnesses execute the unchanged production action functions and
// input/change listeners with owned synthetic IPC replies. They are not a
// browser, native Information publication, or model acceptance test.
const libraryVm = require("node:vm");
const deferredLibraryReply = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};
const libraryHarness = () => {
  const calls = [];
  const reports = [];
  const listeners = new Map();
  let refreshes = 0;
  const commit = {
    disabled: true,
    setAttribute() { this.disabled = true; },
    removeAttribute() { this.disabled = false; },
  };
  const modal = {
    dataset: {}, hidden: false, isConnected: true,
    classList: { toggle() {} }, setAttribute() {},
    querySelector() { return commit; },
  };
  const field = (id, state) => ({
    id, ...state, focus() {},
    matches(selector) { return selector.split(",").some((part) => part.trim() === `#${id}`); },
  });
  const elements = {
    "attachment-library-modal": modal,
    "attachment-library-rights": field("attachment-library-rights", { checked: false }),
    "attachment-library-item-title": field("attachment-library-item-title", { value: "" }),
  };
  const context = {
    document: {
      getElementById(id) { return elements[id] || (elements[id] = { textContent: "" }); },
      addEventListener(type, handler) {
        const handlers = listeners.get(type) || [];
        handlers.push(handler); listeners.set(type, handlers);
      },
    },
    invoke(command, args) {
      const reply = deferredLibraryReply(); calls.push({ command, args, ...reply }); return reply.promise;
    },
    report(value) { reports.push(value); },
    refreshManagedAttachments: async () => { refreshes += 1; },
  };
  const inputStart = ui.indexOf('document.addEventListener("input"', ui.indexOf('document.addEventListener("compositionend"'));
  const inputEnd = ui.indexOf('document.addEventListener("scroll"', inputStart);
  assert.ok(inputStart >= 0 && inputEnd > inputStart, "actual main input listener missing");
  const changeStart = ui.lastIndexOf('document.addEventListener("change"');
  const changeEnd = ui.indexOf("const listen =", changeStart);
  assert.ok(changeStart >= 0 && changeEnd > changeStart, "actual Information change listener missing");
  const actions = block(ui, '"attachment-library-open": async', '"information-alexandria-pick": async');
  libraryVm.runInNewContext(
    block(ui, "const setModalVisibility =", "const selectedInformationLibrary =")
      + ui.slice(inputStart, inputEnd) + ui.slice(changeStart, changeEnd)
      + `;globalThis.libraryActions = { ${actions} };`,
    context,
  );
  const dispatch = async (type, id) => {
    assert.ok(listeners.has(type), `actual ${type} listener not registered`);
    for (const handler of listeners.get(type)) await handler({ target: elements[id] });
  };
  return {
    modal, commit, calls, elements, reports,
    open: (id) => context.libraryActions["attachment-library-open"]({ dataset: {
      attachment: id, title: `Title ${id}`, rootSha256: `root-${id}`,
      artifact: `text-${id}`, policyFingerprint: `policy-${id}`,
    } }),
    close: () => context.libraryActions["attachment-library-close"](),
    preview: () => context.libraryActions["attachment-library-preview"](),
    publish: () => context.libraryActions["attachment-library-commit"](),
    confirm: async () => {
      elements["attachment-library-rights"].checked = true;
      await dispatch("change", "attachment-library-rights");
    },
    dispatch,
    refreshCount: () => refreshes,
  };
};
const libraryPreview = (id) => ({
  preview_id: `fixture-preview-${id}`, impact_sha256: `fixture-impact-${id}`,
  root_sha256: `fixture-root-${id}`, graph_sha256: `fixture-graph-${id}`,
  artifact_id: `fixture-text-${id}`, processor_name: "fixture-text", processor_version: "1",
  policy_fingerprint: "fixture-policy", canonical_text_bytes: 7,
  canonical_text_sha256: "fixture-text-sha", rights_sha256: "fixture-rights-sha",
});

test("current Library actions preserve exact input and publish only the returned preview", async () => {
  const h = libraryHarness(); await h.open("A"); await h.confirm();
  const pending = h.preview();
  assert.equal(h.calls[0].command, "mom_llama_attachment_library_preview");
  assert.equal(h.calls[0].args.title, "Title A");
  assert.equal(h.calls[0].args.confirmedPrivateUse, true);
  assert.deepEqual(JSON.parse(JSON.stringify(h.calls[0].args.anchor)), {
    attachment_id: "A", root_sha256: "root-A", artifact_id: "text-A", policy_fingerprint: "policy-A",
  });
  h.calls[0].resolve(libraryPreview("A")); await pending;
  assert.equal(h.commit.disabled, false);
  const publish = h.publish();
  assert.equal(h.calls[1].command, "mom_llama_attachment_library_commit");
  assert.deepEqual(JSON.parse(JSON.stringify(h.calls[1].args)), {
    previewId: "fixture-preview-A", impactSha256: "fixture-impact-A",
  });
  const result = { title: "fixture saved A" };
  h.calls[1].resolve(result); await publish;
  assert.equal(h.modal.hidden, true);
  assert.deepEqual(h.reports, [result]); assert.equal(h.refreshCount(), 1);
});

for (const [reason, edit] of [
  ["rights change", async (h) => {
    h.elements["attachment-library-rights"].checked = false;
    await h.dispatch("change", "attachment-library-rights");
  }],
  ["title input before blur", async (h) => {
    h.elements["attachment-library-item-title"].value = "Title B";
    await h.dispatch("input", "attachment-library-item-title");
  }],
  ["title change", async (h) => {
    h.elements["attachment-library-item-title"].value = "Title B";
    await h.dispatch("change", "attachment-library-item-title");
  }],
  ["close", async (h) => h.close()],
  ["different attachment", async (h) => h.open("B")],
]) {
  test(`late Library preview cannot re-enable Commit after ${reason}`, async () => {
    const h = libraryHarness(); await h.open("A"); await h.confirm();
    const pending = h.preview(); await edit(h);
    h.calls[0].resolve(libraryPreview("A")); await pending;
    assert.equal(h.commit.disabled, true,
      `stale preview re-enabled Commit after ${reason}; preview=${h.modal.dataset.previewId}, hidden=${h.modal.hidden}`);
    assert.equal(h.modal.dataset.previewId, undefined);
    assert.equal(h.calls.length, 1, "no commit was admitted by this fixture");
  });
}

test("editing a current title invalidates the review through the actual input listener", async () => {
  const h = libraryHarness(); await h.open("A"); await h.confirm();
  const pending = h.preview(); h.calls[0].resolve(libraryPreview("A")); await pending;
  assert.equal(h.commit.disabled, false, "mounted current review must first enable Commit");
  h.elements["attachment-library-item-title"].value = "Title B";
  await h.dispatch("input", "attachment-library-item-title");
  assert.equal(h.commit.disabled, true, "typing must revoke the old review before blur");
  assert.equal(h.modal.dataset.previewId, undefined);
});

test("reverse preview completion preserves the latest exact reviewed result", async () => {
  const h = libraryHarness(); await h.open("A"); await h.confirm();
  const earlier = h.preview(); const latest = h.preview();
  h.calls[1].resolve(libraryPreview("latest")); await latest;
  h.calls[0].resolve(libraryPreview("earlier")); await earlier;
  assert.equal(h.modal.dataset.previewId, "fixture-preview-latest");
  assert.equal(h.modal.dataset.impactSha256, "fixture-impact-latest");
});

test("an admitted older publish reports and refreshes without closing the newer dialog", async () => {
  const h = libraryHarness(); await h.open("A"); await h.confirm();
  const preview = h.preview(); h.calls[0].resolve(libraryPreview("A")); await preview;
  const publish = h.publish(); await h.open("B");
  const result = { title: "fixture admitted A" };
  h.calls[1].resolve(result); await publish;
  assert.equal(h.modal.hidden, false, "older commit completion must not close the newer review");
  assert.equal(h.modal.dataset.attachment, "B");
  assert.deepEqual(h.reports, [result]); assert.equal(h.refreshCount(), 1);
});

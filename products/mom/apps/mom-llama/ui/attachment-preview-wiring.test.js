"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const ui = fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");
const runtime = fs.readFileSync(
  path.join(__dirname, "..", "..", "..", "crates", "mom-llama-runtime", "src", "attachments.rs"),
  "utf8",
);

const block = (source, start, end) => {
  const begin = source.indexOf(start);
  assert.notEqual(begin, -1, `missing ${start}`);
  const finish = source.indexOf(end, begin + start.length);
  assert.notEqual(finish, -1, `missing ${end}`);
  return source.slice(begin, finish);
};

test("preview hydration discovers then re-presents exact canonical authority", () => {
  const load = block(
    ui,
    "const loadAttachmentPreview = async",
    "const drainAttachmentPreviewQueue =",
  );
  const discovery = load.indexOf('invoke("mom_llama_attachment_preview"');
  const text = load.indexOf('invoke("mom_llama_attachment_preview_content"');
  const media = load.indexOf('invoke("mom_llama_attachment_preview_bytes"');
  assert.ok(discovery >= 0 && discovery < text && discovery < media);
  assert.match(ui, /rootSha256: catalog\.root_sha256/);
  assert.match(ui, /artifact: artifact\.artifact_id/);
  assert.match(ui, /policyFingerprint: catalog\.policy_fingerprint/);
  assert.match(load, /sameAttachmentPreviewAnchor\(content\.anchor, anchor\)/);
  assert.match(runtime, /exact_preview_authority\(&store, anchor\)/);
});

test("canonical text remains inert, bounded, and honestly truncated", () => {
  const render = block(
    ui,
    "const renderAttachmentTextPreview =",
    "const loadAttachmentPreview =",
  );
  assert.match(render, /text\.textContent = section\.text/);
  assert.doesNotMatch(render, /innerHTML|markdown_content|DOMParser/);
  assert.match(render, /omitted_bytes/);
  assert.match(render, /omitted_characters/);
  assert.match(render, /omitted_lines/);
  assert.match(runtime, /MAX_ATTACHMENT_PREVIEW_TEXT_BYTES: usize = 128 \* 1024/);
  assert.match(runtime, /MAX_ATTACHMENT_PREVIEW_TEXT_LINES: usize = 1_200/);
});

test("native video never autoplays and every media URL has bounded lifetime", () => {
  const load = block(
    ui,
    "const loadAttachmentPreview = async",
    "const drainAttachmentPreviewQueue =",
  );
  assert.match(load, /\["image", "audio", "video"\]/);
  assert.match(load, /media\.controls = true/);
  assert.match(load, /media\.autoplay = false/);
  assert.match(load, /media\.preload = "metadata"/);
  assert.match(ui, /URL\.revokeObjectURL\(entry\.url\)/);
  assert.match(ui, /preview\.dataset\.previewReleased === "true"/);
  assert.match(runtime, /MAX_ATTACHMENT_PREVIEW_MEDIA_BYTES: u64 = 16 \* 1024 \* 1024/);
  assert.match(runtime, /"video\/mp4" \| "video\/quicktime" \| "video\/webm" \| "video\/ogg"/);
});

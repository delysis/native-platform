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

// Component controls use actual browser action/render/stop bodies together.
// Synthetic transport/media replies are not native synthesis evidence.
const speechVm = require("node:vm");
const speechDeferred = () => {
  let resolve; let reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};
const speechLifetimeHarness = () => {
  const pending = [];
  const calls = [];
  const hashes = [];
  const media = [];
  const urls = [];
  let selected = "fixture-conversation";
  let serial = 0;
  let automaticReplies = false;
  let automaticReadResult = null;
  const element = () => ({
    dataset: {}, children: [], attributes: new Map(), isConnected: true,
    append(...children) { this.children.push(...children); },
    replaceChildren(...children) { this.children = children; },
    setAttribute(name, value) { this.attributes.set(name, value); },
    removeAttribute(name) { this.attributes.delete(name); },
    remove() { this.isConnected = false; },
  });
  const row = element();
  row.dataset.messageId = "fixture-message";
  const card = element();
  row.querySelector = () => card;
  const preview = element();
  preview.dataset.attachmentPreview = "fixture-attachment";
  const transcript = element();
  preview.querySelector = () => transcript;
  const button = element();
  button.dataset.message = "fixture-message";
  const label = element();
  button.querySelector = () => label;
  button.closest = (selector) => selector === ".message-row" ? row : preview;
  const transcribe = element();
  Object.assign(transcribe.dataset, {
    attachment: "fixture-attachment", rootSha256: "fixture-root",
    artifact: "fixture-artifact", policyFingerprint: "fixture-policy",
  });
  transcribe.closest = () => preview;
  const context = {
    Blob, Uint8Array, ArrayBuffer,
    document: { createElement: element },
    selectedConversation: () => selected,
    speechOperationToken: () => `fixture-operation-${++serial}`,
    sha256Hex() { const reply = speechDeferred(); hashes.push(reply); if (automaticReplies) reply.resolve("fixture-wav"); return reply.promise; },
    invoke(command, args) {
      const reply = speechDeferred();
      calls.push({ command, args }); pending.push(reply);
      if (automaticReplies && command === "mom_llama_speech_audio") reply.resolve(new Uint8Array([1, 2]));
      if (automaticReplies && command === "mom_llama_speech_stop") reply.resolve({ status: "passed" });
      if (automaticReadResult && command === "mom_llama_speech_read_aloud") reply.resolve(automaticReadResult);
      return reply.promise;
    },
    report() {}, reportError(error) { throw error; },
    DYNAMIC_CONTROL_SPECS: {
      readAloud: { affordance: "fixture-read", command: "read", tauri: "read", cli: "", effect: "read" },
      readAloudStop: { affordance: "fixture-stop", command: "stop", tauri: "stop", cli: "", effect: "stop" },
      attachmentTranscribe: { affordance: "fixture-transcribe", command: "transcribe", tauri: "transcribe", cli: "", effect: "transcribe" },
      attachmentTranscriptionStop: { affordance: "fixture-transcribe-stop", command: "stop", tauri: "stop", cli: "", effect: "stop" },
      draftUpdate: { affordance: "fixture-draft", command: "draft", tauri: "draft", cli: "", effect: "draft" },
    },
    createCommandElement(tag) {
      const item = element();
      if (tag === "audio") {
        item.events = {};
        item.plays = 0;
        item.play = async () => { item.plays += 1; };
        item.pause = () => { item.paused = true; };
        item.load = () => {};
        item.addEventListener = (name, handler) => { item.events[name] = handler; };
        media.push(item);
      }
      return item;
    },
    URL: {
      createObjectURL() { const url = `blob:fixture-${urls.length}`; urls.push({ url, revoked: false }); return url; },
      revokeObjectURL(url) { const entry = urls.find((item) => item.url === url); assert.ok(entry); entry.revoked = true; },
    },
  };
  speechVm.runInNewContext(
    block(ui, "  const commandMetadata =", "  const DYNAMIC_CONTROL_SPECS =")
    + block(ui, "  const ipcBinaryBytes =", "  const speechOperationToken =")
    + "\nconst attachmentTranscripts = new WeakMap();\n"
    + block(ui, "  let activeSpeechPlayback =", "  const appendAttachmentTranscriptionControl =")
    + block(ui, "  const renderAttachmentTranscript =", "  const attachmentPreviewResult =")
    + "\nconst actions = {" + block(ui, '    "message-read-aloud": async', '    "speech-transcript-insert": async') + "};"
    + "\nglobalThis.api = { actions, stopSpeechPlayback, active: () => activeSpeechPlayback };",
    context,
  );
  const take = async (command, occurrence = 0) => {
    for (let attempt = 0; attempt < 100; attempt += 1) {
      const indices = calls.flatMap((call, index) => call.command === command ? [index] : []);
      if (indices[occurrence] !== undefined) return { call: calls[indices[occurrence]], reply: pending[indices[occurrence]] };
      await Promise.resolve();
    }
    assert.fail(`actual handler never invoked ${command} occurrence ${occurrence}; calls=${JSON.stringify(calls)}`);
  };
  const hash = async (index = 0) => {
    for (let attempt = 0; attempt < 100; attempt += 1) {
      if (hashes[index]) return hashes[index];
      await Promise.resolve();
    }
    assert.fail(`actual render never requested hash ${index}`);
  };
  const result = (id = "fixture-playback", message = row.dataset.messageId) => ({ status: "passed", result: {
    playback_id: id, binding: { conversation_id: selected, message_id: message,
      text_sha256: "fixture-text", backend_descriptor_sha256: "fixture-backend", wav_sha256: "fixture-wav" },
  } });
  const newReadButton = (message) => {
    const nextRow = element(); nextRow.dataset.messageId = message;
    const nextCard = element(); nextRow.querySelector = () => nextCard;
    const nextButton = element(); nextButton.dataset.message = message;
    const nextLabel = element(); nextButton.querySelector = () => nextLabel;
    nextButton.closest = () => nextRow;
    return nextButton;
  };
  const start = async () => {
    const task = context.api.actions["message-read-aloud"](button);
    const read = await take("mom_llama_speech_read_aloud");
    return { task, read };
  };
  const finishPlayback = async (audioBytes = new Uint8Array([1, 2])) => {
    const started = await start(); started.read.reply.resolve(result());
    (await take("mom_llama_speech_audio")).reply.resolve(audioBytes);
    (await hash()).resolve("fixture-wav"); await started.task;
    assert.equal(media.length, 1); assert.equal(media[0].plays, 1);
    return started;
  };
  const respondRemainder = (readResult = null) => {
    automaticReplies = true;
    automaticReadResult = readResult;
    for (let index = 0; index < calls.length; index += 1) {
      if (calls[index].command === "mom_llama_speech_audio") pending[index].resolve(new Uint8Array([1, 2]));
      if (calls[index].command === "mom_llama_speech_stop") pending[index].resolve({ status: "passed" });
    }
    for (const reply of hashes) reply.resolve("fixture-wav");
  };
  const settle = () => { void context.api.stopSpeechPlayback({ notifyNative: false }); for (const reply of pending) reply.resolve({ status: "passed" }); for (const reply of hashes) reply.resolve("fixture-wav"); };
  return { ...context.api, button, transcribe, preview, transcript, card, calls, media, urls, take, hash, result, start, finishPlayback, respondRemainder, newReadButton, settle,
    navigate() { selected = "fixture-other-conversation"; } };
};

test("current Read Aloud reaches real render and Stop releases its media and exact playback", async () => {
  const h = speechLifetimeHarness();
  try {
    await h.finishPlayback();
    const stopping = h.actions["message-read-aloud"](h.button);
    const stop = await h.take("mom_llama_speech_stop");
    assert.equal(stop.call.args.playback, "fixture-playback");
    assert.equal(h.active(), null); assert.equal(h.media[0].paused, true);
    assert.equal(h.urls[0].revoked, true);
    stop.reply.resolve({ status: "passed" }); await stopping;
    assert.equal(h.button.dataset.operation, undefined); assert.equal(h.button.dataset.playback, undefined);
  } finally { h.settle(); }
});

test("Read Aloud accepts the byte array representation delivered by packaged macOS IPC", async () => {
  const h = speechLifetimeHarness();
  try {
    await h.finishPlayback([1, 2]);
    assert.equal(h.media.length, 1);
    assert.equal(h.media[0].plays, 1);
  } finally { h.settle(); }
});

test("IPC byte arrays reject invalid values and payloads beyond their native ceiling", () => {
  const context = { ArrayBuffer, Uint8Array };
  speechVm.runInNewContext(
    block(ui, "  const ipcBinaryBytes =", "  const speechOperationToken =")
      + "\nglobalThis.decode = ipcBinaryBytes;",
    context,
  );
  assert.throws(() => context.decode([0, 256], 2, "Fixture"), /invalid IPC byte array/);
  assert.throws(() => context.decode([0, 1, 2], 2, "Fixture"), /exceeded its 2-byte IPC limit/);
});

for (const interruption of ["operation Stop", "quiesce", "navigation"]) {
  test(`late Read Aloud reply cannot publish after ${interruption}`, async () => {
    const h = speechLifetimeHarness();
    try {
      const { task, read } = await h.start();
      if (interruption === "operation Stop") {
        const stopping = h.actions["message-read-aloud"](h.button);
        (await h.take("mom_llama_speech_stop")).reply.resolve({ status: "passed" }); await stopping;
      } else if (interruption === "quiesce") await h.stopSpeechPlayback({ notifyNative: false });
      else h.navigate();
      // Binding names the originally captured conversation, even after navigation.
      const reply = h.result(); reply.result.binding.conversation_id = "fixture-conversation";
      read.reply.resolve(reply);
      // Release any bytes/hash/exact cleanup actually requested by either
      // Source version; no guessed delay and no permanent pending transport.
      h.respondRemainder();
      await task;
      assert.equal(h.media.length, 0, `revoked ${interruption} reply created browser playback`);
      assert.ok(h.calls.some((call) => call.command === "mom_llama_speech_stop" && call.args.playback === "fixture-playback"));
    } finally { h.settle(); }
  });
}

test("Stop while actual WAV verification waits prevents late playback", async () => {
  const h = speechLifetimeHarness();
  try {
    const { task, read } = await h.start(); read.reply.resolve(h.result());
    (await h.take("mom_llama_speech_audio")).reply.resolve(new Uint8Array([1, 2]));
    const hashing = await h.hash();
    const stopping = h.actions["message-read-aloud"](h.button);
    (await h.take("mom_llama_speech_stop")).reply.resolve({ status: "passed" }); await stopping;
    hashing.resolve("fixture-wav"); h.respondRemainder(); await task;
    assert.equal(h.media.length, 0, "stopped playback was published after the real WAV hash await");
  } finally { h.settle(); }
});

test("old Stop acknowledgement cannot clear a newer button operation", async () => {
  const h = speechLifetimeHarness(); h.button.dataset.operation = "fixture-old-operation";
  try {
    const stopping = h.actions["message-read-aloud"](h.button);
    const stop = await h.take("mom_llama_speech_stop");
    h.button.dataset.operation = "fixture-new-operation";
    stop.reply.resolve({ status: "passed" }); await stopping;
    assert.equal(h.button.dataset.operation, "fixture-new-operation");
  } finally { h.settle(); }
});

test("old transcription Stop acknowledgement preserves newer operation controls", async () => {
  const h = speechLifetimeHarness(); h.transcribe.dataset.operation = "fixture-old-operation";
  try {
    const stopping = h.actions["attachment-transcribe"](h.transcribe);
    const stop = await h.take("mom_llama_speech_stop");
    h.transcribe.dataset.operation = "fixture-new-operation";
    h.transcribe.textContent = "Stop transcription";
    stop.reply.resolve({ status: "passed" }); await stopping;
    assert.equal(h.transcribe.dataset.operation, "fixture-new-operation");
    assert.equal(h.transcribe.textContent, "Stop transcription");
  } finally { h.settle(); }
});

test("late transcription result preserves newer controls and cannot insert stale text", async () => {
  const h = speechLifetimeHarness();
  try {
    const task = h.actions["attachment-transcribe"](h.transcribe);
    const transcribe = await h.take("mom_llama_speech_transcribe_attachment");
    h.transcribe.dataset.operation = "fixture-new-operation";
    h.transcribe.textContent = "Stop transcription";
    transcribe.reply.resolve({ status: "passed", result: { transcript: "old text", provenance: {
      conversation_id: "fixture-conversation", attachment_id: "fixture-attachment", root_sha256: "fixture-root",
      artifact_id: "fixture-artifact", policy_fingerprint: "fixture-policy",
    } } });
    await task;
    assert.equal(h.transcript.children.length, 0);
    assert.equal(h.transcribe.dataset.operation, "fixture-new-operation");
    assert.equal(h.transcribe.textContent, "Stop transcription");
  } finally { h.settle(); }
});


test("quiesce during previous-playback cleanup cannot start a late read", async () => {
  const h = speechLifetimeHarness();
  try {
    await h.finishPlayback();
    const nextButton = h.newReadButton("fixture-next-message");
    const pendingRead = h.actions["message-read-aloud"](nextButton);
    const cleanup = await h.take("mom_llama_speech_stop");
    assert.equal(cleanup.call.args.playback, "fixture-playback");
    await h.stopSpeechPlayback({ notifyNative: false });
    h.respondRemainder(h.result("fixture-next-playback", "fixture-next-message"));
    cleanup.reply.resolve({ status: "passed" }); await pendingRead;
    assert.equal(h.calls.filter((call) => call.command === "mom_llama_speech_read_aloud").length, 1);
    assert.equal(h.media.length, 1, "only the earlier current playback reached the browser adapter");
  } finally { h.settle(); }
});


test("current transcription publishes inert text only after exact owner checks", async () => {
  const h = speechLifetimeHarness();
  try {
    const task = h.actions["attachment-transcribe"](h.transcribe);
    const started = await h.take("mom_llama_speech_transcribe_attachment");
    assert.equal(started.call.args.rootSha256, "fixture-root");
    assert.equal(started.call.args.artifact, "fixture-artifact");
    started.reply.resolve({ status: "passed", result: { transcript: "<fixture text>", provenance: {
      conversation_id: "fixture-conversation", attachment_id: "fixture-attachment", root_sha256: "fixture-root",
      artifact_id: "fixture-artifact", policy_fingerprint: "fixture-policy", blob_object_id: "fixture-blob",
      model_id: "fixture-model", model_content_sha256: "fixture-model-content",
    } } });
    await task;
    assert.equal(h.transcript.children[0].textContent, "<fixture text>");
    assert.equal(h.transcript.children[2].dataset.action, "speech-transcript-insert");
    assert.equal(h.transcribe.textContent, "Transcribe audio");
    assert.equal(h.transcribe.dataset.operation, undefined);
    assert.equal(h.calls.length, 1, "displaying a transcript must not send a chat");
  } finally { h.settle(); }
});

test("an older ended event cannot stop the newer actual playback", async () => {
  const h = speechLifetimeHarness();
  try {
    await h.finishPlayback();
    const older = h.media[0];
    const nextButton = h.newReadButton("fixture-next-message");
    const next = h.actions["message-read-aloud"](nextButton);
    const cleanup = await h.take("mom_llama_speech_stop");
    cleanup.reply.resolve({ status: "passed" });
    const nextRead = await h.take("mom_llama_speech_read_aloud", 1);
    h.respondRemainder(); nextRead.reply.resolve(h.result("fixture-next-playback", "fixture-next-message")); await next;
    assert.equal(h.active().id, "fixture-next-playback");
    assert.equal(h.media.length, 2); assert.equal(h.media[1].plays, 1);
    const callsBeforeEnded = h.calls.length;
    older.events.ended();
    assert.equal(h.active()?.id, "fixture-next-playback");
    assert.equal(h.calls.length, callsBeforeEnded, "old media event must not release newer native playback");
  } finally { h.settle(); }
});


test("Stop transport errors remain observable without clearing newer ownership", async () => {
  const h = speechLifetimeHarness(); h.button.dataset.operation = "fixture-old-operation";
  try {
    const stopping = h.actions["message-read-aloud"](h.button);
    const failure = assert.rejects(stopping, /fixture-stop-refused/);
    const stop = await h.take("mom_llama_speech_stop");
    h.button.dataset.operation = "fixture-new-operation";
    stop.reply.reject(new Error("fixture-stop-refused")); await failure;
    assert.equal(h.button.dataset.operation, "fixture-new-operation");
  } finally { h.settle(); }
});

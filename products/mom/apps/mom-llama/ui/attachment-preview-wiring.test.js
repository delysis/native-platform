"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const ui = fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");
const style = fs.readFileSync(path.join(__dirname, "style.css"), "utf8");
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

test("preview failures use the shared total error formatter", () => {
  assert.match(ui, /const errorMessage = \(error\) => \(/);
  assert.match(ui, /const reportError = \(error\) => \{[\s\S]*?const message = errorMessage\(error\);/);
  const load = block(
    ui,
    "const loadAttachmentPreview = async",
    "const drainAttachmentPreviewQueue =",
  );
  assert.match(load, /preview\.title = errorMessage\(error\)/);
});

test("a failed persona synthesis republishes its pending voice row", () => {
  const settle = block(
    ui,
    "const settleUnavailableVoiceReply =",
    "const restoreReadAloudButton =",
  );
  assert.match(settle, /card\.replaceChildren\(unavailableVoiceMessage\(\)\)/);
  assert.match(settle, /row\.hidden = false/);
  assert.match(settle, /row\.classList\.remove\("voice-reply-pending"\)/);
  const action = block(
    ui,
    '"message-read-aloud": async',
    '"attachment-transcribe": async',
  );
  assert.match(action, /finally \{/);
  assert.match(action, /voiceRow\?\.classList\.contains\("voice-reply-pending"\)/);
  assert.match(action, /activeSpeechPlayback\?\.button === button\) await stopSpeechPlayback\(\)/);
  assert.match(action, /settleUnavailableVoiceReply\(button\)/);
});

test("explicit Read Aloud preserves the text card", () => {
  const action = block(
    ui,
    '"message-read-aloud": async',
    '"attachment-transcribe": async',
  );
  assert.match(action, /if \(prepareOnly && personaVoice && row && card\)/);
  assert.doesNotMatch(
    action,
    /if \(personaVoice && row && card\) \{\s*row\.classList\.add\("voice-reply-row"\)/,
  );
  const playbackStyle = style.match(/\.speech-playback\s*\{([^}]*)\}/)?.[1] || "";
  assert.match(playbackStyle, /display:\s*none/);
  assert.doesNotMatch(playbackStyle, /border|margin|padding/);
});

test("cancelled Read Aloud is discarded before user-visible reporting", () => {
  const action = block(
    ui,
    '"message-read-aloud": async',
    '"attachment-transcribe": async',
  );
  const ownership = action.indexOf("activeSpeechPlayback?.controller !== controller || epoch !== speechPlaybackEpoch");
  const publication = action.indexOf("report(response)");
  assert.ok(ownership >= 0 && ownership < publication);
});

test("call playback rejects audio bound to any response except the new reply", () => {
  const callPlayback = block(
    ui,
    "  const playCallReply = async",
    "  const appendAttachmentTranscriptionControl =",
  );
  assert.match(callPlayback, /result\.binding\?\.message_id !== message/);
  assert.match(
    callPlayback,
    /mom_llama_speech_stop[\s\S]*?retainedPlayback = null;[\s\S]*?return false/,
  );
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

test("voice note offers eager loading and becomes a real player after hydration", async () => {
  const element = () => ({
    children: [], attributes: new Map(), events: new Map(), dataset: {}, append(...children) { this.children.push(...children); },
    querySelector(selector) {
      if (selector === ".voice-message-play") {
        return this.children.find((child) => child.className === "voice-message-play") || null;
      }
      return null;
    },
    setAttribute(name, value) { this.attributes.set(name, value); },
    removeAttribute(name) { this.attributes.delete(name); },
    addEventListener(name, handler) { this.events.set(name, handler); },
    classList: {
      values: new Set(),
      add(name) { this.values.add(name); },
      remove(name) { this.values.delete(name); },
      contains(name) { return this.values.has(name); },
      replace(from, to) { this.replaced = [from, to]; this.values.delete(from); this.values.add(to); },
    },
  });
  const playbackErrors = [];
  const context = {
    document: { createElement: element },
    DYNAMIC_CONTROL_SPECS: { attachmentPreview: {} },
    createCommandElement: () => element(),
    waveform: () => ({ kind: "waveform" }),
    reportError: (error) => playbackErrors.push(error),
  };
  speechVm.createContext(context);
  speechVm.runInContext(
    block(ui, "  const markCompactAudioUnavailable =", "  const pendingVoiceMessage =")
      + block(ui, "  const pendingVoiceMessage =", "  const restoreReadAloudButton =")
      + "\nglobalThis.makeVoicePlayers = { compactAudioPlayer, pendingVoiceMessage, pendingVoiceReply, unavailableVoiceMessage };",
    context,
  );
  const pending = context.makeVoicePlayers.pendingVoiceMessage();
  assert.equal(pending.className, "voice-message-player voice-message-pending");
  assert.equal(pending.children[0].disabled, false);
  assert.equal(pending.children[0].dataset.action, "voice-message-pending-play");
  assert.equal(pending.children[0].dataset.audioState, "pending");
  assert.equal(pending.children[0].textContent, "");
  assert.equal(pending.children[0].attributes.get("aria-label"), "Loading voice message");
  assert.equal(pending.children[1].kind, "waveform");

  const pendingReply = context.makeVoicePlayers.pendingVoiceReply();
  assert.equal(pendingReply.className, "typing-indicator");
  assert.equal(pendingReply.attributes.get("aria-label"), "Typing");
  assert.equal(pendingReply.children.length, 3);

  const unavailable = context.makeVoicePlayers.unavailableVoiceMessage();
  assert.deepEqual(Array.from(unavailable.classList.replaced), ["voice-message-pending", "voice-message-unavailable"]);
  assert.equal(unavailable.children[0].attributes.get("aria-label"), "Voice message unavailable");
  assert.equal(unavailable.children[0].dataset.audioState, "failed");
  assert.equal(unavailable.children[0].textContent, "");

  const audio = element(); audio.paused = true;
  const ready = context.makeVoicePlayers.compactAudioPlayer(audio);
  assert.equal(ready.className, "voice-message-player voice-message-pending");
  assert.equal(ready.children[0].disabled, true);
  assert.equal(ready.children[0].attributes.get("aria-label"), "Loading voice message");
  assert.equal(ready.children[0].dataset.audioState, "pending");
  assert.equal(ready.children[0].attributes.get("aria-pressed"), undefined);
  audio.events.get("pause")();
  assert.equal(ready.children[0].disabled, true);
  assert.equal(ready.children[0].dataset.audioState, "pending");
  audio.duration = 1;
  audio.events.get("loadedmetadata")();
  assert.equal(ready.children[0].disabled, true);
  assert.equal(ready.children[0].dataset.audioState, "pending");
  audio.events.get("canplay")();
  assert.equal(ready.children[0].disabled, false);
  assert.equal(ready.children[0].attributes.get("aria-label"), "Play voice message");
  assert.equal(ready.children[0].dataset.audioState, "ready");
  assert.equal(ready.children[0].dataset.audioProof, "decoded");
  assert.equal(ready.children[0].attributes.get("aria-pressed"), undefined);

  let playbackStarted = 0;
  const verifiedAudio = element(); verifiedAudio.paused = false;
  const verifiedPlayer = context.makeVoicePlayers.compactAudioPlayer(verifiedAudio, null, {
    onPlaybackStarted: () => { playbackStarted += 1; },
  });
  verifiedAudio.duration = 1;
  verifiedAudio.events.get("canplay")();
  assert.equal(playbackStarted, 0, "an accepted play request is not audible playback proof");
  verifiedAudio.events.get("playing")();
  assert.equal(playbackStarted, 1);
  const verifiedButton = verifiedPlayer.children[0];
  assert.equal(verifiedButton.dataset.audioState, "playing");
  assert.equal(verifiedButton.attributes.get("aria-pressed"), undefined);
  verifiedAudio.events.get("pause")();
  assert.equal(verifiedButton.attributes.get("aria-pressed"), undefined);
  audio.events.get("emptied")();
  assert.equal(ready.children[0].disabled, true);
  assert.equal(ready.children[0].dataset.audioState, "pending");
  audio.events.get("canplay")();
  assert.equal(ready.children[0].disabled, false);
  assert.equal(ready.children[0].dataset.audioState, "ready");
  assert.equal(ready.children[0].dataset.audioProof, "decoded");

  const playButton = ready.children[0];
  audio.play = async () => { throw new Error("fixture play rejection"); };
  await playButton.events.get("click")();
  assert.equal(playButton.textContent, "");
  assert.equal(playButton.disabled, true);
  assert.equal(playButton.dataset.audioState, "failed");
  assert.equal(playButton.attributes.get("aria-label"), "Voice message unavailable");
  assert.equal(ready.classList.contains("is-playing"), false);
  assert.equal(playbackErrors[0]?.message, "fixture play rejection");

  assert.equal(ready.classList.contains("voice-message-unavailable"), true);
  assert.equal(playbackErrors.length, 1);
  const hydrate = block(ui, "  const hydrateAttachmentPreviews = async", "  window.addEventListener(\"beforeunload\"");
  assert.ok(hydrate.indexOf("body.replaceChildren(pendingVoiceMessage())") < hydrate.indexOf("attachmentPreviewObserver.observe(preview)"));
  const load = block(ui, "  const loadAttachmentPreview = async", "  const drainAttachmentPreviewQueue =");
  assert.match(load, /body\.replaceChildren\(compactAudioPlayer\(media\)\)/);
  assert.match(load, /replaceChildren\(unavailableVoiceMessage\(\)\)/);
});

test("voice players expose play only after structural and media validation", () => {
  const player = block(ui, "  const isCanonicalPcmWav =", "  const pendingVoiceMessage =");
  assert.match(player, /bytes instanceof Uint8Array/);
  assert.match(player, /text\(0, 4\) !== "RIFF"/);
  assert.match(player, /text\(8, 4\) !== "WAVE"/);
  assert.match(player, /dataBytes === bytes\.byteLength - 44/);
  assert.match(player, /toggle\.dataset\.audioState = "pending"/);
  assert.match(player, /toggle\.disabled = true/);
  assert.match(player, /audio\.addEventListener\("loadedmetadata", validateMetadata/);
  assert.match(player, /audio\.addEventListener\("canplay", ready/);
  assert.match(player, /audio\.addEventListener\("playing"/);
  assert.doesNotMatch(player, /audio\.addEventListener\("play",/);
  assert.match(player, /toggle\.disabled = false;[\s\S]*toggle\.dataset\.audioState = "ready"/);
  assert.ok(
    player.indexOf("if (bytes && !isCanonicalPcmWav(bytes))")
      < player.indexOf("audio.load?.()"),
  );
  const outgoingStyle = style.match(
    /body \.message-row\.voice-message-row \.voice-message-play\s*\{([^}]*)\}/,
  )?.[1] || "";
  assert.match(outgoingStyle, /box-shadow:\s*none/);
  const activeStyle = style.match(
    /\.voice-message-play:active:not\(:disabled\)\s*\{([^}]*)\}/,
  )?.[1] || "";
  assert.match(activeStyle, /transform:\s*none/);
  assert.match(activeStyle, /box-shadow:\s*none/);
  assert.doesNotMatch(activeStyle, /inset/);
});

test("only the exact generated voice reply is hidden while preparing", async () => {
  const classList = (initial = []) => {
    const values = new Set(initial);
    return {
      add: (...names) => names.forEach((name) => values.add(name)),
      contains: (name) => values.has(name),
      toggle: (name, enabled) => enabled ? values.add(name) : values.delete(name),
      remove: (...names) => names.forEach((name) => values.delete(name)),
      has: (name) => values.has(name),
    };
  };
  const pairs = ["old", "newest"].map((id) => {
    const assistant = {
      hidden: false,
      dataset: { messageId: id },
      classList: classList(["assistant"]),
      querySelector(selector) {
        if (selector === "[data-action='message-read-aloud']") return this.button;
        if (selector === ".message-card") return this.card;
        if (selector === ".message-actions") return null;
        return null;
      },
    };
    assistant.button = {
      dataset: { action: "message-read-aloud" },
      classList: classList(),
      removeAttribute(name) { delete this[name]; },
    };
    assistant.card = { replaceChildren() {} };
    const user = {
      classList: classList(["user"]),
      querySelector: (selector) => selector === ".voice-message-bubble" ? {} : null,
      nextElementSibling: assistant,
    };
    return { id, user, assistant };
  });
  let shellCount = 0;
  const observedPreviews = [];
  const previews = ["old", "newest"].map((id) => ({
    id,
    dataset: {},
    classList: { contains: (name) => name === "voice-message-bubble" },
    body: {
      childNodes: [],
      querySelector: () => null,
      replaceChildren() { shellCount += 1; },
    },
    querySelector: () => previews.find((preview) => preview.id === id).body,
  }));
  const root = {
    matches: () => false,
    querySelectorAll(selector) {
      return selector === ".message-row.user" ? pairs.map(({ user }) => user) : [];
    },
  };
  const hydrate = block(ui, "  const hydrateAttachmentPreviews = async", "  window.addEventListener(\"beforeunload\"");
  const context = {
    document: root,
    attachmentPreviewsWithin: () => previews,
    rememberAttachmentPreviewFallback() {},
    selectedConversation: () => "fixture-conversation",
    waveform: () => ({ kind: "waveform" }),
    voiceReplyConversation: "fixture-conversation",
    voiceReplyMessageId: "newest",
    voiceCallActive: false,
    pendingVoiceMessage: () => { shellCount += 1; return {}; },
    registerPreview(preview) {
      assert.equal(shellCount, 4, "all preview shells must exist before lazy registration");
      observedPreviews.push(preview.id);
    },
  };
  speechVm.createContext(context);
  speechVm.runInContext(
    "let attachmentPreviewObserver = { observe(preview) { registerPreview(preview); } };\n"
      + hydrate + "\nthis.hydrate = hydrateAttachmentPreviews;",
    context,
  );
  await context.hydrate(root);

  assert.equal(pairs[0].assistant.hidden, false, "historical voice reply must stay visible");
  assert.equal(pairs[0].assistant.classList.has("voice-reply-pending"), false);
  assert.equal(pairs[0].assistant.classList.has("voice-reply-row"), false);
  assert.equal(pairs[0].assistant.button.classList.has("voice-reply-play"), false);
  assert.equal(pairs[1].assistant.hidden, true, "only the exact generated voice reply may hide during preparation");
  assert.equal(pairs[1].assistant.classList.has("voice-reply-pending"), true);
  assert.equal(pairs[0].assistant.button.dataset.prepareOnly, undefined);
  assert.equal(pairs[1].assistant.button.dataset.prepareOnly, "true");
  assert.deepEqual(observedPreviews, ["newest", "old"], "lazy attachment hydration must begin nearest the composer");
});

test("voice dispatch keeps its live PCM row mounted through completion", () => {
  const submit = block(
    ui,
    "  document.addEventListener(\"submit\"",
    "  document.addEventListener(\"compositionstart\"",
  );
  assert.match(submit, /if \(!voiceDispatch\) \{[\s\S]*await refreshConversationProjection\(\);[\s\S]*\} else \{[\s\S]*Keep the live audio rows mounted/);
  assert.match(submit, /const voiceTtsOwner = voiceDispatch/);
  assert.match(submit, /await voiceTtsOwner\.finish\(output\.assistant_text\)/);
  assert.match(submit, /voice_reply_owner_missing/);
  assert.match(submit, /refreshSidebar\(\), refreshConversationSettingsProjection\(\)/);
  assert.match(ui, /const replayBytes = capture\.wavBytes \|\| encodePcmWav\(capture\.samples\(\)\)/);
  assert.match(ui, /appendOptimisticVoiceMessage\(replayBytes\)/);

  const binding = block(
    ui,
    "  const prepareVoiceReplyRow =",
    "  const hydrateAttachmentPreviews = async",
  );
  assert.match(binding, /button\.dataset\.personaVoice = "true"/);
  assert.match(binding, /card\?\.replaceChildren\(button, waveform\(\)\)/);
  assert.match(binding, /row\.classList\.add\("voice-reply-pending"\)/);
  assert.doesNotMatch(binding, /row\.hidden = true/);

  const liveVoice = block(
    ui,
    "  const beginLiveVoiceTts =",
    "  const chatRequestIsCancellable =",
  );
  assert.match(liveVoice, /if \(!started\)/);
  assert.doesNotMatch(liveVoice, /if \(!started && !controller\.ready\)/);
  assert.match(liveVoice, /classList\.remove\("is-loading"\)/);
  assert.match(liveVoice, /unavailable\(error\)/);
});

// Component controls use actual browser action/render/stop bodies together.
// Synthetic transport/media replies are not native synthesis evidence.
const speechVm = require("node:vm");
const speechDeferred = () => {
  let resolve; let reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};
const speechLifetimeHarness = ({ worklet = false } = {}) => {
  const pending = [];
  const calls = [];
  const hashes = [];
  const media = [];
  const urls = [];
  const audioContexts = [];
  const workletNodes = [];
  const timers = [];
  let selected = "fixture-conversation";
  let serial = 0;
  let automaticReplies = false;
  let automaticReadResult = null;
  let mediaReadyState = 3;
  let playbackResumeError = null;
  let playbackResumePending = false;
  const element = () => ({
    dataset: {}, children: [], attributes: new Map(), isConnected: true, style: {}, clientWidth: 0,
    append(...children) { this.children.push(...children); },
    replaceChildren(...children) { this.children = children; },
    replaceWith(replacement) { this.replacement = replacement; this.isConnected = false; },
    setAttribute(name, value) { this.attributes.set(name, value); },
    removeAttribute(name) { this.attributes.delete(name); },
    remove() { this.isConnected = false; },
  });
  const row = element();
  row.dataset.messageId = "fixture-message";
  row.classList = { values: new Set(), add(name) { this.values.add(name); }, remove(name) { this.values.delete(name); }, contains(name) { return this.values.has(name); } };
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
    Blob, Uint8Array, ArrayBuffer, DataView, JSON,
    performance: { now: () => 1 },
    HTMLMediaElement: { HAVE_CURRENT_DATA: 2, HAVE_FUTURE_DATA: 3 },
    document: {
      createElement: element,
      createDocumentFragment: () => ({ children: [], append(child) { this.children.push(child); } }),
    },
    selectedConversation: () => selected,
    voiceCallActive: false,
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
        // Production waits for decoded media before revealing/playable audio.
        // The fixture represents bytes that are already ready to present.
        item.readyState = mediaReadyState;
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
  context.setTimeout = (callback) => { timers.push(callback); return timers.length; };
  context.clearTimeout = () => {};
  context.AudioContext = class {
    constructor(options) {
      this.options = options; this.sampleRate = options?.sampleRate || 24_000;
      this.currentTime = 0; this.destination = {}; this.sources = [];
      this.closed = false; this.state = "running";
      if (worklet) this.audioWorklet = { async addModule() {} };
      audioContexts.push(this);
    }
    async resume() {
      if (playbackResumeError) throw playbackResumeError;
      if (playbackResumePending) return new Promise(() => {});
      this.state = "running";
    }
    async suspend() { this.state = "suspended"; }
    async close() { this.closed = true; this.state = "closed"; }
    createBuffer(channels, length, sampleRate) {
      return { channels, length, sampleRate, duration: length / sampleRate, copyToChannel() {} };
    }
    createBufferSource() {
      const events = {};
      const source = {
        connect() {}, addEventListener(name, handler) { events[name] = handler; },
        start(time) { this.startedAt = time; },
        stop() { this.stopped = true; events.ended?.(); },
      };
      this.sources.push(source); return source;
    }
  };
  if (worklet) {
    context.AudioWorkletNode = class {
      constructor(audioContext, name, options) {
        this.context = audioContext; this.name = name; this.options = options;
        this.port = { messages: [], onmessage: null, postMessage: (message) => this.port.messages.push(message) };
        workletNodes.push(this);
      }
      connect() { this.connected = true; }
      disconnect() { this.connected = false; }
    };
  }
  context.__TAURI__ = { core: { Channel: class { constructor(onmessage) { this.onmessage = onmessage; } } } };
  speechVm.runInNewContext(
    "globalThis.window = globalThis;\n"
    + block(ui, "  const commandMetadata =", "  const DYNAMIC_CONTROL_SPECS =")
    + block(ui, "  const ipcBinaryBytes =", "  const speechOperationToken =")
    + "\nconst attachmentTranscripts = new WeakMap();\n"
    + block(ui, "  let activeSpeechPlayback =", "  const appendAttachmentTranscriptionControl =")
    + block(ui, "  const renderAttachmentTranscript =", "  const attachmentPreviewResult =")
    + "\nconst actions = {" + block(ui, '    "message-read-aloud": async', '    "speech-transcript-insert": async') + "};"
    + "\nglobalThis.api = { actions, stopSpeechPlayback, createPcmPlaybackBuffer, active: () => activeSpeechPlayback };",
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
    nextRow.classList = { values: new Set(), add(name) { this.values.add(name); }, remove(name) { this.values.delete(name); }, contains(name) { return this.values.has(name); } };
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
    const started = await start();
    started.read.call.args.pcmStream.onmessage(pcmFrame(audioBytes));
    started.read.reply.resolve(result());
    await started.task;
    await Promise.resolve();
    assert.equal(audioContexts.length, 1); assert.equal(audioContexts[0].sources.length, 1);
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
  const pcmFrame = (payload = new Uint8Array([0, 0]), sequence = 0, end = true) => {
    const samples = payload.byteLength % 2 === 0 ? payload : new Uint8Array([0, 0]);
    const frame = new Uint8Array(32 + samples.byteLength); const view = new DataView(frame.buffer);
    frame.set([0x4d, 0x50, 0x43, 0x4d, 1, end ? 3 : 2, 1, 1]);
    view.setUint32(8, 24000, true); view.setBigUint64(12, BigInt(sequence), true);
    view.setBigUint64(20, 0n, true); view.setUint32(28, samples.byteLength, true); frame.set(samples, 32);
    return frame;
  };
  return { ...context.api, button, transcribe, preview, transcript, card, calls, media, urls, audioContexts, workletNodes, timers, pcmFrame, take, hash, result, start, finishPlayback, respondRemainder, newReadButton, settle,
    setMediaReadyState(state) { mediaReadyState = state; },
    rejectPlaybackWith(error) { playbackResumeError = error; },
    hangPlaybackResume() { playbackResumePending = true; },
    navigate() { selected = "fixture-other-conversation"; } };
};

test("a completed streamed voice reply retains PCM for a real replay", async () => {
  const h = speechLifetimeHarness();
  const ended = [];
  const firstPcm = [];
  const controller = h.createPcmPlaybackBuffer({
    onFirstChunk: (receipt) => firstPcm.push(receipt),
    onEnded: () => ended.push(true),
  });
  const pcm = new Uint8Array(6_000);
  controller.push(h.pcmFrame(pcm));
  assert.equal(firstPcm.length, 1);
  assert.equal(firstPcm[0].bytes, 6_000);
  assert.equal(firstPcm[0].frames, 3_000);
  assert.equal(firstPcm[0].sampleRate, 24_000);
  assert.equal(firstPcm[0].channels, 1);
  const wav = controller.wavBytes();
  assert.equal(String.fromCharCode(...wav.subarray(0, 4)), "RIFF");
  assert.equal(String.fromCharCode(...wav.subarray(8, 12)), "WAVE");
  assert.equal(new DataView(wav.buffer, wav.byteOffset, wav.byteLength).getUint32(24, true), 24_000);
  assert.equal(wav.byteLength, 44 + pcm.byteLength);
  assert.equal(await controller.play(), true);
  assert.equal(h.audioContexts.length, 1);
  assert.equal(h.audioContexts[0].sources.length, 1);
  // Audio completion was armed before the independently bounded resume call.
  h.timers[0]();
  await Promise.resolve();
  assert.equal(ended.length, 1);
  assert.equal(await controller.play(), true);
  assert.equal(h.audioContexts.length, 2);
  assert.equal(h.audioContexts[1].sources.length, 1);
  await controller.stop();
});

test("call playback releases scheduled IPC packets instead of retaining replay history", async () => {
  const h = speechLifetimeHarness();
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.audioContexts.length, 1);
  assert.equal(h.audioContexts[0].sources.length, 1);
  assert.equal(controller.wavBytes(), null);
  await controller.stop();
});

test("call playback waits a bounded wall-clock guard without hoarding another PCM packet", async () => {
  const h = speechLifetimeHarness();
  let startupDelayMs = 11;
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
    startupDelayMs: () => {
      const delay = startupDelayMs;
      startupDelayMs = 0;
      return delay;
    },
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000), 0, false));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.audioContexts.length, 0, "first PCM remains queued during the jitter guard");
  assert.equal(h.timers.length, 1);
  h.timers[0]();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.audioContexts.length, 1);
  assert.equal(h.audioContexts[0].sources.length, 1, "the first packet starts without waiting for a second packet");
  await controller.stop();
});

test("call playback transfers PCM to the preallocated audio-thread ring", async () => {
  const h = speechLifetimeHarness({ worklet: true });
  const ended = [];
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
    onEnded: () => ended.push(true),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.audioContexts.length, 1);
  assert.equal(h.audioContexts[0].sources.length, 0);
  assert.equal(h.workletNodes.length, 1);
  assert.equal(h.workletNodes[0].name, "mom-pcm-ring");
  assert.equal(h.workletNodes[0].port.messages[0].type, "pcm");
  assert.equal(h.workletNodes[0].port.messages[0].byteLength, 6_000);
  controller.finish();
  assert.equal(h.workletNodes[0].port.messages.at(-1).type, "finish");
  h.workletNodes[0].port.onmessage({ data: { type: "drained", renderedFrames: 3_000 } });
  assert.equal(ended.length, 1);
  assert.equal(h.audioContexts[0].state, "suspended");
  await controller.stop();
});

test("worklet playback joins producer, admission, render, and consumption on the audio clock", async () => {
  const h = speechLifetimeHarness({ worklet: true });
  const events = [];
  const metrics = [];
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
    onRendererEvent: (event) => events.push(event),
    onRendererMetrics: (value) => metrics.push(value),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  await new Promise((resolve) => setImmediate(resolve));
  const node = h.workletNodes[0];
  const pcm = node.port.messages.find((message) => message.type === "pcm");
  assert.equal(pcm.packetIndex, 0);
  assert.equal(pcm.sequence, 0);
  assert.equal(pcm.sampleOffset, 0);
  assert.ok(events.some((event) => event.type === "packet_enqueued"));
  node.port.onmessage({ data: {
    type: "packet_admitted", packetIndex: 0, sequence: 0, sampleOffset: 0,
    frames: 3_000, bufferedFrames: 3_000, audioContextFrame: 240,
    enqueuedContextTimeMs: 0,
  } });
  node.port.onmessage({ data: {
    type: "render_started", bufferedFrames: 3_000, audioContextFrame: 256,
  } });
  node.port.onmessage({ data: {
    type: "consumed", frames: 2_048, renderedFrames: 2_048,
    bufferedFrames: 952, audioContextFrame: 2_304,
  } });
  assert.deepEqual(
    events.filter((event) => event.type !== "packet_enqueued").map((event) => event.type),
    ["packet_admitted", "render_started", "consumed"],
  );
  assert.equal(events.find((event) => event.type === "render_started").audio_context_ms, 10.67);
  controller.finish();
  node.port.onmessage({ data: {
    type: "drained", renderedFrames: 3_000, renderQuanta: 24,
    maxBufferedFrames: 3_000, minBufferedFrames: 128,
    firstReceiveContextFrame: 240, firstRenderContextFrame: 256,
  } });
  assert.equal(metrics[0].worklet_min_buffered_frames, 128);
  assert.equal(metrics[0].worklet_first_receive_context_frame, 240);
  assert.equal(metrics[0].worklet_first_render_context_frame, 256);
  await controller.stop();
});

test("retained voice playback uses the audio-thread ring and can replay", async () => {
  const h = speechLifetimeHarness({ worklet: true });
  const ended = [];
  const controller = h.createPcmPlaybackBuffer({
    retainHistory: true,
    onEnded: () => ended.push(true),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  assert.ok(controller.wavBytes()?.byteLength > 44);

  assert.equal(await controller.play(), true);
  assert.equal(h.audioContexts.length, 1);
  assert.equal(h.audioContexts[0].sources.length, 0);
  assert.equal(h.workletNodes.length, 1);
  assert.equal(h.workletNodes[0].port.messages[0].type, "pcm");
  controller.finish();
  assert.equal(h.workletNodes[0].port.messages.at(-1).type, "finish");
  h.workletNodes[0].port.onmessage({ data: { type: "drained", renderedFrames: 3_000 } });
  assert.equal(ended.length, 1);

  assert.equal(await controller.play(), true);
  assert.equal(h.audioContexts.length, 1, "replay reuses the prepared AudioContext");
  assert.equal(h.workletNodes.length, 2, "replay gets a reset audio-thread ring");
  assert.equal(h.workletNodes[1].port.messages[0].type, "pcm");
  assert.equal(h.workletNodes[1].port.messages.at(-1).type, "finish");
  h.workletNodes[1].port.onmessage({ data: { type: "drained", renderedFrames: 3_000 } });
  assert.equal(ended.length, 2);
  await controller.stop();
});

test("a live voice reply uses one controller for reciprocal pause and resume", async () => {
  const h = speechLifetimeHarness();
  const transitions = [];
  const controller = h.createPcmPlaybackBuffer({
    onPlaybackStarted: () => transitions.push("playing"),
    onPlaybackPaused: () => transitions.push("paused"),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  assert.equal(await controller.play(), true);
  assert.equal(controller.playing, true);
  assert.equal(h.audioContexts.length, 1);
  assert.equal(await controller.pause(), true);
  assert.equal(controller.playing, false);
  assert.equal(h.audioContexts[0].state, "suspended");
  assert.equal(await controller.play(), true);
  assert.equal(controller.playing, true);
  assert.equal(h.audioContexts.length, 1, "resume must not create a second audio timeline");
  assert.deepEqual(transitions, ["playing", "paused", "playing"]);
  await controller.stop();
});

test("optimistic voice-note hydration replaces the pending control with a validated player", () => {
  const append = block(ui, "  const appendOptimisticVoiceMessage =", "  const hydrateOptimisticVoiceMessage =");
  assert.match(append, /if \(bytes\)[\s\S]*compactAudioPlayer\(audio, bytes\)/);
  assert.match(append, /else \{\s*player = pendingVoiceMessage\("Preparing voice message", false\)/);
  assert.doesNotMatch(append, /player\.querySelector\("\.voice-message-play"\)\.disabled/);
  const hydrate = block(ui, "  const hydrateOptimisticVoiceMessage =", "  const appendLiveVoiceReply =");
  assert.match(hydrate, /player\.replaceWith\(compactAudioPlayer\(audio, bytes\)\)/);
  assert.doesNotMatch(hydrate, /play\.disabled = false/);
  assert.doesNotMatch(hydrate, /classList\.remove\("voice-message-pending"\)/);
});

test("an early voice-reply tap starts automatically when the startup buffer fills", async () => {
  const h = speechLifetimeHarness();
  const started = [];
  const controller = h.createPcmPlaybackBuffer({
    onPlaybackStarted: () => started.push(true),
  });
  // Each segment is 50 ms at 24 kHz: the first validated PCM makes the
  // control real, but remains below the 250 ms anti-underrun threshold.
  const pcm = new Uint8Array(2_400);
  controller.pushSegment(h.pcmFrame(pcm), "segment-1");
  assert.equal(controller.ready, false);
  assert.equal(await controller.play(), false);
  assert.equal(h.audioContexts.length, 0);
  controller.pushSegment(h.pcmFrame(pcm), "segment-2");
  assert.equal(h.audioContexts.length, 0);
  controller.pushSegment(h.pcmFrame(pcm), "segment-3");
  assert.equal(h.audioContexts.length, 0);
  controller.pushSegment(h.pcmFrame(pcm), "segment-4");
  assert.equal(h.audioContexts.length, 0);
  controller.pushSegment(h.pcmFrame(pcm), "segment-5");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(controller.ready, true);
  assert.equal(h.audioContexts.length, 1);
  assert.equal(h.audioContexts[0].sources.length, 5);
  assert.equal(started.length, 1);
  assert.equal(started[0], true);
  await controller.stop();
});

test("a streaming player reports a real render failure instead of keeping a dead ready state", async () => {
  const h = speechLifetimeHarness();
  const failures = [];
  h.rejectPlaybackWith(new Error("fixture audio context rejection"));
  const controller = h.createPcmPlaybackBuffer({
    onPlaybackError: (error) => failures.push(error.message),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  assert.equal(controller.ready, true);
  assert.equal(await controller.play(), false);
  assert.deepEqual(failures, ["fixture audio context rejection"]);
  assert.equal(controller.playing, false);
  await controller.stop();
});

test("a hung browser audio resume reaches a terminal failure instead of spinning forever", async () => {
  const h = speechLifetimeHarness();
  const failures = [];
  h.hangPlaybackResume();
  const controller = h.createPcmPlaybackBuffer({
    onPlaybackError: (error) => failures.push(error.message),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  const playing = controller.play();
  await Promise.resolve();
  await Promise.resolve();
  h.timers.at(-1)();
  assert.equal(await playing, false);
  assert.deepEqual(failures, ["speech_playback_start_timeout: Audio playback did not start."]);
  assert.equal(controller.playing, false);
  assert.equal(h.audioContexts[0].closed, true);
});

test("live voice synthesis has an idle-progress deadline after first PCM", () => {
  const live = block(ui, "  const beginLiveVoiceTts =", "  const chatRequestIsCancellable =");
  assert.match(live, /const armAudioProgressWatchdog =/);
  assert.doesNotMatch(live, /if \(!deferProgressWatchdog\) armAudioProgressWatchdog\(\)/);
  assert.match(live, /running = true;[\s\S]*?armAudioProgressWatchdog\(\);/);
  assert.match(live, /audioProgressWatchdog = null;\s*watchdogActive = false;/);
  assert.match(live, /if \(decoded\.data\.byteLength > 0\) armAudioProgressWatchdog\(\)/);
  assert.doesNotMatch(live, /controller\.pushSegment\(frame, operation\);\s*armAudioProgressWatchdog\(\)/);
  assert.doesNotMatch(live, /onPlayable: \(\) => \{\s*window\.clearTimeout\(audioProgressWatchdog\)/);
  assert.match(live, /voice_reply_timeout: Voice synthesis stopped making progress\./);
  assert.match(live, /invoke\("mom_llama_speech_stop", \{ operation \}\)/);
});

test("natural playback end clears pressed state and restores the Read Aloud button", async () => {
  const h = speechLifetimeHarness();
  try {
    await h.finishPlayback();
    h.timers.splice(0).forEach((timer) => timer());
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(h.active(), null);
    assert.equal(h.button.attributes.get("aria-pressed"), undefined);
    assert.equal(h.button.dataset.playback, undefined);
    const stopping = await h.take("mom_llama_speech_stop");
    stopping.reply.resolve({ status: "passed" });
  } finally { h.settle(); }
});

test("audio ending before the native receipt releases playback without cancelling synthesis", async () => {
  const h = speechLifetimeHarness();
  try {
    const started = await h.start();
    started.read.call.args.pcmStream.onmessage(h.pcmFrame());
    await Promise.resolve();
    await Promise.resolve();
    h.timers.splice(0).forEach((timer) => timer());
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(
      h.calls.filter((call) => call.command === "mom_llama_speech_stop").length,
      0,
      "natural audio completion must not cancel an operation awaiting its native receipt",
    );
    started.read.reply.resolve(h.result());
    const stop = await h.take("mom_llama_speech_stop");
    assert.equal(stop.call.args.playback, "fixture-playback");
    assert.equal(stop.call.args.operation, undefined);
    stop.reply.resolve({ status: "passed" });
    await started.task;
    assert.equal(h.button.attributes.get("aria-pressed"), undefined);
  } finally { h.settle(); }
});

test("current Read Aloud reaches real render and Stop releases its media and exact playback", async () => {
  const h = speechLifetimeHarness();
  try {
    await h.finishPlayback();
    const stopping = h.actions["message-read-aloud"](h.button);
    const stop = await h.take("mom_llama_speech_stop");
    assert.equal(stop.call.args.playback, "fixture-playback");
    assert.equal(h.active(), null); assert.equal(h.audioContexts[0].closed, true);
    stop.reply.resolve({ status: "passed" }); await stopping;
    assert.equal(h.button.dataset.operation, undefined); assert.equal(h.button.dataset.playback, undefined);
  } finally { h.settle(); }
});

test("Read Aloud accepts framed PCM delivered by packaged macOS IPC", async () => {
  const h = speechLifetimeHarness();
  try {
    await h.finishPlayback([1, 2]);
    assert.equal(h.audioContexts.length, 1);
    assert.equal(h.audioContexts[0].sources.length, 1);
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
      assert.equal(h.active(), null, `revoked ${interruption} reply retained browser playback`);
      assert.ok(h.calls.some((call) => call.command === "mom_llama_speech_stop" && call.args.playback === "fixture-playback"));
    } finally { h.settle(); }
  });
}

test("Stop after first PCM prevents a late final response from restoring playback", async () => {
  const h = speechLifetimeHarness();
  try {
    const { task, read } = await h.start();
    read.call.args.pcmStream.onmessage(h.pcmFrame());
    await Promise.resolve();
    const stopping = h.actions["message-read-aloud"](h.button);
    (await h.take("mom_llama_speech_stop")).reply.resolve({ status: "passed" }); await stopping;
    read.reply.resolve(h.result()); h.respondRemainder(); await task;
    assert.equal(h.active(), null, "stopped playback was restored by the late final response");
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
    assert.equal(h.audioContexts.length, 1, "only the earlier current playback reached the browser adapter");
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

test("an older completion timer cannot stop the newer streaming playback", async () => {
  const h = speechLifetimeHarness();
  try {
    await h.finishPlayback();
    const olderTimer = h.timers[0];
    const nextButton = h.newReadButton("fixture-next-message");
    const next = h.actions["message-read-aloud"](nextButton);
    const cleanup = await h.take("mom_llama_speech_stop");
    cleanup.reply.resolve({ status: "passed" });
    const nextRead = await h.take("mom_llama_speech_read_aloud", 1);
    nextRead.call.args.pcmStream.onmessage(h.pcmFrame());
    h.respondRemainder(); nextRead.reply.resolve(h.result("fixture-next-playback", "fixture-next-message")); await next;
    assert.equal(h.active().id, "fixture-next-playback");
    assert.equal(h.audioContexts.length, 2);
    const callsBeforeEnded = h.calls.length;
    olderTimer();
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

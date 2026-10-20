"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const voicePipelineScheduler = require("./voice-pipeline-scheduler.js");
const voiceCapturePolicy = require("./voice-capture-policy.js");
const { AdaptiveVoiceLeadController } = voicePipelineScheduler;

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

test("failed live voice synthesis republishes its unavailable reply row", () => {
  const settle = block(
    ui,
    "const settleUnavailableVoiceReply =",
    "const restoreReadAloudButton =",
  );
  assert.match(settle, /card\.replaceChildren\(unavailableVoiceMessage\(\)\)/);
  assert.match(settle, /row\.hidden = false/);
  assert.match(settle, /row\.classList\.remove\("voice-reply-pending"\)/);
  const live = block(ui, "  const beginLiveVoiceTts =", "  const chatRequestIsCancellable =");
  assert.match(live, /if \(!continueCall\) abortUnclaimedCallPrefill\(\);[\s\S]*?settleUnavailableVoiceReply\(button\)/);
});

test("explicit Read Aloud preserves the text card", () => {
  const action = block(
    ui,
    '"message-read-aloud": async',
    '"attachment-transcribe": async',
  );
  assert.doesNotMatch(action, /personaVoice|prepareOnly|voice-reply-row/);
  assert.match(action, /card\?\.append\(panel\)/);
  assert.match(action, /requirePersonaVoice: false/);
  assert.doesNotMatch(action, /mom_llama_speech_precommit_synthesize|purpose:\s*["']call/,
    "Read Aloud remains on its separate native request");
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

test("hydration preserves saved or unavailable voice rows and hides only the exact live reply", async () => {
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
  const pairs = ["old", "missing", "saved", "newest"].map((id) => {
    const assistant = {
      hidden: false,
      dataset: { messageId: id },
      classList: classList(id === "missing" || id === "saved"
        ? ["assistant", "voice-message-row"] : ["assistant"]),
      querySelector(selector) {
        if (selector === "[data-action='message-read-aloud']") return this.button;
        if (selector === ".message-card") return this.card;
        if (selector === ".message-actions") return null;
        if (selector === ".message-attachments [data-attachment-preview]") {
          return id === "saved" ? {} : null;
        }
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
      if (selector === ".message-row.user") return pairs.map(({ user }) => user);
      if (selector === ".message-row.assistant") return pairs.map(({ assistant }) => assistant);
      return [];
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
    failedVoiceReplyOwners: new Map(),
    voiceReplyPersistenceKey: (conversation, message) => `${conversation}\u0000${message}`,
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
  assert.equal(pairs[1].assistant.hidden, false, "a missing historical recording stays an unavailable voice row");
  assert.equal(pairs[1].assistant.classList.has("voice-message-row"), true);
  assert.equal(pairs[2].assistant.hidden, false, "a saved voice attachment remains visible");
  assert.equal(pairs[2].assistant.classList.has("voice-message-row"), true);
  assert.equal(pairs[3].assistant.hidden, true, "only the exact live reply stays hidden until publication");
  assert.equal(pairs[3].assistant.classList.has("voice-reply-pending"), true);
  assert.equal(pairs[3].assistant.button.dataset.prepareOnly, undefined);
  assert.deepEqual(observedPreviews, ["newest", "old"], "lazy attachment hydration must begin nearest the composer");
});

test("navigation rebinds a retained failed-save owner to its exact voice reply", async () => {
  const classes = new Set(["assistant", "voice-message-row"]);
  const buttonClasses = new Set();
  const button = {
    dataset: { action: "voice-reply-pending-play" },
    classList: { add: (...values) => values.forEach((value) => buttonClasses.add(value)) },
    setAttribute(name, value) { this[name] = value; },
    removeAttribute(name) { delete this[name]; },
  };
  const player = { querySelector: (selector) => selector === ".voice-reply-play" ? button : null };
  const card = { replaceChildren(...children) { this.children = children; } };
  const row = {
    hidden: true,
    dataset: { messageId: "assistant-20" },
    classList: { add: (...values) => values.forEach((value) => classes.add(value)) },
    querySelector(selector) {
      if (selector === ".message-card") return card;
      if (selector === ".message-attachments [data-attachment-preview]") return null;
      return null;
    },
  };
  const playback = {
    controller: { ready: true },
    trace: { id: "trace-20" },
    bindButton(next) { this.button = next; return true; },
  };
  const retained = new Map([["chat-20\u0000assistant-20", playback]]);
  const root = {
    matches: () => false,
    querySelectorAll: (selector) => selector === ".message-row.assistant" ? [row] : [],
  };
  const hydrate = block(ui, "  const hydrateAttachmentPreviews = async", "  window.addEventListener(\"beforeunload\"");
  const context = {
    document: root,
    attachmentPreviewsWithin: () => [],
    selectedConversation: () => "chat-20",
    voiceReplyConversation: null,
    voiceReplyMessageId: null,
    failedVoiceReplyOwners: retained,
    voiceReplyPersistenceKey: (conversation, message) => `${conversation}\u0000${message}`,
    pendingVoiceReplyPlayer: () => player,
    settleInMemoryVoiceReply(nextButton, controller, trace) {
      assert.equal(nextButton, button);
      assert.equal(controller, playback.controller);
      assert.equal(trace, playback.trace);
      nextButton.dataset.audioState = "ready";
    },
  };
  speechVm.createContext(context);
  speechVm.runInContext(hydrate + "\nthis.hydrate = hydrateAttachmentPreviews;", context);
  await context.hydrate(root);

  assert.equal(row.hidden, false);
  assert.ok(classes.has("voice-reply-row"));
  assert.equal(playback.button, button);
  assert.equal(button.dataset.action, "voice-reply-pending-play");
  assert.equal(button.dataset.audioPersistence, "failed");
  assert.equal(button.dataset.audioState, "ready");
});

test("a retained failed voice-note save blocks another capture but leaves calls available", async () => {
  const begin = block(ui, "  const beginVoiceCapture = async", "  const resumeBrowserVoicePrefill =");
  const prepared = [];
  const context = {
    failedVoiceReplyOwners: new Map([["chat\u0000message", {}]]),
    MAX_FAILED_VOICE_REPLY_OWNERS: 1,
    activeVoiceNotePublications: new Set(),
    preparePcmWorklet: () => prepared.push("worklet"),
    beginBrowserVoiceCapture: async (options) => ({ call: options.call }),
    beginNativeVoiceCapture: async () => ({ native: true }),
    markVoiceLatency() {},
    errorMessage: String,
  };
  speechVm.createContext(context);
  speechVm.runInContext(`${begin}\nthis.beginVoiceCapture = beginVoiceCapture;`, context);

  await assert.rejects(
    context.beginVoiceCapture(),
    /voice_reply_save_retry_required/,
  );
  assert.deepEqual(prepared, [], "blocked note capture must not start microphone preparation");
  context.failedVoiceReplyOwners.clear();
  context.activeVoiceNotePublications.add({});
  await assert.rejects(context.beginVoiceCapture(), /voice_reply_save_retry_required/);
  assert.deepEqual(prepared, [], "a pending publication also reserves the single note slot");
  context.activeVoiceNotePublications.clear();
  assert.deepEqual(await context.beginVoiceCapture({ call: true }), { call: true });
  assert.deepEqual(prepared, ["worklet"], "call capture remains available and starts its normal setup");
});

test("voice dispatch keeps its live PCM row mounted through completion", () => {
  const submit = block(
    ui,
    "  document.addEventListener(\"submit\"",
    "  document.addEventListener(\"compositionstart\"",
  );
  assert.match(submit, /if \(!voiceDispatch\) \{[\s\S]*await refreshConversationProjection\(\);[\s\S]*\} else \{[\s\S]*Keep the live audio rows mounted/);
  assert.match(submit, /const voiceTtsOwner = voiceDispatch/);
  assert.match(submit, /finishVoiceReplyAndIndexTurn\([\s\S]*result\?\.result\?\.kind === "direct"[\s\S]*indexVoiceTurn\(conversation, output, voiceTranscriptAttachment,[\s\S]*\(\) => voiceTtsOwner\.finish\(output\.assistant_text\)/,
    "call transcript indexing is admitted before TTS drains; voice-note backfill waits for the helper's successful finish");
  assert.match(submit, /voice_reply_owner_missing/);
  assert.match(submit, /refreshSidebar\(\), refreshConversationSettingsProjection\(\)/);
  assert.match(ui, /const replayBytes = capture\.wavBytes \|\| encodePcmWav\(capture\.samples\(\)\)/);
  assert.match(ui, /appendOptimisticVoiceMessage\(replayBytes\)/);

  assert.doesNotMatch(ui, /prepareVoiceReplyRow/);
  assert.doesNotMatch(ui, /dataset\.personaVoice\s*=|dataset\.prepareOnly\s*=/);

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
const speechLifetimeHarness = ({ worklet = false, workletPreparationGate = null, playbackResumeGate = null, suspendGate = null } = {}) => {
  const pending = [];
  const calls = [];
  const hashes = [];
  const media = [];
  const urls = [];
  const audioContexts = [];
  const analysers = [];
  const workletNodes = [];
  const timers = [];
  const progressIntervals = [];
  let selected = "fixture-conversation";
  let now = 1;
  const traces = [];
  let serial = 0;
  let automaticReplies = false;
  let automaticReadResult = null;
  let mediaReadyState = 3;
  let playbackResumeError = null;
  let playbackResumePending = false;
  let suspendGateEnabled = false;
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
    performance: { now: () => now },
    MomVoiceCapturePolicy: voiceCapturePolicy,
    setInterval(callback) { progressIntervals.push({ callback, active: true }); return progressIntervals.length; },
    clearInterval(id) { if (progressIntervals[id - 1]) progressIntervals[id - 1].active = false; },
    crypto: { randomUUID: () => `fixture-trace-${++serial}` },
    MomVoicePipelineScheduler: { AdaptiveVoiceLeadController },
    markVoiceLatencyIn: (trace, stage, detail = {}) => traces.push({ id: trace.id, stage, ...detail }),
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
      this.closed = false; this.state = "running"; this.suspendCount = 0;
      if (worklet) this.audioWorklet = { addModule: () => workletPreparationGate?.promise || Promise.resolve() };
      audioContexts.push(this);
    }
    async resume() {
      if (playbackResumeError) throw playbackResumeError;
      if (playbackResumePending) return new Promise(() => {});
      if (playbackResumeGate) await playbackResumeGate.promise;
      this.state = "running";
    }
    async suspend() {
      this.suspendCount += 1;
      this.suspendCalled = true;
      if (suspendGate && suspendGateEnabled) await suspendGate.promise;
      this.state = "suspended";
    }
    async close() { this.closed = true; this.state = "closed"; }
    createAnalyser() {
      const analyser = {
        fftSize: 0, connected: false, disconnected: false,
        connect() { this.connected = true; },
        disconnect() { this.connected = false; this.disconnected = true; },
        getFloatTimeDomainData(samples) { samples.fill(0); },
      };
      analysers.push(analyser);
      return analyser;
    }
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
    + block(ui, "  const withProgressDeadline = async", "  const withDeadline = async")
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
  const pcmFrame = (payload = new Uint8Array([0, 0]), sequence = 0, end = true, sampleOffset = 0) => {
    const samples = payload.byteLength % 2 === 0 ? payload : new Uint8Array([0, 0]);
    const frame = new Uint8Array(32 + samples.byteLength); const view = new DataView(frame.buffer);
    frame.set([0x4d, 0x50, 0x43, 0x4d, 1, end ? 3 : 2, 1, 1]);
    view.setUint32(8, 24000, true); view.setBigUint64(12, BigInt(sequence), true);
    view.setBigUint64(20, BigInt(sampleOffset), true); view.setUint32(28, samples.byteLength, true); frame.set(samples, 32);
    return frame;
  };
  return { ...context.api, traces, advance(ms) { now += ms; }, fireProgressDeadline() { progressIntervals.filter(({ active }) => active).at(-1)?.callback(); }, button, transcribe, preview, transcript, card, calls, media, urls, audioContexts, analysers, workletNodes, timers, pcmFrame, take, hash, result, start, finishPlayback, respondRemainder, newReadButton, settle,
    setMediaReadyState(state) { mediaReadyState = state; },
    rejectPlaybackWith(error) { playbackResumeError = error; },
    holdSuspend() { suspendGateEnabled = true; },
    hangPlaybackResume() { playbackResumePending = true; },
    navigate() { selected = "fixture-other-conversation"; } };
};

test("Read Aloud buffers packet jitter through the shared adaptive renderer", async () => {
  const h = speechLifetimeHarness({ worklet: true });
  const { task, read } = await h.start();
  const firstFrames = 3328; // The real helper's short initial codec packet (~139 ms).
  read.call.args.pcmStream.onmessage(h.pcmFrame(new Uint8Array(firstFrames * 2), 0, false));
  for (let i = 0; i < 20; i += 1) await Promise.resolve();
  assert.equal(h.workletNodes.length, 0, "one short packet must not start Read Aloud");
  h.advance(220);
  const nextFrames = 11520;
  read.call.args.pcmStream.onmessage(h.pcmFrame(new Uint8Array(nextFrames * 2), 1, false, firstFrames));
  for (let i = 0; i < 40; i += 1) await Promise.resolve();
  assert.equal(h.workletNodes.length, 1, "bounded headroom permits playback before message EOF");
  const started = h.traces.find((entry) => entry.stage === "playback_started");
  assert.ok(started.buffered_ms >= 600);
  read.call.args.pcmStream.onmessage(h.pcmFrame(new Uint8Array(), 2, true, firstFrames + nextFrames));
  const response = h.result();
  response.result.binding.backend_id = "fixture-qwen";
  response.result.binding.model_id = "fixture-read-aloud-model";
  response.result.binding.voice_id = "fixture-persona-voice";
  response.result.binding.tts_generation_segments = [{ stop_reason: "eos", generated_codec_frames: 7 }];
  read.reply.resolve(response);
  await task;
  assert.equal(h.traces.find((entry) => entry.stage === "tts_backend_complete")
    .generation_segments[0].stop_reason, "eos");
  const route = h.traces.find((entry) => entry.stage === "tts_backend_complete");
  assert.deepEqual(
    { backend_id: route.backend_id, model_id: route.model_id, voice_id: route.voice_id },
    { backend_id: "fixture-qwen", model_id: "fixture-read-aloud-model", voice_id: "fixture-persona-voice" },
  );
  assert.equal(h.traces.find((entry) => entry.stage === "tts_pcm_complete").pcm_packets, 2);
  h.settle();
});

test("Read Aloud timeout revokes PCM ownership and waits for native cancellation", async () => {
  const h = speechLifetimeHarness();
  const { task, read } = await h.start();
  h.advance(30_001);
  h.fireProgressDeadline();
  const cancellation = await h.take("mom_llama_speech_stop_and_wait");
  assert.equal(h.active(), null, "the timed-out renderer is revoked before native cancellation settles");
  read.call.args.pcmStream.onmessage(h.pcmFrame(new Uint8Array(64), 0, false));
  assert.ok(h.audioContexts.every((context) => context.sources.length === 0),
    "late PCM cannot start playback after timeout");
  assert.equal(cancellation.call.args.operation, read.call.args.operation);
  assert.equal(cancellation.call.args.allowColdTranscriptionSettlement, false);
  cancellation.reply.resolve({ status: "passed", result: { terminal_acknowledged: true } });
  await assert.rejects(task, /Read Aloud stopped making progress/);
  assert.ok(h.traces.some((entry) => entry.stage === "tts_timed_out"
    && entry.waiting_for === "mom_llama_speech_read_aloud"));
  read.reply.resolve(h.result("late-read-aloud-playback"));
  const lateStop = await h.take("mom_llama_speech_stop");
  assert.equal(lateStop.call.args.playback, "late-read-aloud-playback",
    "a handle retained after timeout is released even after operation cancellation acknowledgement");
});

test("Read Aloud progress remains eligible beyond the call-generation wall-clock cap", async () => {
  const h = speechLifetimeHarness();
  const { task, read } = await h.start();
  h.advance(121_000);
  read.call.args.pcmStream.onmessage(h.pcmFrame(new Uint8Array(24_000), 0, true));
  h.fireProgressDeadline();
  assert.ok(h.active(), "fresh PCM progress keeps long saved-message synthesis alive");
  read.reply.resolve(h.result());
  await task;
  h.settle();
});

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

test("a cold reply's pending segment stays gated until its measured completion reconsiders startup", async () => {
  const h = speechLifetimeHarness();
  let probePending = true;
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
    waitForCompletion: () => probePending,
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000), 0, false));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(controller.ready, true, "the first PCM is playable in size");
  assert.equal(h.audioContexts.length, 0, "the cold probe still blocks audible playback");

  probePending = false;
  await controller.reconsiderStartup();
  assert.equal(h.audioContexts.length, 1, "clearing the measured gate starts without another packet or reply finish");
  assert.equal(h.audioContexts[0].sources.length, 1);
  await controller.stop();
});

test("the packet-level cold probe starts the playback controller before EOF and cannot wake a stopped player", async () => {
  const timeline = [
    [3_328, 0], [5_760, 123.7], [9_600, 319.3], [9_600, 517.6], [9_600, 720.5],
  ];
  const make = () => {
    const h = speechLifetimeHarness();
    const lead = new AdaptiveVoiceLeadController({
      initialBufferComplete: true,
      initialProbePending: true,
    });
    const playback = h.createPcmPlaybackBuffer({
      autoplay: true,
      retainHistory: false,
      waitForCompletion: () => lead.shouldBufferCompleteReply(),
    });
    let previousArrival = 0;
    let previousPcmMs = 0;
    let sampleOffset = 0;
    const push = async (index) => {
      const [samples, arrivalMs] = timeline[index];
      const audioMs = samples / 24;
      lead.notePacket({
        audioMs,
        gapMs: index === 0 ? null : arrivalMs - previousArrival,
        startupDeficitMs: Math.max(0, arrivalMs - previousPcmMs),
      });
      playback.push(h.pcmFrame(new Uint8Array(samples * 2), index, false, sampleOffset));
      previousArrival = arrivalMs;
      previousPcmMs += audioMs;
      sampleOffset += samples;
      await new Promise((resolve) => setImmediate(resolve));
    };
    return { h, lead, playback, push };
  };

  const active = make();
  for (let index = 0; index < 4; index += 1) {
    await active.push(index);
    assert.equal(active.h.audioContexts.length, 0, `packet ${index + 1} stays behind the cold gate`);
  }
  assert.equal(active.lead.shouldBufferCompleteReply(), true);
  await active.push(4);
  assert.equal(active.lead.shouldBufferCompleteReply(), false);
  assert.equal(active.playback.ready, true);
  assert.equal(active.h.audioContexts.length, 1, "packet five creates the test renderer before EOF");
  assert.equal(active.playback.playing, true);
  await active.playback.stop();

  const stopped = make();
  for (let index = 0; index < 4; index += 1) await stopped.push(index);
  assert.equal(stopped.h.audioContexts.length, 0);
  await stopped.playback.stop();
  await stopped.push(4);
  await stopped.playback.reconsiderStartup();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(stopped.lead.shouldBufferCompleteReply(), false, "the evidence can settle after stop");
  assert.equal(stopped.h.audioContexts.length, 0, "late packet evidence cannot wake the stopped renderer");
});

test("a stopped player cannot be woken by clearing its cold startup gate", async () => {
  const h = speechLifetimeHarness();
  let probePending = true;
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
    waitForCompletion: () => probePending,
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000), 0, false));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.audioContexts.length, 0);

  await controller.stop();
  probePending = false;
  await controller.reconsiderStartup();
  assert.equal(h.audioContexts.length, 0, "stopped buffers cannot restart after late calibration");
});

test("stop during delayed worklet preparation settles queued playback without creating a renderer", async () => {
  const preparation = speechDeferred();
  const h = speechLifetimeHarness({ worklet: true, workletPreparationGate: preparation });
  const started = [];
  const ended = [];
  const firstChunks = [];
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
    onPlaybackStarted: () => started.push(true),
    onEnded: () => ended.push(true),
    onFirstChunk: (event) => firstChunks.push(event),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000), 0, false));
  const playback = controller.play();
  controller.finish();
  assert.equal(firstChunks.length, 1, "accepted PCM still settles first-chunk notification");
  assert.equal(h.workletNodes.length, 0);

  await controller.stop();
  assert.equal(await playback, false, "stop settles the queued start request");
  preparation.resolve();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.workletNodes.length, 0, "late preparation cannot create a capture-triggering renderer");
  assert.deepEqual(started, []);
  assert.deepEqual(ended, [], "stopped queued completion is not reported as natural playback end");
});

test("cancelled delayed resume cannot suspend a successor on the shared context", async () => {
  const resume = speechDeferred();
  const h = speechLifetimeHarness({ worklet: true, playbackResumeGate: resume });
  const started = [];
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
    onPlaybackStarted: () => started.push(true),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000), 0, false));
  const playback = controller.play();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.workletNodes.length, 1);
  const context = h.audioContexts[0];
  const oldNode = h.workletNodes[0];
  const oldMessageHandler = oldNode.port.onmessage;
  const suspendsBeforeStop = context.suspendCount;

  await controller.stop();
  assert.equal(await playback, false, "stop settles even while browser resume is pending");
  assert.equal(context.suspendCount, suspendsBeforeStop, "stopping a cancelled owner does not suspend the shared clock");

  const successorStarted = [];
  const successor = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
    onPlaybackStarted: () => successorStarted.push(true),
  });
  successor.push(h.pcmFrame(new Uint8Array(6_000)));
  const successorPlayback = successor.play();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.workletNodes.length, 2);
  const successorNode = h.workletNodes[1];
  resume.resolve();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(started, [], "stale resume cannot announce playback or open capture");
  assert.equal(await successorPlayback, true);
  assert.deepEqual(successorStarted, [true]);
  assert.equal(context.state, "running", "late completion from the cancelled owner cannot suspend its successor");
  assert.equal(successorNode.connected, true);
  assert.doesNotThrow(() => oldMessageHandler({ data: { type: "drained", renderedFrames: 0 } }));
  assert.equal(context.state, "running");
  await successor.stop();
});

test("stop during a paused-context resume cannot schedule queued PCM or announce playback", async () => {
  const resumeGate = { promise: Promise.resolve() };
  const h = speechLifetimeHarness({ playbackResumeGate: resumeGate });
  const started = [];
  const controller = h.createPcmPlaybackBuffer({
    retainHistory: false,
    onPlaybackStarted: () => started.push(true),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000), 0, false));
  assert.equal(await controller.play(), true);
  assert.deepEqual(started, [true]);
  assert.equal(await controller.pause(), true);
  const delayedResume = speechDeferred();
  resumeGate.promise = delayedResume.promise;

  const playback = controller.play();
  await Promise.resolve();
  await controller.stop();
  assert.equal(await playback, false, "stop settles the paused-context resume request");
  delayedResume.resolve();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(started, [true], "a stale resume cannot announce a second audible start");
  assert.equal(h.audioContexts[0].closed, true, "late resume cannot leave the stopped context running");
  assert.equal(h.audioContexts[0].sources.length, 1, "stale resume does not reschedule queued PCM");
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
  assert.equal(h.audioContexts[0].state, "running", "drain leaves the shared output clock running");
  assert.equal(h.analysers[0].disconnected, true);
  await controller.stop();
});

test("shared worklet context survives drain and stop; stale old drain cannot pause its successor", async () => {
  const h = speechLifetimeHarness({ worklet: true });
  const firstEnded = [];
  const first = h.createPcmPlaybackBuffer({
    autoplay: true, retainHistory: false,
    onEnded: () => firstEnded.push(true),
  });
  first.push(h.pcmFrame(new Uint8Array(6_000)));
  await new Promise((resolve) => setImmediate(resolve));
  const context = h.audioContexts[0];
  const oldNode = h.workletNodes[0];
  const alreadyQueuedDrainHandler = oldNode.port.onmessage;
  const suspendsAfterStart = context.suspendCount;
  first.finish();
  alreadyQueuedDrainHandler({ data: { type: "drained", renderedFrames: 3_000 } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(context.state, "running", "natural drain keeps the prepared shared audio clock alive");
  assert.equal(context.suspendCount, suspendsAfterStart);
  assert.deepEqual(firstEnded, [true]);
  assert.equal(h.analysers[0].disconnected, true, "natural EOF releases the old analyser graph");

  const second = h.createPcmPlaybackBuffer({ autoplay: true, retainHistory: false });
  second.push(h.pcmFrame(new Uint8Array(6_000)));
  await new Promise((resolve) => setImmediate(resolve));
  const newNode = h.workletNodes[1];
  assert.equal(h.audioContexts.length, 1, "the successor reuses the prepared context");
  assert.equal(context.state, "running");
  assert.doesNotThrow(() => alreadyQueuedDrainHandler({ data: { type: "drained", renderedFrames: 3_000 } }));
  assert.equal(context.state, "running", "a late old-owner event cannot suspend successor playback");
  assert.equal(newNode.connected, true, "late old-owner events leave the successor renderer connected");
  assert.deepEqual(firstEnded, [true], "a stale duplicate drain is ignored after the old renderer ended");
  assert.equal(h.analysers[1].connected, true, "the successor retains its own connected analyser graph");

  await second.stop();
  assert.equal(context.state, "running", "stopping one owner does not suspend the shared context");
  assert.equal(context.suspendCount, suspendsAfterStart);
});

test("explicit worklet pause and resume preserve the running shared audio clock", async () => {
  const h = speechLifetimeHarness({ worklet: true });
  const controller = h.createPcmPlaybackBuffer({ autoplay: true, retainHistory: false });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  await new Promise((resolve) => setImmediate(resolve));
  const context = h.audioContexts[0];
  const node = h.workletNodes[0];
  const suspendsAfterStart = context.suspendCount;

  assert.equal(await controller.pause(), true);
  assert.equal(context.state, "running", "pause is implemented in the worklet, not by suspending the shared context");
  assert.equal(context.suspendCount, suspendsAfterStart);
  assert.equal(node.port.messages.at(-1).type, "pause");
  assert.equal(await controller.play(), true);
  assert.equal(context.state, "running");
  assert.equal(context.suspendCount, suspendsAfterStart);
  assert.equal(node.port.messages.at(-1).type, "resume");
  await controller.stop();
  assert.equal(h.analysers[0].disconnected, true, "stop disconnects the paused owner graph");
});

test("a drain already queued before pause still settles natural playback", async () => {
  const h = speechLifetimeHarness({ worklet: true });
  const ended = [];
  const metrics = [];
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
    onEnded: () => ended.push(true),
    onRendererMetrics: (value) => metrics.push(value),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  await new Promise((resolve) => setImmediate(resolve));
  const node = h.workletNodes[0];
  const alreadyQueuedDrainHandler = node.port.onmessage;
  controller.finish();
  assert.equal(node.port.messages.at(-1).type, "finish");
  assert.equal(await controller.pause(), true);
  assert.equal(controller.playing, false);
  assert.equal(node.port.messages.at(-1).type, "pause");

  alreadyQueuedDrainHandler({ data: { type: "drained", renderedFrames: 3_000, underruns: 0 } });
  assert.deepEqual(ended, [true], "a valid terminal drain is honored even after pause changed UI playback state");
  assert.equal(metrics.at(-1).reason, "ended");
  assert.equal(metrics.at(-1).underruns, 0);
  assert.equal(node.connected, false);
  assert.equal(h.analysers[0].disconnected, true);
  assert.equal(h.audioContexts[0].state, "running");
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

test("stopped worklet reports unknown final counters but keeps observed queue and first render", async () => {
  const h = speechLifetimeHarness({ worklet: true });
  const receipts = [];
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true, retainHistory: false,
    onRendererMetrics: (metrics) => receipts.push(metrics),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  await new Promise((resolve) => setImmediate(resolve));
  const node = h.workletNodes[0];
  node.port.onmessage({ data: {
    type: "render_started", bufferedFrames: 3_000, audioContextFrame: 17,
  } });
  await controller.stop();
  const receipt = receipts.at(-1);
  assert.equal(receipt.worklet_metrics_status, "unreported");
  assert.equal(receipt.worklet_final_metrics_received, false);
  assert.equal(receipt.underruns, null);
  assert.equal(receipt.worklet_underrun_events_observed, 0);
  assert.equal(receipt.worklet_render_quanta, null);
  assert.equal(receipt.worklet_pcm_low_energy_frames, null);
  assert.equal(receipt.worklet_max_queued_frames, 3_000);
  assert.equal(receipt.worklet_first_render_context_frame, 17);
});

test("drained worklet preserves measured zeros and an unknown nullable minimum", async () => {
  const h = speechLifetimeHarness({ worklet: true });
  const receipts = [];
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true, retainHistory: false,
    onRendererMetrics: (metrics) => receipts.push(metrics),
  });
  controller.push(h.pcmFrame(new Uint8Array(6_000)));
  await new Promise((resolve) => setImmediate(resolve));
  const node = h.workletNodes[0];
  controller.finish();
  node.port.onmessage({ data: {
    type: "drained", renderQuanta: 0, maxBufferedFrames: 0,
    underruns: 0, recoveries: 0, rebufferedFrames: 0,
    minBufferedFrames: null, firstReceiveContextFrame: 0,
    firstRenderContextFrame: null, pcmSignalFrames: 0, pcmSignalRms: null,
    pcmSignalPeak: null, pcmSignalMinQuantumRms: null,
    pcmSignalMaxQuantumRms: null, pcmLowEnergyThresholdRms: null,
    pcmLowEnergyFrames: 0, pcmLowEnergyQuanta: 0,
    pcmMaxLowEnergyFrames: 0,
  } });
  const receipt = receipts.at(-1);
  assert.equal(receipt.worklet_metrics_status, "complete");
  assert.equal(receipt.worklet_final_metrics_received, true);
  assert.equal(receipt.underruns, 0);
  assert.equal(receipt.worklet_render_quanta, 0);
  assert.equal(receipt.worklet_pcm_low_energy_frames, 0);
  assert.equal(receipt.worklet_min_buffered_frames, null);
  assert.equal(receipt.worklet_first_receive_context_frame, 0);
  assert.equal(receipt.worklet_first_render_context_frame, null);
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
  assert.match(live, /if \(decoded\.data\.byteLength > 0\) \{\s*playback\.lastProgressAt = performance\.now\(\);\s*armAudioProgressWatchdog\(\);/);
  assert.doesNotMatch(live, /controller\.pushSegment\(frame, operation\);\s*armAudioProgressWatchdog\(\)/);
  assert.doesNotMatch(live, /onPlayable: \(\) => \{\s*window\.clearTimeout\(audioProgressWatchdog\)/);
  assert.match(live, /voice_reply_timeout: Voice synthesis stopped making progress\./);
  assert.match(live, /invoke\("mom_llama_speech_stop", \{ operation \}\)/);
});

const liveTtsTailHarness = ({ callActive = false, callEpoch = 0 } = {}) => {
  const invocations = [];
  const delivered = [];
  const timers = new Map();
  let operationNumber = 0;
  let now = 0;
  let timerNumber = 0;
  let playbackCallbacks = null;
  let firstPcmObserved = false;
  let bufferedMs = 0;
  const wavBytes = new Uint8Array(46);
  wavBytes.set(Buffer.from("RIFF"), 0);
  new DataView(wavBytes.buffer).setUint32(4, 38, true);
  wavBytes.set(Buffer.from("WAVEfmt "), 8);
  new DataView(wavBytes.buffer).setUint32(16, 16, true);
  new DataView(wavBytes.buffer).setUint16(20, 1, true);
  new DataView(wavBytes.buffer).setUint16(22, 1, true);
  new DataView(wavBytes.buffer).setUint32(24, 24_000, true);
  new DataView(wavBytes.buffer).setUint32(28, 48_000, true);
  new DataView(wavBytes.buffer).setUint16(32, 2, true);
  new DataView(wavBytes.buffer).setUint16(34, 16, true);
  wavBytes.set(Buffer.from("data"), 36);
  new DataView(wavBytes.buffer).setUint32(40, 2, true);
  let activeLeaf = "previous-leaf";
  const prefills = [];
  const capture = { retainedSamples: [1, 2, 3] };
  const controller = {
    playing: false,
    ready: true,
    bufferedMs: () => bufferedMs,
    wavBytes: () => wavBytes.slice(),
    reconsiderStartup: async () => false,
    pushSegment(frame, operation) {
      delivered.push({ frame, operation });
      if (!firstPcmObserved && frame?.data?.byteLength > 0) {
        firstPcmObserved = true;
        playbackCallbacks?.onFirstChunk?.({
          bytes: frame.data.byteLength,
          frames: frame.data.byteLength / (2 * (frame.channels || 1)),
          sampleRate: frame.sampleRate || 24_000,
          channels: frame.channels || 1,
        });
      }
    },
    finish() {},
    stop: async () => {},
  };
  const setTimer = (callback) => {
    const id = ++timerNumber;
    timers.set(id, callback);
    return id;
  };
  const clearTimer = (id) => timers.delete(id);
  const context = {
    window: { setTimeout: setTimer, clearTimeout: clearTimer },
    performance: { now: () => ++now },
    document: {
      createElement: () => ({ className: "" }),
      querySelector: () => null,
      getElementById: () => null,
    },
    MomVoicePipelineScheduler: { ...voicePipelineScheduler },
    voiceLeadCalibration: null,
    voiceLatencyTrace: { stages: [] },
    MAX_FAILED_VOICE_REPLY_OWNERS: 1,
    failedVoiceReplyOwners: new Map(),
    activeVoiceNotePublications: new Set(),
    selectedConversation: () => "voice-note-conversation",
    activeDispatchConversation: null,
    retainComposerDraft: async () => {},
    refreshChat: async () => {},
    voiceReplyPersistenceKey: (conversation, message) => `${conversation}\u0000${message}`,
    markVoiceLatencyIn(trace, stage, detail) { trace.stages.push({ stage, ...detail }); },
    decodePcmFrame: (frame) => frame,
    isCanonicalPcmWav: (bytes) => bytes instanceof Uint8Array
      && bytes.byteLength === 46
      && Buffer.from(bytes.subarray(0, 4)).toString() === "RIFF"
      && Buffer.from(bytes.subarray(8, 12)).toString() === "WAVE",
    pcmChannel: (onmessage) => ({ onmessage }),
    createPcmPlaybackBuffer: (callbacks) => {
      playbackCallbacks = callbacks;
      return controller;
    },
    speechOperationToken: () => `tts-op-${++operationNumber}`,
    rememberVoiceStreamRequest() {},
    voiceStreamRequestIds: new Set(),
    voiceReplyPlayback: new WeakMap(),
    voiceCallActive: callActive,
    voiceCallEpoch: callEpoch,
    activeCallPlayback: null,
    liveVoiceTts: null,
    voiceReplyPlayRequested: false,
    retryCallPrefillAfterTranscript: null,
    pendingCallTranscriptJobs: [],
    activeCallTranscriptJob: null,
    activeCallTranscriptTask: null,
    voiceCallTranscriptSlotsDisabled: false,
    voiceRecording: null,
    beginVoiceCapture: async () => { context.voiceRecording = capture; },
    startVoicePrefill: async (conversation) => {
      prefills.push({ conversation, activeLeaf });
      return { session: `prefill-${prefills.length}`, leaf: activeLeaf };
    },
    errorMessage: (error) => String(error),
    reportError() {},
    chatEvents: [],
    acquireStableChatBusy: (lease) => context.chatEvents.push(["acquire", lease]),
    releaseChatBusy: (lease) => context.chatEvents.push(["release", lease]),
    finishLiveMessage: (message) => context.chatEvents.push(["terminal", message]),
    appendLiveMessage: (...args) => context.chatEvents.push(["message", ...args]),
    appendLiveDelta: (...args) => context.chatEvents.push(["delta", ...args]),
    settingEnabled: () => false,
    keepLiveTailVisible() {},
    invoke(command, args) {
      if (command === "mom_llama_speech_stop") {
        invocations.push({ command, args, reply: null });
        return Promise.resolve({ status: "passed" });
      }
      const reply = speechDeferred();
      invocations.push({ command, args, reply });
      return reply.promise;
    },
    queueMicrotask,
  };
  context.globalThis = context;
  const chunkStart = ui.indexOf("  const takeLiveSpeechChunk =");
  const chunkEnd = ui.indexOf("\n  const beginLiveVoiceTts =", chunkStart);
  assert.ok(chunkStart >= 0 && chunkEnd > chunkStart);
  speechVm.runInNewContext(
    ui.slice(chunkStart, chunkEnd)
      + "\nlet pendingCallTranscriptJobs = globalThis.pendingCallTranscriptJobs;"
      + "\nlet activeCallTranscriptJob = globalThis.activeCallTranscriptJob;"
      + "\nlet activeCallTranscriptTask = globalThis.activeCallTranscriptTask;"
      + "\nlet activeCallTranscriptOperation = globalThis.activeCallTranscriptOperation;"
      + block(ui, "  const beginLiveVoiceTts =", "  const chatRequestIsCancellable =")
      + "\nglobalThis.beginLiveVoiceTts = beginLiveVoiceTts;",
    context,
  );
  const synthesis = () => invocations.filter(
    (entry) => entry.command === "mom_llama_speech_precommit_synthesize",
  );
  const pcm = (label, end = false) => ({
    data: new Uint8Array(end ? 0 : [label.charCodeAt(0), 0]),
    sampleRate: 24_000,
    channels: 1,
    label,
    end,
  });
  const fireLatestTimer = () => {
    const entry = [...timers.entries()].at(-1);
    assert.ok(entry, "a bounded PCM progress timer is armed");
    timers.delete(entry[0]);
    entry[1]();
  };
  const startPlayback = async (playback) => {
    controller.playing = true;
    playbackCallbacks?.onPlaybackStarted?.({});
    await new Promise((resolve) => setImmediate(resolve));
    return playback;
  };
  const notifyBufferLevel = () => playbackCallbacks?.onBufferLevel?.();
  const wireChatEvents = () => {
    const eventStart = ui.indexOf("  const voiceStreamRequestIds = new Set();");
    const eventEnd = ui.indexOf("\n  const mentionLiveId", eventStart);
    assert.ok(eventStart >= 0 && eventEnd > eventStart);
    context.chatRequestLease = ({ request_id }) => request_id
      ? `chat-request:${request_id}` : null;
    speechVm.runInNewContext(
      ui.slice(eventStart, eventEnd)
        + "\nglobalThis.dispatchChatEvent = onChatEvent;",
      context,
    );
  };
  return {
    context, invocations, delivered, synthesis, pcm, fireLatestTimer,
    startPlayback, notifyBufferLevel, prefills, capture, wireChatEvents,
    setBufferedMs(value) { bufferedMs = value; },
    setActiveLeaf(value) { activeLeaf = value; },
    wavBytes: () => wavBytes.slice(),
  };
};

// Exercise the production event router, sentence admission and adaptive scheduler
// together. Only native IPC and audio-device I/O are controlled test boundaries;
// these assertions are behavior checks, never real-model latency measurements.
test("production voice route excludes reasoning and dispatches whole sentences against actual runway policy", async () => {
  const h = liveTtsTailHarness();
  const playback = h.context.beginLiveVoiceTts({
    conversation: "voice-note-conversation", mode: "call", requestId: "shared-route",
  });
  h.context.liveVoiceTts = playback;
  h.wireChatEvents();
  const send = (event, delta = "") => h.context.dispatchChatEvent({ payload: {
    event, delta, request_id: "shared-route", conversation_id: "voice-note-conversation",
  } });
  send("started");
  send("reasoning_delta", "Private reasoning must never be spoken. ");
  assert.equal(h.synthesis().length, 0, "reasoning never reaches native speech synthesis");
  const prefix = "We can take our time and look at ";
  send("delta", prefix);
  assert.equal(h.synthesis().length, 0,
    "eight complete words are not a sentence and cannot trigger the retired word-count route");
  const firstSentence = `${prefix}what matters most to you.`;
  send("delta", "what matters most to you. ");
  assert.equal(h.synthesis().length, 1);
  assert.equal(h.synthesis()[0].args.text, firstSentence);
  assert.equal(h.synthesis()[0].args.purpose, "call");

  const secondSentence = "There is no need to rush this conversation.";
  send("delta", secondSentence);
  send("text_completed", `${firstSentence} ${secondSentence}`);
  const completed = playback.finish(`${firstSentence} ${secondSentence}`);
  h.setBufferedMs(6_000);
  await h.startPlayback(playback);
  const settle = (segment, label) => {
    segment.args.pcmStream.onmessage(h.pcm(label));
    segment.args.pcmStream.onmessage(h.pcm(`${label}-eof`, true));
    segment.reply.resolve({ status: "passed", result: { binding: {
      backend_id: "fixture-pocket-cpu", model_id: "fixture-model", voice_id: "fixture-voice",
      tts_total_ms: 1, duration_ms: 1,
    } } });
  };
  settle(h.synthesis()[0], "first");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.synthesis().length, 1,
    "the real adaptive scheduler holds the next sentence while playback has ample audio");
  assert.ok(h.context.voiceLatencyTrace.stages.some(
    ({ stage }) => stage === "tts_scheduler_wait_started",
  ));
  h.setBufferedMs(0);
  h.notifyBufferLevel();
  h.fireLatestTimer();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(h.synthesis().map(({ args }) => args.text), [firstSentence, secondSentence]);
  settle(h.synthesis()[1], "second");
  assert.equal(await completed, true);
  assert.deepEqual(h.delivered.map(({ frame }) => frame.label),
    ["first", "first-eof", "second", "second-eof"]);
  const finished = h.context.voiceLatencyTrace.stages.filter(
    ({ stage }) => stage === "tts_segment_finished",
  );
  assert.equal(finished[0].backend_id, "fixture-pocket-cpu");
  assert.equal(finished[0].model_id, "fixture-model");
  assert.equal(finished[0].voice_id, "fixture-voice");
});

const finishVoiceNoteAudio = async (h, playback) => {
  const completed = playback.finish("A saved spoken reply.");
  assert.equal(h.synthesis().length, 1, "one bounded sentence starts one TTS request");
  const segment = h.synthesis()[0];
  segment.args.pcmStream.onmessage(h.pcm("audio"));
  segment.args.pcmStream.onmessage(h.pcm("eof", true));
  segment.reply.resolve({
    status: "passed",
    result: { binding: { tts_total_ms: 2, duration_ms: 1 } },
  });
  assert.equal(await completed, true);
};

test("voice-note audio publication waits for both committed message identity and completed PCM", async () => {
  const h = liveTtsTailHarness();
  const playback = h.context.beginLiveVoiceTts({
    conversation: "voice-note-conversation", mode: "voice_note",
  });
  const target = {
    conversation: "voice-note-conversation",
    message: "assistant-message-17",
    text: "A saved spoken reply.",
  };
  const saved = playback.bindDurableVoiceReply(target);
  assert.equal(h.invocations.some(({ command }) => command === "mom_llama_voice_reply_store"), false,
    "native persistence must wait for the complete WAV");
  await finishVoiceNoteAudio(h, playback);
  assert.equal(h.synthesis()[0].args.purpose, "voice_note");
  const store = h.invocations.find(({ command }) => command === "mom_llama_voice_reply_store");
  assert.ok(store, "publication runs even with no mounted message row");
  assert.deepEqual({ ...store.args, bytes: undefined }, {
    ...target, bytes: undefined,
  });
  assert.equal(store.args.bytes.length, 46);
  store.reply.resolve({
    status: "contracted",
    result: { attachment: { id: "voice-attachment-17" } },
  });
  assert.equal(await saved, "voice-attachment-17");
  assert.equal(playback.durableVoiceAttachmentId, "voice-attachment-17");
  assert.ok(playback.trace.stages.some(({ stage }) => stage === "durable_voice_audio_ready"));
});

test("voice-note audio completed before the native message ID is still published", async () => {
  const h = liveTtsTailHarness();
  const playback = h.context.beginLiveVoiceTts({
    conversation: "voice-note-conversation", mode: "voice_note",
  });
  await finishVoiceNoteAudio(h, playback);
  const saved = playback.bindDurableVoiceReply({
    conversation: "voice-note-conversation",
    message: "assistant-message-18",
    text: "A saved spoken reply.",
  });
  await new Promise((resolve) => setImmediate(resolve));
  const store = h.invocations.find(({ command }) => command === "mom_llama_voice_reply_store");
  assert.ok(store, `completed audio remains owned after TTS EOF and view navigation: ${String(playback.durableVoiceStoreError)}`);
  store.reply.resolve({
    status: "contracted",
    result: { attachment: { id: "voice-attachment-18" } },
  });
  assert.equal(await saved, "voice-attachment-18");
});

test("failed voice-note audio save stays in-memory and can be retried", async () => {
  const h = liveTtsTailHarness();
  const playback = h.context.beginLiveVoiceTts({
    conversation: "voice-note-conversation", mode: "voice_note",
  });
  await finishVoiceNoteAudio(h, playback);
  const target = {
    conversation: "voice-note-conversation",
    message: "assistant-message-19",
    text: "A saved spoken reply.",
  };
  const failedSave = playback.bindDurableVoiceReply(target);
  await new Promise((resolve) => setImmediate(resolve));
  const failedStore = h.invocations.find(({ command }) => command === "mom_llama_voice_reply_store");
  assert.equal(h.context.activeVoiceNotePublications.size, 1, "pending native persistence keeps the note slot reserved");
  failedStore.reply.resolve({ status: "blocked", blocker: { code: "offline", message: "Offline" } });
  assert.equal(await failedSave, null);
  assert.equal(h.context.activeVoiceNotePublications.size, 0, "save failure transfers ownership to the retryable PCM map");
  assert.equal(h.context.failedVoiceReplyOwners.size, 1);
  assert.equal(playback.durableVoiceAttachmentId, null);
  assert.ok(playback.durableVoiceStoreError);
  assert.equal(h.wavBytes().byteLength, 46, "the completed PCM remains available in this session");
  const retried = playback.retryDurableVoiceReply();
  assert.equal(h.context.activeVoiceNotePublications.size, 1, "retry reserves the capture slot while native save is pending");
  await new Promise((resolve) => setImmediate(resolve));
  const stores = h.invocations.filter(({ command }) => command === "mom_llama_voice_reply_store");
  assert.equal(stores.length, 2);
  stores[1].reply.resolve({
    status: "contracted",
    result: { attachment: { id: "voice-attachment-19" } },
  });
  assert.equal(await retried, "voice-attachment-19");
  assert.equal(h.context.activeVoiceNotePublications.size, 0, "successful retry releases the capture slot");
});

test("failed synthesis does not occupy the retryable save-owner slot", async () => {
  const h = liveTtsTailHarness();
  const playback = h.context.beginLiveVoiceTts({
    conversation: "voice-note-conversation", mode: "voice_note",
  });
  assert.equal(h.context.activeVoiceNotePublications.size, 1, "the note slot is reserved during synthesis");
  await playback.unavailable(new Error("synthetic generation failure"));
  assert.equal(h.context.activeVoiceNotePublications.size, 0, "a synthesis failure releases the reservation");
  const save = playback.bindDurableVoiceReply({
    conversation: "voice-note-conversation",
    message: "assistant-message-failed-synthesis",
    text: "A reply that has no completed audio.",
  });
  assert.equal(await save, null);
  assert.equal(h.context.failedVoiceReplyOwners.size, 0, "no unplayable PCM is retained as a retryable save");
  assert.equal(h.invocations.some(({ command }) => command === "mom_llama_voice_reply_store"), false);
  assert.equal(h.context.activeVoiceNotePublications.size, 0);
});

test("failed unconfirmed speculation leaves the caller's capture and prefill alone", async () => {
  const h = liveTtsTailHarness({ callActive: true, callEpoch: 7 });
  const caller = { trace: { id: "caller" }, retainedSamples: [1, 2, 3] };
  h.context.voiceRecording = caller;
  let admissions = 0;
  h.context.beginVoiceCapture = async () => {
    admissions += 1;
    throw new Error("Voice capture is already active.");
  };
  const playback = h.context.beginLiveVoiceTts({
    conversation: "fixture-conversation", mode: "call", callEpoch: 7,
    deferPlayback: true,
  });
  assert.equal(await playback.unavailable(new Error("empty model response")), false);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(admissions, 0, "unconfirmed failure must not reopen an owned microphone");
  assert.equal(h.context.voiceRecording, caller);
  assert.deepEqual(h.prefills, [], "the speculative owner must not create a competing prompt");
});

test("late reply failure cannot replace a newer capture in the same call", async () => {
  const h = liveTtsTailHarness({ callActive: true, callEpoch: 7 });
  const playback = h.context.beginLiveVoiceTts({
    conversation: "fixture-conversation", mode: "call", callEpoch: 7,
  });
  const newer = { trace: { id: "newer-capture" } };
  h.context.voiceRecording = newer;
  let admissions = 0;
  h.context.beginVoiceCapture = async () => { admissions += 1; };
  await playback.unavailable(new Error("late producer failure"));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(admissions, 0);
  assert.equal(h.context.voiceRecording, newer);
  assert.deepEqual(h.prefills, []);
});

test("failed audible reply preserves its capture and prepares its next turn", async () => {
  const h = liveTtsTailHarness({ callActive: true, callEpoch: 7 });
  const playback = h.context.beginLiveVoiceTts({
    conversation: "fixture-conversation", mode: "call", callEpoch: 7,
  });
  await h.startPlayback(playback);
  await playback.unavailable(new Error("producer failed after playback"));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.context.voiceRecording, h.capture);
  assert.equal(h.prefills.length, 1);
});

test("text EOF flushes a held call reply and history waits for confirmed commit", async () => {
  for (const text of [
    "This short answer is complete.",
    "This short answer has no punctuation",
  ]) {
    const h = liveTtsTailHarness({ callActive: true, callEpoch: 23 });
    const pcmConfirmed = speechDeferred();
    const commit = speechDeferred();
    const nativeFinish = pcmConfirmed.promise.then(() => commit.promise).then((result) => {
      h.setActiveLeaf(result.committedLeaf);
      return result;
    });
    const playback = h.context.beginLiveVoiceTts({
      conversation: "fixture-conversation",
      mode: "call",
      requestId: "request-final",
      callEpoch: 23,
      onFirstChunk: () => pcmConfirmed.resolve(true),
    });
    playback.trackGeneration(nativeFinish);
    h.context.liveVoiceTts = playback;
    h.wireChatEvents();

    const finish = playback.finish.bind(playback);
    let finishCalls = 0;
    const finishResults = [];
    playback.finish = (authoritativeText) => {
      finishCalls += 1;
      const result = finish(authoritativeText);
      finishResults.push(result);
      return result;
    };

    const payload = (event, delta = "") => ({
      event,
      request_id: "request-final",
      conversation_id: "fixture-conversation",
      delta,
    });
    h.context.dispatchChatEvent({ payload: payload("started") });
    h.context.dispatchChatEvent({ payload: payload("delta", text) });
    assert.equal(h.synthesis().length, 0,
      "the last word is held until the native text EOF arrives");

    h.context.dispatchChatEvent({ payload: payload("text_completed", text) });
    assert.equal(h.synthesis().length, 1,
      "the nonterminal text EOF must flush the last sentence while native completion is pending");
    assert.equal(h.synthesis()[0].args.text, text);
    assert.equal(h.context.chatEvents.some(([kind]) => kind === "release"), false,
      "text EOF must not release the native chat request lease");

    h.context.dispatchChatEvent({ payload: payload("text_completed", text) });
    assert.equal(finishCalls, 2, "duplicate EOF reaches the closed owner idempotently");
    assert.equal(finishResults[0], finishResults[1],
      "duplicate EOF returns the same production completion promise");
    assert.equal(h.synthesis().length, 1, "duplicate EOF must not replay synthesis");
    assert.deepEqual(h.prefills, [], "history preparation remains behind native settlement");

    const segment = h.synthesis()[0];
    segment.args.pcmStream.onmessage(h.pcm("first-pcm"));
    assert.equal(await pcmConfirmed.promise, true,
      "the first validated PCM packet supplies the pending native confirmation");
    assert.deepEqual(h.prefills, [],
      "PCM confirmation alone does not release next-turn history before the commit settles");

    segment.args.pcmStream.onmessage(h.pcm("eof", true));
    segment.reply.resolve({
      status: "passed",
      result: { binding: { tts_total_ms: 1, duration_ms: 1 } },
    });
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(h.prefills, [], "TTS completion cannot substitute for native commit settlement");

    commit.resolve({ committedLeaf: "committed-after-voice-confirmation" });
    await nativeFinish;
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(h.prefills, [{
      conversation: "fixture-conversation",
      activeLeaf: "committed-after-voice-confirmation",
    }], "the next prefill must observe the committed assistant turn");
    assert.equal(await finishResults[0], true);

    h.context.dispatchChatEvent({ payload: payload("completed") });
    assert.equal(h.synthesis().length, 1,
      "the terminal event after commit must not replay the already-flushed text");
    assert.equal(h.context.chatEvents.some(([kind]) => kind === "release"), true,
      "only the native terminal event releases the request lease");
  }
});

test("queued transcript slot waits for EOF-triggered native settlement and rechecks caller state", async () => {
  for (const changedState of [null, "hangup", "caller_onset"]) {
    const h = liveTtsTailHarness({ callActive: true, callEpoch: 23 });
    const generation = speechDeferred();
    const resident = speechDeferred();
    const slotStarted = [];
    h.context.voiceCallActive = true;
    h.context.voiceCallEpoch = 23;
    h.context.voiceRecording = {
      call: true,
      prefillDeferred: true,
      voiceActivity: { heardVoice: false },
    };
    h.context.pendingCallTranscriptJobs.push({ output: {} });
    h.context.MomVoicePipelineScheduler.shouldRunCallTranscriptSlot = () => true;
    h.context.startPendingCallTranscriptSlot = () => {
      slotStarted.push(true);
      return new Promise(() => {});
    };
    const invoke = h.context.invoke;
    h.context.invoke = (command, args) => command === "mom_llama_speech_parakeet_resident"
      ? resident.promise : invoke(command, args);

    const playback = h.context.beginLiveVoiceTts({
      conversation: "fixture-conversation",
      mode: "call",
      requestId: "request-transcript-slot",
      callEpoch: 23,
    });
    playback.trackGeneration(generation.promise);
    h.context.liveVoiceTts = playback;
    h.wireChatEvents();
    const payload = (event, delta = "") => ({
      event,
      request_id: "request-transcript-slot",
      conversation_id: "fixture-conversation",
      delta,
    });
    h.context.dispatchChatEvent({ payload: payload("started") });
    h.context.dispatchChatEvent({ payload: payload("delta", "A final short reply") });
    assert.equal(h.synthesis().length, 0, "the unfinished final sentence stays buffered");
    h.context.dispatchChatEvent({ payload: payload("text_completed", "A final short reply") });
    assert.equal(h.synthesis().length, 1, "EOF flushes the final sentence before native settlement");

    const segment = h.synthesis()[0];
    segment.args.pcmStream.onmessage(h.pcm("first-pcm"));
    segment.args.pcmStream.onmessage(h.pcm("eof", true));
    segment.reply.resolve({
      status: "passed",
      result: { binding: { tts_total_ms: 1, duration_ms: 1 } },
    });
    await new Promise((resolve) => setImmediate(resolve));
    h.notifyBufferLevel();

    resident.resolve({ status: "passed", result: true });
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(slotStarted, [],
      "the transcript slot cannot start while the native assistant commit is pending");

    if (changedState === "hangup") h.context.voiceCallActive = false;
    if (changedState === "caller_onset") h.context.voiceRecording.voiceActivity.heardVoice = true;
    generation.resolve();
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(slotStarted.length, changedState === null ? 1 : 0,
      `${changedState === null
        ? "the queued transcript slot may start after settlement"
        : `the settled slot rechecks ${changedState} before starting`}; stages=${JSON.stringify(h.context.voiceLatencyTrace.stages.map(({ stage }) => stage))}`);
  }
});

test("live TTS keeps the next segment behind the old channel's terminal PCM tail", async () => {
  const h = liveTtsTailHarness();
  const playback = h.context.beginLiveVoiceTts({ conversation: "fixture", mode: "call" });
  playback.push("alpha beta gamma delta epsilon zeta. ");
  const first = h.synthesis()[0];
  assert.ok(first, "first synthesis segment is dispatched");
  first.args.pcmStream.onmessage(h.pcm("head"));

  playback.push("eta theta iota kappa lambda mu nu xi. ");
  const completed = playback.finish("alpha beta gamma delta epsilon zeta. eta theta iota kappa lambda mu nu xi. ");
  first.reply.resolve({ status: "passed", result: { binding: { tts_total_ms: 1, duration_ms: 1 } } });
  await new Promise((resolve) => setImmediate(resolve));
  const prematureNextSegment = h.synthesis().length > 1;

  // Tauri response and channel callbacks are separate transports: the final
  // PCM and EOF may arrive after the response promise has already resolved.
  first.args.pcmStream.onmessage(h.pcm("tail"));
  first.args.pcmStream.onmessage(h.pcm("eof", true));
  await new Promise((resolve) => setImmediate(resolve));
  const second = h.synthesis()[1];
  if (second) {
    second.args.pcmStream.onmessage(h.pcm("next"));
    second.args.pcmStream.onmessage(h.pcm("next-eof", true));
    second.reply.resolve({ status: "passed", result: { binding: { tts_total_ms: 1, duration_ms: 1 } } });
  }
  await completed;

  assert.equal(prematureNextSegment, false, "next request must wait until prior-channel EOF");
  assert.deepEqual(h.delivered.map(({ frame }) => frame.label), ["head", "tail", "eof", "next", "next-eof"]);
});

test("stopping live TTS releases promptly while it waits for channel EOF", async () => {
  const h = liveTtsTailHarness();
  const playback = h.context.beginLiveVoiceTts({ conversation: "fixture", mode: "call" });
  playback.push("alpha beta gamma delta epsilon zeta. ");
  const first = h.synthesis()[0];
  assert.ok(first);
  first.args.pcmStream.onmessage(h.pcm("head"));
  playback.push("eta theta iota kappa lambda mu nu xi. ");
  const completed = playback.finish("alpha beta gamma delta epsilon zeta. eta theta iota kappa lambda mu nu xi. ");
  first.reply.resolve({ status: "passed", result: { binding: { tts_total_ms: 1, duration_ms: 1 } } });
  await new Promise((resolve) => setImmediate(resolve));

  const stopping = playback.stop();
  // If the pre-fix pump already started the queued segment, let its native
  // response settle only after stop has claimed it. This verifies stop does
  // not wait for the channel's missing EOF or dispatch more queued text.
  const premature = h.synthesis()[1];
  premature?.reply.resolve({ status: "passed", result: { binding: { tts_total_ms: 1, duration_ms: 1 } } });
  const stopSettled = await Promise.race([
    stopping.then(() => true),
    new Promise((resolve) => setTimeout(() => resolve(false), 50)),
  ]);
  assert.equal(stopSettled, true, "stop must not await the missing PCM EOF");
  assert.equal(h.synthesis().length, 1, "stop must not dispatch queued text before old channel EOF");
  assert.equal(await completed, false, "stop settles without waiting for channel EOF");
});

test("interrupted call keeps capture live but waits for native commit before next prefill", async () => {
  const h = liveTtsTailHarness({ callActive: true, callEpoch: 7 });
  const playback = h.context.beginLiveVoiceTts({
    conversation: "fixture-conversation", mode: "call", callEpoch: 7,
  });
  const native = speechDeferred();
  const committedGeneration = native.promise.then((result) => {
    h.setActiveLeaf(result.committedLeaf);
    return result;
  });
  assert.equal(playback.trackGeneration(committedGeneration), committedGeneration,
    "tracking must return the exact native operation to its existing caller");
  await h.startPlayback(playback);
  assert.equal(h.context.voiceRecording, h.capture,
    "playback hands off to a capture that retains the caller's samples");

  const stopping = playback.interruptForSpeech();
  const stoppedBeforeCommit = await Promise.race([
    stopping.then(() => true),
    new Promise((resolve) => setTimeout(() => resolve(false), 50)),
  ]);
  if (!stoppedBeforeCommit) native.resolve({ committedLeaf: "assistant-committed" });
  assert.equal(stoppedBeforeCommit, true,
    "stopping audible speech must not wait for Gemma's history commit");
  assert.equal(h.context.voiceRecording, h.capture,
    "interrupting playback must preserve the active microphone capture");
  assert.deepEqual(h.prefills, [],
    "next-turn prompt preparation must wait while the previous native request is pending");

  native.resolve({ committedLeaf: "assistant-committed" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(h.prefills, [{ conversation: "fixture-conversation", activeLeaf: "assistant-committed" }],
    "the next prefill must observe the leaf after the native operation settles");
});

test("a rejected native generation still releases next-turn prefill safely", async () => {
  const h = liveTtsTailHarness({ callActive: true, callEpoch: 3 });
  const playback = h.context.beginLiveVoiceTts({
    conversation: "fixture-conversation", mode: "call", callEpoch: 3,
  });
  const native = speechDeferred();
  const tracked = playback.trackGeneration(native.promise);
  tracked.catch(() => {});
  await h.startPlayback(playback);
  const stopping = playback.stop();
  const stoppedBeforeSettlement = await Promise.race([
    stopping.then(() => true),
    new Promise((resolve) => setTimeout(() => resolve(false), 50)),
  ]);
  if (!stoppedBeforeSettlement) native.reject(new Error("native request failed"));
  assert.equal(stoppedBeforeSettlement, true,
    "playback stop must remain prompt while native generation is pending");
  assert.deepEqual(h.prefills, []);

  native.reject(new Error("native request failed"));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.prefills.length, 1,
    "a failed native request has settled and must not strand the next capture");
  assert.equal(h.prefills[0].activeLeaf, "previous-leaf");
});

test("hangup while waiting for native settlement suppresses next-turn prefill", async () => {
  const h = liveTtsTailHarness({ callActive: true, callEpoch: 11 });
  const playback = h.context.beginLiveVoiceTts({
    conversation: "fixture-conversation", mode: "call", callEpoch: 11,
  });
  const native = speechDeferred();
  playback.trackGeneration(native.promise);
  await h.startPlayback(playback);
  const stopping = playback.stop();
  const stoppedBeforeSettlement = await Promise.race([
    stopping.then(() => true),
    new Promise((resolve) => setTimeout(() => resolve(false), 50)),
  ]);
  if (!stoppedBeforeSettlement) native.resolve({});
  assert.equal(stoppedBeforeSettlement, true,
    "playback teardown must not wait for the native request");
  assert.deepEqual(h.prefills, []);

  h.context.voiceCallActive = false;
  h.context.voiceCallEpoch += 1;
  native.resolve({ committedLeaf: "late-commit" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(h.prefills, [],
    "the delayed settlement must recheck call ownership before opening a prefill");
});

test("live TTS also waits for native success when channel EOF arrives first", async () => {
  const h = liveTtsTailHarness();
  const playback = h.context.beginLiveVoiceTts({ conversation: "fixture", mode: "call" });
  playback.push("alpha beta gamma delta epsilon zeta. ");
  const first = h.synthesis()[0];
  assert.ok(first);
  first.args.pcmStream.onmessage(h.pcm("head"));
  first.args.pcmStream.onmessage(h.pcm("eof", true));
  playback.push("eta theta iota kappa lambda mu nu xi. ");
  const completed = playback.finish("alpha beta gamma delta epsilon zeta. eta theta iota kappa lambda mu nu xi. ");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.synthesis().length, 1, "terminal PCM alone cannot release the next synthesis request");

  first.reply.resolve({ status: "passed", result: { binding: { tts_total_ms: 1, duration_ms: 1 } } });
  await new Promise((resolve) => setImmediate(resolve));
  const second = h.synthesis()[1];
  assert.ok(second, "the next segment starts after both response and EOF");
  second.args.pcmStream.onmessage(h.pcm("next"));
  second.args.pcmStream.onmessage(h.pcm("next-eof", true));
  second.reply.resolve({ status: "passed", result: { binding: { tts_total_ms: 1, duration_ms: 1 } } });
  await completed;
  assert.deepEqual(h.delivered.map(({ frame }) => frame.label), ["head", "eof", "next", "next-eof"]);
});

test("missing PCM EOF is bounded by the progress watchdog", async () => {
  const h = liveTtsTailHarness();
  const playback = h.context.beginLiveVoiceTts({ conversation: "fixture", mode: "call" });
  playback.push("alpha beta gamma delta epsilon zeta. ");
  const first = h.synthesis()[0];
  assert.ok(first);
  first.args.pcmStream.onmessage(h.pcm("head"));
  const completed = playback.finish("alpha beta gamma delta epsilon zeta. ");
  first.reply.resolve({ status: "passed", result: { binding: { tts_total_ms: 1, duration_ms: 1 } } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.synthesis().length, 1, "the operation remains owned while EOF is outstanding");

  h.fireLatestTimer();
  assert.equal(await completed, false, "missing EOF fails the reply instead of hanging forever");
  assert.equal(h.synthesis().length, 1);
  assert.ok(h.invocations.some((entry) => entry.command === "mom_llama_speech_stop"));
});

test("PCM callbacks after terminal EOF fail the active segment instead of extending it", async () => {
  const h = liveTtsTailHarness();
  const failures = [];
  const playback = h.context.beginLiveVoiceTts({
    conversation: "fixture", mode: "call", onFailure: (error) => failures.push(String(error)),
  });
  playback.push("alpha beta gamma delta epsilon zeta. ");
  const first = h.synthesis()[0];
  assert.ok(first);
  first.args.pcmStream.onmessage(h.pcm("head"));
  first.args.pcmStream.onmessage(h.pcm("eof", true));
  first.args.pcmStream.onmessage(h.pcm("late"));
  assert.deepEqual(h.delivered.map(({ frame }) => frame.label), ["head", "eof"]);
  assert.equal(failures.length, 1);
  const completed = playback.finish("alpha beta gamma delta epsilon zeta. ");
  first.reply.resolve({ status: "passed", result: { binding: { tts_total_ms: 1, duration_ms: 1 } } });
  assert.equal(await completed, false);
  assert.equal(h.synthesis().length, 1);
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

test("late worklet callbacks after stop neither schedule stale PCM nor update owner feedback", async () => {
  const suspendGate = speechDeferred();
  const h = speechLifetimeHarness({ worklet: true, suspendGate });
  const lateUnderruns = [];
  const controller = h.createPcmPlaybackBuffer({
    autoplay: true,
    retainHistory: false,
    onUnderrun: (gapMs) => lateUnderruns.push(gapMs),
  });
  const oneSecond = new Uint8Array(24_000 * 2);
  for (let packet = 0; packet < 8; packet += 1) {
    controller.push(h.pcmFrame(oneSecond, packet, false, packet * 24_000));
  }

  const playing = controller.play();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(await playing, true);
  const node = h.workletNodes[0];
  const alreadyQueuedHandler = node.port.onmessage;
  assert.equal(node.port.messages.filter((message) => message.type === "pcm").length, 8);

  // Fill the 8-second ring and leave one packet queued in the main-thread
  // buffer. A consumed callback arriving after stop must not schedule it.
  controller.push(h.pcmFrame(oneSecond, 8, false, 8 * 24_000));
  assert.equal(h.audioContexts[0].sources.length, 0);
  await controller.stop();
  assert.equal(h.audioContexts[0].state, "running", "stop retires only the old graph on the shared context");
  assert.equal(node.connected, false);
  assert.equal(node.port.onmessage, null, "teardown detaches future event delivery");
  // This saved callback models an event already queued in the renderer before
  // teardown detached the port handler.
  alreadyQueuedHandler({ data: { type: "consumed", frames: 24_000, renderedFrames: 24_000 } });
  alreadyQueuedHandler({ data: { type: "underrun", missingFrames: 240 } });
  const observed = {
    staleBufferSources: h.audioContexts[0].sources.length,
    lateUnderrunFeedback: lateUnderruns.length,
  };
  assert.deepEqual(observed, { staleBufferSources: 0, lateUnderrunFeedback: 0 });
});

"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const productRoot = path.resolve(__dirname, "../../..");
const read = (relative) => fs.readFileSync(path.join(productRoot, relative), "utf8");
const commands = JSON.parse(read("contracts/commands.json")).commands;
const view = read("apps/mom-llama/src-tauri/src/view.rs");
const viewProduction = view.split("#[cfg(test)]", 1)[0];
const bridge = read("apps/mom-llama/ui/coop-hx.js");
const voiceScheduler = read("apps/mom-llama/ui/voice-pipeline-scheduler.js");
const index = read("apps/mom-llama/ui/index.html");
const styles = read("apps/mom-llama/ui/style.css");
const qwenHelper = read("apps/mom-llama/qwen3-tts-helper/Sources/Qwen3TTSHelper/main.swift");
const voiceQc = read("scripts/voice-sample-qc.mjs");
const chatRuntime = read("crates/mom-llama-runtime/src/chat.rs");
const attachmentRuntime = read("crates/mom-llama-runtime/src/attachments.rs");
const configRuntime = read("crates/mom-llama-runtime/src/config.rs");
const personasRuntime = read("crates/mom-llama-runtime/src/personas.rs");
const cli = read("crates/mom-llama-cli/src/main.rs");

const backendOnlyCommandIds = [
  "mom_llama.conversation_new",
  "mom_llama.engine_check",
  "mom_llama.engine_configure",
  "mom_llama.kv_cache_clear",
  "mom_llama.kv_cache_prepare_voice_persona_resident",
  "mom_llama.kv_cache_restore",
  "mom_llama.kv_cache_save",
  "mom_llama.kv_cache_status",
  "mom_llama.model_slot_list",
  "mom_llama.model_slot_load",
  "mom_llama.model_slot_unload",
  "mom_llama.skill_apply",
  "mom_llama.skill_create",
  "mom_llama.skill_list",
  "mom_llama.skill_update",
];

const controlPattern =
  /ControlSpec \{\s*affordance: "([^"]+)",\s*command: "([^"]+)"/g;
const projectedCommandIds = [...viewProduction.matchAll(controlPattern)].map((match) => match[2]);

test("diagnostic and legacy commands remain available without claiming native-view controls", () => {
  const classified = commands
    .filter((command) => command.surface === "backend_only")
    .map((command) => command.command_id)
    .sort();
  assert.deepEqual(classified, backendOnlyCommandIds);

  for (const commandId of backendOnlyCommandIds) {
    const command = commands.find((candidate) => candidate.command_id === commandId);
    assert.ok(command, `missing command contract ${commandId}`);
    assert.equal(command.surface, "backend_only");
    assert.deepEqual(command.affordances, []);
    assert.ok(command.cli);
    assert.ok(command.tauri_command);
    assert.ok(!projectedCommandIds.includes(commandId), `${commandId} leaked into ControlSpec`);
  }
});

test("ordinary model selection stays visible while internal engine and resident controls stay hidden", () => {
  for (const commandId of [
    "mom_llama.model_list",
    "mom_llama.model_select",
    "mom_llama.path_select",
  ]) {
    const command = commands.find((candidate) => candidate.command_id === commandId);
    assert.ok(command);
    assert.equal(command.surface ?? "native_view", "native_view");
    assert.ok(command.affordances.length > 0);
    assert.ok(projectedCommandIds.includes(commandId));
  }
  const modelSelect = commands.find((candidate) => candidate.command_id === "mom_llama.model_select");
  assert.ok(modelSelect.cli.includes("[--conversation <id>]"));
  assert.ok(cli.includes("conversation_model_select_and_load"));
  assert.ok(cli.includes("None => mom_llama_runtime::model_select(&operation_scope, model_path)?"));
});

test("model choice is a scoped searchable picker with automatic projector pairing", () => {
  const composerStart = viewProduction.indexOf("fn composer(");
  const composerEnd = viewProduction.indexOf("\nfn message_row(", composerStart);
  const composer = viewProduction.slice(composerStart, composerEnd);

  for (const marker of [
    'data-model-picker="true"',
    'data-model-search="true"',
    'data-action="model-select"',
    'data-action="model-browse"',
    'data-conversation=[conversation_id]',
    '"Loaded"',
    '"Available"',
    '"Choose GGUF file…"',
  ]) {
    assert.ok(viewProduction.includes(marker), `missing model picker marker: ${marker}`);
  }
  for (const marker of [
    "Conversation model",
    "conversation-model-chip",
    "runtime-dot",
    "Vision projector",
    'name="mmproj_path"',
    'name="persona_mmproj_path"',
    'name="persona_model_path"',
  ]) {
    assert.ok(!viewProduction.includes(marker), `obsolete model UI remains: ${marker}`);
  }
  assert.ok(!composer.includes("model_picker("));
  assert.ok(!composer.includes("effective_conversation_model_path(active, settings)"));

  for (const marker of [
    "const selectAndLoadModel",
    'conversation: button.dataset.conversation || null',
    'picker.dataset.state = "loading"',
    '.model-picker-error',
    '[data-model-search]',
  ]) {
    assert.ok(bridge.includes(marker), `missing model picker bridge behavior: ${marker}`);
  }
  const selectionStart = bridge.indexOf("const selectAndLoadModel");
  const selectionEnd = bridge.indexOf("\n  const actionHandlers", selectionStart);
  const selection = bridge.slice(selectionStart, selectionEnd);
  assert.ok(selection.includes("recordCommandResult(result)"));
  assert.ok(selection.indexOf("recordCommandResult(result)") < selection.indexOf("report(result)"));
  assert.ok(selection.includes('if (result?.status === "blocked")'));
  assert.ok(!bridge.includes('modelPath: formValue(form, "model_path")'));
  assert.ok(!bridge.includes('mmprojPath: formValue(form, "mmproj_path")'));
  assert.ok(styles.includes('.model-picker[data-state="loading"]'));
  assert.ok(styles.includes(".composer-model-picker"));
  assert.ok(configRuntime.includes('option_env!("MOM_LLAMA_DEFAULT_MODEL_PATH")'));
  assert.ok(configRuntime.includes('option_env!("MOM_LLAMA_DEFAULT_MMPROJ_PATH")'));
  assert.ok(configRuntime.includes("if !projector_path_explicit"));
  assert.ok(viewProduction.includes('name="persona_model_choice"'));
  assert.ok(viewProduction.includes('"Use default model"'));
  assert.ok(bridge.includes('formValue(editor, "persona_model_choice")'));
  assert.ok(!bridge.includes("persona_model_path"));
  assert.ok(bridge.includes("auto_discover_mmproj: modelPath !=="));
  assert.ok(personasRuntime.includes("if auto_discover_mmproj"));
  assert.ok(personasRuntime.includes("discover_projector_for_model(model_path)"));
  for (const marker of [
    "Choose the matching mmproj",
    "mmproj GGUF in Settings",
    "model check to verify the model and projector",
  ]) {
    assert.ok(!`${chatRuntime}\n${attachmentRuntime}`.includes(marker), `internal projector jargon remains: ${marker}`);
  }
});

test("the sidebar exposes only the compact authoritative Persona menu", () => {
  const sidebarStart = viewProduction.indexOf("fn sidebar(");
  const sidebarEnd = viewProduction.indexOf("\nfn composer(", sidebarStart);
  assert.ok(sidebarStart >= 0 && sidebarEnd > sidebarStart, "sidebar renderer missing");
  const sidebar = viewProduction.slice(sidebarStart, sidebarEnd);

  for (const marker of [
    'data-sidebar-section="personas"',
    'id="sidebar-persona-list"',
    'class="sidebar-persona-row"',
    'data-persona-menu-target="true"',
    'data-action="persona-draft-open"',
  ]) {
    assert.ok(sidebar.includes(marker), `missing compact Persona marker: ${marker}`);
  }
  assert.ok(!sidebar.includes('data-action="persona-menu-open"'));
  assert.ok(!sidebar.includes('data-action="settings-section"'));
  assert.ok(sidebar.includes('class="sidebar-resizer"'));
  assert.ok(bridge.includes('await Menu.new({ items })'));
  assert.ok(bridge.includes('data-conversation-menu-target'));
  assert.ok(viewProduction.includes("fn persona_projection()"));
  assert.ok(!sidebar.includes("mom_llama_runtime::persona_list()"));

  for (const marker of [
    "mom_llama_runtime::persona_group_list()",
    'data-sidebar-section="consult-groups"',
    'id="sidebar-consult-group-list"',
    'data-action="sidebar-persona-start"',
    'data-action="sidebar-consult-group-start"',
    'data-settings-card="skills"',
    'data-settings-card="cache"',
    'data-settings-card="engine"',
    "Resident models",
  ]) {
    assert.ok(!sidebar.includes(marker), `forbidden sidebar marker remains: ${marker}`);
  }

  for (const action of [
    'data-action="persona-menu-start"',
    'data-action="persona-menu-edit"',
    'data-action="persona-menu-removal-preview"',
  ]) {
    assert.ok(viewProduction.includes(action), `existing Persona menu action missing: ${action}`);
  }

  assert.ok(styles.includes(".sidebar-persona-row"));
  assert.ok(styles.includes(".sidebar-persona-copy"));
});

test("consult replies preserve visible persona identity in the Messages presentation", () => {
  const messageStart = viewProduction.indexOf("fn message_row(");
  const messageEnd = viewProduction.indexOf("\nfn attachment_preview_card(", messageStart);
  const message = viewProduction.slice(messageStart, messageEnd);

  assert.ok(message.includes('class="message-speaker-avatar"'));
  assert.ok(message.includes("contact_avatar(contact_id)"));
  assert.ok(bridge.includes('speakerAvatar.className = "message-speaker-avatar"'));
  assert.ok(bridge.includes("recipientAvatar.cloneNode(true)"));
  assert.ok(styles.includes("--message-blue: #0a84ff"));
  assert.ok(styles.includes(".message-row.assistant"));
  assert.ok(styles.includes(".message-speaker-avatar"));
  assert.ok(styles.includes('.recipients-header[data-unsent="false"]'));
});

test("reasoning and generation expose an iMessage-style typing bubble", () => {
  assert.ok(bridge.includes('indicator.className = "typing-indicator"'));
  assert.ok(bridge.includes('indicator.setAttribute("aria-label", "Typing")'));
  assert.ok(bridge.includes('content.querySelector(".typing-indicator")?.remove()'));
  assert.ok(bridge.includes("appendLiveDelta(content, payload.delta)"));
  assert.ok(styles.includes("@keyframes typing-pulse"));
  assert.ok(styles.includes("@media (prefers-reduced-motion: reduce)"));
  assert.ok(styles.includes(":not(.voice-message-player):not(.typing-indicator)"));
});

test("startup follows the system appearance before the application shell loads", () => {
  const lightMedia = styles.indexOf("@media (prefers-color-scheme: light)");
  const lightRoot = styles.indexOf(":root {", lightMedia);
  assert.ok(lightMedia >= 0 && lightRoot > lightMedia);
  assert.ok(styles.slice(lightRoot, lightRoot + 500).includes("--bg: #fafafa"));
  assert.ok(styles.includes('.llama-ui-shell[data-theme="dark"]'));
  assert.ok(styles.includes("body {\n  overflow: hidden;\n  background: var(--bg)"));
});

test("voice reference screening measures signal quality without claiming identity", () => {
  assert.ok(voiceQc.includes("estimated_snr_db"));
  assert.ok(voiceQc.includes("clipped_sample_fraction"));
  assert.ok(voiceQc.includes("recording_context_unclassified"));
  assert.ok(voiceQc.includes("ranking heuristic, not a laboratory SNR measurement"));
  assert.ok(voiceQc.includes("listening_review"));
});

test("the composer uses a glass microphone affordance and keeps recording controls reachable", () => {
  const composerStart = viewProduction.indexOf("fn composer(");
  const composerEnd = viewProduction.indexOf("\nfn message_row(", composerStart);
  const composer = viewProduction.slice(composerStart, composerEnd);
  assert.ok(composer.includes('data-action="voice-message-start"'));
  assert.ok(composer.includes('data-affordance=(voice_control.affordance)'));
  assert.ok(composer.includes('data-model-ready=(readiness.enabled)'));
  assert.ok(composer.includes('aria-label="Listening"'));
  assert.ok(composer.includes('aria-label="Use keyboard"'));
  assert.ok(composer.includes('data-action="voice-message-send"'));
  assert.ok(!composer.includes('button type="submit"'));
  assert.ok(bridge.includes('.voice-message-button'));
  assert.ok(bridge.includes('voice.dataset.modelReady === "false"'));
  assert.ok(bridge.includes('"voice-message-start": async () => beginVoiceCapture()'));
  assert.ok(bridge.includes('"voice-message-send": async () => sendVoiceCapture()'));
  assert.ok(bridge.includes('"voice-message-pending-play": async (button) =>'));
  assert.ok(bridge.includes("await ensureAttachmentPreviewLoaded(preview)"));
  assert.ok(bridge.includes('"voice-message-cancel": async () =>'));
  const voiceCapture = bridge.slice(
    bridge.indexOf("  const sendVoiceCapture = async"),
    bridge.indexOf("  const acquireChatBusy =", bridge.indexOf("  const sendVoiceCapture = async")),
  );
  assert.ok(voiceCapture.includes("bytes: Array.from(wav)"));
  assert.ok(voiceCapture.includes("pendingVoiceAudioSession"));
  assert.ok(voiceCapture.includes('invoke("mom_llama_voice_message_import"'), "attachment import remains only as the explicit failed-prefill fallback");
  assert.match(
    voiceCapture,
    /if \(live\?\.session && !expectedRecording\.prefillFailed\)[\s\S]*pendingVoiceAudioSession = \{[\s\S]*bytes: Array\.from\(wav\)/,
    "the normal path must hand persistence bytes to the exact incremental session",
  );
  assert.ok(!voiceCapture.includes('invoke("mom_llama_speech_transcribe_attachment"'));
  const submitStart = bridge.indexOf('  document.addEventListener("submit"');
  const submitHandler = bridge.slice(
    submitStart,
    bridge.indexOf('  document.addEventListener("input"', submitStart),
  );
  assert.match(submitHandler, /result = await invoke\("mom_llama_chat_dispatch"/);
  assert.ok(!submitHandler.includes("waitForVoiceTranscript"), "voice dispatch cannot wait for transcription");
  assert.match(submitHandler, /if \(result\?\.status !== "blocked" && result\?\.result\?\.kind === "direct"\) \{[\s\S]*indexVoiceTurn\(conversation, output, voiceTranscriptAttachment,[\s\S]*sourceCapture/);
  const voiceTranscript = bridge.slice(
    bridge.indexOf("  const indexVoiceTurn ="),
    bridge.indexOf("  const stopVoiceCapture =", bridge.indexOf("  const indexVoiceTurn =")),
  );
  assert.ok(voiceTranscript.includes('invoke("mom_llama_speech_transcribe_attachment"'));
  assert.ok(voiceTranscript.includes('invoke("mom_llama_message_record_voice_transcript"'));
  assert.match(voiceTranscript, /const startVoicePrefill = async \(conversation, call, trace\) => \{[\s\S]*invoke\("mom_llama_voice_audio_begin"/);
  assert.ok(!voiceTranscript.includes("waitForVoiceTranscript"), "new microphone turns cannot wait for transcription");
  assert.ok(!voiceTranscript.includes("await previous"), "one failed transcript cannot strand later backfills");
  const speculate = bridge.slice(
    bridge.indexOf('const generation = invoke("mom_llama_voice_audio_speculate"'),
    bridge.indexOf("    } catch (error) {", bridge.indexOf('const generation = invoke("mom_llama_voice_audio_speculate"')),
  );
  assert.match(speculate, /indexVoiceTurn\(live\.conversation, response\.result, null, speculation\.captureReady,\s*recording\.trace\);\s*const synthesized = await tts\.finish/);
  assert.ok(!bridge.includes("const composerHasSendableDraft"));
  assert.ok(!bridge.includes('send.dataset.modelReady !== "true"'));
  const glassControls = styles.slice(
    styles.indexOf(".composer .round-button,\n.composer .send-button {\n  color: var(--glass-glyph)"),
    styles.indexOf("/* Messages-style toolbar items", styles.indexOf(".composer .round-button,\n.composer .send-button {\n  color: var(--glass-glyph)")),
  );
  assert.ok(glassControls.includes("border: 1px solid var(--glass-edge)"));
  assert.ok(glassControls.includes("linear-gradient(180deg"));
  assert.ok(!glassControls.includes("#bfe5fb"));
  assert.ok(!glassControls.includes("#91c7e8"));
  assert.ok(!glassControls.includes("#699bc0"));
  assert.ok(styles.includes("backdrop-filter: blur(24px) saturate(1.65) brightness(1.06)"));
  assert.ok(styles.includes(".composer .send-button:disabled"));
  assert.ok(styles.includes(".voice-listening-orb"));
  assert.ok(styles.includes("color: var(--glass-glyph)"));
  assert.ok(!styles.includes("linear-gradient(145deg, #36a5ff"));
  assert.ok(styles.includes("background-clip: border-box"));
  assert.ok(!styles.includes("border-color: rgba(255, 255, 255, .9)"));
});

test("window chrome is centered and every native resize edge is discoverable", () => {
  assert.ok(styles.includes("--toolbar-height: 64px"));
  assert.ok(!styles.includes(".chrome { padding-bottom:"));
  assert.ok(styles.includes(".window-resize-north-east"));
  assert.ok(styles.includes("cursor: ns-resize"));
  assert.ok(styles.includes("cursor: ew-resize"));
  assert.ok(styles.includes("cursor: nwse-resize"));
  assert.ok(styles.includes("cursor: nesw-resize"));
  assert.ok(viewProduction.includes('data-resize-direction=(direction)'));
  assert.ok(viewProduction.includes('data-resize-cursor=(cursor)'));
  assert.ok(bridge.includes("const resizeHitForPoint = (clientX, clientY)"));
  assert.ok(bridge.includes("resizeHitForPoint(event.clientX, event.clientY)"));
  assert.ok(bridge.includes("nativeWindow.startResizeDragging(resizeDirection)"));
  assert.ok(bridge.includes("nativeWindow.setCursorIcon(cursor)"));
  assert.ok(bridge.includes("pointerleave"));
  const sharedChrome = styles.slice(
    styles.indexOf(".llama-ui-shell .chrome {\n  border-bottom"),
    styles.indexOf(".llama-ui-shell .sidebar {\n  padding"),
  );
  assert.ok(sharedChrome.includes("var(--sidebar) 0 var(--sidebar-width)"));
  assert.ok(sharedChrome.includes("var(--chat-pane) var(--sidebar-width) 100%"));
  assert.ok(sharedChrome.includes("border-bottom: 0"));
  assert.ok(styles.includes(".llama-ui-shell .chat-main { background: var(--chat-pane); }"));
  const flatToolbar = styles.slice(
    styles.indexOf("/* Messages-style toolbar items"),
    styles.indexOf(".composer .send-button:disabled"),
  );
  assert.ok(flatToolbar.includes("border: 0"));
  assert.ok(flatToolbar.includes("background: transparent"));
  assert.ok(flatToolbar.includes("box-shadow: none"));
  assert.ok(!flatToolbar.includes("linear-gradient"));
});

test("titlebar gives search to the sidebar and voice identity to the chat", () => {
  const sidebarZone = viewProduction.slice(
    viewProduction.indexOf('div class="sidebar-titlebar-zone"'),
    viewProduction.indexOf('div class="chat-titlebar-zone"'),
  );
  const chatZoneStart = viewProduction.indexOf('div class="chat-titlebar-zone"');
  const chatZone = viewProduction.slice(
    chatZoneStart,
    viewProduction.indexOf('(chat_view_with_draft', chatZoneStart),
  );

  assert.ok(sidebarZone.includes('conversation-search-open'));
  assert.ok(sidebarZone.includes('class="search-field-icon"'));
  assert.ok(sidebarZone.includes('input type="search"'));
  assert.ok(!sidebarZone.includes('class="icon-button search-submit"'));
  assert.ok(!sidebarZone.includes('conversation-search-close'));
  assert.ok(!sidebarZone.includes('voice-call-open'));
  assert.ok(chatZone.includes('conversation-new'));
  assert.ok(chatZone.includes('voice-call-open'));
  assert.ok(viewProduction.includes('id="voice-call-modal"'));
  assert.ok(viewProduction.includes('data-voice-persona=""'));
  assert.ok(viewProduction.includes('aria-label="End call"'));
  assert.ok(!viewProduction.includes('class="voice-call-titlebar"'));
  assert.ok(!viewProduction.includes('aria-label="Close" title="Close"'));
  assert.ok(!viewProduction.includes('aria-label="Microphone"'));
  assert.ok(bridge.includes("const openVoiceCallSurface = async () =>"));
  assert.ok(bridge.includes("modal.dataset.voicePersona = recipientId"));
  const callStart = bridge.slice(
    bridge.indexOf("  const openVoiceCallSurface = async"),
    bridge.indexOf("  const closeVoiceCallSurface = async"),
  );
  assert.match(
    callStart,
    /conversation === "default"[\s\S]*?mom_llama_conversation_draft_call_start/,
    "a call from a visible unsent recipient must atomically create that Persona conversation",
  );
  assert.match(callStart, /started\.result\.source_conversation_id !== recipientId/);
  assert.doesNotMatch(callStart, /throw new Error\("The call was not answered\."\)/);
  assert.ok(bridge.includes('.sidebar-titlebar-zone")?.classList.toggle("searching", enabled)'));
  assert.ok(styles.includes(".chrome .icon { width: 18px; height: 18px; flex-basis: 18px; }"));
  assert.ok(styles.includes("left: calc(var(--sidebar-width) + 50px)"));
  assert.ok(styles.includes("flex-wrap: nowrap"));
  assert.ok(styles.includes("max-width: calc(100% - 34px)"));
  assert.ok(styles.includes("height: min(520px, calc(100dvh - 24px))"));
  const savedMedallionStart = styles.indexOf('.recipients-header[data-unsent="false"] .recipient-chip {');
  const savedMedallionEnd = styles.indexOf('.recipients-header[data-unsent="true"]', savedMedallionStart);
  const savedMedallion = styles.slice(savedMedallionStart, savedMedallionEnd);
  assert.ok(savedMedallion.includes("flex-direction: column"));
  assert.ok(savedMedallion.includes("background: transparent"));
  assert.ok(styles.includes("@media (max-height: 600px)"));
});

test("chat messages use quiet symmetric Messages bubbles", () => {
  const bubbleStart = styles.indexOf(".message-row.user .message-card {", styles.indexOf("/* Messages-inspired consultation surface. */"));
  const bubbleEnd = styles.indexOf(".message-row.assistant > .message-actions", bubbleStart);
  const bubbles = styles.slice(bubbleStart, bubbleEnd);

  assert.ok(bubbles.includes("border-radius: 18px"));
  assert.ok(bubbles.includes("background: var(--message-blue)"));
  assert.ok(bubbles.includes("box-shadow: none"));
  assert.ok(!bubbles.includes("20px 20px 6px 20px"));
  assert.ok(!bubbles.includes("20px 20px 20px 6px"));
});

test("read aloud keeps lifecycle controls accessible without implementation prose", () => {
  const playbackStart = bridge.indexOf("const createPcmPlaybackBuffer =");
  const playbackEnd = bridge.indexOf("const appendAttachmentTranscriptionControl", playbackStart);
  const playback = bridge.slice(playbackStart, playbackEnd);

  assert.ok(playback.includes("decodePcmFrame(value)"));
  assert.ok(playback.includes("frame.sampleOffset !== nextSampleOffset"));
  assert.ok(playback.includes("new AudioContext"));
  assert.ok(playback.includes("startupBufferMs = 120"));
  assert.ok(playback.includes('typeof startupBufferMs === "function" ? startupBufferMs() : startupBufferMs'));
  assert.ok(playback.includes("bufferedSeconds() < startupBufferSeconds()"));
  assert.ok(playback.includes("retainHistory = true"));
  assert.ok(!playback.includes("chunks.shift()"));
  assert.ok(!playback.includes('createCommandElement("audio"'));
  assert.ok(bridge.includes('button.title = "Stop reading"'));
  assert.ok(playback.includes('invoke("mom_llama_speech_stop"'));
  assert.ok(!playback.includes("Stop suppresses playback immediately"));
  assert.ok(!playback.includes("Apple’s inner synthesis call is non-preemptive"));
  assert.ok(!playback.includes('document.createElement("small")'));
  assert.ok(!playback.includes('document.createElement("output")'));
});

test("voice PCM playback uses a prewarmed audio-thread ring while retaining replay bytes", () => {
  const playbackStart = bridge.indexOf("const pcmWorkletEngine =");
  const playbackEnd = bridge.indexOf("const appendAttachmentTranscriptionControl", playbackStart);
  const playback = bridge.slice(playbackStart, playbackEnd);
  const callStart = bridge.indexOf("const openVoiceCallSurface = async");
  const callEnd = bridge.indexOf("const closeVoiceCallSurface = async", callStart);
  const call = bridge.slice(callStart, callEnd);

  assert.ok(playback.includes('audioWorklet.addModule("pcm-playback-worklet.js")'));
  assert.ok(playback.includes('new AudioWorkletNode(context, "mom-pcm-ring"'));
  assert.ok(playback.includes("workletCapacityFrames = format.sampleRate * 8"));
  assert.ok(playback.includes('data?.type === "consumed"'));
  assert.ok(playback.includes("}, [buffer])"));
  assert.ok(playback.includes("const engine = await preparePcmWorklet(format.sampleRate)"));
  assert.ok(playback.includes("history.push({ ...frame, data: frame.data.slice() })"));
  assert.ok(playback.includes("onBufferLevel?.(bufferedPlaybackSeconds() * 1000)"));
  assert.ok(!playback.includes("retainHistory ? null : await preparePcmWorklet"));
  assert.ok(playback.includes('renderer: workletNode ? "audio_worklet_ring" : "audio_buffer_source"'));
  assert.ok(playback.includes("buffered_ms: bufferedAtStartMs"));
  assert.ok(playback.includes("bufferedMs: () => Math.round(bufferedPlaybackSeconds()"));
  assert.ok(playback.includes("resume_ms:"));
  assert.ok(playback.includes("total_start_ms:"));
  assert.ok(playback.includes("startup_target_ms:"));
  assert.ok(bridge.includes('startupBufferMs: mode === "call" ? () => leadController.startupRunwayMs() : 120'));
  assert.ok(bridge.includes("boundary: segmentBoundary"));
  assert.ok(bridge.includes("gapMs: aggregatePacketGapMs"));
  assert.ok(call.includes("void preparePcmWorklet(24_000)"));
  assert.match(bridge, /const beginVoiceCapture = async[\s\S]*void preparePcmWorklet\(24_000\)/);
});

test("call hang-up is an absolute local audio and generation boundary", () => {
  const closeStart = bridge.indexOf("  const closeVoiceCallSurface = async");
  const closeEnd = bridge.indexOf("\n  const setText =", closeStart);
  const close = bridge.slice(closeStart, closeEnd);

  assert.ok(close.includes('liveVoiceTts?.mode === "call"'));
  assert.ok(close.includes("callTts?.stop()"));
  assert.ok(close.includes("stopSpeechPlayback()"));
  assert.ok(close.includes("stopCallBed()"));
  assert.ok(close.includes('invoke("mom_llama_chat_cancel", { conversation })'));
  assert.ok(
    close.indexOf('setModalVisibility("voice-call-modal", false)')
      < close.indexOf("stopVoiceCapture({ discard: true"),
    "the red button must close before local audio cleanup or native cancellation starts",
  );

  const liveStart = bridge.indexOf("  const beginLiveVoiceTts =");
  const liveEnd = bridge.indexOf("\n  const chatRequestIsCancellable", liveStart);
  const live = bridge.slice(liveStart, liveEnd);
  assert.ok(live.includes('retainHistory: mode !== "call"'));
  const stop = live.slice(live.indexOf("      async stop()"));
  assert.ok(
    stop.indexOf("const stoppedController = controller.stop()")
      < stop.indexOf('invoke("mom_llama_speech_stop", { operation })'),
    "streamed call audio must stop locally before native cancellation",
  );
  assert.ok(live.includes("speech_stream_runaway"));
});

test("call startup waits for idle speech cancellation to reach its terminal before model warmup", () => {
  const callStart = bridge.indexOf("  const openVoiceCallSurface = async");
  const callEnd = bridge.indexOf("\n  const closeVoiceCallSurface = async", callStart);
  const call = bridge.slice(callStart, callEnd);
  const preempt = call.indexOf('await stopSpeechOperationAndWait(voiceHistoryRecoveryOperation, "call_start")');
  const warm = call.indexOf('runtimeWarmPromise.then');
  const microphone = call.indexOf('prepareCallMicrophoneStream()');

  assert.ok(preempt >= 0, "call pickup must request exact speech terminal acknowledgment");
  assert.ok(call.includes("await voiceHistoryRecovery?.catch(() => {})"));
  assert.ok(preempt < warm && preempt < microphone,
    "Gemma warmup and microphone setup must wait until the idle Parakeet operation settles");
  assert.ok(bridge.includes('invoke("mom_llama_speech_stop_and_wait", { operation })'));
});

test("call capture remains live during playback through the shared echo-cancelled route", () => {
  const liveStart = bridge.indexOf("  const beginLiveVoiceTts =");
  const liveEnd = bridge.indexOf("\n  const chatRequestIsCancellable", liveStart);
  const live = bridge.slice(liveStart, liveEnd);
  const captureStart = bridge.indexOf("  const interruptCallPlaybackForSpeech =");
  const captureEnd = bridge.indexOf("\n  const beginBrowserVoiceCapture", captureStart);
  const interrupt = bridge.slice(captureStart, captureEnd);

  assert.ok(live.includes('const nextCallPrefillGate = mode === "call" ? new Promise'));
  assert.ok(live.includes("onPlaybackStarted: (metrics = {}) => {\n        markTtsLatency"));
  const playbackStartedAt = live.indexOf("onPlaybackStarted: (metrics = {}) => {");
  const playbackPausedAt = live.indexOf("onPlaybackPaused: () => {", playbackStartedAt);
  const playbackStarted = live.slice(playbackStartedAt, playbackPausedAt);
  assert.ok(playbackStarted.includes("startCallCapture();"));
  const playbackEndedAt = live.indexOf("onEnded: () => {");
  const playbackEnded = live.slice(playbackEndedAt, live.indexOf("    });", playbackEndedAt));
  assert.ok(playbackEnded.includes("startCallCapture();"));
  assert.ok(live.includes("prefillSession: deferPrefill ? null : (nextCallPrefill || nextCallPrefillGate)"));
  assert.ok(live.includes("deferPrefill,"));
  assert.ok(live.includes("startNextCallPrefill();\n          controller.finish();"));
  assert.ok(live.includes("const continueCall = !onFailure && mode === \"call\""));
  assert.ok(live.includes("startCallCapture();\n          startNextCallPrefill();"));
  assert.ok(!live.includes("void beginVoiceCapture({ call: true }).catch(reportError);"));
  assert.ok(playbackStarted.includes("activeCallPlayback = playback;"));
  assert.ok(interrupt.includes("recording?.call ? activeCallPlayback : null"));
  assert.ok(live.includes("if (stopped || activeOperation !== operation) return;"));
  assert.ok(interrupt.includes('markVoiceLatencyIn(recording.trace, "barge_in_detected"'));
  assert.ok(interrupt.includes("void reply.interruptForSpeech()"));
  assert.ok(bridge.includes("interruptCallPlaybackForSpeech(recording"));
  assert.ok(bridge.includes("const sharedCallStream = call && voiceCallMicrophoneStream?.active"));
  assert.ok(bridge.includes("sharedCallStream: stream === voiceCallMicrophoneStream"));
  assert.ok(bridge.includes("const sharedCallGraph = call && sharedCallStream"));
  assert.ok(bridge.includes("sharedCallGraph: Boolean(sharedCallGraph)"));
  assert.ok(bridge.includes("if (!recording.sharedCallGraph)"));
  assert.ok(bridge.includes("recording.echoGuardSuppressedCallbacks += 1"));
  assert.ok(bridge.includes("if (options.call) return beginBrowserVoiceCapture(options);"));
});

test("native microphone admission precedes Gemma preparation", () => {
  const start = bridge.indexOf("  const beginNativeVoiceCapture = async");
  const end = bridge.indexOf("\n  const beginVoiceCapture = async", start);
  const capture = bridge.slice(start, end);
  assert.ok(
    capture.indexOf('recording.startPromise = invoke("mom_llama_voice_capture_start"')
      < capture.indexOf("recording.prefillSession ="),
    "capture must retain the utterance while model preparation runs",
  );
  assert.ok(capture.includes('markVoiceLatencyIn(recording.trace, "native_capture_requested")'));
});

test("pickup uses the ordinary streamed call reply buffer", () => {
  const greetingStart = bridge.indexOf("  const startStreamedCallGreeting = async");
  const greetingEnd = bridge.indexOf("\n  const openVoiceCallSurface = async", greetingStart);
  const greeting = bridge.slice(greetingStart, greetingEnd);
  const openStart = greetingEnd;
  const openEnd = bridge.indexOf("\n  const closeVoiceCallSurface = async", openStart);
  const open = bridge.slice(openStart, openEnd);

  assert.ok(greeting.includes("beginLiveVoiceTts({"));
  assert.ok(greeting.includes('mode: "call"'));
  assert.ok(greeting.includes('invoke("mom_llama_chat_send"'));
  assert.ok(!greeting.includes('invoke("mom_llama_chat_dispatch"'));
  assert.ok(
    greeting.includes("onPlaybackStarted"),
    "pickup stays on the ring until the ordinary reply buffer actually starts playback",
  );
  assert.ok(greeting.includes("stopCallBed()"));
  assert.ok(!greeting.includes("beginCallNoiseFloor(epoch)"));
  assert.ok(greeting.includes("tts.text.trim()"));
  assert.ok(greeting.includes("tts.finish(greeting)"));
  assert.ok(greeting.includes("onFailure: (error) => { greetingFailure = error; }"));
  assert.ok(!greeting.includes("void closeVoiceCallSurface().catch(reportError)"));
  assert.match(
    greeting,
    /catch \(error\)[\s\S]*voiceCallEpoch !== epoch \|\| !voiceCallActive[\s\S]*return false/,
    "an intentional hang-up must consume the native cancellation acknowledgement",
  );
  assert.ok(open.includes("startCallRinging(epoch)"));
  assert.ok(open.includes("startStreamedCallGreeting(epoch, conversation)"));
  assert.ok(open.includes("await prepareCallMicrophoneStream()"));
  assert.ok(
    open.indexOf("await prepareCallMicrophoneStream()")
      < open.indexOf('setModalVisibility("voice-call-modal", true)'),
    "microphone permission and device readiness must precede the call modal",
  );
  assert.ok(open.includes("const warmReady = runtimeWarmPromise.then((warm) => {"));
  assert.ok(open.includes("const warm = await warmReady;"));
  assert.ok(
    open.indexOf("const warmReady = runtimeWarmPromise.then((warm) => {")
      < open.indexOf("await prepareCallMicrophoneStream()"),
    "resident readiness must be observed concurrently with microphone admission",
  );
  assert.ok(
    open.indexOf("void preparePcmWorklet(24_000);")
      < open.indexOf("await prepareCallMicrophoneStream()"),
    "the PCM renderer must warm under the required microphone readiness wait",
  );
  assert.ok(open.includes("!warm.audio_projector_warmed"));
  assert.ok(open.includes('markVoiceLatency("call_runtime_ready", warm)'));
  assert.ok(!bridge.includes("playPreparedCallSpeech"));
  assert.ok(!bridge.includes("activeCallSpeech"));
  assert.ok(!greeting.includes('invoke("mom_llama_speech_precommit_synthesize"'));
  assert.ok(!bridge.includes("Hi Jan, it’s"), "the greeting must be generated rather than canned");
});

test("call and voice-note replies speculate on the first silence edge and commit at their mode boundary", () => {
  const captureStart = bridge.indexOf("  const beginNativeVoiceCapture = async");
  const captureEnd = bridge.indexOf("\n  const sendVoiceCapture = async", captureStart);
  const capture = bridge.slice(captureStart, captureEnd);
  const speculateStart = bridge.indexOf("  const resumeBrowserVoicePrefill =");
  const speculateEnd = bridge.indexOf("\n  const sendVoiceCapture = async", speculateStart);
  const speculate = bridge.slice(speculateStart, speculateEnd);

  assert.ok(capture.includes('invoke("mom_llama_voice_capture_start"'));
  assert.ok(capture.includes('invoke("mom_llama_voice_capture_retarget"'));
  assert.ok(capture.includes('startVoiceSpeculation(recording, { prearm: true })'));
  assert.ok(capture.includes("recording.prefillSession"));
  assert.ok(bridge.includes('events.listen("mom_llama_voice_capture_event"'));
  assert.ok(bridge.includes('payload.event === "silence_onset"'));
  assert.ok(bridge.includes('payload.event === "speech_resumed"'));
  assert.ok(bridge.includes('payload.event === "first_sample"'));
  assert.ok(bridge.includes('markVoiceLatency("native_capture_first_sample"'));
  assert.ok(bridge.includes("const recoverFailedNativeCapture = async"));
  assert.match(bridge, /payload\.event === "failed"[\s\S]{0,300}recoverFailedNativeCapture/);
  assert.ok(capture.includes("preserveLatencyTrace = false"));
  assert.ok(capture.includes("recoveryAttempt"));
  assert.ok(bridge.includes('markVoiceLatency("capture_processing"'));
  assert.ok(!capture.includes("createScriptProcessor"));
  const captureCallback = bridge.slice(
    bridge.indexOf("recording.processor.onaudioprocess =", captureStart),
    bridge.indexOf("if (!sharedCallGraph)", captureStart),
  );
  assert.ok(!captureCallback.includes("Array.from"), "the PCM capture callback must not serialize samples");
  assert.ok(speculate.includes("recording: sealedVoiceRecordingBytes(capture)"),
    "only a complete physical recording may confirm a speculative turn");
  assert.ok(capture.includes("if (!call && chatBusyLeases.size)"));
  assert.match(
    bridge,
    /payload\.event === "silence_onset"[\s\S]*startVoiceSpeculation\(recording\)/,
    "the first voiced-to-silent edge must start speculative decode",
  );
  assert.ok(bridge.includes("abortCallSpeculation(recording, { restartPrefill: true })"));
  assert.ok(speculate.includes('invoke("mom_llama_voice_audio_speculate"'));
  assert.ok(speculate.includes("capture: recording.native ? recording.capture : null"));
  assert.ok(!speculate.includes('invoke("mom_llama_voice_capture_pause"'));
  const speculativeGeneration = speculate.slice(speculate.indexOf("  const startVoiceSpeculation ="));
  assert.ok(!speculativeGeneration.includes("encodePcmWav"),
    "silence-onset generation must not encode the physical recording");
  assert.doesNotMatch(
    speculate,
    /mom_llama_voice_audio_speculate[\s\S]{0,160}bytes:/,
    "silence onset must not re-encode and resend audio already resident in the native ring",
  );
  assert.ok(speculate.includes("deferPlayback: true"));
  assert.ok(!speculate.includes("deferProgressWatchdog"));
  assert.ok(speculate.includes("const waitForNativeSilence = Boolean(prearm && recording.native)"));
  assert.ok(speculate.includes("waitForSilence: waitForNativeSilence"));
  assert.ok(speculate.includes("generationTimeoutMs"));
  assert.ok(speculate.includes("const rendererFailsafeMs = generationTimeoutMs + 2_000"));
  assert.ok(bridge.includes('payload.stage === "native_silence_released"'));
  assert.match(
    bridge,
    /payload\.stage === "native_silence_released"[\s\S]{0,320}speculation\.tts\?\.activate/,
    "the prearmed TTS watchdog must start only when native silence releases generation",
  );
  assert.ok(speculate.includes("onFirstChunk"));
  assert.ok(speculate.includes("confirmCallSpeculation(recording, speculation)"));
  assert.match(
    speculate,
    /const playback = speculation\.tts\.play\(\);[\s\S]*Promise\.all\(\[playback, confirmed, stoppedCapture\]\)/,
    "first PCM must begin playback while native commit and capture drain settle concurrently",
  );
  assert.ok(speculate.includes("const confirmVoiceNoteSpeculation = async"));
  assert.ok(speculate.includes('mode: recording.call ? "call" : "voice_note"'));
  assert.ok(speculate.includes("speculation.tts.bindButton(pendingButton)"));
  assert.ok(speculate.includes("Promise.all([stoppedCapture, confirmed])"));
  assert.match(
    bridge,
    /payload\.event === "send_ready"[\s\S]{0,500}confirmVoiceNoteSpeculation/,
    "voice-note commit must reuse the in-flight speculative request",
  );
  assert.ok(speculate.includes('invoke("mom_llama_voice_audio_speculation_confirm"'));
  assert.ok(speculate.includes('invoke("mom_llama_voice_audio_speculation_abort"'));
  assert.ok(speculate.includes('invoke("mom_llama_voice_audio_speculation_abort_and_retarget"'));
  assert.match(
    speculate,
    /void speculation\.tts\.stop\(\)[\s\S]*invoke\("mom_llama_voice_audio_speculation_abort_and_retarget"/,
    "resumed speech must silence playback before the fused native handoff",
  );
  assert.doesNotMatch(
    speculate,
    /await speculation\.tts\.stop\(\)[\s\S]*mom_llama_voice_audio_speculation_abort_and_retarget/,
    "capture retarget must not wait for TTS teardown",
  );
  assert.match(speculate, /recording\.prefillSession = Promise\.resolve\(live\)/);
  assert.match(speculate, /speculation_handoff_ready/);
  assert.ok(speculate.includes("resume_wait_us"));
  assert.ok(speculate.includes("resume_reused"));
  assert.ok(speculate.includes("resume_sample_offset"));
  assert.ok(speculate.includes("resume_binding_us"));
  assert.ok(speculate.includes("resume_restore_us"));
  assert.ok(speculate.includes("requestId: live.request_id"));
  assert.match(
    speculate,
    /const resumedSpeech = recording\.voiceActivity\.voiced === true;[\s\S]*abortCallSpeculation\(recording, \{ restartPrefill: resumedSpeech \}\)[\s\S]*closeVoiceCallSurface\(\)/,
    "a terminal silent-call failure must release the call instead of immediately rearming the same silence",
  );

  const ordinarySend = bridge.slice(
    bridge.indexOf("  const sendVoiceCapture = async"),
    bridge.indexOf("\n  const acquireChatBusy =", bridge.indexOf("  const sendVoiceCapture = async")),
  );
  assert.ok(
    ordinarySend.indexOf("currentForm?.requestSubmit()")
      < ordinarySend.indexOf("refreshConversationSettingsProjection()"),
    "presentation refreshes must happen after dispatch begins",
  );
  assert.ok(ordinarySend.includes("appendOptimisticVoiceMessage(replayBytes)"));
  assert.ok(
    ordinarySend.indexOf("encodePcmWav(capture.samples())")
      < ordinarySend.indexOf("appendOptimisticVoiceMessage(replayBytes)")
      && ordinarySend.indexOf("appendOptimisticVoiceMessage(replayBytes)")
        < ordinarySend.indexOf("currentForm?.requestSubmit()"),
    "the outgoing row must mount with playable local audio instead of a disabled play button",
  );
  assert.ok(!ordinarySend.includes("appendOptimisticVoiceMessage(wav)"));
});

test("streamed voice playback is owned by the exact native request", () => {
  const liveStart = bridge.indexOf("  const beginLiveVoiceTts =");
  const liveEnd = bridge.indexOf("\n  const chatRequestIsCancellable", liveStart);
  const live = bridge.slice(liveStart, liveEnd);
  const eventsStart = bridge.indexOf("  const onChatEvent =");
  const eventsEnd = bridge.indexOf("\n  const mentionLiveId", eventsStart);
  const events = bridge.slice(eventsStart, eventsEnd);

  assert.ok(live.includes("acceptsRequest(candidate"));
  assert.ok(live.includes("return requestId === candidate"));
  assert.ok(events.includes("liveVoiceTts.acceptsRequest(payload.request_id, payload.event)"));
  assert.ok(events.includes("(sameLiveConversation || voiceStreamRequestIds.has(payload.request_id)) && !ownsLiveVoice"));
  assert.ok(events.includes("const eventTrace = ownsLiveVoice ? liveVoiceTts.trace : null"));
  assert.ok(live.includes("voice_reply_timeout: Voice synthesis stopped making progress."));
  assert.ok(!live.includes("if (!deferProgressWatchdog) armAudioProgressWatchdog();"));
  assert.ok(live.includes("running = true;\n      // The audio watchdog covers only time owned by an active synthesis"));
  assert.ok(live.includes("watchdogActive = false;"));
  assert.ok(live.includes("if (decoded.data.byteLength > 0) {"));
  assert.ok(live.includes("playback.lastProgressAt = performance.now();\n            armAudioProgressWatchdog();"));
  assert.ok(live.includes("leadController.progressTimeoutMs()"));
  assert.ok(live.includes("const ttsLatencyTrace = voiceLatencyTrace"));
  assert.ok(live.includes("markVoiceLatencyIn(ttsLatencyTrace, stage, detail)"));
  assert.ok(live.includes("onPlayable: () => {"));
  assert.ok(live.includes('markTtsLatency("live_pcm_playable")'));
  assert.ok(!live.includes('onPlayable: () => {\n        if (button)'));
  assert.ok(live.includes('onPlaybackStarted: (metrics = {}) => {'));
  assert.ok(live.includes('markTtsLatency("playback_started", metrics)'));
  assert.ok(live.includes('button.classList.remove("is-loading")'));
  assert.ok(live.includes("if (!started)"));
  assert.ok(!live.includes("if (!started && !controller.ready)"));
  assert.ok(bridge.includes('play.classList.add("is-awaiting-audio")'));
  assert.ok(styles.includes(".voice-reply-play.is-awaiting-audio { display: grid; }"));
  assert.ok(styles.includes("color: var(--message-blue);"));
  assert.ok(!styles.includes("color: var(--accent);"));
  assert.ok(live.includes("finalText.startsWith(streamedText)"));
  assert.ok(bridge.includes("const voiceTtsOwner = voiceDispatch"));
  assert.ok(bridge.includes("await voiceTtsOwner.finish(output.assistant_text)"));
  assert.ok(bridge.includes("pendingVoiceTtsOwner"));
  assert.ok(bridge.includes('markVoiceLatencyIn(trace, "audio_prefill_session_ready", { begin: live?.begin_metrics || null })'));
  assert.ok(live.includes('markTtsLatency("tts_segment_finished"'));
  assert.ok(live.includes("queue_us: binding.tts_queue_us"));
  assert.ok(live.includes("host_route_us: binding.tts_host_route_us"));
  assert.ok(live.includes("host_queue_us: binding.tts_host_queue_us"));
  assert.ok(live.includes("host_dispatch_us: binding.tts_host_dispatch_us"));
  assert.ok(live.includes("host_admission_us: binding.tts_host_admission_us"));
  assert.ok(live.includes("background_warm_active: binding.tts_background_warm_active"));
  assert.ok(live.includes("background_warm_completed: binding.tts_background_warm_completed"));
  assert.ok(live.includes("background_warm_total: binding.tts_background_warm_total"));
  assert.ok(live.includes("reference_admission_us: binding.tts_reference_admission_us"));
  assert.ok(live.includes("reference_cache: binding.tts_reference_cache"));
  assert.ok(live.includes("reference_us: binding.tts_reference_us"));
  assert.ok(live.includes("helper_first_pcm_us: binding.tts_helper_first_pcm_us"));
  assert.ok(live.includes("time_to_first_audio_ms: binding.tts_time_to_first_audio_ms"));
  assert.ok(live.includes('startNextCallPrefill("generation_complete_buffered_playback")'));
  assert.ok(live.includes('leadController.shouldPrepareNextCall({'));
  assert.ok(live.includes('startBufferedNextCallPrefill();'));
  assert.ok(bridge.includes("onVoiceOnset: (recording) => preemptCallTranscriptForSpeech(recording, startNextCallPrefill)"));
  assert.ok(bridge.includes("onVoiceOnset(recording)"));
  assert.ok(bridge.includes("await stopSpeechOperationAndWait(operation, \"caller_speech_onset\")"));
  assert.ok(live.includes('markTtsLatency("tts_all_segments_finished"'));
  assert.ok(live.includes('markTtsLatency("next_turn_prefill_ready"'));
  assert.ok(live.includes("production_rtf: aggregateRtf"));
  assert.ok(index.includes('src="./voice-pipeline-scheduler.js"'));
  assert.ok(voiceScheduler.includes("class AdaptiveVoiceLeadController"));
  assert.ok(voiceScheduler.includes("this.productionRtf - 0.8"));
  assert.ok(live.includes("leadController.shouldDispatch"));
  assert.ok(live.includes('markTtsLatency("tts_scheduler_wait_started"'));
  assert.ok(live.includes('markTtsLatency("tts_scheduler_wait_finished"'));
  assert.ok(live.includes("leadController.noteUnderrun(gapMs)"));
  assert.ok(live.includes("leadController.noteSegment"));
  assert.ok(live.includes("buffered_runway_ms: controller.bufferedMs()"));
  assert.ok(live.includes("startNextCallPrefill();\n          controller.finish();"));
  assert.ok(live.includes("abortUnclaimedCallPrefill();"));
  const finishStart = live.indexOf("      finish(finalText = \"\") {");
  const finishEnd = live.indexOf("\n      async play()", finishStart);
  assert.ok(finishStart >= 0 && finishEnd > finishStart);
  assert.ok(!live.slice(finishStart, finishEnd).includes("startVoicePrefill"));
  assert.ok(live.includes('if (mode !== "call") {\n      void finished.finally'));
  assert.ok(live.includes('if (mode === "call" && liveVoiceTts === playback) liveVoiceTts = null;'));
  assert.ok(bridge.includes("tts_worker_queue_ms:"));
  assert.ok(bridge.includes("tts_host_route_ms:"));
  assert.ok(bridge.includes("tts_host_queue_ms:"));
  assert.ok(bridge.includes("tts_host_dispatch_ms:"));
  assert.ok(bridge.includes("tts_host_admission_ms:"));
  assert.ok(bridge.includes("tts_reference_admission_ms:"));
  assert.ok(bridge.includes("tts_reference_ms:"));
  assert.ok(bridge.includes("tts_helper_first_pcm_ms:"));
});

test("voice transport prompts never enter the visible composer", () => {
  const sendStart = bridge.indexOf("  const sendVoiceCapture = async");
  const sendEnd = bridge.indexOf("\n  const acquireChatBusy =", sendStart);
  const send = bridge.slice(sendStart, sendEnd);
  const submitStart = bridge.indexOf('  document.addEventListener("submit"');
  const submitEnd = bridge.indexOf('\n  document.addEventListener("input"', submitStart);
  const submit = bridge.slice(submitStart, submitEnd);

  assert.ok(send.includes("pendingVoiceDispatchPrompt = voiceModePrompt(call)"));
  assert.ok(!send.includes("textarea.value = voiceModePrompt(call)"));
  assert.ok(submit.includes("const privateVoicePrompt = pendingVoiceDispatchPrompt"));
  assert.ok(submit.includes("pendingVoiceDispatchPrompt = null"));
  assert.ok(submit.includes('const message = privateVoicePrompt || formValue(form, "message")'));
});

test("completed voice traces are persisted outside WebKit for latency inspection", () => {
  assert.ok(bridge.includes('invoke("mom_llama_voice_latency_record", { trace: serialized })'));
  assert.ok(bridge.includes("voiceLatencyPersistence = voiceLatencyPersistence"));
  assert.ok(bridge.includes("voiceLatencyPending.set(trace.id, trace)"));
  assert.ok(bridge.includes("window.setTimeout(flushVoiceLatencyCheckpoints, 50)"));
  assert.ok(bridge.includes("recorded.instrumentation_us"));
  assert.ok(!bridge.includes('localStorage.getItem("mom-voice-latency-traces")'));
  assert.ok(!bridge.includes('localStorage.setItem("mom-voice-latency-traces"'));
  assert.ok(bridge.includes("capture_to_first_sample_ms"));
  assert.ok(bridge.includes("voice_session_prepare_native_ms"));
  assert.ok(bridge.includes("silence_event_delivery_ms"));
  assert.ok(bridge.includes("native_emitted_at_unix_us"));
  assert.ok(bridge.includes("const nativeDelta = (from, to)"));
  assert.ok(bridge.includes("silence_to_native_release_ms"));
  assert.ok(bridge.includes("native_release_to_worker_ms"));
  assert.ok(bridge.includes("capture_processing_cpu_ms"));
  assert.ok(bridge.includes("capture_vad_latched_ms"));
  assert.ok(bridge.includes("capture_vad_settle_ms"));
  assert.ok(bridge.includes("capture_vad_voiced_evidence_ms"));
  assert.ok(bridge.includes("capture_vad_stationary_noise_releases"));
  assert.ok(bridge.includes("capture_queue_delay_ms"));
  assert.ok(bridge.includes("capture_prefill_push_cpu_ms"));
  assert.ok(bridge.includes("capture_input_seal_cpu_ms"));
  assert.ok(bridge.includes("capture_input_seal_max_ms"));
  assert.ok(bridge.includes("voice_session_cached_prefix_tokens"));
  assert.ok(bridge.includes("audio_first_accept_ms"));
  assert.ok(bridge.includes("prompt_prefix_cpu_ms"));
  assert.ok(bridge.includes("projector_first_finish_ms"));
  assert.ok(bridge.includes("projector_overlap_before_finish_ms"));
  assert.ok(bridge.includes("projector_reader_waits"));
  assert.ok(bridge.includes("projector_reader_wakeups"));
  assert.ok(bridge.includes("audio_producer_lock_wait_ms"));
  assert.ok(bridge.includes("audio_ring_write_ms"));
  assert.ok(bridge.includes("audio_archive_copy_ms"));
  assert.ok(bridge.includes("audio_consumer_lock_wait_ms"));
  assert.ok(bridge.includes("audio_ring_read_ms"));
  assert.ok(bridge.includes("silence_to_dispatch_ms"));
  assert.ok(bridge.includes("silence_to_first_content_ms"));
  assert.ok(bridge.includes("silence_to_tts_pcm_ms"));
  assert.ok(bridge.includes("observed_at_unix_us"));
  assert.ok(bridge.includes("dispatch_to_native_command_ms"));
  assert.ok(bridge.includes("dispatch_to_native_worker_ms"));
  assert.ok(bridge.includes("native_admission_to_worker_prearmed_ms"));
  assert.ok(bridge.includes("native_worker_to_first_content_ms"));
  assert.ok(bridge.includes("const currentSpeculation = latestSpeculationStart?.entry?.speculation"));
  assert.ok(bridge.includes("entry.stage === stage && entry.speculation === currentSpeculation"));
  assert.ok(bridge.includes("entry.speculation === payload?.speculation"));
  assert.ok(bridge.includes("first_content_to_tts_pcm_ms"));
  assert.ok(bridge.includes("first_content_to_tts_chunk_ms"));
  assert.ok(bridge.includes("tts_chunk_to_native_dispatch_ms"));
  assert.ok(bridge.includes("tts_native_dispatch_to_pcm_ms"));
  assert.ok(bridge.includes("tts_pcm_to_playback_ms"));
  assert.ok(bridge.includes("silence_to_playback_ms"));
  assert.ok(bridge.includes("silence_decision_delay_ms"));
  assert.ok(bridge.includes("silence_release_to_playback_ms"));
  assert.ok(bridge.includes('"silence_onset"'));
  assert.ok(bridge.includes('"capture_processing"'));
  assert.ok(bridge.includes('"native_capture_failed"'));
  assert.ok(bridge.includes('"generation_dispatched"'));
  assert.ok(bridge.includes('"native_generation_worker_started"'));
  assert.ok(bridge.includes('"generation_finished"'));
  assert.ok(bridge.includes('"first_content_delta"'));
  assert.ok(bridge.includes('"tts_first_pcm"'));
  assert.ok(bridge.includes('"durable_voice_audio_ready"'));
  assert.ok(bridge.includes("occurrences: 0, total_gap_ms: 0, max_gap_ms: 0"));
  assert.ok(bridge.includes("COALESCED_VOICE_LATENCY_STAGES"));
  assert.ok(bridge.includes("first_observed_at_unix_us"));
  assert.ok(bridge.includes("VOICE_LATENCY_STAGE_LIMIT = 512"));
  assert.ok(bridge.includes("trace.checkpoint = (trace.checkpoint || 0) + 1"));
  assert.ok(bridge.includes('markVoiceLatencyIn(recording.trace, "audio_prefill_begin_failed"'));
  assert.ok(bridge.includes('markVoiceLatencyIn(recording.trace, "audio_prefill_push_failed"'));
  for (const stage of ["audio_prefill_begin_requested", "audio_prefill_session_ready",
    "audio_prefill_begin_failed", "next_turn_prefill_failed"]) {
    assert.ok(bridge.includes(`      "${stage}",`), `${stage} must persist a diagnostic checkpoint`);
  }
  assert.ok(bridge.includes('markVoiceLatency("runtime_warm_state", runtimeWarmState)'));
  assert.ok(bridge.includes('markVoiceLatency("runtime_warm_completed", runtimeWarmState)'));
  assert.ok(bridge.includes('markVoiceLatency("runtime_warm_failed", runtimeWarmState)'));
  assert.ok(bridge.includes("audio_projector_warmed"));
  assert.ok(bridge.includes("audio_projector_warm_us"));
  assert.ok(bridge.includes("call_request_to_runtime_ready_ms"));
  assert.ok(bridge.includes("runtime_warm_audio_projector_ms"));
  assert.ok(bridge.includes("persona_prefixes_reused"));
  assert.ok(bridge.includes("voice_session_resident_context_tokens"));
  assert.ok(bridge.includes("voice_session_settings_cache_hit"));
  assert.ok(bridge.includes("voice_session_model_resident_hit"));
});

test("voice latency summary uses native and absolute clocks across event delivery", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  assert.ok(start >= 0 && end > start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const stages = [
    { stage: "silence_onset", elapsed_ms: 140, native_emitted_at_unix_us: 1_000_000 },
    { stage: "native_silence_released", elapsed_ms: 100, native_emitted_at_unix_us: 1_020_000 },
    { stage: "native_generation_worker_started", elapsed_ms: 110, native_emitted_at_unix_us: 1_030_000 },
    { stage: "first_content_delta", elapsed_ms: 610, native_emitted_at_unix_us: 1_500_000 },
    { stage: "first_content_delta_received", elapsed_ms: 620, observed_at_unix_us: 1_510_000 },
    { stage: "generation_finished", elapsed_ms: 640, observed_at_unix_us: 1_530_000,
      first_token_ms: 1_240, voice_audio: {
        finish_us: 1_000_000, decode_start_us: 1_110_000,
      } },
    { stage: "tts_first_chunk_ready", elapsed_ms: 650, observed_at_unix_us: 1_540_000 },
    { stage: "tts_native_dispatched", elapsed_ms: 651, observed_at_unix_us: 1_541_000 },
    { stage: "tts_first_pcm", elapsed_ms: 810, observed_at_unix_us: 1_700_000 },
    { stage: "playback_started", elapsed_ms: 860, observed_at_unix_us: 1_750_000 },
    { stage: "tts_segment_finished", elapsed_ms: 1_000, total_ms: 400,
      output_audio_ms: 800, production_rtf: 0.5,
      compute_lease_acquisitions: 1, compute_lease_releases: 1,
      compute_lease_held_us: 410000, compute_lease_released_us: 90000,
      generation_yields: 1, generation_yield_us: 135_250,
      compute_minimum_lead_ms: 132, compute_maximum_lead_ms: 390,
      compute_low_water_ms: 180, compute_high_water_ms: 350 },
    { stage: "tts_segment_finished", elapsed_ms: 1_800, total_ms: 800,
      output_audio_ms: 1_000, production_rtf: 0.8,
      compute_lease_acquisitions: 1, compute_lease_releases: 1,
      compute_lease_held_us: 320000, compute_lease_released_us: 80000,
      generation_yields: 0, generation_yield_us: 0,
      compute_minimum_lead_ms: 140, compute_maximum_lead_ms: 370,
      compute_low_water_ms: 190, compute_high_water_ms: 360 },
    { stage: "tts_all_segments_finished", elapsed_ms: 1_801, synthesis_ms: 1_200,
      output_audio_ms: 1_800, production_rtf: 0.6667, pcm_packets: 9,
      pcm_audio_ms: 1_800, first_packet_audio_ms: 138.67, max_packet_gap_ms: 117,
      minimum_producer_lead_ms: 138.67, buffered_runway_ms: 700,
      scheduler: { target_runway_ms: 220, waits: 1, wait_ms: 500,
        maximum_runway_ms: 900, observed_production_rtf: 0.67,
        underrun_penalty_ms: 0 },
      semantic_backpressure: { switches: 4, pause_ms: 840, command_ms: 3.2,
        generation_yields: 2, generation_yield_ms: 832, maximum_queued_words: 24 } },
    { stage: "next_turn_prefill_started", elapsed_ms: 1_802 },
    { stage: "next_turn_prefill_ready", elapsed_ms: 1_920, buffered_runway_ms: 582 },
    { stage: "barge_in_detected", elapsed_ms: 1_930 },
    { stage: "barge_in_playback_stopped", elapsed_ms: 1_948 },
    { stage: "playback_underrun", elapsed_ms: 900, occurrences: 2, total_gap_ms: 21.5, max_gap_ms: 14 },
    { stage: "playback_renderer", elapsed_ms: 950, worklet: true, packets: 8, frames: 24_000,
      schedule_cpu_us: 1_250, schedule_max_us: 310, underruns: 2, worklet_recoveries: 1,
      worklet_rebuffered_frames: 2_400, worklet_max_buffered_frames: 8_000,
      worklet_max_queued_frames: 9_000 },
  ];
  const summary = summarize({ stages });
  assert.equal(summary.silence_to_native_release_ms, 20);
  assert.equal(summary.native_release_to_worker_ms, 10);
  assert.equal(summary.native_worker_to_first_content_ms, 470);
  assert.equal(summary.first_content_delivery_ms, 10);
  assert.equal(summary.silence_to_first_content_ms, 500);
  assert.equal(summary.first_content_to_tts_pcm_ms, 200);
  assert.equal(summary.first_content_to_tts_chunk_ms, 40);
  assert.equal(summary.tts_chunk_to_native_dispatch_ms, 1);
  assert.equal(summary.tts_native_dispatch_to_pcm_ms, 159);
  assert.equal(summary.audio_finish_to_decode_ms, 110);
  assert.equal(summary.decode_to_first_token_ms, 130);
  assert.equal(summary.silence_to_first_token_native_ms, 240);
  assert.equal(summary.generation_finished_to_tts_pcm_ms, 170);
  assert.equal(summary.silence_to_tts_pcm_ms, 700);
  assert.equal(summary.tts_pcm_to_playback_ms, 50);
  assert.equal(summary.silence_to_playback_ms, 750);
  assert.equal(summary.tts_segment_count, 2);
  assert.equal(summary.tts_total_synthesis_ms, 1_200);
  assert.equal(summary.tts_total_output_audio_ms, 1_800);
  assert.equal(summary.tts_aggregate_production_rtf, 0.6667);
  assert.equal(summary.tts_pcm_packets, 9);
  assert.equal(summary.tts_pcm_audio_ms, 1_800);
  assert.equal(summary.tts_first_packet_audio_ms, 138.67);
  assert.equal(summary.tts_max_packet_gap_ms, 117);
  assert.equal(summary.tts_minimum_producer_lead_ms, 138.67);
  assert.equal(summary.tts_scheduler_target_runway_ms, 220);
  assert.equal(summary.tts_scheduler_waits, 1);
  assert.equal(summary.tts_scheduler_wait_ms, 500);
  assert.equal(summary.tts_scheduler_maximum_runway_ms, 900);
  assert.equal(summary.tts_scheduler_observed_rtf, 0.67);
  assert.equal(summary.tts_scheduler_underrun_penalty_ms, 0);
  assert.equal(summary.semantic_backpressure_switches, 4);
  assert.equal(summary.semantic_backpressure_pause_ms, 840);
  assert.equal(summary.semantic_backpressure_command_ms, 3.2);
  assert.equal(summary.semantic_backpressure_generation_yields, 2);
  assert.equal(summary.semantic_backpressure_generation_yield_ms, 832);
  assert.equal(summary.semantic_backpressure_maximum_words, 24);
  assert.equal(summary.tts_worst_segment_rtf, 0.8);
  assert.equal(summary.tts_compute_lease_acquisitions, 2);
  assert.equal(summary.tts_compute_lease_releases, 2);
  assert.equal(summary.tts_generation_yields, 1);
  assert.equal(summary.tts_generation_yield_ms, 135.25);
  assert.equal(summary.tts_compute_lease_held_ms, 730);
  assert.equal(summary.tts_compute_lease_released_ms, 170);
  assert.equal(summary.tts_compute_minimum_lead_ms, 132);
  assert.equal(summary.tts_compute_maximum_lead_ms, 390);
  assert.equal(summary.tts_compute_low_water_ms, 190);
  assert.equal(summary.tts_compute_high_water_ms, 360);
  assert.equal(summary.tts_all_segments_buffered_runway_ms, 700);
  assert.equal(summary.tts_to_next_prefill_start_ms, 1);
  assert.equal(summary.next_prefill_prepare_ms, 118);
  assert.equal(summary.next_prefill_buffered_runway_ms, 582);
  assert.equal(summary.next_prefill_overlapped_active_tts, false);
  assert.equal(summary.barge_in_playback_stop_ms, 18);
  assert.equal(summary.playback_renderer_worklet, true);
  assert.equal(summary.playback_renderer_packets, 8);
  assert.equal(summary.playback_renderer_frames, 24_000);
  assert.equal(summary.playback_renderer_schedule_cpu_ms, 1.25);
  assert.equal(summary.playback_renderer_schedule_max_ms, 0.31);
  assert.equal(summary.playback_renderer_underruns, 2);
  assert.equal(summary.playback_renderer_recoveries, 1);
  assert.equal(summary.playback_renderer_rebuffered_frames, 2_400);
  assert.equal(summary.playback_renderer_max_buffered_frames, 8_000);
  assert.equal(summary.playback_renderer_max_queued_frames, 9_000);
  assert.equal(summary.playback_underrun_occurrences, 2);
  assert.equal(summary.playback_underrun_total_gap_ms, 21.5);
  assert.equal(summary.playback_underrun_max_gap_ms, 14);
});

test("voice latency includes endpoint-decision time from the physical silence edge", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  assert.ok(start >= 0 && end > start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const summary = summarize({ stages: [
    { stage: "silence_onset", elapsed_ms: 500, observed_at_unix_us: 1_500_000,
      native_emitted_at_unix_us: 1_500_000, silence_ms: 250 },
    { stage: "native_silence_released", elapsed_ms: 510,
      native_emitted_at_unix_us: 1_510_000 },
    { stage: "generation_dispatched", elapsed_ms: 520, observed_at_unix_us: 1_520_000 },
    { stage: "first_content_delta", elapsed_ms: 700, observed_at_unix_us: 1_700_000 },
    { stage: "generation_finished", elapsed_ms: 720, observed_at_unix_us: 1_720_000,
      first_token_ms: 1_600, voice_audio: { finish_us: 1_500_000 } },
    { stage: "tts_first_pcm", elapsed_ms: 900, observed_at_unix_us: 1_900_000 },
    { stage: "playback_render_started", elapsed_ms: 1_000,
      observed_at_unix_us: 2_000_000 },
  ] });

  assert.equal(summary.silence_decision_delay_ms, 250);
  assert.equal(summary.silence_release_to_native_release_ms, 10);
  assert.equal(summary.silence_to_native_release_ms, 260);
  assert.equal(summary.silence_release_to_first_token_native_ms, 100);
  assert.equal(summary.silence_to_first_token_native_ms, 350);
  assert.equal(summary.silence_release_to_first_content_ms, 200);
  assert.equal(summary.silence_to_first_content_ms, 450);
  assert.equal(summary.silence_release_to_tts_pcm_ms, 400);
  assert.equal(summary.silence_to_tts_pcm_ms, 650);
  assert.equal(summary.silence_release_to_playback_ms, 500);
  assert.equal(summary.silence_to_playback_ms, 750);
});

test("browser microphone cost and queued PCM survive into the latency summary", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const summary = summarize({ stages: [
    { stage: "capture_processing", callback_cpu_ms: 24.5, callback_max_ms: 1.25,
      detector_cpu_ms: 8.5, resample_cpu_ms: 6,
      prefill_ipc_queued_samples: 1_280, prefill_ipc_queue_high_water_samples: 4_800,
      prefill_suppressed_silence_samples: 3_200 },
  ] });
  assert.equal(summary.capture_processing_cpu_ms, 24.5);
  assert.equal(summary.capture_processing_max_ms, 1.25);
  assert.equal(summary.capture_detector_cpu_ms, 8.5);
  assert.equal(summary.capture_resample_cpu_ms, 6);
  assert.equal(summary.capture_prefill_ipc_queued_at_stop_ms, 80);
  assert.equal(summary.capture_prefill_ipc_queue_high_water_ms, 300);
  assert.equal(summary.capture_prefill_suppressed_silence_ms, 200);
  const absent = summarize({ stages: [] });
  assert.equal(absent.capture_prefill_ipc_queued_at_stop_ms, null);
});

test("a late pickup result cannot become the next call turn's generation", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const stages = [
    { stage: "capture_started", elapsed_ms: 0 },
    { stage: "generation_finished", purpose: "call_greeting", elapsed_ms: 685,
      prompt_tokens: 2984, completion_tokens: 11 },
  ];
  assert.equal(summarize({ mode: "call", stages }).generation_completion_tokens, null);
  assert.equal(summarize({ mode: "call_greeting", stages }).generation_completion_tokens, 11);
});

test("physical silence timing stays paired with the latest release attempt", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const summary = summarize({ stages: [
    { stage: "silence_onset", observed_at_unix_us: 1_500_000, silence_ms: 0 },
    { stage: "speculation_aborted", observed_at_unix_us: 1_600_000 },
    { stage: "silence_onset", observed_at_unix_us: 2_000_000, silence_ms: 800 },
    { stage: "first_content_delta", observed_at_unix_us: 2_200_000 },
  ] });

  assert.equal(summary.silence_release_to_first_content_ms, 200);
  assert.equal(summary.silence_to_first_content_ms, 1_000);
});

test("voice latency pairs every terminal milestone with its own speculative attempt", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const summary = summarize({ stages: [
    { stage: "silence_onset", elapsed_ms: 100, native_emitted_at_unix_us: 1_000_000 },
    { stage: "generation_dispatched", elapsed_ms: 101, observed_at_unix_us: 1_001_000 },
    { stage: "native_generation_worker_started", elapsed_ms: 102, native_emitted_at_unix_us: 1_002_000 },
    { stage: "speculation_aborted", elapsed_ms: 150 },
    { stage: "silence_onset", elapsed_ms: 1_000, native_emitted_at_unix_us: 2_000_000 },
    { stage: "generation_dispatched", elapsed_ms: 1_001, observed_at_unix_us: 2_001_000 },
    { stage: "native_generation_worker_started", elapsed_ms: 1_003, native_emitted_at_unix_us: 2_003_000 },
    { stage: "first_content_delta", elapsed_ms: 1_203, native_emitted_at_unix_us: 2_203_000 },
  ] });
  assert.equal(summary.dispatch_to_native_worker_ms, 2);
  assert.equal(summary.native_worker_to_first_content_ms, 200);
  assert.equal(summary.silence_to_first_content_ms, 203);
});

test("voice latency pairs native silence edges by clock when listener delivery is reversed", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const summary = summarize({ stages: [
    { stage: "silence_onset", elapsed_ms: 100, native_emitted_at_unix_us: 1_000_000 },
    { stage: "native_silence_released", elapsed_ms: 101, native_emitted_at_unix_us: 1_000_040 },
    { stage: "native_silence_released", elapsed_ms: 950, native_emitted_at_unix_us: 2_000_040 },
    // The renderer receives this exact second silence edge after its native
    // release notification. Array order must not pair the release to attempt 1.
    { stage: "silence_onset", elapsed_ms: 951, native_emitted_at_unix_us: 2_000_000 },
    { stage: "native_generation_worker_started", elapsed_ms: 952,
      native_emitted_at_unix_us: 2_000_110 },
  ] });
  assert.equal(summary.silence_to_native_release_ms, 0.04);
  assert.equal(summary.native_release_to_worker_ms, 0.07);
});

test("voice latency retains terminal metrics from the exact speculative attempt", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const summary = summarize({ stages: [
    { stage: "generation_timeout", elapsed_ms: 100, speculation: "attempt-a" },
    { stage: "native_audio_finish_failed", elapsed_ms: 110, speculation: "attempt-a",
      audio: { accepted_samples: 640, sampled_tokens: 0 } },
    { stage: "generation_timeout", elapsed_ms: 200, speculation: "attempt-b" },
    { stage: "native_audio_finish_failed", elapsed_ms: 210, speculation: "attempt-b",
      audio: { accepted_samples: 12_800, sampled_tokens: 3 } },
  ] });
  assert.equal(summary.audio_accepted_samples, 12_800);
  assert.equal(summary.generation_sampled_tokens, 3);
});

test("an in-flight speculative turn cannot inherit the prior turn's audio or TTS timing", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const summary = summarize({ stages: [
    { stage: "silence_onset", elapsed_ms: 100, silence_ms: 180 },
    { stage: "speculation_started", elapsed_ms: 101, speculation: "old" },
    { stage: "first_content_delta", elapsed_ms: 200 },
    { stage: "tts_first_pcm", elapsed_ms: 300 },
    { stage: "native_audio_finish_returned", elapsed_ms: 310, speculation: "old",
      audio: { accepted_samples: 12_800 } },
    { stage: "generation_finished", elapsed_ms: 320, speculation: "old" },
    { stage: "silence_onset", elapsed_ms: 500, silence_ms: 320 },
    { stage: "speculation_started", elapsed_ms: 501, speculation: "new" },
  ] });
  assert.equal(summary.audio_accepted_samples, null);
  assert.equal(summary.first_content_to_tts_pcm_ms, null);
  assert.equal(summary.silence_decision_delay_ms, 320);
});

test("next-turn prefetch is never charged to the current microphone session", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const prefetched = summarize({ stages: [
    { stage: "capture_started", elapsed_ms: 0 },
    { stage: "audio_prefill_native_targeted", elapsed_ms: 1 },
    { stage: "next_turn_prefill_started", elapsed_ms: 2_000 },
    { stage: "audio_prefill_session_ready", elapsed_ms: 2_140,
      begin: { total_us: 140_000 } },
  ] });
  assert.equal(prefetched.voice_session_prepare_ms, null);
  assert.equal(prefetched.voice_session_prepared_before_capture, true);
  assert.equal(prefetched.voice_session_prepare_native_ms, null);
  assert.equal(prefetched.voice_session_handoff_ms, null);

  const ordinary = summarize({ stages: [
    { stage: "capture_started", elapsed_ms: 10 },
    { stage: "audio_prefill_begin_requested", elapsed_ms: 11 },
    { stage: "audio_prefill_session_ready", elapsed_ms: 42,
      begin: { total_us: 20_000 } },
    { stage: "audio_prefill_native_targeted", elapsed_ms: 43 },
  ] });
  assert.equal(ordinary.voice_session_prepare_ms, 32);
  assert.equal(ordinary.voice_session_prepared_before_capture, false);
  assert.equal(ordinary.voice_session_prepare_native_ms, 20);
  assert.equal(ordinary.voice_session_request_to_ready_ms, 31);
  assert.equal(ordinary.voice_session_handoff_ms, 11);
});

test("live speech uses punctuation or a mode-appropriate PCM runway", () => {
  const start = bridge.indexOf("  const takeLiveSpeechChunk =");
  const end = bridge.indexOf("\n  const beginLiveVoiceTts =", start);
  assert.ok(start >= 0 && end > start);
  const chunk = Function(`${bridge.slice(start, end)}\nreturn takeLiveSpeechChunk;`)();

  assert.equal(chunk("One two three four five six", false, true), null);
  assert.deepEqual(chunk("One two three four five six seven", false, true), {
    chunk: "One two three four five six",
    rest: "seven",
  });
  assert.deepEqual(chunk("One two three,", false, true), {
    chunk: "One two three,",
    rest: "",
  });
  assert.equal(chunk("One two thr", false, true), null);
  assert.deepEqual(chunk("One two", true, true), {
    chunk: "One two",
    rest: "",
  });
  assert.equal(chunk("Yes. More text is arriving", false, true), null);
  assert.deepEqual(chunk("Hello, Jan. How are you", false, true), {
    chunk: "Hello, Jan.",
    rest: "How are you",
  });
  assert.equal(chunk("Hello, Jan", false, true), null);
  assert.equal(
    chunk("One two three four five six seven eight nine ten", false, true, 10, "voice_note"),
    null,
  );
  assert.equal(
    chunk("Yes. This short sentence must not reset prosody", false, true, 10, "voice_note"),
    null,
  );
  assert.deepEqual(
    chunk("Yes. This short sentence joins the following complete phrase naturally.", false, true, 10, "voice_note"),
    { chunk: "Yes. This short sentence joins the following complete phrase naturally.", rest: "" },
  );
  assert.equal(
    chunk("One two three four five six seven eight nine ten eleven", false, true, 10, "voice_note"),
    null,
  );
});

test("call barge-in can stop only an actually audible reply", () => {
  const start = bridge.indexOf("  const interruptCallPlaybackForSpeech =");
  const end = bridge.indexOf("\n  const beginBrowserVoiceCapture =", start);
  const interruption = bridge.slice(start, end);
  assert.ok(interruption.includes("if (!reply?.audible) return;"));
  assert.ok(bridge.includes("get audible() { return controller.playing; }"));
  assert.ok(interruption.includes("reply.interruptForSpeech()"));
  assert.ok(bridge.includes("if (!synthesized && tts.interruptedForSpeech) return true;"));
});

test("call generation cannot stall forever or disappear without a visible terminal error", () => {
  assert.ok(bridge.includes('code = "voice_generation_timeout"'));
  assert.ok(bridge.includes("error.code = code"));
  assert.ok(bridge.includes('"call_reply_timeout: The contact did not finish answering the call."'));
  assert.ok(bridge.includes('"voice_generation_timeout: The contact stopped making progress while preparing a reply."'));
  assert.ok(bridge.includes('markVoiceLatencyIn(recording.trace, "generation_timeout"'));
  assert.ok(bridge.includes('void invoke("mom_llama_chat_cancel", { conversation }).catch(() => {});'));
  assert.ok(bridge.includes("speculation.generationReleased.then(() => withProgressDeadline("));
  assert.ok(bridge.includes("voiceProgressDeadlineExpired({"));
  assert.ok(bridge.includes("speculation.releaseGeneration?.();"));
  assert.ok(bridge.includes('latestAttemptStage("native_audio_finish_failed")'));
  assert.ok(bridge.includes('payload.detail && typeof payload.detail === "object"'));
});

test("failed voice generation retains partial native audio progress", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const summary = summarize({ stages: [
    { stage: "native_generation_worker_started", elapsed_ms: 100 },
    { stage: "native_audio_finish_failed", elapsed_ms: 10_100, audio: {
      accepted_samples: 32_000,
      consumed_samples: 32_000,
      finish_us: 2_000_000,
      decode_start_us: 2_110_000,
      first_projector_finish_us: 400_000,
      last_projector_finish_us: 1_950_000,
      projected_blocks: 20,
      projected_samples: 32_000,
      reader_waits: 19,
      reader_wakeups: 19,
      cancellation_requested_us: 2_250_000,
      cancellation_observed_us: 2_908_110,
    } },
    { stage: "generation_timeout", elapsed_ms: 10_101 },
  ] });
  assert.equal(summary.audio_accepted_samples, 32_000);
  assert.equal(summary.audio_consumed_samples, 32_000);
  assert.equal(summary.projector_blocks, 20);
  assert.equal(summary.audio_finish_to_decode_ms, 110);
  assert.equal(summary.projector_tail_to_decode_ms, 160);
  assert.equal(summary.projector_reader_waits, 19);
  assert.equal(summary.voice_cancel_to_observed_ms, 658.11);
});

test("decode boundary preserves prefix and projector timing before a reply completes", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const summary = summarize({ stages: [
    { stage: "speculation_started", elapsed_ms: 100, speculation: "current" },
    { stage: "native_chat_stream_event", event: "decode_started", elapsed_ms: 4_500,
      speculation: "current", audio: {
        accepted_samples: 16_000, consumed_samples: 16_000,
        prefix_start_us: 100_000, prefix_finish_us: 3_100_000,
        prefix_us: 3_000_000, prefix_tokens: 2_900, restored_prefix_tokens: 0,
        projector_us: 900_000, projected_blocks: 25, projected_samples: 16_000,
        first_projector_start_us: 3_110_000, last_projector_finish_us: 4_050_000,
        finish_us: 4_100_000, decode_start_us: 4_300_000,
      } },
  ] });
  assert.equal(summary.prompt_prefix_cpu_ms, 3_000);
  assert.equal(summary.projector_cpu_ms, 900);
  assert.equal(summary.projector_audio_ms, 1_000);
  assert.equal(summary.audio_finish_to_decode_ms, 200);
});

test("voice latency exposes resumed-speech checkpoint and replay cost", () => {
  const start = bridge.indexOf("  const voiceLatencySummary =");
  const end = bridge.indexOf("\n  const persistVoiceLatency =", start);
  const summarize = Function(`${bridge.slice(start, end)}\nreturn voiceLatencySummary;`)();
  const summary = summarize({ stages: [
    { stage: "speculation_handoff_ready", elapsed_ms: 2_170,
      round_trip_ms: 2_170, abort_ms: 0.03, begin_ms: 2_168.3,
      resume_wait_us: 2_168_216, resume_restore_us: 2_158_351,
      resume_reused: false, resume_checkpoint_available: false,
      replay: { replay_samples: 54_896, total_us: 131 } },
  ] });
  assert.equal(summary.speculation_handoff_round_trip_ms, 2_170);
  assert.equal(summary.speculation_handoff_resume_wait_ms, 2_168.22);
  assert.equal(summary.speculation_handoff_restore_ms, 2_158.35);
  assert.equal(summary.speculation_handoff_checkpoint_reused, false);
  assert.equal(summary.speculation_handoff_checkpoint_available, false);
  assert.equal(summary.speculation_handoff_replay_samples, 54_896);
  assert.equal(summary.speculation_handoff_replay_ms, 0.13);
});

test("a hydrated voice reply keeps its portrait and player in one row", () => {
  assert.ok(bridge.includes("article.prepend(speakerAvatar);"));
  assert.match(styles, /\.message-row\.voice-reply-row\.voice-reply-has-audio\s*> \.message-speaker-avatar/);
  assert.match(styles, /\.message-row\.voice-reply-row\.voice-reply-has-audio\s*> \.message-card/);
  assert.match(
    styles,
    /\.message-row\.assistant \.message-card:has\(> \.voice-message-player\)[\s\S]*background: transparent/,
    "the replaceable message card must not be the durable voice bubble",
  );
  assert.match(
    styles,
    /\.message-row\.assistant \.message-card > \.voice-message-player[\s\S]*border-radius: 22px[\s\S]*background:/,
    "the retained incoming player must paint its own visible bubble",
  );
});

test("voice controls publish play only with proven audio and never inherit a pressed glyph", () => {
  assert.ok(bridge.includes('button.dataset.audioProof = "pcm-complete"'));
  assert.ok(bridge.includes("!isCanonicalPcmWav(bytes)"));
  assert.ok(bridge.includes("settleUnavailableVoiceReply(button)"));
  assert.ok(bridge.includes("history.map((frame) => ({ ...frame, data: frame.data.slice() }))"));
  assert.ok(styles.includes('.voice-message-play[data-audio-state="ready"]::before'));
  assert.ok(styles.includes("background: var(--message-blue);"));
  assert.ok(styles.includes('.voice-message-play[data-audio-state="playing"]::before'));
  assert.ok(styles.includes("appearance: none;"));
});

test("the startup TTS compile probe primes one bounded exact-persona emission", () => {
  const warmStart = qwenHelper.indexOf("if request.warm_only == true");
  const warmEnd = qwenHelper.indexOf('event: "warmed"', warmStart);
  assert.ok(warmStart >= 0 && warmEnd > warmStart);
  const warmPath = qwenHelper.slice(warmStart, warmEnd);
  assert.ok(warmPath.includes("warmedPrefixKeys.insert(preparedPrefixKey).inserted"));
  assert.ok(warmPath.includes("preparedPrefixKey: preparedPrefixKey"));
  assert.ok(warmPath.includes("maxTokens: 2"));
  assert.ok(warmPath.includes("chunkSize: 2"));
});

test("live speech has one segmentation authority before Qwen synthesis", () => {
  assert.ok(qwenHelper.includes("The app scheduler is the sole segmentation authority"));
  assert.ok(qwenHelper.includes("TextChunker.spokenChunks(request.text, maxWords: .max)"));
  assert.ok(qwenHelper.includes('let speechChunks = speechText.isEmpty ? [] : [speechText]'));
  assert.ok(!qwenHelper.includes("let speechChunks = TextChunker.spokenChunks(request.text)"));
});

test("historical text after a voice note is not presented as retained audio", () => {
  const start = bridge.indexOf("  const hydrateAttachmentPreviews = async");
  const end = bridge.indexOf("\n  window.addEventListener(\"beforeunload\"", start);
  const hydration = bridge.slice(start, end);

  assert.ok(hydration.includes("A preceding user voice attachment proves how the user spoke"));
  assert.ok(hydration.includes('next.classList.remove("voice-reply-pending", "voice-reply-row")'));
  assert.ok(hydration.includes('button.classList.remove("voice-reply-play", "is-loading", "is-awaiting-audio")'));
  assert.ok(!hydration.includes('button.dataset.personaVoice = "true";\n      if (preparingLiveReply)'));
});

test("a degraded voice import binds its exact private prompt before dispatch", () => {
  assert.ok(bridge.includes("let pendingVoiceFallbackAttachment = null"));
  assert.match(
    bridge,
    /if \(fallbackVoice\) \{[\s\S]*?persistDraftNow\(message, \[fallbackVoice\.attachment\], conversation\)[\s\S]*?mom_llama_chat_dispatch/,
  );
});

test("the native presentation contains no Skills, cache, engine, or resident-slot panels", () => {
  for (const marker of [
    'data-settings-card="skills"',
    'data-settings-card="cache"',
    'data-settings-card="engine"',
    'class="settings-subgrid"',
    'id="prompt-cache-card"',
    'class="settings-card cache-preferences"',
    'name="kv_cache_policy"',
    'class="native-number-grid resident-fields"',
    'id="skill-form"',
    "fn prompt_cache_card",
    "Gentle explainer",
    "Warm, plain language",
    "Explain simply and warmly.",
    "Create Skill",
    "No Skills yet.",
    "Policy: disabled until verified",
    "KV-cache persistence is surfaced",
  ]) {
    assert.ok(!viewProduction.includes(marker), `forbidden native-view marker remains: ${marker}`);
  }

  for (const marker of [
    "mom_llama_engine_check",
    "mom_llama_engine_configure",
    "mom_llama_skill_",
    "mom_llama_kv_cache_",
    "mom_llama_model_slot_",
    "kvCachePolicy",
    '"skills-open"',
    '"engine-check"',
    '"kv-status"',
    '"kv-clear"',
    '"resident-slot-load"',
    '"resident-slot-unload"',
  ]) {
    assert.ok(!bridge.includes(marker), `forbidden renderer bridge remains: ${marker}`);
  }

  assert.ok(!index.includes("cache-inspector.js"));
  for (const marker of [
    ".settings-subgrid",
    ".cache-card-heading",
    ".cache-action-status",
    ".skill-row",
    ".skill-form",
  ]) {
    assert.ok(!styles.includes(marker), `orphaned presentation style remains: ${marker}`);
  }
});

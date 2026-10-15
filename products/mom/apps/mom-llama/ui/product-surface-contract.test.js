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
const index = read("apps/mom-llama/ui/index.html");
const styles = read("apps/mom-llama/ui/style.css");
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
  assert.ok(bridge.includes('"voice-message-cancel": async () =>'));
  assert.ok(!bridge.includes("const composerHasSendableDraft"));
  assert.ok(!bridge.includes('send.dataset.modelReady !== "true"'));
  const glassControls = styles.slice(
    styles.indexOf(".chrome .icon-button,\n.composer .round-button"),
    styles.indexOf(".sidebar-section-toggle", styles.indexOf(".chrome .icon-button,\n.composer .round-button")),
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
  assert.ok(!viewProduction.includes('aria-label="Microphone"'));
  assert.ok(bridge.includes("const openVoiceCallSurface = async () =>"));
  assert.ok(bridge.includes("modal.dataset.voicePersona = recipientId"));
  assert.ok(bridge.includes('.sidebar-titlebar-zone")?.classList.toggle("searching", enabled)'));
  assert.ok(styles.includes(".chrome .icon { width: 18px; height: 18px; flex-basis: 18px; }"));
  assert.ok(styles.includes("left: calc(var(--sidebar-width) + 50px)"));
  assert.ok(styles.includes("height: min(520px, calc(100dvh - 24px))"));
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
  const playbackStart = bridge.indexOf("const renderSpeechPlayback =");
  const playbackEnd = bridge.indexOf("const appendAttachmentTranscriptionControl", playbackStart);
  const playback = bridge.slice(playbackStart, playbackEnd);

  assert.ok(playback.includes('createCommandElement("audio", DYNAMIC_CONTROL_SPECS.readAloudStop)'));
  assert.ok(playback.includes('button.title = "Stop reading"'));
  assert.ok(playback.includes('invoke("mom_llama_speech_stop"'));
  assert.ok(!playback.includes("Stop suppresses playback immediately"));
  assert.ok(!playback.includes("Apple’s inner synthesis call is non-preemptive"));
  assert.ok(!playback.includes('document.createElement("small")'));
  assert.ok(!playback.includes('document.createElement("output")'));
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

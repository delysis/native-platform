#!/usr/bin/env node

import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { webkit } from "playwright";

const uiRoot = process.env.MOM_VOICE_REPLY_FIXTURE_ROOT || path.dirname(fileURLToPath(import.meta.url));
const scriptPath = path.join(uiRoot, "coop-hx.js");
const stylePath = path.join(uiRoot, "style.css");
const avatarPath = path.join(uiRoot, "contact-portraits.png");
const source = fs.readFileSync(scriptPath, "utf8");

function declaration(name, nextMarker) {
  const start = source.indexOf(`  const ${name} =`);
  assert.notEqual(start, -1, `production declaration ${name} exists`);
  const end = source.indexOf(nextMarker, start + 1);
  assert.notEqual(end, -1, `production declaration ${name} has a stable boundary`);
  return source.slice(start, end).trimEnd();
}

const productionCode = [
  declaration("ensureMessageStream", "  const appendLiveDelta ="),
  declaration("appendMessageSpeakerAvatar", "  const appendLiveMessage ="),
  declaration("appendLiveMessage", "  const appendOptimisticVoiceMessage ="),
  declaration("createTypingIndicator", "  let voiceReplyPlayRequested ="),
  source.slice(source.indexOf("  const WAVEFORM_MOTIF ="), source.indexOf("  const markCompactAudioUnavailable =")),
  declaration("isCanonicalPcmWav", "  const compactAudioPlayer ="),
  declaration("pendingVoiceMessage", "  const createTypingIndicator ="),
  declaration("pendingVoiceReply", "  const pendingVoiceReplyPlayer ="),
  declaration("pendingVoiceReplyPlayer", "  const unavailableVoiceMessage ="),
  declaration("settleInMemoryVoiceReply", "  const restoreReadAloudButton ="),
  declaration("appendLiveVoiceReply", "  const keepLiveTailVisible ="),
].join("\n\n");

const mountStart = source.indexOf("    const mountVoiceReplyPlayer = () => {");
const mountEnd = source.indexOf("    const pendingWordCount =", mountStart);
assert.ok(mountStart >= 0 && mountEnd > mountStart, "production ready-player mount has a stable boundary");
const mountCode = source.slice(mountStart, mountEnd).trimEnd();
const fixtureCode = `(() => {
  const chat = () => document.querySelector(".chat-main");
  const settingEnabled = () => false;
  const keepLiveTailVisible = () => {};
  const createCommandElement = (tag) => document.createElement(tag);
  const DYNAMIC_CONTROL_SPECS = { attachmentPreview: {} };
  const voiceReplyPlayback = new WeakMap();
  const voiceLatencyTrace = {};
  const markVoiceLatencyIn = () => {};
  const wavBytes = new Uint8Array(46);
  const wavView = new DataView(wavBytes.buffer);
  const writeText = (offset, value) => [...value].forEach((character, index) => {
    wavBytes[offset + index] = character.charCodeAt(0);
  });
  writeText(0, "RIFF");
  wavView.setUint32(4, 38, true);
  writeText(8, "WAVE");
  writeText(12, "fmt ");
  wavView.setUint32(16, 16, true);
  wavView.setUint16(20, 1, true);
  wavView.setUint16(22, 1, true);
  wavView.setUint32(24, 16000, true);
  wavView.setUint32(28, 32000, true);
  wavView.setUint16(32, 2, true);
  wavView.setUint16(34, 16, true);
  writeText(36, "data");
  wavView.setUint32(40, 2, true);
  const completedController = { ready: true, playing: false, wavBytes: () => wavBytes };
  const playback = {};
  const mode = "voice_note";
  let button = null;
  const firstPcmSeen = true;
  ${productionCode}
  ${mountCode}
  return {
    append: appendLiveVoiceReply,
    mount: mountVoiceReplyPlayer,
    settle: (button) => settleInMemoryVoiceReply(button, completedController, voiceLatencyTrace),
  };
})()`;

const serverDocument = `<!doctype html>
<html><head><meta charset="utf-8"><link rel="stylesheet" href="style.css"></head>
<body><div class="llama-ui-shell" data-theme="light" style="--sidebar-width: 0px; --toolbar-height: 0px;">
  <main class="chat-main has-messages">
    <header class="recipients-header"><div class="recipient-line"><div class="recipient-chip">
      <span class="contact-avatar illustrated" style="background-position: 0% 0%"></span>
    </div></div></header>
    <section class="message-stream" aria-label="Messages"></section>
  </main>
</div></body></html>`;

async function inspectViewport(page, width, persona, screenshotDir) {
  await page.setViewportSize({ width, height: 820 });
  await page.evaluate(({ id, position }) => {
    const chip = document.querySelector(".recipient-chip");
    chip.dataset.personaId = id;
    chip.querySelector(".contact-avatar").style.backgroundPosition = position;
  }, persona);
  await page.evaluate(() => document.querySelector(".message-stream").replaceChildren());
  const row = await page.evaluate((code) => {
    const api = new Function(`return ${code};`)();
    return api.append("voice-reply-layout-fixture").id;
  }, fixtureCode);

  const pending = await page.evaluate((rowId) => {
    const rect = (element) => {
      const { x, y, width, height } = element.getBoundingClientRect();
      return { x, y, width, height };
    };
    const row = document.getElementById(rowId);
    const avatar = row.querySelector(":scope > .message-speaker-avatar");
    const portrait = avatar?.querySelector(".contact-avatar.illustrated");
    const card = row.querySelector(":scope > .message-card");
    const grid = getComputedStyle(row);
    return {
      avatarVisible: Boolean(avatar && avatar.getClientRects().length && getComputedStyle(avatar).visibility !== "hidden"),
      portraitBackground: portrait ? getComputedStyle(portrait).backgroundImage : "",
      portraitPosition: portrait ? getComputedStyle(portrait).backgroundPosition : "",
      recipientPosition: getComputedStyle(document.querySelector(".recipient-chip .contact-avatar")).backgroundPosition,
      avatar: avatar ? rect(avatar) : null,
      card: card ? rect(card) : null,
      gridColumns: grid.gridTemplateColumns,
      typingVisible: Boolean(card?.querySelector(".typing-indicator")),
      streamWidth: row.parentElement.getBoundingClientRect().width,
    };
  }, row);
  assert.equal(pending.avatarVisible, true, `avatar is visible at ${width}px`);
  assert.match(pending.portraitBackground, /contact-portraits\.png/);
  assert.equal(pending.portraitPosition, pending.recipientPosition, "the cloned avatar keeps the selected persona artwork");
  assert.equal(pending.typingVisible, true);
  assert.ok(pending.avatar.height >= 30, `avatar has a rendered size at ${width}px`);
  assert.ok(Math.abs((pending.avatar.y + pending.avatar.height / 2) - (pending.card.y + pending.card.height / 2)) <= 3,
    `pending avatar and bubble are vertically aligned at ${width}px`);
  assert.ok(pending.avatar.x + pending.avatar.width <= pending.card.x + 2,
    `pending avatar is in the leading grid column at ${width}px`);
  assert.ok(pending.card.width <= 130 && pending.card.width < pending.streamWidth * 0.5,
    `typing bubble stays compact at ${width}px`);

  if (screenshotDir) {
    const personaFile = persona.id;
    await page.screenshot({
      path: path.join(screenshotDir, `voice-reply-pending-${personaFile}-${width}.png`),
      clip: { x: 0, y: 0, width, height: 110 },
    });
  }

  const ready = await page.evaluate((args) => {
    const rect = (element) => {
      const { x, y, width, height } = element.getBoundingClientRect();
      return { x, y, width, height };
    };
    const row = document.getElementById(args.rowId);
    const api = new Function(`return ${args.code};`)();
    const button = api.mount();
    const settled = api.settle(button);
    const avatar = row.querySelector(":scope > .message-speaker-avatar");
    const card = row.querySelector(":scope > .message-card");
    return {
      buttonClass: button?.className || "",
      settled,
      rowReady: row.classList.contains("voice-reply-has-audio"),
      pendingClassRemoved: !row.classList.contains("voice-reply-pending"),
      avatarVisible: Boolean(avatar && avatar.getClientRects().length && getComputedStyle(avatar).visibility !== "hidden"),
      avatarCount: row.querySelectorAll(":scope > .message-speaker-avatar").length,
      avatarX: avatar?.getBoundingClientRect().x ?? -1,
      portraitPosition: avatar ? getComputedStyle(avatar.querySelector(".contact-avatar")).backgroundPosition : "",
      avatar: avatar ? rect(avatar) : null,
      card: card ? rect(card) : null,
      playerWidth: card?.querySelector(".voice-message-player")?.getBoundingClientRect().width || 0,
      typingGone: !card?.querySelector(".typing-indicator"),
      playDisabled: Boolean(card?.querySelector(".voice-reply-play")?.disabled),
      audioState: card?.querySelector(".voice-reply-play")?.dataset.audioState || "",
      waveformBars: card?.querySelectorAll(".voice-waveform > i").length || 0,
      recipientPosition: getComputedStyle(document.querySelector(".recipient-chip .contact-avatar")).backgroundPosition,
    };
  }, { rowId: row, code: fixtureCode });
  assert.equal(ready.rowReady, true);
  assert.equal(ready.settled, true, "the production settle helper promotes the player after completed PCM");
  assert.equal(ready.pendingClassRemoved, true);
  assert.equal(ready.avatarVisible, true, `ready player retains avatar at ${width}px`);
  assert.equal(ready.avatarCount, 1, "the reply has exactly one speaker avatar");
  assert.equal(ready.avatarX, pending.avatar.x, "the same avatar remains in place across the transition");
  assert.equal(ready.portraitPosition, ready.recipientPosition, "the ready player keeps that persona's portrait");
  assert.match(ready.buttonClass, /voice-reply-play/);
  assert.equal(ready.typingGone, true);
  assert.equal(ready.playDisabled, false, "the completed player exposes playback");
  assert.equal(ready.audioState, "ready");
  assert.ok(ready.waveformBars >= 11, "the production waveform helper renders audio bars");
  assert.ok(ready.playerWidth >= 170 && ready.playerWidth <= 360, `player has production width at ${width}px`);
  assert.ok(Math.abs((ready.avatar.y + ready.avatar.height / 2) - (ready.card.y + ready.card.height / 2)) <= 3,
    `ready avatar and player are vertically aligned at ${width}px`);
  if (screenshotDir) {
    const personaFile = persona.id;
    await page.screenshot({
      path: path.join(screenshotDir, `voice-reply-ready-${personaFile}-${width}.png`),
      clip: { x: 0, y: 0, width, height: 110 },
    });
  }
  return { persona: persona.id, width, pending, ready };
}

const temp = fs.mkdtempSync(path.join(os.tmpdir(), "mom-voice-reply-layout-"));
const screenshotDir = process.env.MOM_VOICE_REPLY_SCREENSHOT_DIR || "";
if (screenshotDir) fs.mkdirSync(screenshotDir, { recursive: true });
const style = fs.readFileSync(stylePath);
const avatar = fs.readFileSync(avatarPath);
fs.writeFileSync(path.join(temp, "style.css"), style);
fs.writeFileSync(path.join(temp, "contact-portraits.png"), avatar);
fs.writeFileSync(path.join(temp, "fixture.html"), serverDocument);

let browser;
try {
  browser = await webkit.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 820 } });
  await page.goto(pathToFileURL(path.join(temp, "fixture.html")).href);
  await page.waitForFunction(() => document.styleSheets.length > 0 && document.fonts.status === "loaded");
  const avatarLoaded = await page.evaluate(async () => {
    const image = new Image();
    image.src = new URL("contact-portraits.png", document.baseURI).href;
    await image.decode();
    const applied = getComputedStyle(document.querySelector(".contact-avatar")).backgroundImage;
    return { width: image.naturalWidth, height: image.naturalHeight, applied };
  });
  assert.ok(avatarLoaded.width > 0 && avatarLoaded.height > 0, "real persona sprite loads in WebKit");
  assert.match(avatarLoaded.applied, /contact-portraits\.png/);

  const results = [];
  for (const persona of [
    { id: "persona-bessel_van_der_kolk", position: "0% 0%" },
    { id: "persona-dolores_mosquera", position: "33.333333333333336% 100%" },
  ]) {
    for (const width of [1280, 390]) {
      results.push(await inspectViewport(page, width, persona, screenshotDir));
    }
  }
  const receipt = {
    schema: "mom-voice-reply-layout-webkit-v1",
    browser: browser.version(),
    source_sha256: crypto.createHash("sha256").update(source).digest("hex"),
    stylesheet_sha256: crypto.createHash("sha256").update(style).digest("hex"),
    avatar_sha256: crypto.createHash("sha256").update(avatar).digest("hex"),
    avatar_dimensions: [avatarLoaded.width, avatarLoaded.height],
    viewports: results,
    screenshots: screenshotDir || null,
  };
  if (screenshotDir) {
    fs.writeFileSync(path.join(screenshotDir, "layout-receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`);
  }
  process.stdout.write(`${JSON.stringify(receipt, null, 2)}\n`);
} finally {
  await browser?.close();
  fs.rmSync(temp, { recursive: true, force: true });
}

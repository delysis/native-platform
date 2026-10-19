(() => {
  "use strict";

  const MAX_PERSONA_TOOL_BINDINGS = 8;

  const tauri = () => window.__TAURI__;
  const invoke = (command, payload = {}) => {
    const core = tauri() && tauri().core;
    if (!core || typeof core.invoke !== "function") {
      return Promise.reject(new Error("Tauri IPC is unavailable."));
    }
    return core.invoke(command, payload);
  };
  const invokeMarkup = async (command) => {
    const response = await invoke(command);
    if (typeof response === "string") return response;
    if (response instanceof ArrayBuffer) {
      return new TextDecoder().decode(new Uint8Array(response));
    }
    if (ArrayBuffer.isView(response)) {
      return new TextDecoder().decode(response);
    }
    if (Array.isArray(response)) {
      return new TextDecoder().decode(Uint8Array.from(response));
    }
    const responseType = Object.prototype.toString.call(response);
    const constructorName = response?.constructor?.name || "unknown";
    throw new Error(
      `Renderer ${command} returned ${responseType} (${constructorName}) instead of text or bytes.`,
    );
  };

  const shell = () => document.querySelector(".llama-ui-shell");
  const chat = () => document.getElementById("chat");
  const selectedConversation = () =>
    (chat() && chat().dataset.currentConversation) || "default";
  const selectedConversationKind = () =>
    (chat() && chat().dataset.conversationKind) || "chat";
  const sizeComposer = () => {
    const textarea = document.querySelector("#chat-form textarea[name='message']");
    if (!textarea) return;
    textarea.style.height = "auto";
    textarea.style.height = `${textarea.scrollHeight}px`;
  };
  const initializeWindowChrome = async () => {
    const nativeWindow = tauri()?.window?.getCurrentWindow();
    if (!nativeWindow) return;
    const root = document.documentElement;
    const macos = navigator.platform.startsWith("Mac");
    if (macos) root.dataset.nativePlatform = "macos";
    const syncChrome = async () => {
      if (macos) root.classList.toggle("native-fullscreen", await nativeWindow.isFullscreen());
      sizeComposer();
    };
    await syncChrome();
    await nativeWindow.onResized(() => { void syncChrome().catch(reportError); });
  };
  const composerPolicy = globalThis.MomLlamaComposerKeyPolicy;
  let composerState = composerPolicy?.initialState?.() || { kind: "idle" };
  const transitionComposer = (event) => {
    if (!composerPolicy?.reduce) return [];
    const transition = composerPolicy.reduce(composerState, event);
    composerState = transition.state;
    return transition.effects;
  };
  const settingEnabled = (key, fallback = false) => {
    const field = document.querySelector(`[data-setting-key="${key}"]`);
    return field ? Boolean(field.checked) : fallback;
  };
  const settingNumber = (key, fallback) => {
    const field = document.querySelector(`[data-setting-key="${key}"]`);
    if (!field) return fallback;
    const value = Number(field.value);
    return Number.isFinite(value) ? value : fallback;
  };
  const wait = (milliseconds) =>
    new Promise((resolve) => window.setTimeout(resolve, milliseconds));
  const withProgressDeadline = async (promise, idleMs, progress, message) => {
    const started = performance.now();
    let timer = null;
    try {
      return await Promise.race([promise, new Promise((_, reject) => {
        timer = window.setInterval(() => {
          if (globalThis.MomVoiceCapturePolicy.voiceProgressDeadlineExpired({
            now: performance.now(), started, lastProgress: Math.max(started, progress()),
            idleMs, maxElapsedMs: 120_000,
          })) {
            const error = new Error(message);
            error.code = "voice_generation_timeout";
            reject(error);
          }
        }, 100);
      })]);
    } finally { if (timer !== null) window.clearInterval(timer); }
  };
  const withDeadline = async (promise, milliseconds, message, code = "voice_generation_timeout") => {
    let timeout = null;
    try {
      return await Promise.race([
        promise,
        new Promise((_, reject) => {
          timeout = window.setTimeout(() => {
            const error = new Error(message);
            error.code = code;
            reject(error);
          }, milliseconds);
        }),
      ]);
    } finally {
      if (timeout !== null) window.clearTimeout(timeout);
    }
  };
  const applyTheme = (theme) => {
    const root = shell();
    if (root) root.dataset.theme = theme || "system";
  };
  const applyCustomCss = (css) => {
    let style = document.getElementById("mom-llama-custom-css");
    if (!style) {
      style = document.createElement("style");
      style.id = "mom-llama-custom-css";
      document.head.append(style);
    }
    style.textContent = typeof css === "string" ? css : "";
  };
  const attachmentCopyText = (content) => {
    if (!settingEnabled("copyTextAttachmentsAsPlainText")) return content;
    const match = String(content).match(/^Attached text file `[^`]+`:\s*```text\s*([\s\S]*?)\s*```\s*$/);
    return match ? match[1] : content;
  };

  const recordCommandResult = (value) => {
    const output = document.getElementById("command-output");
    if (output) output.value = JSON.stringify(value);
  };

  const renderStatusContent = (status, label, onDismiss) => {
    const message = document.createElement("span");
    message.textContent = String(label);
    const dismiss = document.createElement("button");
    dismiss.type = "button";
    dismiss.className = "command-status-dismiss";
    dismiss.setAttribute("aria-label", "Dismiss");
    dismiss.textContent = "×";
    dismiss.addEventListener("click", onDismiss, { once: true });
    status.replaceChildren(message, dismiss);
  };

  const report = (value) => {
    recordCommandResult(value);
    if (persistentVoiceCallFailure) {
      renderPersistentVoiceCallFailure();
      return;
    }
    const status = document.getElementById("command-status");
    if (status) {
      window.clearTimeout(status.hideTimer);
      const blocker = value?.blocker?.message;
      const label = blocker
        || value?.result?.message
        || (value?.status === "blocked" ? "That action could not be completed." : null);
      if (!label) {
        status.classList.add("is-hidden");
        status.replaceChildren();
        return;
      }
      renderStatusContent(status, String(label).replaceAll("_", " "), () => report(null));
      const blocked = Boolean(blocker) || value?.status === "blocked";
      status.classList.toggle("blocked", blocked);
      status.classList.remove("is-hidden");
      status.hideTimer = window.setTimeout(() => report(null), blocked ? 9000 : 5000);
    }
  };

  const errorMessage = (error) => (
    error && error.message ? error.message : String(error)
  );

  let persistentVoiceCallFailure = null;
  const renderPersistentVoiceCallFailure = () => {
    const status = document.getElementById("command-status");
    if (!status || !persistentVoiceCallFailure) return;
    window.clearTimeout(status.hideTimer);
    renderStatusContent(status, persistentVoiceCallFailure.message, () => {
      persistentVoiceCallFailure = null;
      report(null);
    });
    status.classList.add("blocked");
    status.classList.remove("is-hidden");
    delete status.hideTimer;
  };
  const setPersistentVoiceCallFailure = (message) => {
    persistentVoiceCallFailure = { message: String(message) };
    renderPersistentVoiceCallFailure();
  };
  const clearPersistentVoiceCallFailure = () => {
    if (!persistentVoiceCallFailure) return;
    persistentVoiceCallFailure = null;
    report(null);
  };
  const safeVoiceFailureCode = (error) => {
    const value = typeof error?.code === "string" ? error.code : "";
    return /^[a-zA-Z0-9][a-zA-Z0-9_.:-]{0,79}$/.test(value) ? value : "startup_error";
  };

  const visiblyReportedErrors = new WeakSet();
  const reportError = (error) => {
    if (error && (typeof error === "object" || typeof error === "function")) {
      if (visiblyReportedErrors.has(error)) return;
      visiblyReportedErrors.add(error);
    }
    const message = errorMessage(error);
    report({ status: "blocked", blocker: { code: "view_command_failed", message } });
    if (!document.getElementById("command-status")) {
      const root = document.getElementById("app");
      if (root) {
        const state = document.createElement("section");
        state.className = "boot boot-error";
        const heading = document.createElement("h1");
        heading.textContent = "Blessenger could not open";
        const detail = document.createElement("p");
        detail.textContent = message;
        state.append(heading, detail);
        root.replaceChildren(state);
      }
    }
    console.error(message);
  };

  const mcpProcessUiSupported = () =>
    shell()?.dataset.mcpProcessUiSupported === "true";
  const requireMcpProcessUi = () => {
    if (mcpProcessUiSupported()) return true;
    report({
      status: "blocked",
      blocker: {
        code: "mcp_platform_unsupported",
        message: "Configured external MCP child processes are available only on macOS and Linux.",
      },
    });
    return false;
  };

  const parseFragment = (markup) => {
    const template = document.createElement("template");
    template.innerHTML = markup.trim();
    return template.content.firstElementChild;
  };

  const ATTACHMENT_PREVIEW_CONCURRENCY = 2;
  const ATTACHMENT_PREVIEW_LIVE_LIMIT = 4;
  const attachmentObjectUrls = new Map();
  const attachmentPreviewMedia = new WeakMap();
  const attachmentPreviewPromises = new WeakMap();
  const attachmentPreviewFallbacks = new WeakMap();
  const attachmentPreviewQueue = [];
  let attachmentPreviewLoads = 0;
  let attachmentPreviewObserver = null;

  const attachmentPreviewsWithin = (root) => {
    if (!root) return [];
    const previews = root.matches?.("[data-attachment-preview]") ? [root] : [];
    previews.push(...(root.querySelectorAll?.("[data-attachment-preview]") || []));
    return previews;
  };

  const rememberAttachmentPreviewFallback = (preview) => {
    if (attachmentPreviewFallbacks.has(preview)) return;
    const body = preview.querySelector(".attachment-preview-body");
    if (!body) return;
    attachmentPreviewFallbacks.set(
      preview,
      [...body.childNodes].map((node) => node.cloneNode(true)),
    );
  };

  const restoreAttachmentPreviewFallback = (preview) => {
    const body = preview.querySelector(".attachment-preview-body");
    const fallback = attachmentPreviewFallbacks.get(preview);
    if (!body || !fallback) return;
    body.replaceChildren(...fallback.map((node) => node.cloneNode(true)));
  };

  const touchAttachmentPreview = (media) => {
    const entry = attachmentObjectUrls.get(media);
    if (!entry) return;
    attachmentObjectUrls.delete(media);
    attachmentObjectUrls.set(media, entry);
  };

  const releaseAttachmentPreview = (preview, restoreFallback = false) => {
    const transcription = preview.querySelector("[data-action='attachment-transcribe']");
    if (transcription?.dataset.operation) {
      void invoke("mom_llama_speech_stop", { operation: transcription.dataset.operation })
        .catch(() => {});
    }
    attachmentTranscripts.delete(preview);
    preview.querySelector(".attachment-transcription")?.remove();
    const media = attachmentPreviewMedia.get(preview);
    const entry = media && attachmentObjectUrls.get(media);
    if (entry) URL.revokeObjectURL(entry.url);
    if (media) attachmentObjectUrls.delete(media);
    attachmentPreviewMedia.delete(preview);
    if (restoreFallback) {
      restoreAttachmentPreviewFallback(preview);
      preview.classList.remove("hydrated-document");
      preview.removeAttribute("title");
    }
  };

  const enforceAttachmentPreviewLimit = () => {
    while (attachmentObjectUrls.size > ATTACHMENT_PREVIEW_LIVE_LIMIT) {
      const oldest = attachmentObjectUrls.entries().next().value;
      if (!oldest) return;
      const [, entry] = oldest;
      releaseAttachmentPreview(entry.preview, true);
      entry.preview.dataset.previewHydrated = "evicted";
    }
  };

  const releaseAttachmentObjectUrls = (root) => {
    const previews = attachmentPreviewsWithin(root);
    previews.forEach((preview) => {
      preview.dataset.previewReleased = "true";
      attachmentPreviewObserver?.unobserve(preview);
      releaseAttachmentPreview(preview);
    });
    for (let index = attachmentPreviewQueue.length - 1; index >= 0; index -= 1) {
      if (previews.includes(attachmentPreviewQueue[index])) {
        attachmentPreviewQueue.splice(index, 1);
      }
    }
  };

  const projectionRevisions = new Map();
  const swap = async (selector, command) => {
    const current = document.querySelector(selector);
    if (!current) return null;
    const revision = (projectionRevisions.get(selector) || 0) + 1;
    projectionRevisions.set(selector, revision);
    const replacement = parseFragment(await invokeMarkup(command));
    if (projectionRevisions.get(selector) !== revision || document.querySelector(selector) !== current) return null;
    if (!replacement) throw new Error(`Renderer ${command} returned no element.`);
    if (activeSpeechPlayback?.panel && current.contains(activeSpeechPlayback.panel)) {
      await stopSpeechPlayback();
    }
    if (projectionRevisions.get(selector) !== revision || document.querySelector(selector) !== current) return null;
    releaseAttachmentObjectUrls(current);
    current.replaceWith(replacement);
    sizeComposer();
    return replacement;
  };

  const ipcBinaryBytes = (response, maxBytes, label) => {
    let bytes;
    if (response instanceof ArrayBuffer) {
      bytes = new Uint8Array(response);
    } else if (ArrayBuffer.isView(response)) {
      bytes = new Uint8Array(response.buffer, response.byteOffset, response.byteLength);
    } else if (Array.isArray(response)) {
      if (response.length > maxBytes) {
        throw new Error(`${label} exceeded its ${maxBytes}-byte IPC limit.`);
      }
      if (!response.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)) {
        throw new Error(`${label} returned an invalid IPC byte array.`);
      }
      bytes = Uint8Array.from(response);
    } else {
      throw new Error(`${label} returned an unsupported IPC byte representation.`);
    }
    if (bytes.byteLength > maxBytes) {
      throw new Error(`${label} exceeded its ${maxBytes}-byte IPC limit.`);
    }
    return bytes;
  };

  const speechOperationToken = () => {
    const value = globalThis.crypto?.randomUUID?.();
    if (!value) throw new Error("Secure opaque speech operation IDs are unavailable.");
    return value;
  };
  const createVoiceCallActivityGuard = (sessionId) => {
    let active = false;
    let clearPromise = null;
    let activation;
    try {
      activation = invoke("mom_llama_kv_cache_set_voice_call_activity", {
        sessionId,
        active: true,
      });
    } catch (error) {
      activation = Promise.reject(error);
    }
    const ready = Promise.resolve(activation).then((response) => {
      if (response?.status === "blocked") {
        throw new Error(response?.blocker?.message || "Voice cache activity could not be recorded.");
      }
      active = true;
      return true;
    });
    return {
      sessionId,
      ready,
      clear: async () => {
        await ready.catch(() => false);
        if (!active) return false;
        if (!clearPromise) {
          clearPromise = Promise.resolve().then(() => invoke("mom_llama_kv_cache_set_voice_call_activity", {
            sessionId,
            active: false,
          })).then((response) => {
            if (response?.status === "blocked") {
              throw new Error(response?.blocker?.message || "Voice cache activity could not be cleared.");
            }
            active = false;
            return true;
          }).catch((error) => {
            clearPromise = null;
            throw error;
          });
        }
        return clearPromise;
      },
    };
  };
  const sha256Hex = async (bytes) => {
    const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
    return [...new Uint8Array(digest)]
      .map((byte) => byte.toString(16).padStart(2, "0"))
      .join("");
  };
  const attachmentTranscripts = new WeakMap();
  let activeSpeechPlayback = null;
  let speechPlaybackEpoch = null;

  const WAVEFORM_MOTIF = Object.freeze([7, 13, 20, 11, 17, 8, 15, 10, 19, 12, 7]);
  const WAVEFORM_BAR_COUNT = Math.ceil(160 / WAVEFORM_MOTIF.length) * WAVEFORM_MOTIF.length;
  const waveformMotifs = new WeakMap();
  const fillWaveform = (bars) => {
    const motif = waveformMotifs.get(bars);
    if (!motif) return;
    // Render enough complete motifs for the widest supported bubble and let
    // overflow clip them. Measuring a detached node left the first frame with
    // one motif stretched across the row until ResizeObserver caught up.
    if (bars.childElementCount === WAVEFORM_BAR_COUNT) return;
    const fragment = document.createDocumentFragment();
    for (let index = 0; index < WAVEFORM_BAR_COUNT; index += 1) {
      const bar = document.createElement("i");
      bar.style.height = `${motif[index % motif.length]}px`;
      fragment.append(bar);
    }
    bars.replaceChildren(fragment);
  };

  const waveform = (bytes = null) => {
    const bars = document.createElement("span");
    bars.className = "voice-waveform";
    bars.setAttribute("aria-hidden", "true");
    let heights = WAVEFORM_MOTIF;
    if (bytes?.byteLength >= 46) {
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      const samples = Math.floor((bytes.byteLength - 44) / 2);
      if (samples > 0) {
        const barCount = 11;
        heights = Array.from({ length: barCount }, (_, bar) => {
          const start = Math.floor((bar * samples) / barCount);
          const end = Math.max(start + 1, Math.floor(((bar + 1) * samples) / barCount));
          let peak = 0;
          for (let sample = start; sample < end; sample += Math.max(1, Math.floor((end - start) / 96))) {
            peak = Math.max(peak, Math.abs(view.getInt16(44 + sample * 2, true)) / 32768);
          }
          return Math.round(6 + Math.min(1, peak * 2.4) * 16);
        });
      }
    }
    waveformMotifs.set(bars, heights);
    fillWaveform(bars);
    return bars;
  };

  const markCompactAudioUnavailable = (toggle, player, error) => {
    toggle.dataset.audioState = "failed";
    toggle.dataset.audioProof = "failed";
    toggle.textContent = "";
    toggle.disabled = true;
    toggle.setAttribute("aria-label", "Voice message unavailable");
    toggle.removeAttribute("aria-pressed");
    player.classList.remove("is-playing", "voice-message-pending");
    player.classList.add("voice-message-unavailable");
    if (error) reportError(error);
  };

  const isCanonicalPcmWav = (bytes) => {
    if (!(bytes instanceof Uint8Array) || bytes.byteLength < 46) return false;
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const text = (offset, length) => String.fromCharCode(
      ...bytes.subarray(offset, offset + length),
    );
    if (text(0, 4) !== "RIFF" || text(8, 4) !== "WAVE") return false;
    if (text(12, 4) !== "fmt " || view.getUint32(16, true) !== 16) return false;
    if (view.getUint16(20, true) !== 1 || view.getUint16(34, true) !== 16) return false;
    const channels = view.getUint16(22, true);
    const sampleRate = view.getUint32(24, true);
    const bytesPerFrame = channels * 2;
    if (!channels || channels > 2 || sampleRate < 8_000 || sampleRate > 192_000) return false;
    if (view.getUint16(32, true) !== bytesPerFrame) return false;
    if (text(36, 4) !== "data") return false;
    const dataBytes = view.getUint32(40, true);
    return dataBytes > 0
      && dataBytes % bytesPerFrame === 0
      && dataBytes === bytes.byteLength - 44;
  };

  const compactAudioPlayer = (audio, bytes = null, {
    onReady = null,
    onPlaybackStarted = null,
    onPlaybackError = null,
  } = {}) => {
    const player = document.createElement("div");
    player.className = "voice-message-player voice-message-pending";
    const toggle = createCommandElement("button", DYNAMIC_CONTROL_SPECS.attachmentPreview);
    toggle.type = "button";
    toggle.className = "voice-message-play";
    toggle.dataset.audioState = "pending";
    toggle.dataset.audioProof = "pending";
    toggle.setAttribute("aria-label", "Loading voice message");
    toggle.textContent = "";
    toggle.disabled = true;
    let mediaVerified = false;
    const fail = (error) => {
      markCompactAudioUnavailable(toggle, player, error);
      onPlaybackError?.(error);
    };
    toggle.addEventListener("click", async () => {
      if (audio.paused) {
        try {
          await audio.play();
        } catch (error) {
          fail(error);
        }
      } else {
        audio.pause();
      }
    });
    // `play` only means that WebKit accepted a request. `playing` is the first
    // event that proves the decoded media clock actually started advancing.
    audio.addEventListener("playing", () => {
      toggle.dataset.audioState = "playing";
      toggle.textContent = "";
      toggle.setAttribute("aria-label", "Pause voice message");
      toggle.removeAttribute("aria-pressed");
      player.classList.add("is-playing");
      onPlaybackStarted?.();
    });
    const stopped = () => {
      // WebKit may emit pause while a newly assigned source is still loading.
      // Pause is a playback transition, never evidence that the source decoded.
      // Promoting here used to expose a dead play triangle before metadata had
      // established that any audio existed.
      if (!mediaVerified || toggle.dataset.audioState === "failed") return;
      toggle.dataset.audioState = "ready";
      toggle.textContent = "";
      toggle.setAttribute("aria-label", "Play voice message");
      toggle.removeAttribute("aria-pressed");
      player.classList.remove("is-playing");
    };
    audio.addEventListener("pause", stopped);
    audio.addEventListener("ended", stopped);
    audio.addEventListener("error", () => fail(
      new Error("voice_message_decode_failed: The retained voice message could not be decoded."),
    ));
    const resetUnverifiedMedia = () => {
      if (toggle.dataset.audioState === "failed") return;
      mediaVerified = false;
      toggle.disabled = true;
      toggle.dataset.audioState = "pending";
      toggle.dataset.audioProof = "pending";
      toggle.textContent = "";
      toggle.setAttribute("aria-label", "Loading voice message");
      toggle.removeAttribute("aria-pressed");
      player.classList.remove("is-playing", "voice-message-unavailable");
      player.classList.add("voice-message-pending");
    };
    audio.addEventListener("abort", resetUnverifiedMedia);
    audio.addEventListener("emptied", resetUnverifiedMedia);
    const validateMetadata = () => {
      if (toggle.dataset.audioState === "failed") return;
      if (typeof audio.duration === "number"
        && (!Number.isFinite(audio.duration) || audio.duration <= 0)) {
        markCompactAudioUnavailable(
          toggle,
          player,
          new Error("voice_message_empty: The retained voice message contains no playable audio."),
        );
      }
    };
    const ready = () => {
      if (toggle.dataset.audioState === "failed") return;
      validateMetadata();
      if (toggle.dataset.audioState === "failed") return;
      mediaVerified = true;
      toggle.disabled = false;
      toggle.dataset.audioState = "ready";
      toggle.dataset.audioProof = "decoded";
      toggle.setAttribute("aria-label", "Play voice message");
      toggle.removeAttribute("aria-pressed");
      toggle.textContent = "";
      player.classList.remove("voice-message-pending");
      onReady?.();
    };
    // Metadata proves only that WebKit parsed a container header. It does not
    // prove that the payload can be decoded. Publish a play control only after
    // WebKit has decoded enough media to report `canplay`.
    audio.addEventListener("loadedmetadata", validateMetadata);
    audio.addEventListener("canplay", ready);
    audio.hidden = true;
    player.append(toggle, waveform(bytes), audio);
    if (bytes && !isCanonicalPcmWav(bytes)) {
      markCompactAudioUnavailable(
        toggle,
        player,
        new Error("voice_message_wav_invalid: Generated voice audio failed structural validation."),
      );
    } else {
      audio.load?.();
      const futureData = globalThis.HTMLMediaElement?.HAVE_FUTURE_DATA ?? 3;
      if (audio.readyState >= futureData) queueMicrotask(ready);
    }
    return player;
  };

  const pendingVoiceMessage = (label = "Loading voice message", interactive = true) => {
    const player = document.createElement("div");
    player.className = "voice-message-player voice-message-pending";
    const play = createCommandElement("button", DYNAMIC_CONTROL_SPECS.attachmentPreview);
    play.type = "button";
    play.className = "voice-message-play";
    play.dataset.audioState = "pending";
    play.dataset.audioProof = "pending";
    play.disabled = !interactive;
    if (interactive) play.dataset.action = "voice-message-pending-play";
    play.textContent = "";
    play.setAttribute("aria-label", label);
    player.append(play, waveform());
    return player;
  };

  const createTypingIndicator = () => {
    const indicator = document.createElement("span");
    indicator.className = "typing-indicator";
    indicator.setAttribute("role", "status");
    indicator.setAttribute("aria-label", "Typing");
    indicator.append(
      document.createElement("span"),
      document.createElement("span"),
      document.createElement("span"),
    );
    return indicator;
  };

  let voiceReplyPlayRequested = false;
  const voiceReplyPlayback = new WeakMap();
  const failedVoiceReplyOwners = new Map();
  const MAX_FAILED_VOICE_REPLY_OWNERS = 1;
  const activeVoiceNotePublications = new Set();
  const voiceReplyPersistenceKey = (conversation, message) => `${conversation}\u0000${message}`;
  const pendingVoiceReply = () => {
    return createTypingIndicator();
  };

  const pendingVoiceReplyPlayer = () => {
    const player = pendingVoiceMessage("Preparing voice reply", false);
    const play = player.querySelector(".voice-message-play");
    play.classList.add("voice-reply-play");
    play.classList.add("is-awaiting-audio");
    play.dataset.action = "voice-reply-pending-play";
    return player;
  };

  const unavailableVoiceMessage = () => {
    const player = pendingVoiceMessage("Voice message unavailable", false);
    player.classList.replace("voice-message-pending", "voice-message-unavailable");
    const play = player.querySelector(".voice-message-play");
    play.dataset.audioState = "failed";
    play.dataset.audioProof = "failed";
    play.textContent = "";
    return player;
  };

  const settleUnavailableVoiceReply = (button) => {
    const row = button?.closest?.(".message-row");
    // First PCM promotes the row from pending to ready. Synthesis can still
    // fail after that point, and deleting its controller while leaving the
    // ready-looking button behind creates a control with no audio owner.
    // Any live voice-reply row owned by this button must therefore settle to
    // unavailable when its controller fails, regardless of whether PCM had
    // already arrived.
    if (!row?.classList.contains("voice-reply-row")) return;
    const card = row.querySelector(".message-card");
    if (card) card.replaceChildren(unavailableVoiceMessage());
    row.hidden = false;
    row.classList.remove("voice-reply-pending");
  };

  const settleInMemoryVoiceReply = (button, controller, trace = voiceLatencyTrace) => {
    const row = button?.closest?.(".message-row");
    const card = row?.querySelector?.(".message-card");
    const player = button?.closest?.(".voice-message-player");
    const bytes = controller?.wavBytes?.();
    const markThisTrace = (stage, detail = {}) => markVoiceLatencyIn(trace, stage, detail);
    if (!row?.classList.contains("voice-reply-row") || !card || !player
        || !controller?.ready || !isCanonicalPcmWav(bytes)) {
      settleUnavailableVoiceReply(button);
      markThisTrace("in_memory_voice_audio_failed", {
        reason: bytes?.byteLength ? "invalid_wav" : "missing_wav",
        wav_bytes: bytes?.byteLength || 0,
      });
      return false;
    }
    // Keep replay on the PCM controller that received, sequenced, and retained
    // the generated samples. Re-encoding those proven samples as a Blob and
    // handing them to a second WebKit media pipeline created a fresh failure
    // boundary after successful synthesis and detached this button from its
    // actual audio owner.
    button.disabled = false;
    button.dataset.audioState = controller.playing ? "playing" : "ready";
    button.dataset.audioProof = "pcm-complete";
    button.classList.remove("is-loading", "is-awaiting-audio");
    button.setAttribute(
      "aria-label",
      controller.playing ? "Pause voice message" : "Play voice message",
    );
    button.removeAttribute("aria-pressed");
    player.classList.remove("voice-message-pending", "voice-message-unavailable");
    player.classList.toggle("is-playing", controller.playing);
    markThisTrace("in_memory_voice_audio_ready", { wav_bytes: bytes.byteLength });
    row.hidden = false;
    row.classList.remove("voice-reply-pending");
    return true;
  };

  const restoreReadAloudButton = (button) => {
    if (!button) return;
    delete button.dataset.operation;
    delete button.dataset.playback;
    commandMetadata(button, DYNAMIC_CONTROL_SPECS.readAloud);
    button.dataset.action = "message-read-aloud";
    button.title = "Read aloud";
    button.querySelector(".sr-only")?.replaceChildren("Read aloud");
    button.removeAttribute("aria-pressed");
  };

  const pcmChannel = (onmessage) => {
    const Channel = globalThis.__TAURI__?.core?.Channel;
    if (typeof Channel === "function") return new Channel(onmessage);
    // The browser test harness does not serialize IPC arguments. Keeping the
    // same surface there lets it inject framed packets deterministically.
    return { onmessage };
  };

  const decodePcmFrame = (value) => {
    const bytes = ipcBinaryBytes(value, 8 * 1024 * 1024, "Streaming PCM packet");
    if (bytes.byteLength < 32
      || bytes[0] !== 0x4d || bytes[1] !== 0x50 || bytes[2] !== 0x43 || bytes[3] !== 0x4d
      || bytes[4] !== 1) {
      throw new Error("speech_stream_frame_invalid: Native PCM framing was invalid.");
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const payloadLength = view.getUint32(28, true);
    if (payloadLength !== bytes.byteLength - 32) {
      throw new Error("speech_stream_frame_invalid: Native PCM packet length changed at IPC.");
    }
    return {
      end: (bytes[5] & 1) !== 0,
      interleaved: (bytes[5] & 2) !== 0,
      sampleFormat: bytes[6],
      channels: bytes[7],
      sampleRate: view.getUint32(8, true),
      sequence: Number(view.getBigUint64(12, true)),
      sampleOffset: Number(view.getBigUint64(20, true)),
      data: bytes.subarray(32),
    };
  };

  const pcmWorkletEngine = { context: null, sampleRate: 0, promise: null, owner: null };
  let pcmWorkletWarmState = { status: "not_started" };

  const preparePcmWorklet = async (sampleRate = 24_000) => {
    const AudioContext = window.AudioContext || window.webkitAudioContext;
    if (typeof AudioContext !== "function" || typeof globalThis.AudioWorkletNode !== "function") {
      pcmWorkletWarmState = { status: "unsupported" };
      return null;
    }
    if (pcmWorkletEngine.context?.state !== "closed"
        && pcmWorkletEngine.sampleRate === sampleRate) {
      return pcmWorkletEngine.promise;
    }
    if (pcmWorkletEngine.context && pcmWorkletEngine.context.state !== "closed") {
      await pcmWorkletEngine.context.close().catch(() => {});
    }
    const context = new AudioContext({ latencyHint: "interactive", sampleRate });
    const warmStarted = performance.now();
    pcmWorkletWarmState = { status: "loading" };
    pcmWorkletEngine.context = context;
    pcmWorkletEngine.sampleRate = sampleRate;
    pcmWorkletEngine.promise = (async () => {
      if (context.state === "running") await context.suspend();
      if (context.sampleRate !== sampleRate || !context.audioWorklet?.addModule) {
        await context.close().catch(() => {});
        pcmWorkletWarmState = {
          status: "unsupported",
          actual_sample_rate: context.sampleRate,
          requested_sample_rate: sampleRate,
        };
        return null;
      }
      await context.audioWorklet.addModule("pcm-playback-worklet.js");
      pcmWorkletWarmState = {
        status: "ready",
        total_ms: Math.round((performance.now() - warmStarted) * 100) / 100,
      };
      return { context };
    })().catch(async (error) => {
      await context.close().catch(() => {});
      pcmWorkletWarmState = {
        status: "failed",
        total_ms: Math.round((performance.now() - warmStarted) * 100) / 100,
        message: errorMessage(error),
      };
      return null;
    });
    return pcmWorkletEngine.promise;
  };

  const createPcmPlaybackBuffer = ({
    autoplay = false,
    retainHistory = true,
    startupBufferMs = 120,
    startupDelayMs = 0,
    waitForCompletion = false,
    onFirstChunk,
    onPlayable,
    onPlaybackStarted,
    onPlaybackPaused,
    onPlaybackError,
    onUnderrun,
    onBufferLevel,
    onRendererEvent,
    onRendererMetrics,
    onEnded,
  } = {}) => {
    // Calls supply the scheduler's measured packet-jitter runway. Explicit
    // playback retains the 120 ms floor. This avoids beginning from one 139 ms
    // codec packet when this machine has already observed 200+ ms packet gaps,
    // while keeping the threshold bounded rather than accumulating a large
    // latency buffer.
    const startupBufferSeconds = () => Math.min(
      1.2,
      Math.max(0.04, Number(
        (typeof startupBufferMs === "function" ? startupBufferMs() : startupBufferMs) || 120,
      ) / 1_000),
    );
    const startupDelayMilliseconds = () => Math.min(
      250,
      Math.max(0, Number(
        (typeof startupDelayMs === "function" ? startupDelayMs() : startupDelayMs) || 0,
      )),
    );
    const PLAYBACK_START_TIMEOUT_MS = 2_000;
    const chunks = [];
    const history = [];
    let chunkHead = 0;
    let pendingSeconds = 0;
    const sources = new Set();
    let format = null;
    let echoReference = null;
    let echoReferenceSamples = null;
    let expectedSequence = 0;
    let expectedSampleOffset = 0;
    let context = null;
    let nextStart = 0;
    let receivedAudio = false;
    let firstChunkAt = null;
    let startupDelayTimer = null;
    let playable = false;
    let playing = false;
    let playWhenReady = autoplay;
    let starting = null;
    let ended = false;
    let stopped = false;
    let startCancellationResolve = null;
    const startCancellation = new Promise((resolve) => { startCancellationResolve = resolve; });
    let endGeneration = 0;
    let scheduledAudio = false;
    let workletNode = null;
    let workletOwner = null;
    let workletFinishSent = false;
    let workletCapacityFrames = 0;
    let workletQueuedFrames = 0;
    let rendererPacketIndex = 0;
    const rendererPacketOwners = new Map();
    const RENDERER_PACKET_OWNER_LIMIT = 512;
    let metricsReported = false;
    const rendererMetrics = {
      packets: 0,
      frames: 0,
      schedule_cpu_us: 0,
      schedule_max_us: 0,
      retained_bytes: 0,
      underruns: 0,
      worklet: false,
      worklet_render_quanta: 0,
      worklet_max_buffered_frames: 0,
      worklet_max_queued_frames: 0,
      worklet_recoveries: 0,
      worklet_rebuffered_frames: 0,
      worklet_min_buffered_frames: null,
      worklet_first_receive_context_frame: null,
      worklet_first_render_context_frame: null,
      worklet_final_metrics_received: false,
      worklet_pcm_signal_frames: null,
      worklet_pcm_signal_rms: null,
      worklet_pcm_signal_peak: null,
      worklet_pcm_signal_min_quantum_rms: null,
      worklet_pcm_signal_max_quantum_rms: null,
      worklet_pcm_low_energy_threshold_rms: null,
      worklet_pcm_low_energy_frames: null,
      worklet_pcm_low_energy_quanta: null,
      worklet_pcm_max_low_energy_frames: null,
    };
    const rendererNow = () => globalThis.performance?.now?.() ?? Date.now();
    const segmentSequences = new Map();
    const segmentSampleOffsets = new Map();

    const validateFrame = (frame) => {
      if (frame.sampleFormat !== 1 || !frame.interleaved) {
        throw new Error("speech_stream_format_unsupported: Playback requires interleaved i16 PCM.");
      }
      if (!Number.isInteger(frame.channels) || frame.channels < 1 || frame.sampleRate < 1) {
        throw new Error("speech_stream_format_invalid: PCM format was incomplete.");
      }
      const frames = frame.data.byteLength / (2 * frame.channels);
      if (!Number.isInteger(frames)) {
        throw new Error("speech_stream_frame_invalid: PCM bytes did not contain whole frames.");
      }
      return frames;
    };

    const armEnd = () => {
      if (!playing || !ended || chunks.length !== 0) return;
      if (workletNode) {
        if (!workletFinishSent) {
          workletFinishSent = true;
          workletNode.port.postMessage({ type: "finish" });
        }
        return;
      }
      const generation = ++endGeneration;
      const remainingMs = Math.max(0, (nextStart - context.currentTime) * 1000);
      window.setTimeout(() => {
        if (!stopped && playing && ended && generation === endGeneration) {
          playing = false;
          scheduledAudio = false;
          nextStart = 0;
          const completedContext = context;
          context = null;
          if (completedContext) void completedContext.close().catch(() => {});
          reportRendererMetrics("ended");
          onEnded?.();
        }
      }, remainingMs + 12);
    };

    const schedule = (frame, operationId = null) => {
      if (stopped) return false;
      const scheduleStarted = rendererNow();
      const frames = validateFrame(frame);
      if (frames === 0) return true;
      if (!context) return false;
      if (workletNode) {
        if (workletQueuedFrames + frames > workletCapacityFrames) return false;
        const packetIndex = rendererPacketIndex++;
        if (rendererPacketOwners.size >= RENDERER_PACKET_OWNER_LIMIT) {
          rendererPacketOwners.delete(rendererPacketOwners.keys().next().value);
          rendererMetrics.operation_owner_evictions = (rendererMetrics.operation_owner_evictions || 0) + 1;
        }
        rendererPacketOwners.set(packetIndex, operationId);
        const enqueuedContextTimeMs = context.currentTime * 1_000;
        const buffer = frame.data.buffer;
        if (!(buffer instanceof ArrayBuffer)) {
          throw new Error("speech_stream_worklet_transfer_invalid: PCM packet was not transferable.");
        }
        workletNode.port.postMessage({
          type: "pcm",
          buffer,
          byteOffset: frame.data.byteOffset,
          byteLength: frame.data.byteLength,
          channels: frame.channels,
          packetIndex,
          sequence: frame.sequence,
          sampleOffset: frame.sampleOffset,
          enqueuedContextTimeMs,
        }, [buffer]);
        workletQueuedFrames += frames;
        rendererMetrics.worklet_max_queued_frames = Math.max(
          rendererMetrics.worklet_max_queued_frames,
          workletQueuedFrames,
        );
        const scheduleUs = Math.round((rendererNow() - scheduleStarted) * 1000);
        rendererMetrics.packets += 1;
        rendererMetrics.frames += frames;
        rendererMetrics.schedule_cpu_us += scheduleUs;
        rendererMetrics.schedule_max_us = Math.max(rendererMetrics.schedule_max_us, scheduleUs);
        onRendererEvent?.({
          type: "packet_enqueued",
          packet_index: packetIndex,
          sequence: frame.sequence,
          sample_offset: frame.sampleOffset,
          frames,
          queued_frames: workletQueuedFrames,
          audio_context_ms: Math.round(enqueuedContextTimeMs * 100) / 100,
          operation_id: operationId,
          operation_owner_missing: !operationId,
        });
        return true;
      }
      const buffer = context.createBuffer(frame.channels, frames, frame.sampleRate);
      const pcm = new DataView(frame.data.buffer, frame.data.byteOffset, frame.data.byteLength);
      for (let channel = 0; channel < frame.channels; channel += 1) {
        const samples = typeof buffer.getChannelData === "function"
          ? buffer.getChannelData(channel)
          : new Float32Array(frames);
        for (let index = 0; index < frames; index += 1) {
          samples[index] = pcm.getInt16((index * frame.channels + channel) * 2, true) / 32768;
        }
        if (typeof buffer.getChannelData !== "function") buffer.copyToChannel(samples, channel);
      }
      const source = context.createBufferSource();
      source.buffer = buffer;
      source.connect(context.destination);
      sources.add(source);
      source.addEventListener("ended", () => sources.delete(source), { once: true });
      if (scheduledAudio && nextStart < context.currentTime) {
        rendererMetrics.underruns += 1;
        onUnderrun?.((context.currentTime - nextStart) * 1000);
      }
      nextStart = Math.max(nextStart, context.currentTime + 0.035);
      source.start(nextStart);
      nextStart += buffer.duration;
      scheduledAudio = true;
      const scheduleUs = Math.round((rendererNow() - scheduleStarted) * 1000);
      rendererMetrics.packets += 1;
      rendererMetrics.frames += frames;
      rendererMetrics.schedule_cpu_us += scheduleUs;
      rendererMetrics.schedule_max_us = Math.max(rendererMetrics.schedule_max_us, scheduleUs);
      return true;
    };

    const reportRendererMetrics = (reason) => {
      if (metricsReported) return;
      metricsReported = true;
      const workletMetricsStatus = !rendererMetrics.worklet
        ? "not_applicable"
        : rendererMetrics.worklet_final_metrics_received ? "complete" : "unreported";
      const finalOnlyMetrics = [
        "worklet_render_quanta", "worklet_max_buffered_frames", "underruns",
        "worklet_recoveries", "worklet_rebuffered_frames",
        "worklet_min_buffered_frames", "worklet_first_receive_context_frame",
        "worklet_pcm_signal_frames", "worklet_pcm_signal_rms",
        "worklet_pcm_signal_peak", "worklet_pcm_signal_min_quantum_rms",
        "worklet_pcm_signal_max_quantum_rms", "worklet_pcm_low_energy_threshold_rms",
        "worklet_pcm_low_energy_frames", "worklet_pcm_low_energy_quanta",
        "worklet_pcm_max_low_energy_frames",
      ];
      const metrics = { ...rendererMetrics, reason, worklet_metrics_status: workletMetricsStatus };
      if (rendererMetrics.worklet) {
        metrics.worklet_final_metrics_received = rendererMetrics.worklet_final_metrics_received;
        metrics.worklet_underrun_events_observed = rendererMetrics.underruns;
        if (!rendererMetrics.worklet_final_metrics_received) {
          for (const key of finalOnlyMetrics) metrics[key] = null;
        }
      }
      // Queue depth and first-render frame are main-thread observations and
      // remain available when stopped playback never reports its final drain.
      onRendererMetrics?.(metrics);
    };

    const schedulePending = () => {
      if (stopped) return;
      while (chunkHead < chunks.length) {
        const queued = chunks[chunkHead];
        const { frame, operationId } = queued;
        const seconds = frame.data.byteLength / (2 * frame.channels * frame.sampleRate);
        if (!schedule(frame, operationId)) break;
        chunkHead += 1;
        pendingSeconds -= seconds;
      }
      // Avoid both Array.shift()'s repeated compaction and retaining every
      // already scheduled IPC packet through a long call.
      if (chunkHead === chunks.length) {
        chunks.length = 0;
        chunkHead = 0;
        pendingSeconds = 0;
      }
      armEnd();
    };

    const bufferedSeconds = () => pendingSeconds;
    const bufferedPlaybackSeconds = () => {
      let seconds = pendingSeconds;
      if (format && workletNode) {
        seconds += workletQueuedFrames / format.sampleRate;
      } else if (context && scheduledAudio) {
        seconds += Math.max(0, nextStart - context.currentTime);
      }
      return seconds;
    };

    const notifyPlayable = () => {
      if (playable || !receivedAudio) return;
      if (!ended && bufferedSeconds() < startupBufferSeconds()) return;
      playable = true;
      onPlayable?.();
    };

    // Keep the shared output clock running once playback has started. WebKit
    // may retain device-bound PCM across suspend/resume, so suspending at a
    // turn boundary can cut off a tail and replay it under the next speaker.
    // Only this renderer's graph and ring are retired; the idle graph is silent.
    const disconnectWorklet = () => {
      const shared = workletOwner !== null;
      if (workletNode) {
        workletNode.port.postMessage({ type: "reset" });
        workletNode.port.onmessage = null;
        workletNode.disconnect();
        workletNode = null;
      }
      echoReference?.disconnect();
      echoReference = null;
      echoReferenceSamples = null;
      if (workletOwner && pcmWorkletEngine.owner === workletOwner) {
        pcmWorkletEngine.owner = null;
      }
      workletOwner = null;
      return shared;
    };

    const resumeContext = async (candidate) => {
      let timeout = null;
      try {
        await Promise.race([
          candidate.resume(),
          new Promise((_, reject) => {
            timeout = window.setTimeout(() => reject(new Error(
              "speech_playback_start_timeout: Audio playback did not start.",
            )), PLAYBACK_START_TIMEOUT_MS);
          }),
        ]);
      } finally {
        if (timeout !== null) window.clearTimeout(timeout);
      }
    };

    const startPlayback = async () => {
      if (stopped || playing) return false;
      const isStartCurrent = () => !stopped;
      if (startupDelayTimer !== null) {
        window.clearTimeout(startupDelayTimer);
        startupDelayTimer = null;
      }
      const playbackStartAt = rendererNow();
      const bufferedAtStartMs = Math.round(bufferedSeconds() * 100_000) / 100;
      const startupTargetAtStartMs = Math.round(startupBufferSeconds() * 100_000) / 100;
      const AudioContext = window.AudioContext || window.webkitAudioContext;
      if (typeof AudioContext !== "function") throw new Error("Streaming audio playback is unavailable.");
      // Retaining a copy for replay does not require scheduling the live path
      // on the renderer thread. `pushDecoded` copies history before `schedule`
      // transfers the packet, so voice notes and calls can share the same
      // preallocated audio-thread ring without sacrificing durable playback.
      const cancelledStart = {};
      const preparedEngine = await Promise.race([
        preparePcmWorklet(format.sampleRate),
        startCancellation.then(() => cancelledStart),
      ]);
      if (preparedEngine === cancelledStart || !isStartCurrent()) return false;
      if (preparedEngine && !pcmWorkletEngine.owner) {
        context = preparedEngine.context;
        workletOwner = {};
        pcmWorkletEngine.owner = workletOwner;
        workletCapacityFrames = format.sampleRate * 8;
        workletQueuedFrames = 0;
        workletFinishSent = false;
        workletNode = new AudioWorkletNode(context, "mom-pcm-ring", {
          numberOfInputs: 0,
          numberOfOutputs: 1,
          outputChannelCount: [format.channels],
          processorOptions: {
            channels: format.channels,
            capacityFrames: workletCapacityFrames,
            recoveryFrames: Math.ceil(format.sampleRate / 10),
          },
        });
        rendererMetrics.worklet = true;
        const ownedNode = workletNode;
        const ownedEngine = workletOwner;
        workletNode.port.onmessage = ({ data }) => {
          if (stopped || workletNode !== ownedNode
              || pcmWorkletEngine.owner !== ownedEngine) return;
          const audioContextMs = Number(data?.audioContextFrame || 0) * 1_000 / format.sampleRate;
          if (data?.type === "packet_admitted") {
            const packetIndex = Number(data.packetIndex || 0);
            const operationOwnerKnown = rendererPacketOwners.has(packetIndex);
            const operationId = operationOwnerKnown ? rendererPacketOwners.get(packetIndex) : null;
            rendererPacketOwners.delete(packetIndex);
            onRendererEvent?.({
              type: "packet_admitted",
              packet_index: packetIndex,
              sequence: Number(data.sequence || 0),
              sample_offset: Number(data.sampleOffset || 0),
              frames: Number(data.frames || 0),
              buffered_frames: Number(data.bufferedFrames || 0),
              audio_context_ms: Math.round(audioContextMs * 100) / 100,
              enqueue_to_admission_ms: Math.round(Math.max(
                0,
                audioContextMs - Number(data.enqueuedContextTimeMs || 0),
              ) * 100) / 100,
              operation_id: operationId,
              operation_owner_missing: !operationOwnerKnown || !operationId,
            });
          } else if (data?.type === "render_started") {
            rendererMetrics.worklet_first_render_context_frame = Number(data.audioContextFrame || 0);
            onRendererEvent?.({
              type: "render_started",
              buffered_frames: Number(data.bufferedFrames || 0),
              audio_context_ms: Math.round(audioContextMs * 100) / 100,
            });
          } else if (data?.type === "underrun_started") {
            rendererMetrics.underruns += 1;
            onRendererEvent?.({
              type: "underrun_started",
              buffered_frames: Number(data.bufferedFrames || 0),
              audio_context_ms: Math.round(audioContextMs * 100) / 100,
            });
          } else if (data?.type === "underrun") {
            onUnderrun?.(Number(data.missingFrames || 0) * 1000 / format.sampleRate);
            onRendererEvent?.({
              type: "underrun_recovered",
              missing_frames: Number(data.missingFrames || 0),
              buffered_frames: Number(data.bufferedFrames || 0),
              audio_context_ms: Math.round(audioContextMs * 100) / 100,
            });
          } else if (data?.type === "consumed") {
            workletQueuedFrames = Math.max(0, workletQueuedFrames - Number(data.frames || 0));
            onRendererEvent?.({
              type: "consumed",
              frames: Number(data.frames || 0),
              rendered_frames: Number(data.renderedFrames || 0),
              buffered_frames: Number(data.bufferedFrames || 0),
              queued_frames: workletQueuedFrames,
              audio_context_ms: Math.round(audioContextMs * 100) / 100,
            });
            schedulePending();
            onBufferLevel?.(bufferedPlaybackSeconds() * 1000);
          } else if (data?.type === "error") {
            onPlaybackError?.(new Error(`speech_stream_worklet_failed: ${data.message || "PCM output failed."}`));
          } else if (data?.type === "drained" && !stopped) {
            rendererMetrics.worklet_final_metrics_received = true;
            rendererMetrics.worklet_pcm_signal_frames = Number.isFinite(data.pcmSignalFrames)
              ? Number(data.pcmSignalFrames) : null;
            rendererMetrics.worklet_pcm_signal_rms = Number.isFinite(data.pcmSignalRms)
              ? Number(data.pcmSignalRms) : null;
            rendererMetrics.worklet_pcm_signal_peak = Number.isFinite(data.pcmSignalPeak)
              ? Number(data.pcmSignalPeak) : null;
            rendererMetrics.worklet_pcm_signal_min_quantum_rms = Number.isFinite(data.pcmSignalMinQuantumRms)
              ? Number(data.pcmSignalMinQuantumRms) : null;
            rendererMetrics.worklet_pcm_signal_max_quantum_rms = Number.isFinite(data.pcmSignalMaxQuantumRms)
              ? Number(data.pcmSignalMaxQuantumRms) : null;
            rendererMetrics.worklet_pcm_low_energy_threshold_rms = Number.isFinite(data.pcmLowEnergyThresholdRms)
              ? Number(data.pcmLowEnergyThresholdRms) : null;
            rendererMetrics.worklet_pcm_low_energy_frames = Number.isFinite(data.pcmLowEnergyFrames)
              ? Number(data.pcmLowEnergyFrames) : null;
            rendererMetrics.worklet_pcm_low_energy_quanta = Number.isFinite(data.pcmLowEnergyQuanta)
              ? Number(data.pcmLowEnergyQuanta) : null;
            rendererMetrics.worklet_pcm_max_low_energy_frames = Number.isFinite(data.pcmMaxLowEnergyFrames)
              ? Number(data.pcmMaxLowEnergyFrames) : null;
            rendererMetrics.worklet_render_quanta = Number.isFinite(data.renderQuanta)
              ? Number(data.renderQuanta) : null;
            rendererMetrics.worklet_max_buffered_frames = Number.isFinite(data.maxBufferedFrames)
              ? Number(data.maxBufferedFrames) : null;
            rendererMetrics.underruns = Number.isFinite(data.underruns)
              ? Number(data.underruns) : null;
            rendererMetrics.worklet_recoveries = Number.isFinite(data.recoveries)
              ? Number(data.recoveries) : null;
            rendererMetrics.worklet_rebuffered_frames = Number.isFinite(data.rebufferedFrames)
              ? Number(data.rebufferedFrames) : null;
            rendererMetrics.worklet_min_buffered_frames = Number.isFinite(data.minBufferedFrames)
              ? Number(data.minBufferedFrames) : null;
            rendererMetrics.worklet_first_receive_context_frame = Number.isFinite(data.firstReceiveContextFrame)
              ? Number(data.firstReceiveContextFrame) : null;
            rendererMetrics.worklet_first_render_context_frame = Number.isFinite(data.firstRenderContextFrame)
              ? Number(data.firstRenderContextFrame) : rendererMetrics.worklet_first_render_context_frame;
            playing = false;
            disconnectWorklet();
            context = null;
            reportRendererMetrics("ended");
            onEnded?.();
          }
        };
        echoReference = context.createAnalyser();
        echoReference.fftSize = 8192;
        echoReferenceSamples = new Float32Array(echoReference.fftSize);
        workletNode.connect(echoReference);
        echoReference.connect(context.destination);
      } else {
        context = new AudioContext({ latencyHint: "interactive", sampleRate: format.sampleRate });
      }
      const startContext = context;
      const startOwner = workletOwner;
      playing = true;
      nextStart = context.currentTime + 0.035;
      schedulePending();
      const resumeStarted = rendererNow();
      try {
        const resume = resumeContext(startContext);
        const resumeResult = await Promise.race([
          resume.then(() => "resumed"),
          startCancellation.then(() => "cancelled"),
        ]);
        if (resumeResult === "cancelled" || !isStartCurrent()) {
          // A retired renderer has no authority to pause the shared clock.
          // Its graph is disconnected by stop(), including for a late resume.
          if (!startOwner) {
            void resume.then(() => startContext.close()).catch(() => {});
          }
          return false;
        }
      } catch (error) {
        playing = false;
        sources.forEach((source) => { try { source.stop(); } catch (_) {} });
        sources.clear();
        const failedContext = context;
        const shared = disconnectWorklet();
        context = null;
        if (!shared && failedContext) await failedContext.close().catch(() => {});
        throw error;
      }
      onPlaybackStarted?.({
        renderer: workletNode ? "audio_worklet_ring" : "audio_buffer_source",
        buffered_ms: bufferedAtStartMs,
        resume_ms: Math.round((rendererNow() - resumeStarted) * 100) / 100,
        total_start_ms: Math.round((rendererNow() - playbackStartAt) * 100) / 100,
        startup_target_ms: startupTargetAtStartMs,
        startup_delay_target_ms: startupDelayMilliseconds(),
        first_pcm_wait_ms: firstChunkAt === null
          ? 0 : Math.round((rendererNow() - firstChunkAt) * 100) / 100,
      });
      return true;
    };

    const maybeStartPlayback = () => {
      if (stopped || playing || starting || !playWhenReady || chunks.length === 0) return starting;
      if (!ended && (typeof waitForCompletion === "function" ? waitForCompletion() : waitForCompletion)) return null;
      if (!ended && bufferedSeconds() < startupBufferSeconds()) return null;
      const delayRemainingMs = firstChunkAt === null
        ? 0 : startupDelayMilliseconds() - (rendererNow() - firstChunkAt);
      if (!ended && delayRemainingMs > 0) {
        if (startupDelayTimer === null) {
          startupDelayTimer = window.setTimeout(() => {
            startupDelayTimer = null;
            void maybeStartPlayback();
          }, delayRemainingMs);
        }
        return null;
      }
      starting = startPlayback().catch((error) => {
        onPlaybackError?.(error);
        return false;
      }).finally(() => { starting = null; });
      return starting;
    };

    const play = async () => {
      if (stopped || playing) return false;
      if (workletNode && context) {
        playing = true;
        playWhenReady = true;
        workletNode.port.postMessage({ type: "resume" });
        schedulePending();
        onPlaybackStarted?.({
          renderer: "audio_worklet_ring",
          buffered_ms: Math.round(bufferedSeconds() * 100_000) / 100,
          resume_ms: 0,
          total_start_ms: 0,
          resumed: true,
        });
        armEnd();
        return true;
      }
      if (context?.state === "suspended") {
        playing = true;
        playWhenReady = true;
        const resumedContext = context;
        try {
          const resume = resumeContext(resumedContext);
          const resumeResult = await Promise.race([
            resume.then(() => true),
            startCancellation.then(() => false),
          ]);
          if (!resumeResult || stopped || !playing || context !== resumedContext) {
            playing = false;
            void resume.then(async () => {
              await resumedContext.close().catch(() => {});
            }).catch(() => {});
            return false;
          }
          schedulePending();
          onPlaybackStarted?.({
            renderer: workletNode ? "audio_worklet_ring" : "audio_buffer_source",
            buffered_ms: Math.round(bufferedSeconds() * 100_000) / 100,
            resume_ms: 0,
            total_start_ms: 0,
            resumed: true,
          });
          armEnd();
          return true;
        } catch (error) {
          playing = false;
          throw error;
        }
      }
      if (ended && chunks.length === 0 && history.length) {
        // AudioWorklet transfer detaches the scheduled packet's ArrayBuffer.
        // History is the durable replay source, so each replay gets its own
        // transferable copy and the retained frames remain intact forever.
        chunks.push(...history.map((frame) => ({
          frame: { ...frame, data: frame.data.slice() },
          operationId: null,
        })));
        pendingSeconds = history.reduce((seconds, frame) => (
          seconds + (frame.data.byteLength / (2 * frame.channels * frame.sampleRate))
        ), 0);
      }
      playWhenReady = true;
      return maybeStartPlayback() || false;
    };

    const pause = async () => {
      if (stopped || !playing || !context) return false;
      playing = false;
      playWhenReady = false;
      endGeneration += 1;
      if (workletNode) workletNode.port.postMessage({ type: "pause" });
      else await context.suspend();
      onPlaybackPaused?.();
      return true;
    };

    const pushDecoded = (frame, sequenceKey, honorEnd) => {
      if (stopped) return;
      const nextSequence = sequenceKey === null
        ? expectedSequence
        : (segmentSequences.get(sequenceKey) || 0);
      if (frame.sequence !== nextSequence) {
        throw new Error("speech_stream_sequence_invalid: PCM packets were not contiguous at IPC.");
      }
      const nextSampleOffset = sequenceKey === null
        ? expectedSampleOffset
        : (segmentSampleOffsets.get(sequenceKey) || 0);
      if (frame.sampleOffset !== nextSampleOffset) {
        throw new Error("speech_stream_offset_invalid: PCM packets overlapped or skipped samples at IPC.");
      }
      const frames = validateFrame(frame);
      if (sequenceKey === null) expectedSequence += 1;
      else segmentSequences.set(sequenceKey, nextSequence + 1);
      if (sequenceKey === null) expectedSampleOffset += frames;
      else segmentSampleOffsets.set(sequenceKey, nextSampleOffset + frames);
      const nextFormat = {
        sampleRate: frame.sampleRate,
        channels: frame.channels,
        sampleFormat: frame.sampleFormat,
        interleaved: frame.interleaved,
      };
      if (format && JSON.stringify(format) !== JSON.stringify(nextFormat)) {
        throw new Error("speech_stream_format_changed: PCM format changed at IPC.");
      }
      format ||= nextFormat;
      if (frame.data.byteLength) {
        if (retainHistory) {
          history.push({ ...frame, data: frame.data.slice() });
          rendererMetrics.retained_bytes += frame.data.byteLength;
        }
        chunks.push({ frame, operationId: typeof sequenceKey === "string" ? sequenceKey : null });
        pendingSeconds += frame.data.byteLength / (2 * frame.channels * frame.sampleRate);
        if (!receivedAudio) {
          receivedAudio = true;
          firstChunkAt = rendererNow();
          onFirstChunk?.({
            bytes: frame.data.byteLength,
            frames,
            sampleRate: frame.sampleRate,
            channels: frame.channels,
          });
        }
        if (playing) schedulePending();
        onBufferLevel?.(bufferedPlaybackSeconds() * 1000);
      }
      if (honorEnd) ended ||= frame.end;
      notifyPlayable();
      void maybeStartPlayback();
      armEnd();
    };

    const push = (value) => pushDecoded(decodePcmFrame(value), null, true);
    const pushSegment = (value, segment) => pushDecoded(decodePcmFrame(value), segment, false);
    const finish = () => {
      ended = true;
      notifyPlayable();
      void maybeStartPlayback();
      armEnd();
    };

    const stop = async () => {
      stopped = true;
      startCancellationResolve?.();
      startCancellationResolve = null;
      playing = false;
      if (startupDelayTimer !== null) {
        window.clearTimeout(startupDelayTimer);
        startupDelayTimer = null;
      }
      endGeneration += 1;
      sources.forEach((source) => { try { source.stop(); } catch (_) {} });
      sources.clear();
      const stoppedContext = context;
      const shared = disconnectWorklet();
      context = null;
      if (!shared && stoppedContext) await stoppedContext.close().catch(() => {});
      rendererPacketOwners.clear();
      reportRendererMetrics("stopped");
    };

    const wavBytes = () => {
      if (!retainHistory || !format || !history.length) return null;
      const dataBytes = history.reduce((total, frame) => total + frame.data.byteLength, 0);
      if (!dataBytes) return null;
      const buffer = new ArrayBuffer(44 + dataBytes);
      const view = new DataView(buffer);
      const writeText = (offset, value) => [...value].forEach((character, index) => (
        view.setUint8(offset + index, character.charCodeAt(0))
      ));
      const bytesPerFrame = format.channels * 2;
      writeText(0, "RIFF"); view.setUint32(4, 36 + dataBytes, true); writeText(8, "WAVE");
      writeText(12, "fmt "); view.setUint32(16, 16, true); view.setUint16(20, 1, true);
      view.setUint16(22, format.channels, true); view.setUint32(24, format.sampleRate, true);
      view.setUint32(28, format.sampleRate * bytesPerFrame, true);
      view.setUint16(32, bytesPerFrame, true); view.setUint16(34, 16, true);
      writeText(36, "data"); view.setUint32(40, dataBytes, true);
      const output = new Uint8Array(buffer);
      let offset = 44;
      history.forEach((frame) => {
        output.set(frame.data, offset);
        offset += frame.data.byteLength;
      });
      return output;
    };

    return {
      push,
      pushSegment,
      finish,
      play,
      pause,
      stop,
      wavBytes,
      reconsiderStartup: maybeStartPlayback,
      echoReference() {
        if (!playing || !echoReference || !echoReferenceSamples || !context) return null;
        echoReference.getFloatTimeDomainData(echoReferenceSamples);
        return { samples: echoReferenceSamples, sampleRate: context.sampleRate };
      },
      get playing() { return playing; },
      get starting() { return starting !== null; },
      get ready() { return playable; },
      bufferedMs: () => Math.round(bufferedPlaybackSeconds() * 100_000) / 100,
      metrics: () => ({ ...rendererMetrics }),
    };
  };

  const stopSpeechPlayback = async ({ notifyNative = true } = {}) => {
    // Revoke pending byte verification and publication as well as live media.
    speechPlaybackEpoch = {};
    const playback = activeSpeechPlayback;
    activeSpeechPlayback = null;
    if (!playback) return;
    if (playback.controller) await playback.controller.stop();
    if (playback.audio) {
      playback.audio.pause();
      playback.audio.removeAttribute("src");
      playback.audio.load();
    }
    if (playback.url) URL.revokeObjectURL(playback.url);
    playback.panel.remove();
    restoreReadAloudButton(playback.button);
    if (notifyNative) {
      const target = playback.id ? { playback: playback.id } : { operation: playback.operation };
      await invoke("mom_llama_speech_stop", target).catch(reportError);
    }
  };

  const playCallReply = async (conversation, message) => {
    const operation = speechOperationToken();
    let retainedPlayback = null;
    const panel = document.createElement("section");
    const button = createCommandElement("button", DYNAMIC_CONTROL_SPECS.readAloudStop);
    let controller = null;
    let endedBeforeCommit = false;
    try {
      markVoiceLatency("tts_started");
      await stopSpeechPlayback();
      controller = createPcmPlaybackBuffer({
        autoplay: true,
        retainHistory: false,
        onFirstChunk: () => markVoiceLatency("tts_first_pcm"),
        onPlaybackStarted: () => markVoiceLatency("playback_started"),
        onRendererEvent: (event) => markVoiceLatency(`playback_${event.type}`, event),
        onRendererMetrics: (metrics) => markVoiceLatency("playback_renderer", metrics),
        onEnded: () => {
          if (activeSpeechPlayback?.controller !== controller) return;
          if (!activeSpeechPlayback.id) {
            endedBeforeCommit = true;
            void controller.stop().catch(reportError);
            return;
          }
          void stopSpeechPlayback().then(() => (
            voiceCallActive ? beginVoiceCapture({ call: true }) : null
          )).catch(reportError);
        },
      });
      const stream = pcmChannel((frame) => {
        try { controller.push(frame); } catch (error) { reportError(error); }
      });
      activeSpeechPlayback = { id: null, operation, controller, panel, button };
      const response = await invoke("mom_llama_speech_read_aloud", {
        conversation,
        message,
        operation,
        requirePersonaVoice: true,
        pcmStream: stream,
      });
      report(response);
      markVoiceLatency("tts_finished");
      if (response?.status === "blocked" || !response?.result) return false;
      const result = response.result;
      retainedPlayback = result.playback_id;
      if (result.binding?.message_id !== message) {
        await invoke("mom_llama_speech_stop", { playback: result.playback_id }).catch(() => {});
        retainedPlayback = null;
        return false;
      }
      if (!voiceCallActive || activeSpeechPlayback?.controller !== controller) return false;
      activeSpeechPlayback.id = result.playback_id;
      activeSpeechPlayback.operation = null;
      retainedPlayback = null;
      if (endedBeforeCommit) {
        await stopSpeechPlayback();
        if (voiceCallActive) await beginVoiceCapture({ call: true });
      }
      return true;
    } finally {
      if (retainedPlayback) {
        await invoke("mom_llama_speech_stop", { playback: retainedPlayback }).catch(() => {});
      }
      if (activeSpeechPlayback?.controller === controller && !controller?.ready) {
        await stopSpeechPlayback().catch(() => {});
      }
    }
  };

  const appendAttachmentTranscriptionControl = (preview, catalog, artifact) => {
    if (artifact.kind !== "audio" || artifact.media_type !== "audio/wav") return;
    preview.querySelector(".attachment-transcription")?.remove();
    const panel = document.createElement("section");
    panel.className = "attachment-transcription";
    const button = createCommandElement("button", DYNAMIC_CONTROL_SPECS.attachmentTranscribe);
    button.type = "button";
    button.className = "small-button";
    button.dataset.action = "attachment-transcribe";
    button.dataset.attachment = catalog.attachment_id;
    button.dataset.rootSha256 = catalog.root_sha256;
    button.dataset.artifact = artifact.artifact_id;
    button.dataset.policyFingerprint = catalog.policy_fingerprint;
    button.textContent = "Transcribe audio";
    const output = document.createElement("div");
    output.className = "attachment-transcript";
    output.setAttribute("aria-live", "polite");
    panel.append(button, output);
    preview.append(panel);
  };

  const renderAttachmentTranscript = (button, result) => {
    const preview = button.closest("[data-attachment-preview]");
    const output = preview?.querySelector(".attachment-transcript");
    if (!preview?.isConnected || !output) return;
    attachmentTranscripts.set(preview, result);
    const transcript = document.createElement("p");
    transcript.className = "attachment-transcript-text";
    transcript.textContent = result.transcript || "No speech was detected.";
    const provenance = document.createElement("small");
    provenance.className = "attachment-transcript-provenance";
    provenance.textContent = [
      `artifact ${result.provenance.artifact_id}`,
      `blob ${result.provenance.blob_object_id}`,
      `root ${result.provenance.root_sha256}`,
      `policy ${result.provenance.policy_fingerprint}`,
      `model ${result.provenance.model_id}@${result.provenance.model_content_sha256}`,
      "network never",
    ].join(" · ");
    const insert = createCommandElement("button", DYNAMIC_CONTROL_SPECS.draftUpdate);
    insert.type = "button";
    insert.className = "small-button";
    insert.dataset.action = "speech-transcript-insert";
    insert.textContent = "Insert into composer";
    output.replaceChildren(transcript, provenance, insert);
  };

  const attachmentPreviewResult = (response) => {
    if (response?.result) return response.result;
    const blocker = response?.blocker;
    throw new Error(
      blocker ? `${blocker.code}: ${blocker.message}` : "Attachment preview is unavailable.",
    );
  };

  const attachmentPreviewAnchor = (catalog, artifact) => ({
    attachment: catalog.attachment_id,
    rootSha256: catalog.root_sha256,
    artifact: artifact.artifact_id,
    policyFingerprint: catalog.policy_fingerprint,
  });

  const sameAttachmentPreviewAnchor = (actual, expected) => (
    actual?.attachment_id === expected.attachment
    && actual?.root_sha256 === expected.rootSha256
    && actual?.artifact_id === expected.artifact
    && actual?.policy_fingerprint === expected.policyFingerprint
  );

  const appendAttachmentPreviewNotices = (container, notices = []) => {
    if (!notices.length) return;
    const list = document.createElement("ul");
    list.className = "attachment-preview-notices";
    notices.forEach((notice) => {
      const item = document.createElement("li");
      item.textContent = notice.message || notice.code || "Preview is incomplete.";
      list.append(item);
    });
    container.append(list);
  };

  const renderAttachmentPreviewState = (preview, catalog) => {
    const body = preview.querySelector(".attachment-preview-body");
    if (!body) return;
    const state = document.createElement("div");
    state.className = "attachment-preview-state";
    const label = document.createElement("strong");
    label.textContent = catalog.state === "unsupported"
      ? "Preview unsupported"
      : "Metadata only";
    state.append(label);
    appendAttachmentPreviewNotices(state, catalog.notices);
    if (catalog.required_transforms?.length) {
      const transform = document.createElement("p");
      transform.textContent = `Available bounded transform request: ${catalog.required_transforms.join(", ")}. No transform is running.`;
      state.append(transform);
    }
    body.replaceChildren(state);
    preview.classList.add("hydrated-document");
  };

  const renderAttachmentTextPreview = (preview, catalog, content) => {
    const body = preview.querySelector(".attachment-preview-body");
    if (!body) return;
    const documentPreview = document.createElement("div");
    documentPreview.className = "attachment-document-preview";
    documentPreview.dataset.textFormat = content.format;
    content.sections.forEach((section) => {
      const element = document.createElement("section");
      element.className = "attachment-text-section";
      if (section.label || Object.keys(section.coordinates || {}).length) {
        const heading = document.createElement("strong");
        const locator = Object.entries(section.coordinates || {})
          .map(([key, value]) => `${key} ${value}`)
          .join(" · ");
        heading.textContent = [section.label, locator].filter(Boolean).join(" · ");
        element.append(heading);
      }
      const text = document.createElement("pre");
      text.textContent = section.text;
      element.append(text);
      documentPreview.append(element);
    });
    if (!content.sections.length) {
      const empty = document.createElement("p");
      empty.textContent = "The canonical text artifact is empty.";
      documentPreview.append(empty);
    }
    if (content.stats.truncated) {
      const truncated = document.createElement("p");
      truncated.className = "attachment-preview-truncation";
      truncated.textContent = `Preview limit reached: ${content.stats.omitted_bytes} bytes, ${content.stats.omitted_characters} characters, and ${content.stats.omitted_lines} lines omitted.`;
      documentPreview.append(truncated);
    }
    appendAttachmentPreviewNotices(
      documentPreview,
      [...(catalog.notices || []), ...(content.notices || [])],
    );
    const libraryControls = document.createElement("div");
    libraryControls.className = "attachment-library-controls";
    const addToLibrary = createCommandElement(
      "button",
      DYNAMIC_CONTROL_SPECS.attachmentLibraryOpen,
    );
    addToLibrary.type = "button";
    addToLibrary.className = "small-button";
    addToLibrary.dataset.action = "attachment-library-open";
    addToLibrary.dataset.attachment = content.anchor.attachment_id;
    addToLibrary.dataset.rootSha256 = content.anchor.root_sha256;
    addToLibrary.dataset.artifact = content.anchor.artifact_id;
    addToLibrary.dataset.policyFingerprint = content.anchor.policy_fingerprint;
    addToLibrary.dataset.title = preview.querySelector("figcaption strong")?.textContent
      || "Attachment text";
    addToLibrary.textContent = "Add to Library";
    libraryControls.append(addToLibrary);
    documentPreview.append(libraryControls);
    body.replaceChildren(documentPreview);
    preview.classList.add("hydrated-document");
  };

  const loadAttachmentPreview = async (preview) => {
    if (!preview.isConnected || preview.dataset.previewReleased === "true") return;
    const body = preview.querySelector(".attachment-preview-body");
    if (!body) return;
    preview.dataset.previewHydrated = "loading";
    try {
      const catalog = attachmentPreviewResult(await invoke("mom_llama_attachment_preview", {
        attachment: preview.dataset.attachmentPreview,
      }));
      if (!preview.isConnected || preview.dataset.previewReleased === "true") return;
      if (catalog.attachment_id !== preview.dataset.attachmentPreview) {
        throw new Error("attachment_preview_stale: Preview identity changed during discovery.");
      }
      const primary = catalog.primary;
      if (!primary) {
        renderAttachmentPreviewState(preview, catalog);
        preview.dataset.previewHydrated = "true";
        return;
      }
      const anchor = attachmentPreviewAnchor(catalog, primary);
      if (primary.kind === "text") {
        const content = attachmentPreviewResult(
          await invoke("mom_llama_attachment_preview_content", anchor),
        );
        if (!preview.isConnected || preview.dataset.previewReleased === "true") return;
        if (!sameAttachmentPreviewAnchor(content.anchor, anchor)) {
          throw new Error("attachment_preview_stale: Canonical text identity changed during load.");
        }
        renderAttachmentTextPreview(preview, catalog, content);
        preview.dataset.previewHydrated = "true";
        return;
      }
      if (!["image", "audio", "video"].includes(primary.kind) || !primary.media_type) {
        renderAttachmentPreviewState(preview, catalog);
        preview.dataset.previewHydrated = "true";
        return;
      }
      const response = await invoke("mom_llama_attachment_preview_bytes", anchor);
      if (!preview.isConnected || preview.dataset.previewReleased === "true") return;
      const bytes = ipcBinaryBytes(response, 16 * 1024 * 1024, "Attachment preview");
      const url = URL.createObjectURL(new Blob([bytes], { type: primary.media_type }));
      const media = primary.kind === "image"
        ? document.createElement("img")
        : primary.kind === "audio"
          ? createCommandElement("audio", DYNAMIC_CONTROL_SPECS.attachmentPreview)
          : createCommandElement("video", DYNAMIC_CONTROL_SPECS.attachmentPreview);
      media.src = url;
      if (primary.kind === "image") {
        media.alt = preview.querySelector("figcaption strong")?.textContent || "Local attachment";
      } else if (!preview.classList.contains("voice-message-bubble")) {
        media.controls = true;
        media.autoplay = false;
        media.preload = "metadata";
      }
      if (preview.classList.contains("voice-message-bubble") && primary.kind === "audio") {
        media.preload = "metadata";
        body.replaceChildren(compactAudioPlayer(media));
      } else {
        body.replaceChildren(media);
        appendAttachmentPreviewNotices(body, catalog.notices);
        appendAttachmentTranscriptionControl(preview, catalog, primary);
      }
      attachmentPreviewMedia.set(preview, media);
      attachmentObjectUrls.set(media, { preview, url });
      preview.dataset.previewHydrated = "true";
      enforceAttachmentPreviewLimit();
    } catch (error) {
      if (!preview.isConnected || preview.dataset.previewReleased === "true") return;
      preview.dataset.previewHydrated = "blocked";
      preview.title = errorMessage(error);
      if (preview.classList.contains("voice-message-bubble")) {
        preview.querySelector(".attachment-preview-body")?.replaceChildren(unavailableVoiceMessage());
      }
      attachmentPreviewObserver?.unobserve(preview);
    }
  };

  const ensureAttachmentPreviewLoaded = (preview) => {
    if (!preview?.isConnected || preview.dataset.previewReleased === "true") {
      return Promise.resolve();
    }
    if (preview.dataset.previewHydrated === "true") return Promise.resolve();
    const current = attachmentPreviewPromises.get(preview);
    if (current) return current;
    const pending = loadAttachmentPreview(preview).finally(() => {
      if (attachmentPreviewPromises.get(preview) === pending) {
        attachmentPreviewPromises.delete(preview);
      }
    });
    attachmentPreviewPromises.set(preview, pending);
    return pending;
  };

  const drainAttachmentPreviewQueue = () => {
    while (
      attachmentPreviewLoads < ATTACHMENT_PREVIEW_CONCURRENCY
      && attachmentPreviewQueue.length
    ) {
      const preview = attachmentPreviewQueue.shift();
      if (!preview?.isConnected || preview.dataset.previewReleased === "true") continue;
      attachmentPreviewLoads += 1;
      ensureAttachmentPreviewLoaded(preview)
        .catch(reportError)
        .finally(() => {
          attachmentPreviewLoads -= 1;
          drainAttachmentPreviewQueue();
        });
    }
  };

  const queueAttachmentPreview = (preview) => {
    const state = preview.dataset.previewHydrated;
    if (["queued", "loading", "true", "blocked", "metadata"].includes(state)) return;
    preview.dataset.previewHydrated = "queued";
    attachmentPreviewQueue.push(preview);
    drainAttachmentPreviewQueue();
  };

  if ("IntersectionObserver" in window) {
    attachmentPreviewObserver = new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (!entry.isIntersecting) return;
        const preview = entry.target;
        const media = attachmentPreviewMedia.get(preview);
        if (media && attachmentObjectUrls.has(media)) {
          touchAttachmentPreview(media);
          return;
        }
        queueAttachmentPreview(preview);
      });
    }, { rootMargin: "320px 0px" });
  }

  const hydrateAttachmentPreviews = async (root = document) => {
    const previews = attachmentPreviewsWithin(root);
    previews.forEach((preview) => {
      delete preview.dataset.previewReleased;
      rememberAttachmentPreviewFallback(preview);
      if (preview.classList.contains("voice-message-bubble")) {
        const body = preview.querySelector(".attachment-preview-body");
        if (body && !body.querySelector(".voice-message-player")) {
          body.replaceChildren(pendingVoiceMessage());
        }
      }
    });
    // The most recent voice note is nearest the composer and is the one the
    // user is most likely to play. Admit previews newest-first so a long
    // history cannot strand that control in its disabled loading shell.
    previews.reverse().forEach((preview) => {
      if (attachmentPreviewObserver) {
        attachmentPreviewObserver.observe(preview);
      } else {
        queueAttachmentPreview(preview);
      }
    });
    const userRows = Array.from(root?.querySelectorAll?.(".message-row.user") || []);
    userRows.forEach((row) => {
      const next = row.nextElementSibling;
      if (row.classList.contains("call-turn")) next?.classList.add("call-turn");
      if (!row.querySelector(".voice-message-bubble") || !next?.classList.contains("assistant")) return;
      const hasSavedVoiceAttachment = Boolean(
        next.querySelector(".message-attachments [data-attachment-preview]"),
      );
      // Saved audio and native unavailable markers are authoritative; only
      // the exact live result may remain hidden while its owner publishes.
      if (hasSavedVoiceAttachment) return;
      const preparingLiveReply = voiceReplyConversation === selectedConversation()
        && voiceReplyMessageId
        && next.dataset.messageId === voiceReplyMessageId;
      if (preparingLiveReply) {
        next.classList.add("voice-reply-pending");
        next.hidden = true;
        return;
      } else {
        if (next.classList.contains("voice-message-row")) return;
        const button = next.querySelector("[data-action='message-read-aloud']");
        if (!button) return;
        // A preceding user voice attachment proves how the user spoke; it does
        // not prove that the assistant reply has retained audio. Historical
        // text therefore keeps its ordinary read-aloud action. A voice-message
        // play control is published only by the live PCM controller or by an
        // actual assistant audio attachment.
        next.classList.remove("voice-reply-pending", "voice-reply-row");
        button.classList.remove("voice-reply-play", "is-loading", "is-awaiting-audio");
        button.removeAttribute("aria-pressed");
        next.hidden = false;
      }
    });
    const conversation = selectedConversation();
    for (const row of root?.querySelectorAll?.(".message-row.assistant") || []) {
      const message = row.dataset.messageId;
      if (!message) continue;
      const playback = failedVoiceReplyOwners.get(voiceReplyPersistenceKey(conversation, message));
      if (!playback || row.querySelector(".message-attachments [data-attachment-preview]")) continue;
      const card = row.querySelector(".message-card");
      if (!card) continue;
      row.classList.add("voice-message-row", "voice-reply-row", "voice-reply-has-audio");
      row.hidden = false;
      const player = pendingVoiceReplyPlayer();
      card.replaceChildren(player);
      const button = player.querySelector(".voice-reply-play");
      if (!button || !playback.bindButton(button)) continue;
      button.dataset.audioPersistence = "failed";
      button.title = "Audio is available in this session; retry saving";
      settleInMemoryVoiceReply(button, playback.controller, playback.trace);
    }
  };

  window.addEventListener("beforeunload", () => {
    void stopSpeechPlayback({ notifyNative: true });
    attachmentPreviewObserver?.disconnect();
    releaseAttachmentObjectUrls(document);
  });

  const captureChatViewport = () => {
    const currentChat = chat();
    const stream = currentChat?.querySelector(".message-stream");
    if (!currentChat || !stream) return null;
    const distanceFromTail = stream.scrollHeight - stream.scrollTop - stream.clientHeight;
    const streamTop = stream.getBoundingClientRect().top;
    const anchor = [...stream.querySelectorAll(".message-row[data-message-id]")]
      .find((row) => row.getBoundingClientRect().bottom > streamTop + 1);
    return {
      conversation: currentChat.dataset.currentConversation || "",
      followTail: stream.dataset.followTail === "false" ? false : distanceFromTail <= 96,
      scrollTop: stream.scrollTop,
      anchorId: anchor?.dataset.messageId || null,
      anchorOffset: anchor ? anchor.getBoundingClientRect().top - streamTop : 0,
    };
  };

  const restoreChatViewport = (state, replacement) => {
    const stream = replacement?.querySelector(".message-stream");
    if (!stream) return;
    const changedConversation = !state
      || state.conversation !== (replacement.dataset.currentConversation || "");
    if (changedConversation || state.followTail) {
      stream.scrollTop = stream.scrollHeight;
      return;
    }
    const anchor = state.anchorId
      ? stream.querySelector(`[data-message-id="${CSS.escape(state.anchorId)}"]`)
      : null;
    if (anchor) {
      const streamTop = stream.getBoundingClientRect().top;
      stream.scrollTop += anchor.getBoundingClientRect().top - streamTop - state.anchorOffset;
    } else {
      stream.scrollTop = Math.min(state.scrollTop, stream.scrollHeight - stream.clientHeight);
    }
  };

  const refreshChat = async () => {
    cancelComposerAutocomplete({ native: true, announce: false });
    const viewport = captureChatViewport();
    const replacement = await swap("#chat", "mom_llama_render_chat_fragment");
    if (replacement) {
      // Hydration can issue attachment IPC and walk a large message tree. Let
      // the new local conversation reach a completed paint before doing that
      // background work, especially on machines without IntersectionObserver.
      requestAnimationFrame(() => requestAnimationFrame(() => {
        void hydrateAttachmentPreviews(replacement).catch(reportError);
      }));
    }
    renderChatBusyState();
    restoreChatViewport(viewport, replacement);
    return replacement;
  };
  const collapsedSidebarSections = new Set();
  const applySidebarSectionState = (root = document) => {
    root?.querySelectorAll?.("[data-action='sidebar-section-toggle']").forEach((button) => {
      const section = button.dataset.sidebarSection;
      const list = document.getElementById(button.getAttribute("aria-controls"));
      if (!section || !list) return;
      const expanded = !collapsedSidebarSections.has(section);
      button.setAttribute("aria-expanded", String(expanded));
      button.setAttribute(
        "aria-label",
        `${expanded ? "Collapse" : "Expand"} ${button.dataset.sidebarLabel || section}`,
      );
      list.hidden = !expanded;
    });
  };
  const refreshSidebar = async () => {
    const replacement = await swap(".sidebar", "mom_llama_render_sidebar_fragment");
    rebindConversationRename(replacement);
    applySidebarSectionState(replacement);
    const separator = replacement?.querySelector(".sidebar-resizer");
    separator?.setAttribute("aria-valuenow", String(Math.round(replacement.getBoundingClientRect().width)));
    return replacement;
  };
  const refreshSettings = async (section = "general") => {
    const wasOpen = !document.getElementById("settings-sidebar")?.hidden;
    const modal = await swap("#settings-sidebar", "mom_llama_render_settings_fragment");
    refreshAutosaveStatus();
    if (wasOpen && modal) {
      modal.hidden = false;
      modal.classList.remove("is-hidden");
      modal.setAttribute("aria-hidden", "false");
      switchSettingsSection(section);
    }
    return modal;
  };

  const refreshConversationSettingsProjection = async () => {
    const settings = document.getElementById("settings-sidebar");
    if (settings && settings.dataset.currentConversation !== selectedConversation()) {
      const projection = parseFragment(await invokeMarkup("mom_llama_render_settings_fragment"));
      const instructions = projection?.querySelector(".current-chat-settings");
      if (instructions && settings === document.getElementById("settings-sidebar")
        && projection.dataset.currentConversation === selectedConversation()) {
        settings.querySelector(".current-chat-settings")?.replaceWith(instructions);
        settings.dataset.currentConversation = projection.dataset.currentConversation;
      }
    }
  };

  const refreshConversationProjection = async () => {
    await Promise.all([refreshChat(), refreshSidebar()]);
    await refreshConversationSettingsProjection();
  };

  let conversationSwitchTimingId = 0;
  const recordConversationSwitchPaint = (startedAt, paintedAt, timing = performance) => {
    const id = ++conversationSwitchTimingId;
    timing.measure(`mom.conversation.switch-to-paint.${id}`, {
      start: startedAt,
      end: paintedAt,
    });
    return paintedAt - startedAt;
  };

  const formField = (form, name) =>
    form?.elements?.namedItem(name) || form?.querySelector(`[name="${CSS.escape(name)}"]`);
  const formValue = (form, name) => {
    const field = formField(form, name);
    return field && typeof field.value === "string" ? field.value.trim() : "";
  };
  const numberOrNull = (value) => (value === "" ? null : Number(value));
  const jsonField = (form, name) => {
    const raw = formValue(form, name) || "{}";
    try { return JSON.parse(raw); }
    catch { throw new Error(`${name.replaceAll("_", " ")} must be valid JSON.`); }
  };
  const pickFile = async (kind) => {
    const result = await invoke("mom_llama_pick_file", { kind });
    if (result?.status === "blocked") {
      report(result);
      return "";
    }
    return result?.result?.path || "";
  };

  const collectUpstreamSettings = (form) => {
    const values = {};
    if (!form) return values;
    form.querySelectorAll("[data-setting-key]").forEach((field) => {
      const key = field.dataset.settingKey;
      if (!key) return;
      if (field.dataset.settingType === "boolean") values[key] = Boolean(field.checked);
      else if (field.dataset.settingType === "number") values[key] = numberOrNull(field.value.trim());
      else values[key] = field.value;
    });
    return values;
  };

  const autosaveQueues = new Map();
  let autosaveStatusRevision = 0;
  let autosaveSettleTimer = null;

  const setAutosaveStatus = (state, message) => {
    window.clearTimeout(autosaveSettleTimer);
    const autosave = document.querySelector(".settings-autosave");
    const status = document.getElementById("settings-save-status");
    const retry = document.querySelector(".settings-retry");
    if (autosave) autosave.dataset.state = state;
    if (status) status.textContent = message;
    retry?.classList.toggle("is-hidden", state !== "error");
    if (state === "saved") {
      const revision = autosaveStatusRevision;
      autosaveSettleTimer = window.setTimeout(() => {
        if (revision === autosaveStatusRevision && autosave) autosave.dataset.state = "idle";
      }, 1400);
    }
  };

  const refreshAutosaveStatus = () => {
    autosaveStatusRevision += 1;
    const queues = [...autosaveQueues.values()];
    if (queues.some((queue) => queue.failure)) {
      setAutosaveStatus("error", "Couldn’t save changes");
      return;
    }
    if (queues.some((queue) => queue.running || queue.pending)) {
      setAutosaveStatus("saving", "Saving…");
      return;
    }
    if (queues.some((queue) => queue.latestRevision > 0)) {
      setAutosaveStatus("saved", "Saved");
    }
  };

  const runAutosaveQueue = async (key) => {
    const queue = autosaveQueues.get(key);
    if (!queue || queue.running || !queue.pending) return;
    queue.running = true;
    const job = queue.pending;
    queue.pending = null;
    refreshAutosaveStatus();
    try {
      const result = await job.run();
      if (result?.status === "blocked") {
        throw new Error(result?.blocker?.message || "That change could not be saved.");
      }
      job.after?.(result);
      if (job.revision === queue.latestRevision) {
        queue.failure = null;
      }
    } catch (error) {
      if (job.revision === queue.latestRevision) {
        queue.failure = { key, job };
        reportError(error);
      }
    } finally {
      queue.running = false;
      refreshAutosaveStatus();
      if (queue.pending) void runAutosaveQueue(key);
    }
  };

  const queueAutosave = (key, job, delay = 650) => {
    const queue = autosaveQueues.get(key) || {
      timer: null,
      running: false,
      pending: null,
      latestRevision: 0,
      failure: null,
    };
    autosaveQueues.set(key, queue);
    window.clearTimeout(queue.timer);
    const revision = ++queue.latestRevision;
    queue.pending = { ...job, revision };
    refreshAutosaveStatus();
    queue.timer = window.setTimeout(() => void runAutosaveQueue(key), delay);
  };

  const settingsUpdatePayload = (form) => ({
    // Model identity is owned by the explicit model-select command. Omitting
    // both paths here prevents an unrelated autosave from clearing its pair.
    device: formValue(form, "native_device") || null,
    contextTokens: numberOrNull(formValue(form, "context_tokens")),
    batchTokens: numberOrNull(formValue(form, "batch_tokens")),
    maxParallelSequences: numberOrNull(formValue(form, "max_parallel_sequences")),
    memoryBudgetMib: numberOrNull(formValue(form, "memory_budget_mib")),
    temperature: numberOrNull(formValue(form, "temperature")),
    topP: numberOrNull(formValue(form, "top_p")),
    maxTokens: numberOrNull(formValue(form, "max_tokens")),
    upstreamSettings: collectUpstreamSettings(form),
  });

  const scheduleSettingsAutosave = (delay = 650) => {
    const form = document.getElementById("settings-form");
    if (!form) return;
    const input = settingsUpdatePayload(form);
    applyTheme(input.upstreamSettings.theme);
    applyCustomCss(input.upstreamSettings.customCss);
    queueAutosave("settings", {
      run: () => invoke("mom_llama_settings_update", { input }),
    }, delay);
  };

  const scheduleChatInstructionsAutosave = (field, delay = 650) => {
    const modal = field.closest("#settings-sidebar");
    const conversation = field.dataset.conversation || modal?.dataset.currentConversation || "default";
    const systemMessage = field.value.trim() || null;
    queueAutosave(`conversation:${conversation}`, {
      run: () => invoke("mom_llama_conversation_system_message_update", {
        conversation,
        systemMessage,
      }),
    }, delay);
  };

  const openSettings = (section = "general") => {
    const modal = document.getElementById("settings-sidebar");
    if (!modal) return;
    modal.hidden = false;
    modal.classList.remove("is-hidden");
    modal.setAttribute("aria-hidden", "false");
    shell()?.classList.add("settings-open");
    document.querySelector('[data-action="settings-open"]')?.setAttribute("aria-expanded", "true");
    switchSettingsSection(section);
    sizeComposer();
    modal.querySelector(".section-tab.active")?.focus();
  };

  const closeSettings = () => {
    const modal = document.getElementById("settings-sidebar");
    if (!modal) return;
    const hadFocus = modal.contains(document.activeElement);
    modal.hidden = true;
    modal.classList.add("is-hidden");
    modal.setAttribute("aria-hidden", "true");
    shell()?.classList.remove("settings-open");
    const toggle = document.querySelector('[data-action="settings-open"]');
    toggle?.setAttribute("aria-expanded", "false");
    if (hadFocus) {
      (toggle || document.querySelector("#chat-form textarea[name='message']"))?.focus();
    }
    sizeComposer();
  };

  const setModalVisibility = (id, visible) => {
    const modal = document.getElementById(id);
    if (!modal) return;
    if (id === "attachment-library-modal" && !visible) clearAttachmentLibraryPreview(modal);
    modal.hidden = !visible;
    modal.classList.toggle("is-hidden", !visible);
    modal.setAttribute("aria-hidden", visible ? "false" : "true");
  };

  const stopCallBed = async () => {
    const bed = voiceCallAudio;
    voiceCallAudio = null;
    if (!bed) return;
    window.clearInterval(bed.ringTimer);
    for (const node of bed.nodes || []) {
      try { node.stop?.(); } catch (_) {}
      try { node.disconnect?.(); } catch (_) {}
    }
    await bed.context?.close?.().catch(() => {});
  };

  const startCallRinging = async (epoch) => {
    await stopCallBed();
    if (voiceCallEpoch !== epoch || !voiceCallActive) return false;
    const context = new AudioContext();
    // Request resume synchronously on the user-gesture path, then let the
    // Calling surface cross a paint boundary before scheduling audible tones.
    const resumeResult = context.resume().then(
      () => ({ ok: true }),
      (error) => ({ error }),
    );
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const resumed = await resumeResult;
    if (resumed.error) {
      await context.close().catch(() => {});
      throw resumed.error;
    }
    if (voiceCallEpoch !== epoch || !voiceCallActive) {
      await context.close().catch(() => {});
      return false;
    }
    const gain = context.createGain();
    gain.gain.value = 0.035;
    gain.connect(context.destination);
    const ring = () => {
      if (voiceCallEpoch !== epoch || !voiceCallActive) return;
      const now = context.currentTime;
      for (const frequency of [440, 480]) {
        const oscillator = context.createOscillator();
        const envelope = context.createGain();
        oscillator.frequency.value = frequency;
        envelope.gain.setValueAtTime(0, now);
        envelope.gain.linearRampToValueAtTime(0.4, now + 0.025);
        envelope.gain.setValueAtTime(0.4, now + 0.34);
        envelope.gain.linearRampToValueAtTime(0, now + 0.42);
        oscillator.connect(envelope).connect(gain);
        oscillator.start(now);
        oscillator.stop(now + 0.44);
      }
    };
    ring();
    voiceCallAudio = { context, ringTimer: window.setInterval(ring, 1250), nodes: [] };
    return true;
  };

  const prepareVoicePersonaForCall = async (recipientId, epoch) => {
    if (!recipientId) throw new Error("The selected contact is unavailable for voice preparation.");
    const isCurrent = () => voiceCallEpoch === epoch && voiceCallActive;
    if (!isCurrent()) return false;
    const started = performance.now();
    markVoiceLatency("call_persona_prepare_started", { persona_id: recipientId });
    let response;
    try {
      response = await invoke("mom_llama_voice_persona_prepare", { personaId: recipientId });
    } catch (error) {
      if (isCurrent()) {
        markVoiceLatency("call_persona_prepare_failed", {
          persona_id: recipientId,
          elapsed_ms: Math.round((performance.now() - started) * 100) / 100,
          message: errorMessage(error),
        });
      }
      throw error;
    }
    if (!isCurrent()) return false;
    if (response?.status === "blocked" || !response?.result?.audio_projector_warmed) {
      const error = new Error(response?.blocker?.message || "The selected contact could not be prepared for voice.");
      markVoiceLatency("call_persona_prepare_failed", {
        persona_id: recipientId,
        elapsed_ms: Math.round((performance.now() - started) * 100) / 100,
        status: response?.status || "missing_result",
        message: error.message,
      });
      throw error;
    }
    markVoiceLatency("call_persona_prepare_completed", {
      persona_id: recipientId,
      elapsed_ms: Math.round((performance.now() - started) * 100) / 100,
      prefix_cache_id: response.result.metadata?.id || null,
      resident_prefix_tokens: Number(response.result.resident_prefix_tokens || 0),
      prefill_us: Number(response.result.prefill_us || 0),
      verification_us: Number(response.result.verification_us || 0),
      persisted: response.result.persisted === true,
    });
    return true;
  };

  const stopSpeechOperationAndWait = async (
    operation,
    reason,
    { allowColdTranscriptionSettlement = false } = {},
  ) => {
    if (!operation) return;
    const started = performance.now();
    markVoiceLatency("speech_preemption_started", { reason });
    try {
      const response = await invoke("mom_llama_speech_stop_and_wait", {
        operation,
        allowColdTranscriptionSettlement,
      });
      if (response?.status === "blocked" || response?.result?.terminal_acknowledged !== true) {
        throw new Error(response?.blocker?.message || "Speech transcription did not confirm that it stopped.");
      }
    } catch (error) {
      // An unacknowledged stop is not evidence of safe future preemption.
      voiceCallTranscriptSlotsDisabled = true;
      throw error;
    }
    markVoiceLatency("speech_preemption_acknowledged", {
      reason,
      elapsed_ms: Math.round((performance.now() - started) * 100) / 100,
      operation,
    });
    return performance.now() - started;
  };

  const startStreamedCallGreeting = async (epoch, conversation) => {
    if (voiceCallEpoch !== epoch || !voiceCallActive) return false;
    const greetingTrace = voiceLatencyTrace;
    callReplyConversation = conversation;
    activeDispatchConversation = conversation;
    let greetingFailure = null;
    const tts = beginLiveVoiceTts({
      conversation,
      mode: "call",
      clientRequest: speechOperationToken(),
      callEpoch: epoch,
      onPlaybackStarted: () => {
        if (voiceCallEpoch === epoch && voiceCallActive) {
          // Ring until the shared reply buffer has actual audio. The call is
          // answered at this boundary, not at generation dispatch.
          // A persistent synthetic line bed is audible to the native
          // microphone and was cancelling the greeting as a false barge-in.
          void stopCallBed().catch(reportError);
          setText("voice-call-title", "Call");
        }
      },
      // The call action owns teardown and error presentation. Closing here
      // invalidates its epoch and used to turn the real synthesis exception
      // into an unexplained hang-up.
      onFailure: (error) => { greetingFailure = error; },
    });
    liveVoiceTts = tts;
    activeDispatchOwner = tts;
    try {
      // The instantiated conversation already owns the selected Persona's
      // system prompt and model profile. Sending directly avoids re-routing
      // the saved recipient through the multi-consultant mention dispatcher,
      // whose differently shaped stream/result previously stranded pickup.
      markVoiceLatencyIn(greetingTrace, "generation_dispatched", { purpose: "call_greeting" });
      const response = await withProgressDeadline(
        tts.trackGeneration(invoke("mom_llama_chat_send", {
          conversation,
          clientRequest: tts.clientRequest,
          message: "[Phone call] [Greeting] You have just picked up a call from Jan. Greet Jan naturally in one short sentence, using wording unlike earlier pickups in this conversation. No headings, lists, or preamble.",
        })),
        30_000,
        () => tts.lastProgressAt,
        "call_reply_timeout: The contact did not finish answering the call.",
      );
      if (voiceCallEpoch !== epoch || !voiceCallActive || liveVoiceTts !== tts) return false;
      if (response?.status === "blocked" || !response?.result) {
        throw new Error(response?.blocker?.message || "The contact could not answer the call.");
      }
      const output = response.result;
      markVoiceLatencyIn(greetingTrace, "generation_finished", {
        purpose: "call_greeting",
        native_duration_ms: output?.duration_ms,
        first_token_ms: output?.first_token_ms,
        prompt_tokens: output?.prompt_tokens,
        completion_tokens: output?.completion_tokens,
        cache_reused: output?.cache_reused === true,
        cache_metrics: output?.cache_metrics || null,
      });
      // Stream events are the low-latency authority. Some successful native
      // turns deliberately omit the duplicated final assistant_text envelope;
      // rejecting those turns leaves a fully generated greeting stranded
      // behind the ringing tone.
      const greeting = output?.assistant_text?.trim() || tts.text.trim();
      if (!greeting) throw new Error("The contact did not answer the call.");
      const synthesized = await tts.finish(greeting);
      // The caller may speak over the greeting as soon as its first packet is
      // audible. That intentionally stops this TTS controller; the listener
      // is already running and the interrupted pickup is still a valid call.
      if (!synthesized && tts.interruptedForSpeech) return true;
      if (!synthesized) {
        throw greetingFailure
          || new Error("The contact answered, but their voice could not be produced.");
      }
      return true;
    } catch (error) {
      // Red hang-up invalidates the epoch before asking native generation to
      // cancel. Its cancellation rejection is therefore an expected terminal
      // acknowledgement, not a user-visible failure.
      if (voiceCallEpoch !== epoch || !voiceCallActive) return false;
      if (error?.code === "voice_generation_timeout") {
        markVoiceLatencyIn(greetingTrace, "generation_timeout", { phase: "call_greeting", timeout_ms: 30_000 });
        void invoke("mom_llama_chat_cancel", { conversation }).catch(() => {});
      }
      throw error;
    } finally {
      // A later call can use the same conversation while this request is
      // settling after hang-up. Conversation identity alone cannot prove that
      // these shared dispatch slots still belong to this greeting.
      if (activeDispatchOwner === tts) {
        activeDispatchOwner = null;
        if (activeDispatchConversation === conversation) activeDispatchConversation = null;
        if (callReplyConversation === conversation) callReplyConversation = null;
      }
    }
  };

  const prepareCallMicrophoneStream = async (epoch) => {
    const isCurrentCall = () => voiceCallActive && voiceCallEpoch === epoch;
    if (!isCurrentCall()) throw Object.assign(new Error("The call was closed during microphone setup."), { code: "call_closed" });
    if (voiceCallMicrophoneEpoch === epoch
        && voiceCallCaptureGraph?.context?.state !== "closed"
        && voiceCallMicrophoneStream?.active
        && voiceCallMicrophoneStream.getAudioTracks().some((track) => track.readyState === "live")) {
      return { metrics: { reused: true }, stream: voiceCallMicrophoneStream };
    }
    if (voiceCallMicrophoneStream) releaseCallMicrophoneStream(voiceCallMicrophoneStream);
    const startedAt = performance.now();
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
    });
    if (!isCurrentCall()) {
      stream.getTracks().forEach((candidate) => candidate.stop());
      throw Object.assign(new Error("The call was closed during microphone setup."), { code: "call_closed" });
    }
    const track = stream.getAudioTracks()[0];
    if (!track || track.readyState !== "live") {
      stream.getTracks().forEach((candidate) => candidate.stop());
      throw new Error("The microphone did not become ready for the call.");
    }
    voiceCallMicrophoneStream = stream;
    voiceCallMicrophoneEpoch = epoch;
    let context = null;
    let source = null;
    let processor = null;
    let silence = null;
    try {
      context = new AudioContext();
      source = context.createMediaStreamSource(stream);
      processor = context.createScriptProcessor(256, 1, 1);
      silence = context.createGain();
      silence.gain.value = 0;
      // Publish the complete graph before connecting or resuming. Any failure
      // below can then use the same identity-guarded teardown as hang-up.
      voiceCallCaptureGraph = { context, source, processor, silence, stream };
      source.connect(processor);
      processor.connect(silence);
      silence.connect(context.destination);
      await context.resume();
      if (!isCurrentCall() || voiceCallMicrophoneStream !== stream) {
        throw Object.assign(new Error("The call was closed during microphone setup."), { code: "call_closed" });
      }
    } catch (error) {
      if (voiceCallCaptureGraph?.stream === stream) {
        releaseCallMicrophoneStream(stream);
      } else {
        source?.disconnect();
        processor?.disconnect();
        silence?.disconnect();
        void context?.close?.().catch(() => {});
        stream.getTracks().forEach((candidate) => candidate.stop());
        if (voiceCallMicrophoneStream === stream) {
          voiceCallMicrophoneStream = null;
          voiceCallMicrophoneEpoch = null;
        }
      }
      throw error;
    }
    const settings = track.getSettings?.() || {};
    return {
      stream,
      metrics: {
        reused: false,
        permission_ready_ms: Math.round((performance.now() - startedAt) * 100) / 100,
        source_rate_hz: settings.sampleRate || null,
        echo_cancellation: settings.echoCancellation ?? null,
        noise_suppression: settings.noiseSuppression ?? null,
        auto_gain_control: settings.autoGainControl ?? null,
        graph_rate_hz: context.sampleRate,
        graph_state: context.state,
      },
    };
  };

  const releaseCallMicrophoneStream = (expected = voiceCallMicrophoneStream) => {
    if (!expected || voiceCallMicrophoneStream !== expected) return;
    const graph = voiceCallCaptureGraph;
    voiceCallCaptureGraph = null;
    voiceCallMicrophoneStream = null;
    voiceCallMicrophoneEpoch = null;
    if (graph) {
      graph.processor.onaudioprocess = null;
      graph.source.disconnect();
      graph.processor.disconnect();
      graph.silence.disconnect();
      void graph.context.close().catch(() => {});
    }
    expected.getTracks().forEach((track) => track.stop());
  };

  const openVoiceCallSurface = async () => {
    const modal = document.getElementById("voice-call-modal");
    const recipient = document.querySelector(".recipients-header .recipient-chip");
    if (!modal || !recipient || voiceCallActive || voiceCallPreparing) return;
    clearPersistentVoiceCallFailure();
    const startupNavigationRevision = conversationNavigationRevision;
    const recipientId = recipient.dataset.recipientId || "";
    if (!recipientId) {
      reportError(new Error("The selected contact is unavailable for voice preparation."));
      return;
    }
    let activityGuard;
    try {
      activityGuard = createVoiceCallActivityGuard(speechOperationToken());
    } catch (error) {
      reportError(error);
      return;
    }
    voiceCallActivityGuard = activityGuard;
    voiceCallPreparing = true;
    const epoch = {};
    voiceCallEpoch = epoch;
    voiceCallActive = true;
    voiceCallConversation = null;
    let releaseHistoryRecovery = null;
    startVoiceLatency("call_greeting", "call_requested");
    const startupTrace = voiceLatencyTrace;
    let startupPhase = "activity_guard";
    let startupMicrophoneStream = null;
    const name = recipient.querySelector(":scope > span:not(.contact-avatar)")?.textContent?.trim() || "Contact";
    const sourceAvatar = recipient.querySelector(".contact-avatar");
    const targetAvatar = modal.querySelector("#voice-call-avatar");
    if (sourceAvatar && targetAvatar) {
      targetAvatar.className = sourceAvatar.className.replace(/\bcontact-avatar\b/g, "").trim();
      targetAvatar.classList.add("contact-avatar", "voice-call-avatar");
      targetAvatar.setAttribute("style", sourceAvatar.getAttribute("style") || "");
      targetAvatar.replaceChildren(...Array.from(sourceAvatar.childNodes).map((node) => node.cloneNode(true)));
    }
    modal.dataset.voicePersona = recipientId;
    modal.dataset.callState = "ringing";
    modal.setAttribute("aria-label", `Calling ${name}`);
    setText("voice-call-title", "Calling");
    setText("voice-call-name", name);
    setModalVisibility("voice-call-modal", true);
    modal.querySelector('[data-action="voice-call-close"]')?.focus();
    // Request audio resume on the gesture path. The helper waits for the
    // Calling surface to paint before it schedules tones; call preparation
    // then proceeds while the ring and setup waits settle.
    const ringingReady = startCallRinging(epoch).then(
      (started) => {
        if (started && voiceCallEpoch === epoch && voiceCallActive) {
          markVoiceLatency("call_ringing_started");
        }
        return { started };
      },
      (error) => ({ error }),
    );
    try {
      const activityGuardWaitStartedAt = performance.now();
      markVoiceLatency("call_activity_guard_wait_started");
      await activityGuard.ready;
      markVoiceLatencyIn(startupTrace, "call_activity_guard_ready", {
        wait_ms: Math.max(0, Math.round(performance.now() - activityGuardWaitStartedAt)),
      });
      if (voiceCallEpoch !== epoch || !voiceCallActive) return;
      startupPhase = "ringing";
      const ringResult = await ringingReady;
      if (ringResult.error) throw ringResult.error;
      if (!ringResult.started || voiceCallEpoch !== epoch || !voiceCallActive) return;
      // Do not admit Gemma warming or microphone setup while an idle Parakeet
      // ticket is still processing. The stop result is only a request; this
      // wait must observe the matching backend operation leave the registry.
      const recoverySuspendWaitStartedAt = performance.now();
      startupPhase = "history_recovery";
      markVoiceLatency("call_history_recovery_suspend_wait_started");
      releaseHistoryRecovery = await suspendVoiceHistoryRecoveryForForeground("call_start");
      markVoiceLatencyIn(startupTrace, "call_history_recovery_suspend_wait_finished", {
        wait_ms: Math.max(0, Math.round(performance.now() - recoverySuspendWaitStartedAt)),
      });
      if (voiceCallEpoch !== epoch || !voiceCallActive) return;
      // Start independent resident/worklet readiness while the required
      // microphone authorization and first CoreAudio callback are pending.
      // Recording either only after that await falsely charged microphone
      // time to model startup and put Web Audio initialization back on the
      // first-PCM path.
      const warmReady = runtimeWarmPromise.then((warm) => {
        if (voiceCallEpoch === epoch && voiceCallActive
            && voiceLatencyTrace?.mode === "call_greeting") {
          markVoiceLatencyIn(startupTrace, "call_runtime_ready", warm);
        }
        return warm;
      });
      void preparePcmWorklet(24_000);
      startupPhase = "microphone";
      // Permission and a live input route are prerequisites for a call. Keep
      // this exact stream for the whole call so WebKit can continuously apply
      // acoustic echo cancellation while contact audio is playing.
      const preparedMicrophone = await prepareCallMicrophoneStream(epoch);
      startupMicrophoneStream = preparedMicrophone.stream;
      if (voiceCallEpoch !== epoch || !voiceCallActive) {
        releaseCallMicrophoneStream(startupMicrophoneStream);
        return;
      }
      markVoiceLatency("call_microphone_ready", preparedMicrophone.metrics);
      // A cold local engine is still preparation, not a ringing contact.
      // Keep measuring the entire request-to-playback interval; do not hide
      // initialization time by redefining the latency clock at readiness.
      startupPhase = "runtime_warm";
      await warmReady;
      if (voiceCallEpoch !== epoch || !voiceCallActive) return;
      // Startup is opportunistic and can be superseded by foreground model
      // selection. The exact conversation preparation below owns call readiness
      // and warms its own audio encoder; a stale startup failure is not final.
      let conversation = selectedConversation();
      startupPhase = "conversation";
      if (conversation === "default") {
        const started = await mutateConversationSelection(() => {
          if (voiceCallEpoch !== epoch || !voiceCallActive
              || conversationNavigationRevision !== startupNavigationRevision) return null;
          const current = selectedConversation();
          if (current !== "default") return { existingConversation: current };
          return invoke("mom_llama_conversation_draft_call_start").then(async (result) => {
            // Draft creation changes native selection. Publish that selection
            // before releasing the serialized mutation, even if hang-up won
            // while native work was pending. A queued redial then reuses it.
            if (conversationNavigationRevision === startupNavigationRevision
                && result?.status !== "blocked" && result?.result?.id
                && result.result.source_conversation_id === recipientId) {
              chat().dataset.currentConversation = result.result.id;
              chat().dataset.conversationKind = "chat";
              if (!voiceCallActive && voiceCallEpoch !== epoch) await refreshChat();
            }
            return result;
          });
        });
        if (conversationNavigationRevision !== startupNavigationRevision) return;
        if (started?.existingConversation) {
          conversation = started.existingConversation;
        } else {
          if (voiceCallEpoch !== epoch || !voiceCallActive) return;
          report(started);
          if (started?.status === "blocked" || !started?.result?.id) {
            throw new Error(started?.blocker?.message || "The call could not start.");
          }
          if (started.result.source_conversation_id !== recipientId) {
            throw new Error("The selected contact changed before the call began.");
          }
          conversation = started.result.id;
        }
      } else if (selectedConversationKind() === "persona_template") {
        const instantiated = await mutateConversationSelection(() => {
          if (voiceCallEpoch !== epoch || !voiceCallActive
              || conversationNavigationRevision !== startupNavigationRevision) return null;
          const current = selectedConversation();
          if (current !== conversation || selectedConversationKind() !== "persona_template") {
            return { existingConversation: current };
          }
          return invoke("mom_llama_persona_instantiate", { persona: conversation, title: null }).then(async (result) => {
            if (conversationNavigationRevision === startupNavigationRevision
                && result?.status !== "blocked" && result?.result?.id) {
              chat().dataset.currentConversation = result.result.id;
              chat().dataset.conversationKind = "chat";
              if (!voiceCallActive && voiceCallEpoch !== epoch) await refreshChat();
            }
            return result;
          });
        });
        if (conversationNavigationRevision !== startupNavigationRevision) return;
        if (instantiated?.existingConversation) {
          conversation = instantiated.existingConversation;
        } else {
          if (voiceCallEpoch !== epoch || !voiceCallActive) return;
          report(instantiated);
          if (instantiated?.status === "blocked" || !instantiated?.result?.id) {
            throw new Error(instantiated?.blocker?.message || "The call could not start.");
          }
          conversation = instantiated.result.id;
        }
      }
      if (voiceCallEpoch !== epoch || !voiceCallActive) return;
      voiceCallConversation = conversation;
      markVoiceLatency("call_conversation_ready", { conversation });
      // Use the actual frozen call profile. It may differ from its source contact.
      startupPhase = "persona_prepare";
      const personaPrepared = await prepareVoicePersonaForCall(conversation, epoch);
      if (!personaPrepared || voiceCallEpoch !== epoch || !voiceCallActive) return;
      // The first model reply uses the same token -> TTS -> PCM buffer as every
      // later call turn. Its first PCM frame stops ringing; playback startup
      // opens continuous listening, while model attachment waits until Qwen
      // has finished producing PCM.
      startupPhase = "greeting_generation";
      await startStreamedCallGreeting(epoch, conversation);
    } catch (error) {
      if (epoch && voiceCallEpoch === epoch) {
        markVoiceLatency("call_start_failed", {
          phase: startupPhase,
          error_code: safeVoiceFailureCode(error),
        });
        setPersistentVoiceCallFailure(errorMessage(error));
        const closing = closeVoiceCallSurface();
        reportError(error);
        await closing;
      } else {
        releaseCallMicrophoneStream(startupMicrophoneStream);
        // Rejections caused by a completed hang-up are expected cancellation.
        // Returning here keeps the delegated click handler from displaying a
        // second, misleading error after the call has already closed.
        return;
      }
    } finally {
      if (voiceCallEpoch === epoch) voiceCallPreparing = false;
      releaseHistoryRecovery?.();
    }
  };

  const closeVoiceCallSurface = async () => {
    const activityGuard = voiceCallActivityGuard;
    voiceCallActivityGuard = null;
    const conversation = voiceCallConversation;
    const microphoneStream = voiceCallMicrophoneStream;
    const callTts = liveVoiceTts?.mode === "call" ? liveVoiceTts : null;
    const audibleCall = activeCallPlayback;
    const cancelGeneration = Boolean(
      conversation
      && callReplyConversation === conversation
      && activeDispatchConversation === conversation,
    );
    voiceCallActive = false;
    voiceCallEpoch = null;
    voiceCallPreparing = false;
    voiceCallConversation = null;
    retryCallPrefillAfterTranscript = null;
    voiceReplyConversation = null;
    if (cancelGeneration && activeDispatchConversation === conversation) {
      activeDispatchConversation = null;
      activeDispatchOwner = null;
    }
    callReplyConversation = null;
    if (callTts && liveVoiceTts === callTts) liveVoiceTts = null;
    activeCallPlayback = null;
    // Dismiss the call immediately. Some local stop paths touch Web Audio or
    // native cancellation before their first await; neither owns the UI.
    setModalVisibility("voice-call-modal", false);

    // Each stop function severs its local Web Audio nodes before its first
    // await. Start all of them before waiting on IPC.
    const stops = [
      stopVoiceCapture({ discard: true, reason: "call_closed" }),
      stopCallTranscriptForClose(),
      callTts?.stop(),
      audibleCall && audibleCall !== callTts ? audibleCall.stop() : null,
      stopSpeechPlayback(),
      stopCallBed(),
    ];
    if (cancelGeneration) {
      stops.push(invoke("mom_llama_chat_cancel", { conversation }).catch(() => {}));
    }
    await Promise.allSettled(stops.filter(Boolean));
    releaseCallMicrophoneStream(microphoneStream);
    if (activityGuard) {
      try {
        await activityGuard.clear();
      } catch (error) {
        console.warn("voice cache activity guard could not be cleared", error);
      }
    }
    if (conversation) {
      // The call surface is already dismissed and native stop/cancel work has
      // settled. Checkpointing can serialize a large state, so keep it off the
      // UI path and let the runtime skip it if the request is still active.
      void invoke("mom_llama_kv_cache_checkpoint_voice_session", { conversation })
        .catch((error) => console.warn("voice session checkpoint unavailable", error));
    }
    void recoverPendingVoiceHistory().catch(() => {});
  };

  const setText = (id, value) => {
    const element = document.getElementById(id);
    if (element) element.textContent = value == null ? "" : String(value);
  };

  let attachmentLibraryPreviewEpoch = null;

  const clearAttachmentLibraryPreview = (modal) => {
    attachmentLibraryPreviewEpoch = {};
    delete modal.dataset.previewId;
    delete modal.dataset.impactSha256;
    modal.querySelector('[data-action="attachment-library-commit"]')?.setAttribute("disabled", "");
    setText("attachment-library-graph", "Review required");
    setText("attachment-library-text", "Review required");
    setText("attachment-library-rights-sha", "Review required");
    setText("attachment-library-impact-sha", "Review required");
    setText("attachment-library-status", "Confirm rights, then review the exact publish impact.");
  };

  const openAttachmentLibrary = (button) => {
    const modal = document.getElementById("attachment-library-modal");
    if (!modal) return;
    modal.dataset.attachment = button.dataset.attachment || "";
    modal.dataset.rootSha256 = button.dataset.rootSha256 || "";
    modal.dataset.artifact = button.dataset.artifact || "";
    modal.dataset.policyFingerprint = button.dataset.policyFingerprint || "";
    document.getElementById("attachment-library-item-title").value = button.dataset.title || "Attachment text";
    document.getElementById("attachment-library-rights").checked = false;
    setText("attachment-library-root", modal.dataset.rootSha256);
    setText("attachment-library-artifact", modal.dataset.artifact);
    setText("attachment-library-policy", modal.dataset.policyFingerprint);
    clearAttachmentLibraryPreview(modal);
    setModalVisibility("attachment-library-modal", true);
    document.getElementById("attachment-library-rights")?.focus();
  };

  const previewAttachmentLibrary = async () => {
    const modal = document.getElementById("attachment-library-modal");
    if (!modal) return;
    if (!document.getElementById("attachment-library-rights")?.checked) {
      throw new Error("Confirm private local-use rights before reviewing this publish.");
    }
    clearAttachmentLibraryPreview(modal);
    const epoch = attachmentLibraryPreviewEpoch;
    const preview = await invoke("mom_llama_attachment_library_preview", {
      anchor: {
        attachment_id: modal.dataset.attachment || "",
        root_sha256: modal.dataset.rootSha256 || "",
        artifact_id: modal.dataset.artifact || "",
        policy_fingerprint: modal.dataset.policyFingerprint || "",
      },
      title: document.getElementById("attachment-library-item-title")?.value || "Attachment text",
      confirmedPrivateUse: true,
    });
    if (epoch !== attachmentLibraryPreviewEpoch
      || document.getElementById("attachment-library-modal") !== modal
      || modal.hidden
      || !document.getElementById("attachment-library-rights")?.checked) return;
    modal.dataset.previewId = preview.preview_id;
    modal.dataset.impactSha256 = preview.impact_sha256;
    setText("attachment-library-root", preview.root_sha256);
    setText("attachment-library-graph", preview.graph_sha256);
    setText("attachment-library-artifact", preview.artifact_id);
    setText(
      "attachment-library-policy",
      `${preview.processor_name}@${preview.processor_version} · ${preview.policy_fingerprint}`,
    );
    setText(
      "attachment-library-text",
      `${preview.canonical_text_bytes} bytes · SHA-256 ${preview.canonical_text_sha256}`,
    );
    setText("attachment-library-rights-sha", preview.rights_sha256);
    setText("attachment-library-impact-sha", preview.impact_sha256);
    setText(
      "attachment-library-status",
      "Exact Attachment and rights bindings verified. Add exact text publishes an immutable managed copy.",
    );
    modal.querySelector('[data-action="attachment-library-commit"]')?.removeAttribute("disabled");
  };

  const commitAttachmentLibrary = async () => {
    const modal = document.getElementById("attachment-library-modal");
    if (!modal?.dataset.previewId || !modal.dataset.impactSha256) {
      throw new Error("Review the exact attachment library item before publishing it.");
    }
    const epoch = attachmentLibraryPreviewEpoch;
    const result = await invoke("mom_llama_attachment_library_commit", {
      previewId: modal.dataset.previewId,
      impactSha256: modal.dataset.impactSha256,
    });
    report(result);
    if (epoch === attachmentLibraryPreviewEpoch
      && document.getElementById("attachment-library-modal") === modal) {
      setModalVisibility("attachment-library-modal", false);
    }
    await refreshManagedAttachments();
  };

  const selectedInformationLibrary = () =>
    document.getElementById("information-library-select")?.value || "";

  const refreshInformationLibraries = async () => {
    const libraries = await invoke("mom_llama_information_libraries");
    const select = document.getElementById("information-library-select");
    if (!select) return libraries;
    const selected = select.value;
    select.replaceChildren();
    libraries.forEach((library) => {
      const option = document.createElement("option");
      option.value = library.installation_id;
      option.textContent = `${library.label} · ${library.profile} · ${library.source_sha256.slice(0, 12)}`;
      select.append(option);
    });
    if ([...select.options].some((option) => option.value === selected)) select.value = selected;
    return libraries;
  };

  const renderInformationSearch = (result) => {
    const container = document.getElementById("information-search-results");
    if (!container) return;
    container.replaceChildren();
    if (!result.hits?.length) {
      const empty = document.createElement("p");
      empty.textContent = "No bounded local evidence matched.";
      container.append(empty);
      return;
    }
    result.hits.forEach((hit) => {
      const article = document.createElement("article");
      article.className = "information-result";
      const title = document.createElement("strong");
      title.textContent = hit.title;
      const snippet = document.createElement("p");
      snippet.textContent = hit.snippet;
      const provenance = document.createElement("small");
      provenance.textContent = `${hit.publisher} · excerpt SHA-256 ${hit.excerpt_sha256}`;
      const open = createCommandElement("button", DYNAMIC_CONTROL_SPECS.informationOpenCitation);
      open.type = "button";
      open.className = "small-button";
      open.dataset.action = "information-open-citation";
      open.dataset.citation = JSON.stringify(hit.citation);
      open.textContent = "Open exact citation";
      article.append(title, snippet, provenance, open);
      container.append(article);
    });
  };

  const renderManagedEvidence = (container, hits) => {
    container.replaceChildren();
    if (!hits.length) {
      const empty = document.createElement("p");
      empty.textContent = "No canonical text segment matched.";
      container.append(empty);
      return;
    }
    hits.forEach((hit) => {
      const result = document.createElement("div");
      result.className = "information-managed-hit";
      const snippet = document.createElement("p");
      snippet.textContent = hit.snippet;
      const open = createCommandElement(
        "button",
        DYNAMIC_CONTROL_SPECS.informationManagedCitation,
      );
      open.type = "button";
      open.className = "small-button";
      open.dataset.action = "information-open-managed-citation";
      open.dataset.citation = JSON.stringify(hit.citation);
      open.textContent = "Open exact lineage";
      result.append(snippet, open);
      container.append(result);
    });
  };

  const refreshManagedAttachments = async () => {
    const records = await invoke("mom_llama_information_managed_attachments");
    const container = document.getElementById("information-managed-list");
    if (!container) return records;
    container.replaceChildren();
    records.forEach((record) => {
      const article = document.createElement("article");
      article.className = "information-managed-item";
      article.dataset.materialization = record.materialization_id;
      const title = document.createElement("strong");
      title.textContent = record.title;
      const lineage = document.createElement("small");
      lineage.textContent = `root ${record.root_sha256} · artifact ${record.artifact_id} · text ${record.canonical_text_sha256}`;
      const query = createCommandElement(
        "input",
        DYNAMIC_CONTROL_SPECS.informationManagedSearch,
      );
      query.type = "search";
      query.placeholder = "Search this managed text";
      query.setAttribute("aria-label", `Search ${record.title}`);
      const controls = document.createElement("div");
      controls.className = "button-strip";
      const search = createCommandElement(
        "button",
        DYNAMIC_CONTROL_SPECS.informationManagedSearch,
      );
      search.type = "button";
      search.className = "small-button";
      search.dataset.action = "information-search-managed";
      search.textContent = "Search";
      const remove = createCommandElement(
        "button",
        DYNAMIC_CONTROL_SPECS.informationManagedRemovalPreview,
      );
      remove.type = "button";
      remove.className = "small-button danger";
      remove.dataset.action = "information-managed-remove-preview";
      setInformationRemovalControl(remove, "review", "Review", `Review removal of ${record.title}`);
      const results = document.createElement("div");
      results.className = "information-managed-results";
      results.setAttribute("aria-live", "polite");
      controls.append(search, remove);
      article.append(title, lineage, query, controls, results);
      container.append(article);
    });
    const retained = await invoke("mom_llama_information_managed_removals");
    const removals = document.getElementById("information-removal-list");
    if (removals) {
      removals.replaceChildren();
      retained.forEach((record) => {
        const article = document.createElement("article");
        article.className = "information-managed-item";
        article.dataset.materialization = record.preview.materialization_id;
        article.dataset.retainedOperation = record.preview.preview_id;
        const title = document.createElement("strong");
        title.textContent = record.attachment?.title || record.preview.preview_id;
        const review = createCommandElement("button", DYNAMIC_CONTROL_SPECS.informationManagedRemovalPreview);
        review.type = "button";
        review.className = "small-button danger";
        review.dataset.action = "information-managed-remove-preview";
        review.disabled = !record.attachment;
        const description = record.attachment
          ? `Review the exact retained removal ${record.preview.preview_id}`
          : "Fresh removal consent is unavailable without retained Attachment qualification evidence.";
        setInformationRemovalControl(review, "review", "Review", description);
        const results = document.createElement("div");
        results.className = "information-managed-results";
        results.setAttribute("aria-live", "polite");
        article.append(title, review, results);
        removals.append(article);
      });
    }
    return records;
  };

  const setInformationRemovalControl = (button, kind, label, description) => {
    const icon = document.getElementById("information-managed-icons")?.content
      ?.querySelector(`[data-information-icon="${kind}"] svg`)?.cloneNode(true);
    button.replaceChildren(...(icon ? [icon] : []), document.createTextNode(label));
    button.title = description;
    button.setAttribute("aria-label", description);
  };

  const slugHandle = (value) => String(value || "")
    .trim().toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "");

  let activeConversationRename = null;
  const finishConversationRename = (state, { restoreFocus = false } = {}) => {
    if (state.done) return;
    state.done = true;
    if (activeConversationRename === state) activeConversationRename = null;
    state.input.remove();
    state.title.style.visibility = "";
    state.input.style.cssText = "";
    state.row.classList.remove("is-renaming");
    if (restoreFocus && state.navigationRevision === conversationNavigationRevision) state.button.focus();
  };

  const positionConversationRenameEditor = (state) => {
    if (state.done || !state.row.isConnected || !state.title.isConnected) return;
    const rowRect = state.row.getBoundingClientRect();
    const titleRect = state.title.getBoundingClientRect();
    state.input.style.left = `${titleRect.left - rowRect.left}px`;
    state.input.style.top = `${titleRect.top - rowRect.top}px`;
    state.input.style.width = `${titleRect.width}px`;
    state.input.style.height = `${titleRect.height}px`;
  };

  const rebindConversationRename = (root) => {
    const state = activeConversationRename;
    if (!state || state.done || !root) return;
    const rows = [...(root.querySelectorAll?.("[data-conversation-menu-target='true']") || [])];
    const row = rows.find((candidate) => candidate.dataset.conversation === state.conversation);
    const button = row?.querySelector?.('[data-action="conversation-select"]');
    const title = button?.querySelector?.(".conversation-title");
    if (!row || !button || !title) return;
    state.input.remove();
    state.row = row;
    state.button = button;
    state.title = title;
    title.style.visibility = "hidden";
    row.classList.add("is-renaming");
    row.append(state.input);
    positionConversationRenameEditor(state);
  };
  window.addEventListener("resize", () => {
    if (activeConversationRename) positionConversationRenameEditor(activeConversationRename);
  });

  const beginConversationRename = (target) => {
    const row = target.closest?.("[data-conversation-menu-target='true']") || target;
    const button = row.querySelector?.('[data-action="conversation-select"]');
    const title = button?.querySelector?.(".conversation-title");
    if (!row || !button || !title) return;
    if (activeConversationRename?.row === row) {
      activeConversationRename.input.focus();
      activeConversationRename.input.select();
      return;
    }
    if (activeConversationRename) {
      activeConversationRename.input.blur();
      if (activeConversationRename && !activeConversationRename.done) {
        activeConversationRename.input.focus();
        return;
      }
    }
    const input = document.createElement("input");
    input.type = "text";
    input.className = "conversation-rename-editor";
    input.setAttribute("aria-label", "Conversation name");
    input.dataset.affordance = row.dataset.renameAffordance || "conversation.rename";
    input.dataset.command = row.dataset.renameCommand || "mom_llama.conversation_rename";
    input.dataset.tauriCommand = row.dataset.renameTauriCommand || "mom_llama_conversation_rename";
    input.dataset.cli = row.dataset.renameCli || "mom-llama conversation rename --conversation <id> --title <title> --json";
    input.dataset.effect = row.dataset.renameEffect || "mom_llama.effects.conversation_store.v1";
    input.value = row.dataset.conversationTitle || title.textContent || "";
    const state = {
      row, button, title, input, original: input.value,
      conversation: row.dataset.conversation,
      navigationRevision: conversationNavigationRevision,
      saving: false,
      done: false,
    };
    activeConversationRename = state;
    title.style.visibility = "hidden";
    row.classList.add("is-renaming");
    row.append(input);
    positionConversationRenameEditor(state);

    const cancel = () => finishConversationRename(state, { restoreFocus: true });
    const commit = async (restoreFocus = false) => {
      if (state.done || state.saving) return;
      const nextTitle = input.value.trim();
      if (!nextTitle) {
        if (restoreFocus) {
          input.setCustomValidity("Enter a conversation name.");
          input.reportValidity();
        } else cancel();
        return;
      }
      if (nextTitle === state.original.trim()) {
        finishConversationRename(state, { restoreFocus });
        return;
      }
      state.saving = true;
      input.readOnly = true;
      try {
        const result = await invoke("mom_llama_conversation_rename", {
          conversation: row.dataset.conversation,
          title: nextTitle,
        });
        report(result);
        if (result?.status !== "passed") {
          state.saving = false;
          input.readOnly = false;
          return;
        }
        state.row.dataset.conversationTitle = nextTitle;
        state.title.textContent = nextTitle;
        finishConversationRename(state, { restoreFocus });
      } catch (error) {
        state.saving = false;
        input.readOnly = false;
        reportError(error);
      }
    };
    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        void commit(true);
      } else if (event.key === "Escape") {
        event.preventDefault();
        if (!state.saving) cancel();
      }
    });
    input.addEventListener("blur", () => { void commit(false); });
    input.addEventListener("input", () => input.setCustomValidity(""));
    input.focus();
    input.select();
  };

  let conversationNavigationRevision = 0;
  let conversationMutation = Promise.resolve();
  const mutateConversationSelection = (operation) => {
    const mutation = conversationMutation.catch(() => {}).then(operation);
    conversationMutation = mutation.catch(() => {});
    return mutation;
  };

  const selectConversation = async (conversation) => {
    const revision = ++conversationNavigationRevision;
    const startedAt = performance.now();
    void retainComposerDraft().catch(reportError);
    const result = await mutateConversationSelection(
      () => invoke("mom_llama_conversation_select", { conversation }),
    );
    if (revision !== conversationNavigationRevision) return result;
    report(result);
    const replacement = await refreshChat();
    if (revision === conversationNavigationRevision
      && replacement?.dataset.currentConversation === conversation) {
      requestAnimationFrame(() => requestAnimationFrame(() => {
        recordConversationSwitchPaint(startedAt, performance.now());
        void recoverPendingVoiceHistory().catch(() => {});
      }));
    }
    // Sidebar and settings carry secondary state. Refresh them after the chat
    // projection has been installed so neither can delay conversation paint.
    void refreshSidebar().catch(reportError);
    void refreshConversationSettingsProjection().catch(reportError);
    return result;
  };

  const openPersonaFreeze = (button) => {
    const modal = document.getElementById("persona-freeze-modal");
    if (!modal) return;
    modal.dataset.conversation = selectedConversation();
    formField(modal, "freeze_message").value = button.dataset.message || "";
    formField(modal, "freeze_name").value = "";
    formField(modal, "freeze_handle").value = "";
    setModalVisibility("persona-freeze-modal", true);
    formField(modal, "freeze_name")?.focus();
  };

  const setPersonaEditor = (persona) => {
    const editor = document.getElementById("persona-editor");
    if (!editor || !persona) return;
    editor.classList.remove("is-hidden");
    editor.dataset.personaJson = JSON.stringify(persona);
    const profile = persona.execution_profile || {};
    formField(editor, "persona_id").value = persona.id || "";
    formField(editor, "persona_name").value = persona.title || "";
    formField(editor, "persona_handle").value = profile.mention_handle || "";
    const modelChoice = formField(editor, "persona_model_choice");
    const modelPath = profile.model_path || "";
    if (modelChoice) {
      modelChoice.querySelectorAll("[data-current-persona-model]").forEach((option) => option.remove());
      if (modelPath && ![...modelChoice.options].some((option) => option.value === modelPath)) {
        const option = document.createElement("option");
        option.value = modelPath;
        option.textContent = `Current: ${String(modelPath).split(/[\\/]/).pop() || "local model"}`;
        option.dataset.currentPersonaModel = "true";
        modelChoice.append(option);
      }
      modelChoice.value = modelPath;
    }
    formField(editor, "persona_system_message").value = profile.system_message || "";
    const frozen = profile.chat_template?.kind === "frozen_source"
      ? profile.chat_template.template : null;
    formField(editor, "persona_chat_template_policy").value = frozen == null ? "model_default" : "frozen_source";
    formField(editor, "persona_chat_template").value = frozen || "";
    editor.querySelector(".persona-template-source")?.classList.toggle("is-hidden", frozen == null);
    editor.scrollIntoView({ block: "nearest" });
  };

  const openPersonaProfile = async (personaId) => {
    const result = await invoke("mom_llama_persona_get", { persona: personaId });
    report(result);
    if (result?.status === "blocked" || !result?.result) return false;
    openSettings("personas");
    setPersonaEditor(result.result);
    return true;
  };

  const instantiatePersona = async (personaId) => {
    const result = await invoke("mom_llama_persona_instantiate", {
      persona: personaId,
      title: null,
    });
    report(result);
    return result;
  };

  let personaMenuReturnFocus = null;

  const closePersonaMenu = (restoreFocus = true) => {
    const menu = document.getElementById("persona-context-menu");
    if (!menu || menu.hidden) return;
    menu.hidden = true;
    menu.classList.add("is-hidden");
    personaMenuReturnFocus?.setAttribute("aria-expanded", "false");
    if (restoreFocus) personaMenuReturnFocus?.focus();
    personaMenuReturnFocus = null;
  };

  const openPersonaMenu = (target, point = null) => {
    const menu = document.getElementById("persona-context-menu");
    if (!menu || !target?.dataset.persona) return;
    closePersonaMenu(false);
    const trigger = target.matches(".persona-menu-trigger")
      ? target
      : target.querySelector(".persona-menu-trigger");
    personaMenuReturnFocus = trigger || null;
    trigger?.setAttribute("aria-expanded", "true");
    menu.dataset.persona = target.dataset.persona;
    menu.dataset.personaTitle = target.dataset.personaTitle || "Persona";
    menu.dataset.personaVersion = target.dataset.personaVersion || "";
    menu.hidden = false;
    menu.classList.remove("is-hidden");
    const anchor = point || (() => {
      const rect = (trigger || target).getBoundingClientRect();
      return { x: rect.right, y: rect.bottom };
    })();
    const bounds = menu.getBoundingClientRect();
    menu.style.left = `${Math.max(8, Math.min(anchor.x, window.innerWidth - bounds.width - 8))}px`;
    menu.style.top = `${Math.max(8, Math.min(anchor.y, window.innerHeight - bounds.height - 8))}px`;
    menu.querySelector('[role="menuitem"]')?.focus();
  };

  const impactList = (values, fallback = "None") => values?.length ? values.join(", ") : fallback;

  const openPersonaRemoval = async (personaId) => {
    const result = await invoke("mom_llama_persona_removal_preview", { persona: personaId });
    report(result);
    if (result?.status === "blocked" || !result?.result) return false;
    const impact = result.result;
    const modal = document.getElementById("persona-removal-modal");
    if (!modal) return false;
    formField(modal, "persona_removal_id").value = impact.persona_id;
    formField(modal, "persona_removal_version").value = String(impact.persona_version);
    formField(modal, "persona_removal_impact_sha256").value = impact.impact_sha256;
    const text = (id, value) => {
      const element = document.getElementById(id);
      if (element) element.textContent = value;
    };
    text(
      "persona-removal-persona",
      `${impact.persona_title} · version ${impact.persona_version} · snapshot ${impact.persona_snapshot_sha256}`,
    );
    text(
      "persona-removal-groups",
      impactList(impact.groups?.map((group) => `${group.group_name} (${group.group_id}, position ${group.member_index + 1})`)),
    );
    text(
      "persona-removal-draft",
      impactList(impact.drafts?.map((draft) => `${draft.message_sha256}; attachments ${impactList(draft.attachment_ids)}`)),
    );
    text(
      "persona-removal-attachments",
      `Remove ${impactList(impact.attachments?.removed_draft_only_unshared_attachment_ids)}; retain supporting ${impactList(impact.attachments?.retained_supporting_attachment_ids)}`,
    );
    text(
      "persona-removal-caches",
      `Mom ${impactList(impact.caches?.mom_persistent_cache_ids)}; Native ${impactList(impact.caches?.native_persistent_cache_ids)}`,
    );
    text("persona-removal-active", impactList(impact.active_invocation_ids));
    text(
      "persona-removal-retained",
      `messages ${impactList(impact.retained_history?.retained_message_ids)}; versions ${impactList(impact.retained_history?.retained_persona_versions?.map(String))}; invocations ${impactList(impact.retained_history?.retained_invocation_ids)}`,
    );
    text("persona-removal-impact-sha256", impact.impact_sha256);
    closePersonaMenu(false);
    setModalVisibility("persona-removal-modal", true);
    modal.querySelector('[data-action="persona-removal-commit"]')?.focus();
    return true;
  };

  const commitPersonaRemoval = async () => {
    const modal = document.getElementById("persona-removal-modal");
    const result = await invoke("mom_llama_persona_remove_from_library", {
      input: {
        persona_id: formValue(modal, "persona_removal_id"),
        persona_version: Number(formValue(modal, "persona_removal_version")),
        impact_sha256: formValue(modal, "persona_removal_impact_sha256"),
      },
    });
    report(result);
    if (result?.status === "blocked") return result;
    setModalVisibility("persona-removal-modal", false);
    await Promise.all([
      refreshSettings("personas"),
      refreshSidebar(),
      refreshConversationProjection(),
    ]);
    return result;
  };

  const focusComposer = () => {
    const textarea = document.querySelector("#chat-form textarea[name='message']");
    if (!textarea) return;
    textarea.focus();
    textarea.setSelectionRange(textarea.value.length, textarea.value.length);
  };

  const personaProfileFromEditor = () => {
    const editor = document.getElementById("persona-editor");
    const current = JSON.parse(editor?.dataset.personaJson || "{}");
    const profile = current.execution_profile || {};
    const modelPath = formValue(editor, "persona_model_choice") || null;
    const template = formValue(editor, "persona_chat_template_policy") === "frozen_source"
      ? { kind: "frozen_source", template: formValue(editor, "persona_chat_template") }
      : { kind: "model_default" };
    return {
      persona_id: formValue(editor, "persona_id"),
      name: formValue(editor, "persona_name"),
      mention_handle: formValue(editor, "persona_handle"),
      model_path: modelPath,
      // Preserve an imported/advanced explicit projector only while its exact
      // model remains unchanged. A new ordinary model resolves its own pair.
      mmproj_path: modelPath === (profile.model_path || null)
        ? (profile.mmproj_path || null)
        : null,
      auto_discover_mmproj: modelPath !== (profile.model_path || null),
      system_message: formValue(editor, "persona_system_message") || null,
      sampling: profile.sampling || null,
      chat_template: template,
    };
  };

  const setPersonaGroupEditor = (group = null) => {
    const editor = document.getElementById("persona-group-editor");
    if (!editor) return;
    editor.classList.remove("is-hidden");
    formField(editor, "persona_group_id").value = group?.id || "";
    formField(editor, "persona_group_name").value = group?.name || "";
    formField(editor, "persona_group_handle").value = group?.mention_handle || "";
    for (let index = 0; index < 4; index += 1) {
      formField(editor, `persona_group_member_${index}`).value = group?.persona_ids?.[index] || "";
    }
    editor.querySelector(".persona-group-create")?.classList.toggle("is-hidden", Boolean(group));
    editor.querySelector(".persona-group-update")?.classList.toggle("is-hidden", !group);
    formField(editor, "persona_group_name")?.focus();
  };

  const openToolApproval = (approval) => {
    if (!mcpProcessUiSupported()) return;
    const modal = document.getElementById("tool-approval-modal");
    if (!modal || !approval) return;
    modal.dataset.approvalId = approval.id || "";
    modal.dataset.conversation = approval.conversation_id || selectedConversation();
    modal.dataset.prompt = approval.prompt || "";
    modal.dataset.server = approval.server || "";
    modal.dataset.tool = approval.tool || "";
    modal.dataset.arguments = JSON.stringify(approval.arguments || {});
    modal.dataset.maxTurns = String(approval.max_turns || 1);
    modal.dataset.mode = "tool-loop";
    modal.dataset.invocationId = "";
    const setText = (id, value) => {
      const node = document.getElementById(id);
      if (node) node.textContent = String(value);
    };
    setText("tool-approval-server", approval.server || "");
    setText("tool-approval-tool", approval.tool || "");
    setText("tool-approval-prompt", approval.prompt || "");
    setText("tool-approval-turns", approval.max_turns || 1);
    setText("tool-approval-arguments", JSON.stringify(approval.arguments || {}, null, 2));
    const live = document.getElementById("tool-loop-live");
    live?.classList.add("is-hidden");
    live?.removeAttribute("data-request-id");
    document.getElementById("tool-loop-live-events")?.replaceChildren();
    setText("tool-loop-live-state", "Waiting for approval");
    const approve = modal.querySelector('[data-action="tool-loop-run"]');
    const cancel = modal.querySelector('[data-action="tool-loop-cancel"]');
    const close = modal.querySelector('[data-action="tool-approval-close"]');
    if (approve) approve.disabled = !approval.id;
    if (cancel) cancel.disabled = true;
    if (close) close.disabled = false;
    modal.querySelectorAll(".tool-loop-approval-action").forEach((button) => button.classList.remove("is-hidden"));
    modal.querySelectorAll(".persona-tool-approval-action").forEach((button) => button.classList.add("is-hidden"));
    const title = document.getElementById("tool-approval-title");
    if (title) title.textContent = "Approve this tool call?";
    modal.dataset.running = "false";
    modal.hidden = false;
    modal.classList.remove("is-hidden");
    modal.setAttribute("aria-hidden", "false");
    approve?.focus();
  };

  const openPersonaToolApproval = (approval) => {
    if (!mcpProcessUiSupported()) return;
    const modal = document.getElementById("tool-approval-modal");
    if (!modal || !approval?.id || !approval?.invocation_id) return;
    modal.dataset.mode = "persona";
    modal.dataset.approvalId = approval.id;
    modal.dataset.invocationId = approval.invocation_id;
    modal.dataset.conversation = approval.host_conversation_id || selectedConversation();
    modal.dataset.server = approval.server || "";
    modal.dataset.tool = approval.tool || "";
    modal.dataset.arguments = JSON.stringify(approval.arguments || {});
    const setText = (id, value) => {
      const node = document.getElementById(id);
      if (node) node.textContent = String(value);
    };
    const title = document.getElementById("tool-approval-title");
    if (title) title.textContent = `Allow ${approval.label || approval.handle || "this Persona"} to call a configured external-process tool?`;
    setText("tool-approval-server", approval.server || "");
    setText("tool-approval-tool", approval.tool || "");
    const snapshot = String(approval.snapshot_sha256 || "").slice(0, 12);
    const call = String(approval.call_sha256 || "").slice(0, 12);
    setText(
      "tool-approval-prompt",
      `@${approval.handle || "persona"} v${approval.persona_version || 0} · frozen turn ${approval.user_message_id || ""} · snapshot ${snapshot} · call ${call}`,
    );
    setText("tool-approval-turns", "One exact call");
    setText("tool-approval-arguments", JSON.stringify(approval.arguments || {}, null, 2));
    document.getElementById("tool-loop-live")?.classList.add("is-hidden");
    modal.querySelectorAll(".tool-loop-approval-action").forEach((button) => button.classList.add("is-hidden"));
    modal.querySelectorAll(".persona-tool-approval-action").forEach((button) => {
      button.classList.remove("is-hidden");
      button.disabled = false;
    });
    modal.dataset.running = "false";
    modal.hidden = false;
    modal.classList.remove("is-hidden");
    modal.setAttribute("aria-hidden", "false");
    modal.querySelector('[data-action="mention-tool-approve"]')?.focus();
  };

  const closeToolApproval = () => {
    const modal = document.getElementById("tool-approval-modal");
    if (!modal) return;
    modal.hidden = true;
    modal.classList.add("is-hidden");
    modal.setAttribute("aria-hidden", "true");
    modal.dataset.approvalId = "";
    modal.dataset.invocationId = "";
    modal.dataset.mode = "";
  };

  const decidePersonaToolApproval = async (decision) => {
    if (!requireMcpProcessUi()) return;
    const modal = document.getElementById("tool-approval-modal");
    if (modal?.dataset.mode !== "persona") return;
    const invocation = modal.dataset.invocationId || "";
    const approval = modal.dataset.approvalId || "";
    if (!invocation || !approval || !["approve", "deny"].includes(decision)) {
      throw new Error("The frozen Persona tool approval is incomplete.");
    }
    const lease = acquireChatBusy(`mention-tool-approval:${approval}`);
    modal.dataset.running = "true";
    modal.querySelectorAll(".persona-tool-approval-action").forEach((button) => { button.disabled = true; });
    try {
      const result = await invoke("mom_llama_mention_tool_approval_decide", {
        invocation,
        approval,
        decision,
      });
      report(result);
      closeToolApproval();
      await refreshConversationProjection();
    } finally {
      releaseChatBusy(lease);
      modal.dataset.running = "false";
      if (!modal.hidden) {
        modal.querySelectorAll(".persona-tool-approval-action").forEach((button) => { button.disabled = false; });
      }
    }
  };

  const switchSettingsSection = (section) => {
    const title = document.getElementById("settings-section-title");
    const modal = document.getElementById("settings-sidebar");
    if (modal) modal.dataset.activeSection = section;
    document.querySelectorAll(".section-tab[data-section]").forEach((tab) => {
      const active = tab.dataset.section === section;
      tab.classList.toggle("active", active);
      tab.setAttribute("aria-pressed", String(active));
      if (active) tab.scrollIntoView({ block: "nearest", inline: "nearest" });
      if (active && title) title.textContent = tab.textContent.trim();
    });
    document.querySelectorAll("[data-section-panel]").forEach((panel) => {
      panel.classList.toggle("active", panel.dataset.sectionPanel === section);
    });
  };

  const setSearchMode = (enabled) => {
    const form = document.getElementById("conversation-search-form");
    const list = document.getElementById("conversation-list");
    const results = document.getElementById("conversation-search-results");
    if (form) form.classList.toggle("is-hidden", !enabled);
    if (list) list.classList.toggle("is-hidden", enabled);
    if (results) results.classList.toggle("is-hidden", !enabled);
    document.querySelector(".search-toggle")?.classList.toggle("is-hidden", enabled);
    document.querySelector(".sidebar-titlebar-zone")?.classList.toggle("searching", enabled);
    if (enabled) {
      shell()?.classList.add("sidebar-open");
      form?.querySelector("input[name='query']")?.focus();
    } else document.querySelector(".search-toggle")?.focus();
  };

  const commandMetadata = (element, spec) => {
    element.dataset.affordance = spec.affordance;
    element.dataset.command = spec.command;
    element.dataset.tauriCommand = spec.tauri;
    element.dataset.cli = spec.cli;
    element.dataset.effect = spec.effect;
  };

  const DYNAMIC_CONTROL_SPECS = Object.freeze({
    attachmentLibraryOpen: Object.freeze({
      affordance: "attachment.library_open",
      command: "mom_llama.attachment_library_preview",
      tauri: "mom_llama_attachment_library_preview",
      cli: "app-only; impact-hashed exact canonical Attachment preview",
      effect: "mom_llama.effects.attachment_library_preview.v1",
    }),
    attachmentPreview: Object.freeze({
      affordance: "attachment.preview",
      command: "mom_llama.attachment_preview",
      tauri: "mom_llama_attachment_preview",
      cli: "mom-llama attachment preview --attachment <id> --json",
      effect: "mom_llama.effects.attachment_preview.v1",
    }),
    attachmentTranscribe: Object.freeze({
      affordance: "attachment.transcribe_audio",
      command: "mom_llama.speech_transcribe_attachment",
      tauri: "mom_llama_speech_transcribe_attachment",
      cli: "app-only; exact Attachment authority belongs to the shared AppRuntime SpeechHost",
      effect: "mom_llama.effects.speech_transcribe_attachment.v1",
    }),
    attachmentTranscriptionStop: Object.freeze({
      affordance: "attachment.transcription_stop",
      command: "mom_llama.speech_stop",
      tauri: "mom_llama_speech_stop",
      cli: "app-only; stops an opaque operation or playback owned by the running AppRuntime",
      effect: "mom_llama.effects.speech_stop.v1",
    }),
    readAloud: Object.freeze({
      affordance: "message.read_aloud",
      command: "mom_llama.speech_read_aloud",
      tauri: "mom_llama_speech_read_aloud",
      cli: "app-only; exact playback belongs to the shared AppRuntime SpeechHost",
      effect: "mom_llama.effects.speech_read_aloud.v1",
    }),
    readAloudStop: Object.freeze({
      affordance: "message.read_aloud_stop",
      command: "mom_llama.speech_stop",
      tauri: "mom_llama_speech_stop",
      cli: "app-only; stops an opaque operation or playback owned by the running AppRuntime",
      effect: "mom_llama.effects.speech_stop.v1",
    }),
    draftUpdate: Object.freeze({
      affordance: "conversation.draft_update",
      command: "mom_llama.draft_update",
      tauri: "mom_llama_draft_update",
      cli: "mom-llama conversation draft-update --conversation <id> --message <text> --json",
      effect: "mom_llama.effects.conversation_store.v1",
    }),
    conversationSelect: Object.freeze({
      affordance: "conversation.select",
      command: "mom_llama.conversation_select",
      tauri: "mom_llama_conversation_select",
      cli: "mom-llama conversation select --conversation <id> --json",
      effect: "mom_llama.effects.conversation_store.v1",
    }),
    mentionCancel: Object.freeze({
      affordance: "mention.cancel",
      command: "mom_llama.mention_cancel",
      tauri: "mom_llama_mention_cancel",
      cli: "mom-llama mention cancel --invocation <id> --target <id> --json",
      effect: "mom_llama.effects.chat_cancel.v1",
    }),
    mentionCandidates: Object.freeze({
      affordance: "mention.candidates",
      command: "mom_llama.mention_candidates",
      tauri: "mom_llama_mention_candidates",
      cli: "mom-llama mention candidates --query <text> --json",
      effect: "mom_llama.effects.conversation_store.v1",
    }),
    messageEdit: Object.freeze({
      affordance: "message.edit",
      command: "mom_llama.message_edit",
      tauri: "mom_llama_message_edit",
      cli: "mom-llama message edit --conversation <id> --message <id> --content <text> --json",
      effect: "mom_llama.effects.conversation_store.v1",
    }),
    informationOpenCitation: Object.freeze({
      affordance: "information.open_citation",
      command: "mom_llama.information_open_citation",
      tauri: "mom_llama_information_open_citation",
      cli: "app-only; reopens one exact path-free citation",
      effect: "mom_llama.effects.information_local_read.v1",
    }),
    informationManagedSearch: Object.freeze({
      affordance: "information.search_managed_attachment",
      command: "mom_llama.information_search_managed_attachment",
      tauri: "mom_llama_information_search_managed_attachment",
      cli: "app-only; exact managed representation search",
      effect: "mom_llama.effects.information_managed_read.v1",
    }),
    informationManagedCitation: Object.freeze({
      affordance: "information.open_managed_citation",
      command: "mom_llama.information_open_managed_citation",
      tauri: "mom_llama_information_open_managed_citation",
      cli: "app-only; reopens exact managed lineage",
      effect: "mom_llama.effects.information_managed_read.v1",
    }),
    informationManagedRemovalPreview: Object.freeze({
      affordance: "information.managed_removal_preview",
      command: "mom_llama.information_managed_removal_preview",
      tauri: "mom_llama_information_managed_removal_preview",
      cli: "app-only; opaque exact managed-only removal preview",
      effect: "mom_llama.effects.information_managed_removal.v1",
    }),
    informationManagedRemovalCommit: Object.freeze({
      affordance: "information.managed_removal_commit",
      command: "mom_llama.information_managed_removal_commit",
      tauri: "mom_llama_information_managed_removal_commit",
      cli: "app-only; removes only exact Information-owned bytes",
      effect: "mom_llama.effects.information_managed_removal.v1",
    }),
  });

  const createCommandElement = (tag, spec) => {
    const element = document.createElement(tag);
    commandMetadata(element, spec);
    return element;
  };

  const setButtonStateLabel = (button, label) => {
    if (!button) return;
    const explicitLabel = button.querySelector(":scope > [data-button-label], :scope > .button-label");
    const fallbackLabel = [...button.children].find((child) => (
      child.tagName === "SPAN" && child.getAttribute("aria-hidden") !== "true"
    ));
    const labelNode = explicitLabel || fallbackLabel;
    if (labelNode) labelNode.textContent = label;
    button.setAttribute("aria-label", label);
    if (button.hasAttribute("title")) button.setAttribute("title", label);
  };

  const renderSearchResults = (response) => {
    const list = document.getElementById("conversation-search-results");
    if (!list) return;
    list.replaceChildren();
    const hits = response?.result || [];
    if (!hits.length) {
      const empty = document.createElement("li");
      empty.className = "empty-line";
      empty.textContent = "No matching conversations";
      list.appendChild(empty);
      return;
    }
    hits.forEach((hit) => {
      const item = document.createElement("li");
      if (hit.kind === "persona_template") {
        item.dataset.personaMenuTarget = "true";
        item.dataset.persona = hit.conversation_id;
      } else {
        item.dataset.conversationMenuTarget = "true";
        item.dataset.conversation = hit.conversation_id;
        item.dataset.conversationTitle = hit.title;
        item.dataset.message = hit.active_leaf_message_id || "";
      }
      const button = createCommandElement("button", DYNAMIC_CONTROL_SPECS.conversationSelect);
      button.type = "button";
      button.className = "conversation-item search-hit";
      button.dataset.action = hit.kind === "persona_template" ? "persona-draft-open" : "conversation-select";
      button.dataset.persona = hit.conversation_id;
      button.dataset.conversation = hit.conversation_id;
      const title = document.createElement("span");
      title.textContent = hit.title || hit.conversation_id;
      const detail = document.createElement("small");
      detail.textContent = hit.snippet || "";
      button.append(title, detail);
      item.appendChild(button);
      list.appendChild(item);
    });
  };

  const search = async () => {
    const form = document.getElementById("conversation-search-form");
    const result = await invoke("mom_llama_conversation_search", {
      query: formValue(form, "query"),
    });
    renderSearchResults(result);
    report(result);
  };

  let draftTimer = null;
  const draftAttachmentIds = (form = document.getElementById("chat-form")) =>
    [...(form?.querySelectorAll("[data-staged-attachment-id]") || [])]
      .map((attachment) => attachment.dataset.stagedAttachmentId)
      .filter(Boolean);

  const isInternalVoicePrompt = (message) =>
    message.startsWith("[Voice note]") || message.startsWith("[Phone call]");

  const persistDraftNow = async (
    message,
    attachmentIds = draftAttachmentIds(),
    conversation = selectedConversation(),
  ) => {
    window.clearTimeout(draftTimer);
    draftTimer = null;
    return invoke("mom_llama_draft_update", {
      conversation,
      message,
      attachmentIds: [...attachmentIds],
    });
  };

  const scheduleDraft = (message) => {
    window.clearTimeout(draftTimer);
    const conversation = selectedConversation();
    const attachmentIds = draftAttachmentIds();
    draftTimer = window.setTimeout(() => {
      draftTimer = null;
      invoke("mom_llama_draft_update", {
        conversation,
        message,
        attachmentIds,
      }).catch(reportError);
    }, 300);
  };

  let autocompleteTimer = null;
  let autocompleteSerial = 0;
  let presentedAutocomplete = null;
  let autocompleteAccepting = false;
  const autocompleteEncoder = new TextEncoder();

  const composerLogicalAnchor = (textarea = composerTextarea()) => {
    if (!textarea || selectedConversationKind() !== "chat") return null;
    const draft = textarea.value;
    const attachmentIds = draftAttachmentIds(textarea.form);
    const collapsedAtEnd = textarea.selectionStart === draft.length
      && textarea.selectionEnd === draft.length;
    const version = Number(textarea.dataset.executionProfileVersion || 0);
    if (
      !draft
      || autocompleteEncoder.encode(draft).byteLength > 16 * 1024
      || !collapsedAtEnd
      || attachmentIds.length > 0
      || mentionTokenAtCursor(textarea)
      || composerState.kind === "composing"
      || !Number.isSafeInteger(version)
      || version < 1
    ) return null;
    return Object.freeze({
      conversation_id: selectedConversation(),
      active_leaf_message_id: textarea.dataset.activeLeaf || null,
      execution_profile_version: version,
      draft,
      selection_start_utf16: textarea.selectionStart,
      selection_end_utf16: textarea.selectionEnd,
      attachment_ids: Object.freeze([...attachmentIds]),
    });
  };

  const autocompleteAnchorKey = (anchor) => JSON.stringify({
    conversation_id: anchor?.conversation_id || "",
    active_leaf_message_id: anchor?.active_leaf_message_id || null,
    execution_profile_version: anchor?.execution_profile_version || 0,
    draft: anchor?.draft || "",
    selection_start_utf16: anchor?.selection_start_utf16 ?? -1,
    selection_end_utf16: anchor?.selection_end_utf16 ?? -1,
    attachment_ids: anchor?.attachment_ids || [],
  });

  const hideComposerAutocomplete = (announce = false) => {
    const ghost = document.getElementById("composer-ai-ghost");
    ghost?.setAttribute("hidden", "");
    ghost?.querySelector(".composer-ai-anchor")?.replaceChildren();
    ghost?.querySelector(".composer-ai-suffix")?.replaceChildren();
    const status = document.getElementById("composer-ai-status");
    if (status) status.textContent = announce ? "Suggestion dismissed." : "";
    presentedAutocomplete = null;
  };

  const cancelComposerAutocomplete = ({ native = true, announce = false, forceNative = false } = {}) => {
    window.clearTimeout(autocompleteTimer);
    autocompleteTimer = null;
    autocompleteSerial += 1;
    const active = forceNative
      || composerState.kind === "ai_pending"
      || composerState.kind === "ai_presented";
    transitionComposer({ type: "ai_dismiss" });
    hideComposerAutocomplete(announce);
    if (native && active) {
      invoke("mom_llama_composer_autocomplete_cancel").catch(() => {});
    }
  };

  const syncComposerAutocompleteScroll = (textarea = composerTextarea()) => {
    const ghost = document.getElementById("composer-ai-ghost");
    if (!ghost || !textarea) return;
    ghost.scrollLeft = textarea.scrollLeft;
    ghost.scrollTop = textarea.scrollTop;
  };

  const showComposerAutocomplete = (textarea, requestId, anchor, suffix) => {
    const ghost = document.getElementById("composer-ai-ghost");
    if (!ghost) return false;
    ghost.querySelector(".composer-ai-anchor").textContent = anchor.draft;
    ghost.querySelector(".composer-ai-suffix").textContent = suffix;
    ghost.removeAttribute("hidden");
    syncComposerAutocompleteScroll(textarea);
    const status = document.getElementById("composer-ai-status");
    if (status) status.textContent = "Suggestion available. Press Right Arrow to accept.";
    presentedAutocomplete = Object.freeze({ requestId, anchor, suffix });
    return true;
  };

  const scheduleComposerAutocomplete = (textarea) => {
    window.clearTimeout(autocompleteTimer);
    const initial = composerLogicalAnchor(textarea);
    if (!initial || composerState.kind !== "idle") return;
    const serial = ++autocompleteSerial;
    const initialKey = autocompleteAnchorKey(initial);
    autocompleteTimer = window.setTimeout(async () => {
      autocompleteTimer = null;
      try {
        const beforePersist = composerLogicalAnchor(textarea);
        if (serial !== autocompleteSerial || autocompleteAnchorKey(beforePersist) !== initialKey) return;
        const persisted = await persistDraftNow(
          beforePersist.draft,
          beforePersist.attachment_ids,
          beforePersist.conversation_id,
        );
        if (persisted?.status === "blocked") return;
        const anchor = composerLogicalAnchor(textarea);
        if (serial !== autocompleteSerial || autocompleteAnchorKey(anchor) !== initialKey) return;
        const requestId = `composer-${serial}`;
        const pending = transitionComposer({
          type: "ai_request",
          requestId,
          anchor: initialKey,
        });
        if (composerState.kind !== "ai_pending") return;
        for (const effect of pending) {
          if (effect.kind === "ai_cancel") cancelComposerAutocomplete();
        }
        const response = await invoke("mom_llama_composer_autocomplete", {
          conversation: anchor.conversation_id,
          draft: anchor.draft,
          activeLeafMessageId: anchor.active_leaf_message_id,
          executionProfileVersion: anchor.execution_profile_version,
          selectionStartUtf16: anchor.selection_start_utf16,
          selectionEndUtf16: anchor.selection_end_utf16,
          attachmentIds: anchor.attachment_ids,
        });
        if (serial !== autocompleteSerial) return;
        if (response?.status !== "host_integrated") {
          cancelComposerAutocomplete({ native: false });
          return;
        }
        const result = response.result;
        const current = composerLogicalAnchor(textarea);
        const serverKey = autocompleteAnchorKey(result?.anchor);
        if (
          !result
          || autocompleteAnchorKey(current) !== initialKey
          || serverKey !== initialKey
          || typeof result.suffix !== "string"
          || result.suffix.includes("\n")
          || result.suffix.includes("\r")
          || autocompleteEncoder.encode(result.suffix).byteLength > 512
        ) {
          cancelComposerAutocomplete({ native: false });
          return;
        }
        transitionComposer({
          type: "ai_present",
          requestId,
          anchor: initialKey,
          suffix: result.suffix,
        });
        if (composerState.kind !== "ai_presented"
          || !showComposerAutocomplete(textarea, requestId, result.anchor, result.suffix)) {
          cancelComposerAutocomplete({ native: false });
        }
      } catch {
        if (serial === autocompleteSerial) cancelComposerAutocomplete({ native: false });
      }
    }, 350);
  };

  const acceptComposerAutocomplete = async (textarea, effect) => {
    const presented = presentedAutocomplete;
    const current = composerLogicalAnchor(textarea);
    if (
      !presented
      || presented.requestId !== effect.requestId
      || presented.suffix !== effect.suffix
      || autocompleteAnchorKey(presented.anchor) !== effect.anchor
      || autocompleteAnchorKey(current) !== effect.anchor
    ) {
      cancelComposerAutocomplete({ native: false });
      return false;
    }
    autocompleteAccepting = true;
    textarea.readOnly = true;
    try {
      const acceptance = await invoke("mom_llama_composer_autocomplete_accept", {
        anchor: presented.anchor,
        suffix: presented.suffix,
      });
      const accepted = acceptance?.result;
      if (acceptance?.status !== "host_integrated") {
        cancelComposerAutocomplete({ native: false });
        return false;
      }
      const revalidatedCurrent = composerLogicalAnchor(textarea);
      const responseIsExact = autocompleteAnchorKey(accepted?.anchor) === effect.anchor
        && accepted?.anchor?.model_fingerprint_sha256 === presented.anchor.model_fingerprint_sha256
        && accepted?.anchor?.generation_input_sha256 === presented.anchor.generation_input_sha256
        && accepted?.message === `${presented.anchor.draft}${presented.suffix}`;
      const viewIsExact = presentedAutocomplete === presented
        && autocompleteAnchorKey(revalidatedCurrent) === effect.anchor;
      if (!responseIsExact || !viewIsExact) {
        await refreshChat();
        return true;
      }
      const insertion = textarea.selectionEnd;
      transitionComposer({ type: "ai_dismiss" });
      hideComposerAutocomplete(false);
      textarea.dataset.autocompleteCommittedDraft = accepted.message;
      textarea.setRangeText(effect.suffix, insertion, insertion, "end");
      textarea.dispatchEvent(new Event("input", { bubbles: true }));
      return true;
    } finally {
      autocompleteAccepting = false;
      textarea.readOnly = false;
    }
  };

  const ensureMessageStream = () => {
    const chatElement = chat();
    if (!chatElement) return null;
    let stream = chatElement.querySelector(".message-stream");
    if (!stream) {
      chatElement.querySelector(".landing")?.remove();
      stream = document.createElement("section");
      stream.className = "message-stream";
      stream.setAttribute("aria-label", "Messages");
      chatElement.insertBefore(stream, chatElement.querySelector(".composer"));
      chatElement.classList.remove("empty");
      chatElement.classList.add("has-messages");
    }
    return stream;
  };

  const appendLiveDelta = (content, delta) => {
    if (!content) return;
    content.querySelector(".typing-indicator")?.remove();
    content.append(document.createTextNode(delta || ""));
    keepLiveTailVisible(content);
  };

  const finishLiveMessage = (row) => {
    row?.querySelector(".typing-indicator")?.remove();
  };

  const appendMessageSpeakerAvatar = (article) => {
    if (!article || article.querySelector(".message-speaker-avatar")) return;
    const recipientAvatar = document.querySelector(
      ".recipients-header .recipient-chip .contact-avatar, .persona-context .contact-avatar",
    );
    if (!recipientAvatar) return;
    const speakerAvatar = document.createElement("span");
    speakerAvatar.className = "message-speaker-avatar";
    speakerAvatar.appendChild(recipientAvatar.cloneNode(true));
    article.prepend(speakerAvatar);
  };

  const appendLiveMessage = (role, content, id) => {
    const stream = ensureMessageStream();
    if (!stream) return null;
    const article = document.createElement("article");
    article.id = id;
    article.className = `message-row ${role}`;
    if (role === "assistant") appendMessageSpeakerAvatar(article);
    const card = document.createElement("div");
    card.className = "message-card";
    if (role === "assistant") {
      const reasoning = document.createElement("section");
      reasoning.className = "message-reasoning live-reasoning is-hidden";
      const label = document.createElement("p");
      label.className = "message-reasoning-label";
      label.textContent = "Reasoning in progress";
      const reasoningContent = document.createElement("div");
      reasoningContent.className = "reasoning-content";
      reasoning.append(label, reasoningContent);
      card.appendChild(reasoning);
    }
    const visibleContent = document.createElement("div");
    visibleContent.className = "live-content";
    if (role === "assistant" && !content) {
      visibleContent.appendChild(createTypingIndicator());
    } else {
      visibleContent.textContent = content;
    }
    card.appendChild(visibleContent);
    article.appendChild(card);
    stream.appendChild(article);
    if (!settingEnabled("disableAutoScroll")) {
      stream.dataset.followTail = "true";
      stream.scrollTop = stream.scrollHeight;
    }
    return card;
  };

  const appendOptimisticVoiceMessage = (bytes = null) => {
    const stream = ensureMessageStream();
    if (!stream) return null;
    const article = document.createElement("article");
    article.className = "message-row user voice-message-row optimistic-voice-message";
    const card = document.createElement("div");
    card.className = "message-card";
    let player;
    if (bytes) {
      const audio = createCommandElement("audio", DYNAMIC_CONTROL_SPECS.readAloud);
      const url = URL.createObjectURL(new Blob([bytes], { type: "audio/wav" }));
      audio.src = url;
      audio.preload = "auto";
      article.dataset.objectUrl = url;
      player = compactAudioPlayer(audio, bytes);
    } else {
      player = pendingVoiceMessage("Preparing voice message", false);
    }
    card.append(player);
    article.append(card);
    stream.append(article);
    stream.dataset.followTail = "true";
    stream.scrollTop = stream.scrollHeight;
    return article;
  };

  const hydrateOptimisticVoiceMessage = (article, bytes) => {
    if (!article?.isConnected || !bytes?.byteLength) return;
    const player = article.querySelector(".voice-message-player");
    if (!player) return;
    const audio = createCommandElement("audio", DYNAMIC_CONTROL_SPECS.readAloud);
    const url = URL.createObjectURL(new Blob([bytes], { type: "audio/wav" }));
    audio.src = url;
    audio.preload = "auto";
    const priorUrl = article.dataset.objectUrl;
    if (priorUrl) URL.revokeObjectURL(priorUrl);
    article.dataset.objectUrl = url;
    player.replaceWith(compactAudioPlayer(audio, bytes));
  };

  const appendLiveVoiceReply = (id) => {
    const card = appendLiveMessage("assistant", "", id);
    const row = card?.closest(".message-row");
    if (!card || !row) return null;
    row.classList.add("voice-reply-row", "voice-reply-pending");
    // A bare portrait reads as a broken message. Publish the speaker only
    // when PCM exists; until then the compact typing bubble is the complete
    // pending state.
    row.querySelector?.(".message-speaker-avatar")?.remove();
    card.replaceChildren(pendingVoiceReply());
    row.hidden = false;
    return row;
  };

  const keepLiveTailVisible = (element) => {
    if (settingEnabled("disableAutoScroll")) return;
    const stream = element?.closest(".message-stream");
    if (!stream || stream.dataset.followTail === "false") return;
    stream.scrollTop = stream.scrollHeight;
  };

  let activeDispatchConversation = null;
  let activeDispatchOwner = null;
  let voiceRecording = null;
  let voiceReplyConversation = null;
  let voiceReplyMessageId = null;
  let callReplyConversation = null;
  let voiceCallPreparing = false;
  let voiceCallActive = false;
  let voiceCallTranscriptSlotsDisabled = false;
  let voiceCallEpoch = null;
  let voiceCallActivityGuard = null;
  let voiceCallAudio = null;
  let voiceCallConversation = null;
  let retryCallPrefillAfterTranscript = null;
  let voiceCallMicrophoneStream = null;
  let voiceCallCaptureGraph = null;
  let voiceCallMicrophoneEpoch = null;
  let pendingVoiceAudioSession = null;
  let pendingVoiceFallbackAttachment = null;
  let pendingVoiceDispatchPrompt = null;
  let voiceLatencyTrace = null;
  let liveVoiceTts = null;
  let activeCallPlayback = null;
  // Retain measured Qwen cadence across turns on this machine. Each reply
  // still owns independent queue counters, while startup runway and segment
  // sizing begin from observed hardware behavior instead of cold constants.
  let voiceLeadCalibration = null;
  // Bind the speech drain to the exact voice turn that requested it. Sidebar
  // work and a following capture may replace `liveVoiceTts` while the native
  // result is still returning.
  let pendingVoiceTtsOwner = null;
  let activeVoiceSpeculation = null;
  let runtimeWarmState = { status: "not_started" };
  let runtimeWarmPromise = Promise.resolve(runtimeWarmState);
  let voiceLatencyPersistence = Promise.resolve();
  const voiceLatencyPending = new Map();
  const voiceLatencyHistory = [];
  let voiceLatencyFlushTimer = null;
  const activeDispatchStreamLeases = new Set();
  const activeDispatchStreamLeaseOwners = new Map();
  const chatBusyLeases = new Set();
  let chatBusyLeaseSerial = 0;

  const voiceLatencySummary = (trace) => {
    const indexedStages = trace.stages.map((entry, index) => ({ entry, index }));
    const latestIndexed = (stage, beforeIndex = indexedStages.length) => {
      for (let index = Math.min(beforeIndex - 1, indexedStages.length - 1); index >= 0; index -= 1) {
        if (indexedStages[index].entry.stage === stage) return indexedStages[index];
      }
      return null;
    };
    const latest = (stage) => latestIndexed(stage)?.entry;
    const firstIndexed = (stage) => indexedStages.find(({ entry }) => entry.stage === stage) || null;
    const elapsed = (stage) => {
      const entry = latest(stage);
      return Number.isFinite(entry?.elapsed_ms) ? entry.elapsed_ms : null;
    };
    const delta = (from, to) => {
      const endEntry = latestIndexed(to);
      const startEntry = endEntry ? latestIndexed(from, endEntry.index) : null;
      const start = Number.isFinite(startEntry?.entry?.elapsed_ms) ? startEntry.entry.elapsed_ms : null;
      const end = Number.isFinite(endEntry?.entry?.elapsed_ms) ? endEntry.entry.elapsed_ms : null;
      return start === null || end === null || end < start ? null : end - start;
    };
    const absoluteTimestamp = (entry) => {
      const observed = Number(entry?.observed_at_unix_us);
      if (Number.isFinite(observed) && observed > 0) return observed;
      const native = Number(entry?.native_emitted_at_unix_us);
      return Number.isFinite(native) && native > 0 ? native : null;
    };
    const timestampDelta = (from, to, timestamp) => {
      const ends = indexedStages
        .filter(({ entry }) => entry.stage === to)
        .map(({ entry }) => timestamp(entry))
        .filter(Number.isFinite);
      if (!ends.length) return null;
      const end = Math.max(...ends);
      const starts = indexedStages
        .filter(({ entry }) => entry.stage === from)
        .map(({ entry }) => timestamp(entry))
        .filter((value) => Number.isFinite(value) && value <= end);
      const start = starts.length ? Math.max(...starts) : null;
      return start === null || end === null || end < start
        ? null
        : Math.round((end - start) / 10) / 100;
    };
    // Native capture and command events arrive through independent listeners.
    // Their callback order is not their clock order, especially at a silence
    // edge where the native gate can release before the renderer records the
    // VAD event. Pair the closest timestamps instead of unrelated array
    // neighbours from adjacent speculative attempts.
    const nativeDelta = (from, to) => timestampDelta(
      from,
      to,
      (entry) => {
        const value = Number(entry?.native_emitted_at_unix_us);
        return Number.isFinite(value) && value > 0 ? value : null;
      },
    );
    const absoluteDelta = (from, to) => timestampDelta(from, to, absoluteTimestamp);
    // `silence_onset` is published when the native detector releases the
    // speculative turn, after its bounded endpoint wait. Recover the physical
    // edge from the detector's sample-clock duration so user-facing latency
    // includes that wait instead of beginning after it has already elapsed.
    const physicalSilenceDelta = (to) => {
      const ends = indexedStages
        .filter(({ entry }) => entry.stage === to)
        .map(({ entry }) => absoluteTimestamp(entry))
        .filter(Number.isFinite);
      if (!ends.length) return null;
      const end = Math.max(...ends);
      const releases = indexedStages
        .filter(({ entry }) => entry.stage === "silence_onset")
        .map(({ entry }) => {
          const released = absoluteTimestamp(entry);
          return Number.isFinite(released) ? { entry, released } : null;
        })
        .filter((value) => value && value.released <= end);
      const release = releases.toSorted((left, right) => right.released - left.released)[0] || null;
      const detectorDelayMs = Number(release?.entry?.silence_ms);
      const start = release
        ? release.released
          - (Number.isFinite(detectorDelayMs) ? Math.max(0, detectorDelayMs) * 1000 : 0)
        : null;
      return start === null || end < start ? null : Math.round((end - start) / 10) / 100;
    };
    // Older call captures could inherit a late greeting completion when the
    // global trace changed during pickup. It is not a reply from this turn.
    const completedGeneration = [...indexedStages].reverse().find(({ entry }) =>
      entry.stage === "generation_finished"
        && !(entry.purpose === "call_greeting" && trace.mode !== "call_greeting")) || null;
    const timedOutGeneration = latestIndexed("generation_timeout");
    const generationTerminalIndexed = !completedGeneration
      ? timedOutGeneration
      : !timedOutGeneration || completedGeneration.index > timedOutGeneration.index
        ? completedGeneration
        : timedOutGeneration;
    const latestSpeculationStart = [...indexedStages].reverse().find(({ entry }) =>
      entry.speculation && (entry.stage === "speculation_started"
        || entry.stage === "generation_dispatched")) || null;
    const attemptStartIndex = latestSpeculationStart
      ? (latestIndexed("silence_onset", latestSpeculationStart.index)?.index
        ?? latestSpeculationStart.index)
      : 0;
    const currentSpeculation = latestSpeculationStart?.entry?.speculation
      || generationTerminalIndexed?.entry?.speculation || null;
    const generationTerminal = generationTerminalIndexed
      && (!latestSpeculationStart
        || (generationTerminalIndexed.index >= latestSpeculationStart.index
          && generationTerminalIndexed.entry.speculation === currentSpeculation))
      ? generationTerminalIndexed.entry : null;
    const generation = generationTerminal?.stage === "generation_finished"
      ? generationTerminal : null;
    const latestSinceAttempt = (stage) => {
      for (let index = indexedStages.length - 1; index >= attemptStartIndex; index -= 1) {
        const entry = indexedStages[index].entry;
        if (entry.stage === stage
          && (!entry.speculation || entry.speculation === currentSpeculation)) return entry;
      }
      return null;
    };
    const latestAttemptStage = (stage) => {
      if (!currentSpeculation) return latest(stage);
      for (let index = indexedStages.length - 1; index >= 0; index -= 1) {
        const entry = indexedStages[index].entry;
        if (entry.stage === stage && entry.speculation === currentSpeculation) return entry;
      }
      return null;
    };
    // Preserve the incremental engine's partial progress when a supervised
    // turn times out or is cancelled before it can produce a normal result.
    // This is what distinguishes command-queue starvation from projector,
    // suffix, sampler, or first-token stalls in real packaged calls.
    const nativeAudioTerminal = latestAttemptStage("native_audio_finish_failed")
      || latestAttemptStage("native_audio_finish_returned");
    const nativeDecodeAudio = [...indexedStages].reverse().find(({ entry }) =>
      entry.stage === "native_chat_stream_event" && entry.event === "decode_started"
        && entry.audio && (!currentSpeculation || entry.speculation === currentSpeculation)
    )?.entry.audio || null;
    const audio = generation?.voice_audio || nativeAudioTerminal?.audio || nativeDecodeAudio;
    const cancelledAudio = latestAttemptStage("native_audio_finish_failed")?.audio || null;
    const captureStarted = firstIndexed("native_capture_requested")
      || firstIndexed("capture_started");
    const firstNativeTarget = firstIndexed("audio_prefill_native_targeted");
    const firstPrefillRequest = firstIndexed("audio_prefill_begin_requested");
    const firstPrefillReady = firstIndexed("audio_prefill_session_ready");
    // A prefetched next-turn session is recorded later in the same trace.
    // Never attribute that later preparation to the current microphone turn.
    const initialPrefillReady = firstPrefillReady
      && (!firstNativeTarget || firstPrefillReady.index <= firstNativeTarget.index)
      ? firstPrefillReady
      : null;
    const initialPrefillRequest = firstPrefillRequest
      && initialPrefillReady
      && firstPrefillRequest.index <= initialPrefillReady.index
      && (!firstNativeTarget || firstPrefillRequest.index <= firstNativeTarget.index)
      ? firstPrefillRequest
      : null;
    const prefillPreparedBeforeCapture = Boolean(captureStarted && (
      (initialPrefillReady
        && Number(initialPrefillReady.entry.elapsed_ms) <= Number(captureStarted.entry.elapsed_ms))
      || (!initialPrefillReady && firstNativeTarget)
    ));
    const begin = audio?.begin || initialPrefillReady?.entry?.begin || null;
    const capture = latest("capture_processing");
    const silence = latestSinceAttempt("silence_onset");
    const silenceDecisionDelayMs = Number.isFinite(Number(silence?.silence_ms))
      ? Math.max(0, Number(silence.silence_ms)) : null;
    const handoff = latestSinceAttempt("speculation_handoff_ready");
    const tts = latestSinceAttempt("tts_segment_finished");
    const ttsSegments = indexedStages.slice(attemptStartIndex)
      .map(({ entry }) => entry)
      .filter((entry) => entry.stage === "tts_segment_finished");
    const ttsMetricValues = (name) => ttsSegments
      .map((stage) => Number(stage[name]))
      .filter(Number.isFinite);
    const measuredTtsSum = (name, divisor = 1) => {
      if (ttsSegments.length === 0 || !ttsSegments.every((stage) =>
        typeof stage[name] === "number" && Number.isFinite(stage[name]))) return null;
      return Math.round(ttsSegments.reduce((total, stage) => total + stage[name], 0) / divisor * 100) / 100;
    };
    const ttsIdentitySet = (name) => {
      const values = [...new Set(ttsSegments
        .map((stage) => stage[name])
        .filter((value) => typeof value === "string" && value.length > 0))];
      return values.length ? values : null;
    };
    const ttsComplete = latestSinceAttempt("tts_all_segments_finished");
    const semanticBackpressure = latestSinceAttempt("semantic_backpressure_finished")
      || ttsComplete?.semantic_backpressure || null;
    const nextPrefillStarted = latestSinceAttempt("next_turn_prefill_started");
    const nextPrefillReady = latestSinceAttempt("next_turn_prefill_ready");
    const playbackRenderer = latestSinceAttempt("playback_renderer");
    const playbackWorkletMetricsReported = playbackRenderer?.worklet === true
      && playbackRenderer?.worklet_final_metrics_received === true;
    const playbackRendererCountersKnown = playbackRenderer?.worklet !== true
      || playbackWorkletMetricsReported;
    const playbackUnderrun = latestSinceAttempt("playback_underrun");
    const playbackStarted = latestSinceAttempt("playback_started");
    const playbackRendered = latestSinceAttempt("playback_render_started");
    const firstContentThisAttempt = latestSinceAttempt("first_content_delta");
    const microphoneReady = latest("call_microphone_ready");
    const runtimeWarm = latest("runtime_warm_completed") || latest("runtime_warm_state");
    const modelLoad = runtimeWarm?.model_load || null;
    const metricMs = (name) => {
      const precise = Number(audio?.[`${name}_us`]);
      if (Number.isFinite(precise)) return Math.round(precise / 10) / 100;
      const coarse = Number(audio?.[`${name}_ms`]);
      return Number.isFinite(coarse) ? coarse : null;
    };
    const captureMetricMs = (name, browserName = null) => {
      const precise = Number(capture?.[`${name}_us`]);
      if (Number.isFinite(precise)) return Math.round(precise / 10) / 100;
      const browser = browserName === null ? NaN : Number(capture?.[browserName]);
      return Number.isFinite(browser) ? browser : null;
    };
    const firstProjectorFinish = metricMs("first_projector_finish");
    const lastProjectorFinish = metricMs("last_projector_finish");
    const audioFinish = metricMs("finish");
    const decodeStart = metricMs("decode_start");
    const closingBoundaryFinish = metricMs("closing_boundary_finish");
    const suffixTokenizeFinish = metricMs("suffix_tokenize_finish");
    const suffixEvalFinish = metricMs("suffix_eval_finish");
    const samplerStart = metricMs("sampler_start");
    const samplerReady = metricMs("sampler_ready");
    const firstSampleStart = metricMs("first_sample_start");
    const firstSampleFinish = metricMs("first_sample_finish");
    const firstTokenEvent = metricMs("first_token_event");
    const lastSampleStart = metricMs("last_sample_start");
    const lastSampleFinish = metricMs("last_sample_finish");
    const lastTokenEvent = metricMs("last_token_event");
    const nativeFirstToken = Number(generation?.first_token_ms);
    const nativeClockDelta = (from, to) => Number.isFinite(from) && Number.isFinite(to)
      && to >= from
      ? Math.round((to - from) * 100) / 100
      : null;
    const cancelledMetricMs = (name) => {
      const precise = Number(cancelledAudio?.[`${name}_us`]);
      if (Number.isFinite(precise)) return Math.round(precise / 10) / 100;
      const coarse = Number(cancelledAudio?.[`${name}_ms`]);
      return Number.isFinite(coarse) ? coarse : null;
    };
    const cancellationRequested = cancelledMetricMs("cancellation_requested");
    const cancellationObserved = cancelledMetricMs("cancellation_observed");
    const acceptedAudioSamples = Number(audio?.accepted_samples);
    const consumedAudioSamples = Number(audio?.consumed_samples);
    const projectedAudioSamples = Number(audio?.projected_samples);
    const projectorCpuMs = metricMs("projector");
    const audioBacklogSamples = Number.isFinite(acceptedAudioSamples)
      && Number.isFinite(consumedAudioSamples)
      ? Math.max(0, acceptedAudioSamples - consumedAudioSamples)
      : null;
    const projectedAudioMs = Number.isFinite(projectedAudioSamples)
      ? projectedAudioSamples * 1000 / 16_000
      : null;
    return {
      call_request_to_ringing_ms: delta("call_requested", "call_ringing_started"),
      call_activity_guard_wait_ms: delta("call_activity_guard_wait_started", "call_activity_guard_ready"),
      call_history_recovery_suspend_wait_ms: delta(
        "call_history_recovery_suspend_wait_started", "call_history_recovery_suspend_wait_finished",
      ),
      call_request_to_microphone_ready_ms: delta("call_requested", "call_microphone_ready"),
      microphone_ready_to_ringing_ms: delta("call_microphone_ready", "call_ringing_started"),
      microphone_native_first_sample_ms:
        Number.isFinite(Number(microphoneReady?.first_sample_ms))
          ? Number(microphoneReady.first_sample_ms) : null,
      call_request_to_runtime_ready_ms: delta("call_requested", "call_runtime_ready"),
      call_request_to_conversation_ready_ms: delta("call_requested", "call_conversation_ready"),
      call_request_to_dispatch_ms: delta("call_requested", "generation_dispatched"),
      call_dispatch_to_prefill_ms: absoluteDelta("generation_dispatched", "prefill_started")
        ?? delta("generation_dispatched", "prefill_started"),
      call_prefill_to_first_content_ms: absoluteDelta("prefill_started", "first_content_delta")
        ?? delta("prefill_started", "first_content_delta"),
      generation_prompt_tokens: Number.isFinite(Number(generation?.prompt_tokens))
        ? Number(generation.prompt_tokens) : null,
      generation_completion_tokens: Number.isFinite(Number(generation?.completion_tokens))
        ? Number(generation.completion_tokens) : null,
      generation_cache_reused: typeof generation?.cache_reused === "boolean"
        ? generation.cache_reused : null,
      generation_cache_resident_prefix_tokens:
        Number.isFinite(Number(generation?.cache_metrics?.resident_prefix_tokens))
          ? Number(generation.cache_metrics.resident_prefix_tokens) : null,
      generation_cache_restored_prefix_tokens:
        Number.isFinite(Number(generation?.cache_metrics?.restored_prefix_tokens))
          ? Number(generation.cache_metrics.restored_prefix_tokens) : null,
      generation_cache_replayed_prefix_tokens:
        Number.isFinite(Number(generation?.cache_metrics?.replayed_prefix_tokens))
          ? Number(generation.cache_metrics.replayed_prefix_tokens) : null,
      call_request_to_playback_ms: delta(
        "call_requested",
        playbackRendered ? "playback_render_started" : "playback_started",
      ),
      runtime_warm_total_ms: Number.isFinite(Number(runtimeWarm?.total_ms))
        ? Number(runtimeWarm.total_ms) : null,
      runtime_warm_settings_ms: Number.isFinite(Number(runtimeWarm?.settings_us))
        ? Math.round(Number(runtimeWarm.settings_us) / 10) / 100 : null,
      runtime_warm_model_acquire_ms: Number.isFinite(Number(runtimeWarm?.model_acquire_us))
        ? Math.round(Number(runtimeWarm.model_acquire_us) / 10) / 100 : null,
      runtime_warm_audio_projector_ms:
        Number.isFinite(Number(runtimeWarm?.audio_projector_warm_us))
          ? Math.round(Number(runtimeWarm.audio_projector_warm_us) / 10) / 100
          : null,
      runtime_warm_audio_projector_ready:
        typeof runtimeWarm?.audio_projector_warmed === "boolean"
          ? runtimeWarm.audio_projector_warmed
          : null,
      runtime_warm_persona_prefix_ms:
        Number.isFinite(Number(runtimeWarm?.persona_prefix_warm_us))
          ? Math.round(Number(runtimeWarm.persona_prefix_warm_us) / 10) / 100 : null,
      runtime_warm_persona_prefix_status: runtimeWarm?.persona_prefix_warm_status || null,
      runtime_warm_persona_prefixes_ready:
        Number.isFinite(Number(runtimeWarm?.persona_prefixes_ready))
          ? Number(runtimeWarm.persona_prefixes_ready) : null,
      model_load_total_ms: Number.isFinite(Number(modelLoad?.total_us))
        ? Math.round(Number(modelLoad.total_us) / 10) / 100 : null,
      model_load_backend_ms: Number.isFinite(Number(modelLoad?.backend_us))
        ? Math.round(Number(modelLoad.backend_us) / 10) / 100 : null,
      model_load_artifact_open_ms: Number.isFinite(Number(modelLoad?.artifact_open_us))
        ? Math.round(Number(modelLoad.artifact_open_us) / 10) / 100 : null,
      model_load_weights_ms: Number.isFinite(Number(modelLoad?.model_load_us))
        ? Math.round(Number(modelLoad.model_load_us) / 10) / 100 : null,
      model_load_projector_ms: Number.isFinite(Number(modelLoad?.projector_load_us))
        ? Math.round(Number(modelLoad.projector_load_us) / 10) / 100 : null,
      model_load_artifact_verify_ms: Number.isFinite(Number(modelLoad?.artifact_verify_us))
        ? Math.round(Number(modelLoad.artifact_verify_us) / 10) / 100 : null,
      model_load_context_create_ms: Number.isFinite(Number(modelLoad?.context_create_us))
        ? Math.round(Number(modelLoad.context_create_us) / 10) / 100 : null,
      model_load_fingerprint_ms: Number.isFinite(Number(modelLoad?.fingerprint_us))
        ? Math.round(Number(modelLoad.fingerprint_us) / 10) / 100 : null,
      capture_to_first_sample_ms: delta("native_capture_requested", "native_capture_first_sample")
        ?? delta("capture_started", "native_capture_first_sample"),
      first_sample_to_voice_onset_ms: delta("native_capture_first_sample", "voice_activity_first_detected"),
      voice_onset_to_silence_ms: delta("voice_activity_first_detected", "silence_onset"),
      barge_in_playback_stop_ms: delta("barge_in_detected", "barge_in_playback_stopped"),
      silence_event_delivery_ms: Number.isFinite(Number(silence?.event_delivery_ms))
        ? Number(silence.event_delivery_ms)
        : null,
      capture_requested_buffer_frames: Number.isFinite(Number(capture?.requested_buffer_frames))
        ? Number(capture.requested_buffer_frames)
        : null,
      capture_callback_frames_max: Number.isFinite(Number(capture?.callback_frames_max))
        ? Number(capture.callback_frames_max)
        : null,
      capture_processing_cpu_ms: captureMetricMs("processing_cpu", "callback_cpu_ms"),
      capture_processing_max_ms: captureMetricMs("processing_max", "callback_max_ms"),
      capture_resample_cpu_ms: captureMetricMs("resample_cpu", "resample_cpu_ms"),
      capture_resample_max_ms: captureMetricMs("resample_max"),
      capture_detector_cpu_ms: captureMetricMs("detector_cpu", "detector_cpu_ms"),
      capture_detector_max_ms: captureMetricMs("detector_max"),
      capture_prefill_ipc_queue_high_water_ms:
        capture && Number.isFinite(Number(capture.prefill_ipc_queue_high_water_samples))
          ? Number(capture.prefill_ipc_queue_high_water_samples) / 16 : null,
      capture_prefill_ipc_queued_at_stop_ms:
        capture && Number.isFinite(Number(capture.prefill_ipc_queued_samples))
          ? Number(capture.prefill_ipc_queued_samples) / 16 : null,
      capture_prefill_suppressed_silence_ms:
        capture && Number.isFinite(Number(capture.prefill_suppressed_silence_samples))
          ? Number(capture.prefill_suppressed_silence_samples) / 16 : null,
      capture_vad_voice_onsets: Number.isFinite(Number(capture?.vad_voice_onsets))
        ? Number(capture.vad_voice_onsets) : null,
      capture_vad_settle_ms: Number.isFinite(Number(capture?.vad_settle_samples))
        ? Number(capture.vad_settle_samples) * 1000 / 16_000 : null,
      capture_vad_silence_onsets: Number.isFinite(Number(capture?.vad_silence_onsets))
        ? Number(capture.vad_silence_onsets) : null,
      capture_vad_deferred_silence_onsets:
        Number.isFinite(Number(capture?.vad_deferred_silence_onsets))
          ? Number(capture.vad_deferred_silence_onsets) : null,
      capture_vad_speech_resumes: Number.isFinite(Number(capture?.vad_speech_resumes))
        ? Number(capture.vad_speech_resumes) : null,
      capture_vad_latched_ms: Number.isFinite(Number(capture?.vad_latched_samples))
        ? Number(capture.vad_latched_samples) * 1000 / 16_000 : null,
      capture_vad_voiced_evidence_ms:
        Number.isFinite(Number(capture?.vad_voiced_evidence_samples))
          ? Number(capture.vad_voiced_evidence_samples) * 1000 / 16_000 : null,
      capture_vad_stationary_noise_releases:
        Number.isFinite(Number(capture?.vad_stationary_noise_releases))
          ? Number(capture.vad_stationary_noise_releases) : null,
      capture_vad_open_threshold_min: Number.isFinite(Number(capture?.vad_open_threshold_min))
        ? Number(capture.vad_open_threshold_min) : null,
      capture_vad_open_threshold_max: Number.isFinite(Number(capture?.vad_open_threshold_max))
        ? Number(capture.vad_open_threshold_max) : null,
      capture_vad_close_threshold_min: Number.isFinite(Number(capture?.vad_close_threshold_min))
        ? Number(capture.vad_close_threshold_min) : null,
      capture_vad_close_threshold_max: Number.isFinite(Number(capture?.vad_close_threshold_max))
        ? Number(capture.vad_close_threshold_max) : null,
      capture_vad_noise_floor_min: Number.isFinite(Number(capture?.vad_noise_floor_min))
        ? Number(capture.vad_noise_floor_min) : null,
      capture_vad_noise_floor_max: Number.isFinite(Number(capture?.vad_noise_floor_max))
        ? Number(capture.vad_noise_floor_max) : null,
      capture_queue_delay_ms: captureMetricMs("queue_delay"),
      capture_queue_delay_max_ms: captureMetricMs("queue_delay_max"),
      capture_prefill_push_cpu_ms: captureMetricMs("prefill_push_cpu"),
      capture_prefill_push_max_ms: captureMetricMs("prefill_push_max"),
      capture_retained_copy_cpu_ms: captureMetricMs("retained_copy_cpu"),
      capture_retained_copy_max_ms: captureMetricMs("retained_copy_max"),
      capture_retained_only_samples: Number.isFinite(Number(capture?.retained_only_samples))
        ? Number(capture.retained_only_samples) : null,
      capture_speculation_prearms: Number.isFinite(Number(capture?.speculation_prearms))
        ? Number(capture.speculation_prearms) : null,
      capture_speculation_short_utterance_releases:
        Number.isFinite(Number(capture?.speculation_short_utterance_releases))
          ? Number(capture.speculation_short_utterance_releases) : null,
      capture_speculation_releases: Number.isFinite(Number(capture?.speculation_releases))
        ? Number(capture.speculation_releases) : null,
      capture_speculation_resume_cancels: Number.isFinite(Number(capture?.speculation_resume_cancels))
        ? Number(capture.speculation_resume_cancels) : null,
      capture_speculation_backoffs: Number.isFinite(Number(capture?.speculation_backoffs))
        ? Number(capture.speculation_backoffs) : null,
      capture_speculation_release_delay_max_ms:
        Number.isFinite(Number(capture?.speculation_release_delay_samples_max))
          ? Number(capture.speculation_release_delay_samples_max) * 1000 / 16_000
          : null,
      capture_input_seals: Number.isFinite(Number(capture?.input_seals))
        ? Number(capture.input_seals) : null,
      capture_input_seal_cpu_ms: captureMetricMs("input_seal_cpu"),
      capture_input_seal_max_ms: captureMetricMs("input_seal_max"),
      capture_finish_drain_ms: captureMetricMs("finish_drain"),
      capture_finish_flush_ms: captureMetricMs("finish_flush"),
      capture_wav_encode_ms: captureMetricMs("wav_encode"),
      speculation_handoff_round_trip_ms: Number.isFinite(Number(handoff?.round_trip_ms))
        ? Number(handoff.round_trip_ms) : null,
      speculation_handoff_abort_ms: Number.isFinite(Number(handoff?.abort_ms))
        ? Number(handoff.abort_ms) : null,
      speculation_handoff_begin_ms: Number.isFinite(Number(handoff?.begin_ms))
        ? Number(handoff.begin_ms) : null,
      speculation_handoff_resume_wait_ms: Number.isFinite(Number(handoff?.resume_wait_us))
        ? Math.round(Number(handoff.resume_wait_us) / 10) / 100 : null,
      speculation_handoff_restore_ms: Number.isFinite(Number(handoff?.resume_restore_us))
        ? Math.round(Number(handoff.resume_restore_us) / 10) / 100 : null,
      speculation_handoff_checkpoint_reused: typeof handoff?.resume_reused === "boolean"
        ? handoff.resume_reused : null,
      speculation_handoff_checkpoint_available:
        typeof handoff?.resume_checkpoint_available === "boolean"
          ? handoff.resume_checkpoint_available : null,
      speculation_handoff_replay_samples: Number.isFinite(Number(handoff?.replay?.replay_samples))
        ? Number(handoff.replay.replay_samples) : null,
      speculation_handoff_replay_ms: Number.isFinite(Number(handoff?.replay?.total_us))
        ? Math.round(Number(handoff.replay.total_us) / 10) / 100 : null,
      voice_cancel_requested_ms: cancellationRequested,
      voice_cancel_observed_ms: cancellationObserved,
      voice_cancel_to_observed_ms: nativeClockDelta(
        cancellationRequested,
        cancellationObserved,
      ),
      voice_session_prepare_ms: captureStarted && initialPrefillReady
        ? Math.max(0, Math.round((Number(initialPrefillReady.entry.elapsed_ms)
          - Number(captureStarted.entry.elapsed_ms)) * 100) / 100)
        : null,
      voice_session_prepared_before_capture: prefillPreparedBeforeCapture,
      voice_session_prepare_native_ms: Number.isFinite(Number(begin?.total_us))
        ? Math.round(Number(begin.total_us) / 10) / 100
        : null,
      voice_session_request_to_ready_ms: initialPrefillRequest && initialPrefillReady
        ? Math.max(0, Number(initialPrefillReady.entry.elapsed_ms)
          - Number(initialPrefillRequest.entry.elapsed_ms))
        : null,
      voice_session_handoff_ms: initialPrefillRequest && initialPrefillReady
        && Number.isFinite(Number(begin?.total_us))
        ? Math.max(0, Math.round(((Number(initialPrefillReady.entry.elapsed_ms)
          - Number(initialPrefillRequest.entry.elapsed_ms))
          - Number(begin.total_us) / 1000) * 100) / 100)
        : null,
      voice_session_resident_context_tokens: Number.isFinite(Number(begin?.resident_context_tokens))
        ? Number(begin.resident_context_tokens)
        : null,
      voice_session_settings_cache_hit: typeof begin?.settings_cache_hit === "boolean"
        ? begin.settings_cache_hit
        : null,
      voice_session_model_resident_hit: typeof begin?.model_resident_hit === "boolean"
        ? begin.model_resident_hit
        : null,
      voice_session_settings_ms: Number.isFinite(Number(begin?.settings_us))
        ? Math.round(Number(begin.settings_us) / 10) / 100
        : null,
      voice_session_conversation_ms: Number.isFinite(Number(begin?.conversation_us))
        ? Math.round(Number(begin.conversation_us) / 10) / 100
        : null,
      voice_session_model_acquire_ms: Number.isFinite(Number(begin?.model_acquire_us))
        ? Math.round(Number(begin.model_acquire_us) / 10) / 100
        : null,
      voice_session_cache_lookup_ms: Number.isFinite(Number(begin?.cache_lookup_us))
        ? Math.round(Number(begin.cache_lookup_us) / 10) / 100
        : null,
      voice_session_generation_admission_ms: Number.isFinite(Number(begin?.generation_admission_us))
        ? Math.round(Number(begin.generation_admission_us) / 10) / 100
        : null,
      voice_session_cached_prefix_tokens: Number.isFinite(Number(begin?.cached_prefix_tokens))
        ? Number(begin.cached_prefix_tokens)
        : null,
      voice_session_cached_prefix_found: typeof begin?.cached_prefix_found === "boolean"
        ? begin.cached_prefix_found
        : null,
      capture_to_model_target_ms: delta("native_capture_requested", "audio_prefill_native_targeted"),
      audio_accepted_samples: Number.isFinite(Number(audio?.accepted_samples))
        ? Number(audio.accepted_samples) : null,
      audio_consumed_samples: Number.isFinite(Number(audio?.consumed_samples))
        ? Number(audio.consumed_samples) : null,
      audio_chunks: Number.isFinite(Number(audio?.chunks)) ? Number(audio.chunks) : null,
      audio_high_water_samples: Number.isFinite(Number(audio?.high_water_samples))
        ? Number(audio.high_water_samples) : null,
      audio_backlog_samples: audioBacklogSamples,
      audio_backlog_ms: audioBacklogSamples === null
        ? null : Math.round((audioBacklogSamples * 1000 / 16_000) * 100) / 100,
      audio_first_accept_ms: metricMs("first_accept"),
      audio_first_read_ms: metricMs("first_read"),
      prompt_prefix_start_ms: metricMs("prefix_start"),
      prompt_prefix_finish_ms: metricMs("prefix_finish"),
      prompt_prefix_cpu_ms: metricMs("prefix"),
      prompt_prefix_tokens: Number.isFinite(Number(audio?.prefix_tokens))
        ? Number(audio.prefix_tokens) : null,
      prompt_prefix_restored_tokens: Number.isFinite(Number(audio?.restored_prefix_tokens))
        ? Number(audio.restored_prefix_tokens) : null,
      projector_first_start_ms: metricMs("first_projector_start"),
      projector_first_finish_ms: firstProjectorFinish,
      projector_last_finish_ms: lastProjectorFinish,
      projector_cpu_ms: metricMs("projector"),
      projector_audio_ms: projectedAudioMs === null
        ? null : Math.round(projectedAudioMs * 100) / 100,
      projector_realtime_factor: projectorCpuMs !== null && projectedAudioMs > 0
        ? Math.round((projectorCpuMs / projectedAudioMs) * 10_000) / 10_000
        : null,
      projector_max_ms: metricMs("projector_max"),
      projector_tail_cpu_ms: metricMs("tail_projector"),
      projector_tail_samples: Number.isFinite(Number(audio?.tail_samples))
        ? Number(audio.tail_samples)
        : null,
      projector_blocks: Number.isFinite(Number(audio?.projected_blocks))
        ? Number(audio.projected_blocks)
        : null,
      projector_samples: Number.isFinite(Number(audio?.projected_samples))
        ? Number(audio.projected_samples)
        : null,
      projector_reader_notifications: Number.isFinite(Number(audio?.reader_notifications))
        ? Number(audio.reader_notifications)
        : null,
      projector_reader_waits: Number.isFinite(Number(audio?.reader_waits))
        ? Number(audio.reader_waits)
        : null,
      projector_reader_wakeups: Number.isFinite(Number(audio?.reader_wakeups))
        ? Number(audio.reader_wakeups)
        : null,
      audio_producer_lock_wait_ms: metricMs("producer_lock_wait"),
      audio_producer_lock_wait_max_ms: metricMs("producer_lock_wait_max"),
      audio_ring_write_ms: metricMs("ring_write"),
      audio_ring_write_max_ms: metricMs("ring_write_max"),
      audio_archive_copy_ms: metricMs("archive_copy"),
      audio_archive_copy_max_ms: metricMs("archive_copy_max"),
      audio_consumer_lock_wait_ms: metricMs("consumer_lock_wait"),
      audio_consumer_lock_wait_max_ms: metricMs("consumer_lock_wait_max"),
      audio_ring_read_ms: metricMs("ring_read"),
      audio_ring_read_max_ms: metricMs("ring_read_max"),
      projector_overlap_before_finish_ms: firstProjectorFinish !== null && audioFinish !== null
        ? Math.max(0, Math.round((audioFinish - firstProjectorFinish) * 100) / 100)
        : null,
      audio_finish_ms: audioFinish,
      model_decode_start_ms: decodeStart,
      // These three values share the native incremental-generation clock.
      // They remain authoritative when renderer stream delivery is absent,
      // delayed, or filtered, and make a missed silence handoff distinguishable
      // from slow model decoding.
      audio_finish_to_decode_ms: nativeClockDelta(audioFinish, decodeStart),
      audio_finish_to_projector_tail_ms: nativeClockDelta(audioFinish, lastProjectorFinish),
      projector_tail_to_decode_ms: nativeClockDelta(lastProjectorFinish, decodeStart),
      audio_close_boundary_ms: nativeClockDelta(audioFinish, closingBoundaryFinish),
      audio_suffix_tokenize_ms: nativeClockDelta(closingBoundaryFinish, suffixTokenizeFinish),
      audio_suffix_eval_ms: nativeClockDelta(suffixTokenizeFinish, suffixEvalFinish),
      sampler_build_ms: nativeClockDelta(samplerStart, samplerReady),
      first_token_sample_ms: nativeClockDelta(firstSampleStart, firstSampleFinish),
      first_token_piece_and_emit_ms: nativeClockDelta(firstSampleFinish, firstTokenEvent),
      generation_sampled_tokens: Number.isFinite(Number(audio?.sampled_tokens))
        ? Number(audio.sampled_tokens) : null,
      generation_nonempty_token_pieces: Number.isFinite(Number(audio?.nonempty_token_pieces))
        ? Number(audio.nonempty_token_pieces) : null,
      generation_emitted_token_events: Number.isFinite(Number(audio?.emitted_token_events))
        ? Number(audio.emitted_token_events) : null,
      generation_last_sample_start_ms: lastSampleStart,
      generation_last_sample_finish_ms: lastSampleFinish,
      generation_last_token_event_ms: lastTokenEvent,
      generation_last_token_sample_ms: nativeClockDelta(lastSampleStart, lastSampleFinish),
      generation_continuation_decode_steps:
        Number.isFinite(Number(audio?.continuation_decode_steps))
          ? Number(audio.continuation_decode_steps) : null,
      generation_continuation_decode_cpu_ms: metricMs("continuation_decode"),
      generation_continuation_decode_max_ms: metricMs("continuation_decode_max"),
      generation_continuation_decode_inflight:
        Number.isFinite(Number(audio?.continuation_decode_start_us))
          ? !Number.isFinite(Number(audio?.continuation_decode_finish_us))
            || Number(audio.continuation_decode_start_us)
              > Number(audio.continuation_decode_finish_us)
          : null,
      decode_to_first_token_ms: nativeClockDelta(decodeStart, nativeFirstToken),
      silence_decision_delay_ms: silenceDecisionDelayMs,
      silence_release_to_first_token_native_ms: nativeClockDelta(audioFinish, nativeFirstToken),
      silence_to_first_token_native_ms: nativeClockDelta(audioFinish, nativeFirstToken) === null
        ? null
        : Math.round((nativeClockDelta(audioFinish, nativeFirstToken)
          + (silenceDecisionDelayMs ?? 0)) * 100) / 100,
      silence_release_to_native_release_ms:
        nativeDelta("silence_onset", "native_silence_released")
          ?? delta("silence_onset", "native_silence_released"),
      silence_to_native_release_ms: (
        nativeDelta("silence_onset", "native_silence_released")
          ?? delta("silence_onset", "native_silence_released")
      ) === null ? null : Math.round(((
        nativeDelta("silence_onset", "native_silence_released")
          ?? delta("silence_onset", "native_silence_released")
      ) + (silenceDecisionDelayMs ?? 0)) * 100) / 100,
      native_release_to_worker_ms: nativeDelta("native_silence_released", "native_generation_worker_started")
        ?? delta("native_silence_released", "native_generation_worker_started"),
      silence_release_to_dispatch_ms: absoluteDelta("silence_onset", "generation_dispatched")
        ?? delta("silence_onset", "generation_dispatched"),
      silence_to_dispatch_ms: physicalSilenceDelta("generation_dispatched"),
      dispatch_to_native_command_ms: absoluteDelta("generation_dispatched", "native_command_entered")
        ?? delta("generation_dispatched", "native_command_entered"),
      native_command_to_admission_ms: nativeDelta("native_command_entered", "native_admission_complete")
        ?? delta("native_command_entered", "native_admission_complete"),
      native_admission_to_worker_prearmed_ms: nativeDelta("native_admission_complete", "native_generation_worker_prearmed")
        ?? delta("native_admission_complete", "native_generation_worker_prearmed"),
      native_admission_to_capture_detach_ms: nativeDelta("native_admission_complete", "native_capture_detached")
        ?? delta("native_admission_complete", "native_capture_detached"),
      capture_detach_to_session_acquire_ms: nativeDelta("native_capture_detached", "native_voice_session_acquired")
        ?? delta("native_capture_detached", "native_voice_session_acquired"),
      dispatch_to_native_worker_ms: absoluteDelta("generation_dispatched", "native_generation_worker_started")
        ?? delta("generation_dispatched", "native_generation_worker_started"),
      native_worker_to_first_content_ms: absoluteDelta("native_generation_worker_started", "first_content_delta")
        ?? delta("native_generation_worker_started", "first_content_delta"),
      first_content_delivery_ms: absoluteDelta("first_content_delta", "first_content_delta_received"),
      silence_release_to_first_content_ms: absoluteDelta("silence_onset", "first_content_delta")
        ?? delta("silence_onset", "first_content_delta"),
      silence_to_first_content_ms: physicalSilenceDelta("first_content_delta"),
      first_content_to_tts_pcm_ms: firstContentThisAttempt
        ? absoluteDelta("first_content_delta", "tts_first_pcm")
          ?? delta("first_content_delta", "tts_first_pcm") : null,
      first_content_to_tts_chunk_ms: firstContentThisAttempt
        ? absoluteDelta("first_content_delta", "tts_first_chunk_ready")
          ?? delta("first_content_delta", "tts_first_chunk_ready") : null,
      tts_chunk_to_native_dispatch_ms: absoluteDelta("tts_first_chunk_ready", "tts_native_dispatched")
        ?? delta("tts_first_chunk_ready", "tts_native_dispatched"),
      tts_native_dispatch_to_pcm_ms: absoluteDelta("tts_native_dispatched", "tts_first_pcm")
        ?? delta("tts_native_dispatched", "tts_first_pcm"),
      generation_finished_to_tts_pcm_ms: absoluteDelta("generation_finished", "tts_first_pcm")
        ?? delta("generation_finished", "tts_first_pcm"),
      silence_release_to_tts_pcm_ms: absoluteDelta("silence_onset", "tts_first_pcm")
        ?? delta("silence_onset", "tts_first_pcm"),
      silence_to_tts_pcm_ms: physicalSilenceDelta("tts_first_pcm"),
      tts_worker_queue_ms: Number.isFinite(Number(tts?.queue_us))
        ? Math.round(Number(tts.queue_us) / 10) / 100
        : null,
      tts_host_route_ms: Number.isFinite(Number(tts?.host_route_us))
        ? Math.round(Number(tts.host_route_us) / 10) / 100
        : null,
      tts_host_queue_ms: Number.isFinite(Number(tts?.host_queue_us))
        ? Math.round(Number(tts.host_queue_us) / 10) / 100
        : null,
      tts_host_dispatch_ms: Number.isFinite(Number(tts?.host_dispatch_us))
        ? Math.round(Number(tts.host_dispatch_us) / 10) / 100
        : null,
      tts_host_admission_ms: Number.isFinite(Number(tts?.host_admission_us))
        ? Math.round(Number(tts.host_admission_us) / 10) / 100
        : null,
      tts_reference_cache: typeof tts?.reference_cache === "string"
        ? tts.reference_cache
        : null,
      tts_reference_admission_ms: Number.isFinite(Number(tts?.reference_admission_us))
        ? Math.round(Number(tts.reference_admission_us) / 10) / 100
        : null,
      tts_reference_ms: Number.isFinite(Number(tts?.reference_us))
        ? Math.round(Number(tts.reference_us) / 10) / 100
        : null,
      tts_reference_identity_ms: Number.isFinite(Number(tts?.reference_identity_us))
        ? Math.round(Number(tts.reference_identity_us) / 10) / 100
        : null,
      tts_reference_memory_lookup_ms: Number.isFinite(Number(tts?.reference_memory_lookup_us))
        ? Math.round(Number(tts.reference_memory_lookup_us) / 10) / 100
        : null,
      tts_reference_disk_lookup_ms: Number.isFinite(Number(tts?.reference_disk_lookup_us))
        ? Math.round(Number(tts.reference_disk_lookup_us) / 10) / 100
        : null,
      tts_reference_source_read_ms: Number.isFinite(Number(tts?.reference_source_read_us))
        ? Math.round(Number(tts.reference_source_read_us) / 10) / 100
        : null,
      tts_reference_source_hash_ms: Number.isFinite(Number(tts?.reference_source_hash_us))
        ? Math.round(Number(tts.reference_source_hash_us) / 10) / 100
        : null,
      tts_reference_compute_ms: Number.isFinite(Number(tts?.reference_compute_us))
        ? Math.round(Number(tts.reference_compute_us) / 10) / 100
        : null,
      tts_reference_persist_ms: Number.isFinite(Number(tts?.reference_persist_us))
        ? Math.round(Number(tts.reference_persist_us) / 10) / 100
        : null,
      tts_helper_first_pcm_ms: Number.isFinite(Number(tts?.helper_first_pcm_us))
        ? Math.round(Number(tts.helper_first_pcm_us) / 10) / 100
        : null,
      tts_backend_first_audio_ms: Number.isFinite(Number(tts?.time_to_first_audio_ms))
        ? Number(tts.time_to_first_audio_ms)
        : null,
      tts_segment_count: ttsSegments.length,
      tts_backend_ids: ttsIdentitySet("backend_id"),
      tts_model_ids: ttsIdentitySet("model_id"),
      tts_voice_ids: ttsIdentitySet("voice_id"),
      tts_total_synthesis_ms: Number.isFinite(Number(ttsComplete?.synthesis_ms))
        ? Number(ttsComplete.synthesis_ms)
        : null,
      tts_total_output_audio_ms: Number.isFinite(Number(ttsComplete?.output_audio_ms))
        ? Number(ttsComplete.output_audio_ms)
        : null,
      tts_aggregate_production_rtf: Number.isFinite(Number(ttsComplete?.production_rtf))
        ? Number(ttsComplete.production_rtf)
        : null,
      tts_pcm_packets: Number.isFinite(Number(ttsComplete?.pcm_packets))
        ? Number(ttsComplete.pcm_packets)
        : null,
      tts_pcm_audio_ms: Number.isFinite(Number(ttsComplete?.pcm_audio_ms))
        ? Number(ttsComplete.pcm_audio_ms)
        : null,
      tts_first_packet_audio_ms: Number.isFinite(Number(ttsComplete?.first_packet_audio_ms))
        ? Number(ttsComplete.first_packet_audio_ms)
        : null,
      tts_max_packet_gap_ms: Number.isFinite(Number(ttsComplete?.max_packet_gap_ms))
        ? Number(ttsComplete.max_packet_gap_ms)
        : null,
      tts_minimum_producer_lead_ms: Number.isFinite(Number(ttsComplete?.minimum_producer_lead_ms))
        ? Number(ttsComplete.minimum_producer_lead_ms)
        : null,
      tts_scheduler_target_runway_ms: Number.isFinite(Number(ttsComplete?.scheduler?.target_runway_ms))
        ? Number(ttsComplete.scheduler.target_runway_ms)
        : null,
      tts_scheduler_startup_runway_ms: Number.isFinite(Number(ttsComplete?.scheduler?.startup_runway_ms))
        ? Number(ttsComplete.scheduler.startup_runway_ms)
        : null,
      tts_scheduler_observed_packet_gap_ms:
        Number.isFinite(Number(ttsComplete?.scheduler?.observed_max_packet_gap_ms))
          ? Number(ttsComplete.scheduler.observed_max_packet_gap_ms)
          : null,
      tts_scheduler_observed_boundary_gap_ms:
        Number.isFinite(Number(ttsComplete?.scheduler?.observed_max_boundary_gap_ms))
          ? Number(ttsComplete.scheduler.observed_max_boundary_gap_ms)
          : null,
      tts_scheduler_waits: Number.isFinite(Number(ttsComplete?.scheduler?.waits))
        ? Number(ttsComplete.scheduler.waits)
        : null,
      tts_scheduler_wait_ms: Number.isFinite(Number(ttsComplete?.scheduler?.wait_ms))
        ? Number(ttsComplete.scheduler.wait_ms)
        : null,
      tts_scheduler_maximum_runway_ms: Number.isFinite(Number(ttsComplete?.scheduler?.maximum_runway_ms))
        ? Number(ttsComplete.scheduler.maximum_runway_ms)
        : null,
      tts_scheduler_observed_rtf: Number.isFinite(Number(ttsComplete?.scheduler?.observed_production_rtf))
        ? Number(ttsComplete.scheduler.observed_production_rtf)
        : null,
      tts_scheduler_underrun_penalty_ms: Number.isFinite(Number(ttsComplete?.scheduler?.underrun_penalty_ms))
        ? Number(ttsComplete.scheduler.underrun_penalty_ms)
        : null,
      semantic_backpressure_switches: Number.isFinite(Number(semanticBackpressure?.switches))
        ? Number(semanticBackpressure.switches) : null,
      semantic_backpressure_pause_ms: Number.isFinite(Number(semanticBackpressure?.pause_ms))
        ? Number(semanticBackpressure.pause_ms) : null,
      semantic_backpressure_command_ms: Number.isFinite(Number(semanticBackpressure?.command_ms))
        ? Number(semanticBackpressure.command_ms) : null,
      semantic_backpressure_generation_yields: Number.isFinite(Number(semanticBackpressure?.generation_yields))
        ? Number(semanticBackpressure.generation_yields) : null,
      semantic_backpressure_generation_yield_ms: Number.isFinite(Number(semanticBackpressure?.generation_yield_ms))
        ? Number(semanticBackpressure.generation_yield_ms) : null,
      semantic_backpressure_maximum_words: Number.isFinite(Number(semanticBackpressure?.maximum_queued_words))
        ? Number(semanticBackpressure.maximum_queued_words) : null,
      tts_worst_segment_rtf: ttsSegments.reduce((worst, segment) => {
        const candidate = Number(segment.production_rtf);
        return Number.isFinite(candidate) ? Math.max(worst, candidate) : worst;
      }, 0) || null,
      tts_compute_lease_acquisitions: measuredTtsSum("compute_lease_acquisitions"),
      tts_compute_lease_releases: measuredTtsSum("compute_lease_releases"),
      tts_generation_yields: ttsSegments.reduce((total, segment) => (
        total + (Number.isFinite(Number(segment.generation_yields))
          ? Number(segment.generation_yields) : 0)
      ), 0) || null,
      tts_generation_yield_ms: Math.round(ttsSegments.reduce((total, segment) => (
        total + (Number.isFinite(Number(segment.generation_yield_us))
          ? Number(segment.generation_yield_us) : 0)
      ), 0) / 10) / 100 || null,
      tts_generation_fair_passes: ttsSegments.reduce((total, segment) => (
        total + (Number.isFinite(Number(segment.generation_fair_passes))
          ? Number(segment.generation_fair_passes) : 0)
      ), 0) || null,
      tts_compute_lease_held_ms: measuredTtsSum("compute_lease_held_us", 1_000),
      tts_compute_lease_released_ms: measuredTtsSum("compute_lease_released_us", 1_000),
      tts_compute_minimum_lead_ms: ttsMetricValues("compute_minimum_lead_ms").length
        ? Math.min(...ttsMetricValues("compute_minimum_lead_ms")) : null,
      tts_compute_maximum_lead_ms: ttsMetricValues("compute_maximum_lead_ms").length
        ? Math.max(...ttsMetricValues("compute_maximum_lead_ms")) : null,
      tts_compute_low_water_ms: ttsMetricValues("compute_low_water_ms").length
        ? Math.max(...ttsMetricValues("compute_low_water_ms")) : null,
      tts_compute_high_water_ms: ttsMetricValues("compute_high_water_ms").length
        ? Math.max(...ttsMetricValues("compute_high_water_ms")) : null,
      tts_all_segments_buffered_runway_ms: Number.isFinite(Number(ttsComplete?.buffered_runway_ms))
        ? Number(ttsComplete.buffered_runway_ms)
        : null,
      tts_to_next_prefill_start_ms: delta("tts_all_segments_finished", "next_turn_prefill_started"),
      next_prefill_prepare_ms: delta("next_turn_prefill_started", "next_turn_prefill_ready"),
      next_prefill_buffered_runway_ms: Number.isFinite(Number(nextPrefillReady?.buffered_runway_ms))
        ? Number(nextPrefillReady.buffered_runway_ms)
        : null,
      next_prefill_overlapped_active_tts: Number.isFinite(Number(nextPrefillStarted?.elapsed_ms))
          && Number.isFinite(Number(ttsComplete?.elapsed_ms))
        ? Number(nextPrefillStarted.elapsed_ms) < Number(ttsComplete.elapsed_ms)
        : null,
      tts_pcm_to_playback_ms: absoluteDelta(
        "tts_first_pcm",
        playbackRendered ? "playback_render_started" : "playback_started",
      ) ?? delta(
        "tts_first_pcm",
        playbackRendered ? "playback_render_started" : "playback_started",
      ),
      playback_context_resume_to_first_render_ms: playbackRendered
        ? delta("playback_started", "playback_render_started") : null,
      playback_startup_target_ms: Number.isFinite(Number(playbackStarted?.startup_target_ms))
        ? Number(playbackStarted.startup_target_ms) : null,
      playback_startup_buffered_ms: Number.isFinite(Number(playbackStarted?.buffered_ms))
        ? Number(playbackStarted.buffered_ms) : null,
      silence_release_to_playback_ms: absoluteDelta(
        "silence_onset",
        playbackRendered ? "playback_render_started" : "playback_started",
      ) ?? delta(
        "silence_onset",
        playbackRendered ? "playback_render_started" : "playback_started",
      ),
      silence_to_playback_ms: physicalSilenceDelta(
        playbackRendered ? "playback_render_started" : "playback_started",
      ),
      playback_renderer_worklet: typeof playbackRenderer?.worklet === "boolean"
        ? playbackRenderer.worklet
        : null,
      playback_renderer_reason: typeof playbackRenderer?.reason === "string"
        ? playbackRenderer.reason
        : null,
      playback_renderer_packets: Number.isFinite(Number(playbackRenderer?.packets))
        ? Number(playbackRenderer.packets)
        : null,
      playback_renderer_frames: Number.isFinite(Number(playbackRenderer?.frames))
        ? Number(playbackRenderer.frames)
        : null,
      playback_renderer_schedule_cpu_ms: Number.isFinite(Number(playbackRenderer?.schedule_cpu_us))
        ? Math.round(Number(playbackRenderer.schedule_cpu_us) / 10) / 100
        : null,
      playback_renderer_schedule_max_ms: Number.isFinite(Number(playbackRenderer?.schedule_max_us))
        ? Math.round(Number(playbackRenderer.schedule_max_us) / 10) / 100
        : null,
      // These samples reached the AudioWorklet output only; they do not
      // establish what reached the physical speaker.
      playback_renderer_metrics_complete: playbackRenderer?.worklet === true
        ? playbackWorkletMetricsReported : null,
      playback_renderer_underruns: playbackRendererCountersKnown
        && typeof playbackRenderer?.underruns === "number" && Number.isFinite(playbackRenderer.underruns)
        ? playbackRenderer.underruns : null,
      playback_renderer_pcm_signal_frames: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_pcm_signal_frames)
        ? playbackRenderer.worklet_pcm_signal_frames : null,
      playback_renderer_pcm_signal_rms: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_pcm_signal_rms)
        ? playbackRenderer.worklet_pcm_signal_rms : null,
      playback_renderer_pcm_signal_peak: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_pcm_signal_peak)
        ? playbackRenderer.worklet_pcm_signal_peak : null,
      playback_renderer_pcm_signal_min_quantum_rms: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_pcm_signal_min_quantum_rms)
        ? playbackRenderer.worklet_pcm_signal_min_quantum_rms : null,
      playback_renderer_pcm_signal_max_quantum_rms: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_pcm_signal_max_quantum_rms)
        ? playbackRenderer.worklet_pcm_signal_max_quantum_rms : null,
      playback_renderer_pcm_low_energy_threshold_rms: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_pcm_low_energy_threshold_rms)
        ? playbackRenderer.worklet_pcm_low_energy_threshold_rms : null,
      playback_renderer_pcm_low_energy_frames: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_pcm_low_energy_frames)
        ? playbackRenderer.worklet_pcm_low_energy_frames : null,
      playback_renderer_pcm_low_energy_quanta: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_pcm_low_energy_quanta)
        ? playbackRenderer.worklet_pcm_low_energy_quanta : null,
      playback_renderer_pcm_max_low_energy_frames: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_pcm_max_low_energy_frames)
        ? playbackRenderer.worklet_pcm_max_low_energy_frames : null,
      playback_renderer_recoveries: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_recoveries)
        ? playbackRenderer.worklet_recoveries : null,
      playback_renderer_rebuffered_frames: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_rebuffered_frames)
        ? playbackRenderer.worklet_rebuffered_frames : null,
      playback_renderer_max_buffered_frames: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_max_buffered_frames)
        ? playbackRenderer.worklet_max_buffered_frames : null,
      playback_renderer_max_queued_frames: typeof playbackRenderer?.worklet_max_queued_frames === "number" && Number.isFinite(playbackRenderer.worklet_max_queued_frames)
        ? playbackRenderer.worklet_max_queued_frames : null,
      playback_renderer_min_buffered_frames: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_min_buffered_frames)
        ? playbackRenderer.worklet_min_buffered_frames : null,
      playback_renderer_first_receive_context_frame: playbackWorkletMetricsReported
        && Number.isFinite(playbackRenderer?.worklet_first_receive_context_frame)
        ? playbackRenderer.worklet_first_receive_context_frame : null,
      playback_renderer_first_render_context_frame: typeof playbackRenderer?.worklet_first_render_context_frame === "number" && Number.isFinite(playbackRenderer.worklet_first_render_context_frame)
        ? playbackRenderer.worklet_first_render_context_frame : null,
      playback_underrun_occurrences: Number.isFinite(Number(playbackUnderrun?.occurrences))
        ? Number(playbackUnderrun.occurrences)
        : 0,
      playback_underrun_total_gap_ms: Number.isFinite(Number(playbackUnderrun?.total_gap_ms))
        ? Number(playbackUnderrun.total_gap_ms)
        : 0,
      playback_underrun_max_gap_ms: Number.isFinite(Number(playbackUnderrun?.max_gap_ms))
        ? Number(playbackUnderrun.max_gap_ms)
        : 0,
    };
  };

  const persistVoiceLatency = (serialized) => {
    // Preserve checkpoint order without putting file I/O or native admission
    // on the renderer's latency-critical path. A force-quit after dispatch or
    // first PCM must still leave the last completed stage on disk.
    voiceLatencyPersistence = voiceLatencyPersistence
      .catch(() => {})
      .then(() => invoke("mom_llama_voice_latency_record", { trace: serialized }))
      .catch(() => {});
  };

  const flushVoiceLatencyCheckpoints = () => {
    voiceLatencyFlushTimer = null;
    const pending = [...voiceLatencyPending.values()];
    voiceLatencyPending.clear();
    pending.forEach((trace) => {
      // Clone and serialize after the latency-critical event callback returns.
      // Multiple marks for one trace intentionally coalesce to the newest
      // checkpoint; the stage array still contains every individual timing.
      const snapshot = JSON.parse(JSON.stringify(trace));
      // Derive metrics before byte compaction so a bounded receipt keeps the
      // complete summary even when repetitive detail must be omitted.
      const summary = voiceLatencySummary(snapshot);
      const serialized = compactVoiceLatencySnapshot(snapshot, summary);
      voiceLatencyHistory.push(JSON.parse(serialized));
      if (voiceLatencyHistory.length > 20) voiceLatencyHistory.splice(0, voiceLatencyHistory.length - 20);
      persistVoiceLatency(serialized);
    });
  };

  const queueVoiceLatencyCheckpoint = (trace) => {
    voiceLatencyPending.set(trace.id, trace);
    if (voiceLatencyFlushTimer !== null) return;
    // localStorage JSON parsing and synchronous history rewrites used to run
    // inside first-token/first-PCM callbacks. One short coalescing task keeps
    // durable native receipts without delaying playback or the next capture.
    voiceLatencyFlushTimer = window.setTimeout(flushVoiceLatencyCheckpoints, 50);
  };

  // Preserve each pipeline attempt. A call can speculatively start and belay
  // several times; merging those transitions made unrelated starts and ends
  // look like one multi-second operation. Only the semantically singular
  // first-voice milestone is coalesced.
  const COALESCED_VOICE_LATENCY_STAGES = new Set(["voice_activity_first_detected"]);
  const VOICE_LATENCY_STAGE_LIMIT = 512;
  const VOICE_LATENCY_TRACE_BYTE_LIMIT = 64 * 1024;
  const VOICE_LATENCY_OWNER_BUCKET_LIMIT = 128;
  const VOICE_LATENCY_HIGH_VOLUME_STAGES = new Set([
    "native_chat_stream_event",
    "chat_stream_event_received",
    "chat_stream_route_decision",
    "playback_packet_enqueued",
    "playback_packet_admitted",
  ]);
  const VOICE_LATENCY_CHECKPOINT_STAGES = new Set([
    "call_requested", "call_start_failed", "call_ringing_started", "call_activity_guard_wait_started",
    "call_activity_guard_ready", "call_history_recovery_suspend_wait_started",
    "call_history_recovery_suspend_wait_finished", "call_conversation_ready",
    "audio_prefill_begin_requested", "audio_prefill_session_ready",
    "audio_prefill_begin_failed", "audio_prefill_push_failed",
    "audio_prefill_wait_expired", "audio_prefill_drain_failed",
    "audio_prefill_producer_overflow", "native_capture_started", "capture_stopped",
    "capture_processing", "voice_inference_archive", "native_capture_failed",
    "generation_dispatched", "speculation_dispatch_started", "native_silence_armed",
    "native_silence_released", "native_generation_worker_started", "generation_finished",
    "voice_transcript_started", "voice_transcript_inference_started",
    "voice_transcript_inference_finished", "voice_transcript_finished",
    "voice_transcript_skipped", "voice_transcript_failed", "first_content_delta",
    "first_content_delta_received", "tts_first_chunk_ready", "tts_native_dispatched",
    "tts_first_pcm", "tts_segment_finished", "tts_all_segments_finished",
    "tts_segment_dispatched", "tts_segment_first_pcm", "tts_segment_pcm_complete",
    "tts_segment_receipt_received", "tts_backend_complete", "tts_pcm_complete",
    "next_turn_prefill_started", "next_turn_prefill_ready", "next_turn_prefill_failed",
    "barge_in_detected", "barge_in_playback_stopped", "barge_in_playback_stop_failed",
    "playable", "playback_started", "playback_render_started",
    "playback_underrun_started", "playback_underrun_recovered", "durable_voice_audio_ready",
    "durable_voice_audio_failed", "playback_underrun", "playback_renderer",
    "speculation_handoff_ready", "speculation_handoff_failed", "failed",
  ]);
  const VOICE_LATENCY_LIFECYCLE_EVENTS = new Set([
    "started", "prefill_started", "decode_started", "completed", "cancelled", "failed",
  ]);

  const voiceLatencyUtf8Bytes = (value) => new TextEncoder().encode(value).byteLength;

  const noteVoiceLatencyCompaction = (trace, field, amount = 1) => {
    trace.compaction ||= {};
    trace.compaction[field] = (Number(trace.compaction[field]) || 0) + Math.max(0, Number(amount) || 0);
  };

  const isVoiceLatencyCriticalRecord = (record) => {
    if (VOICE_LATENCY_CHECKPOINT_STAGES.has(record.stage)) return true;
    if (record.stage === "native_chat_stream_event"
      || record.stage === "chat_stream_event_received" || record.stage === "chat_stream_route_decision") {
      return VOICE_LATENCY_LIFECYCLE_EVENTS.has(record.event);
    }
    return false;
  };

  const appendVoiceLatencyStage = (trace, record) => {
    if (trace.stages.length >= VOICE_LATENCY_STAGE_LIMIT) {
      let removeAt = trace.stages.findIndex((entry) => entry.voice_latency_aggregate === true);
      if (removeAt < 0 && isVoiceLatencyCriticalRecord(record)) {
        removeAt = trace.stages.findIndex((entry) => !isVoiceLatencyCriticalRecord(entry));
      }
      if (removeAt < 0 && isVoiceLatencyCriticalRecord(record)) removeAt = 0;
      if (removeAt < 0) {
        noteVoiceLatencyCompaction(trace, "stage_limit_events_omitted");
        return null;
      }
      const [removed] = trace.stages.splice(removeAt, 1);
      noteVoiceLatencyCompaction(trace, "stage_limit_records_omitted");
      noteVoiceLatencyCompaction(trace, "stage_limit_events_omitted", removed?.occurrences || 1);
    }
    trace.stages.push(record);
    return record;
  };

  const aggregateVoiceLatencyStage = (trace, stage, detail, elapsedMs, observedAtUnixUs) => {
    const isStream = stage === "native_chat_stream_event"
      || stage === "chat_stream_event_received" || stage === "chat_stream_route_decision";
    if (isStream && !["delta", "reasoning_delta"].includes(detail.event)) return { handled: false };
    if (!VOICE_LATENCY_HIGH_VOLUME_STAGES.has(stage)) return { handled: false };
    const ownerType = isStream ? "request" : "operation";
    const rawOwner = isStream ? detail.request_id : detail.operation_id;
    if (!rawOwner) noteVoiceLatencyCompaction(trace,
      isStream ? "unowned_stream_events" : "unowned_packet_events");
    const ownerId = typeof rawOwner === "string" && rawOwner.length <= 128
      ? rawOwner : (rawOwner ? String(rawOwner).slice(0, 128) : "unattributed");
    let recorded = trace.stages.find((entry) => entry.voice_latency_aggregate === true
      && entry.stage === stage && entry.owner_type === ownerType && entry.owner_id === ownerId);
    if (!recorded) {
      const ownerCount = new Set(trace.stages
        .filter((entry) => entry.voice_latency_aggregate === true && entry.stage === stage)
        .map((entry) => `${entry.owner_type}:${entry.owner_id}`)).size;
      if (ownerCount >= VOICE_LATENCY_OWNER_BUCKET_LIMIT) {
        noteVoiceLatencyCompaction(trace, "owner_bucket_events_omitted");
        return { handled: true, recorded: null };
      }
      recorded = appendVoiceLatencyStage(trace, {
        stage,
        owner_type: ownerType,
        owner_id: ownerId,
        request_id: isStream ? ownerId : null,
        operation_id: isStream ? null : ownerId,
        conversation_id: detail.conversation_id || null,
        event: detail.event || null,
        first_elapsed_ms: elapsedMs,
        first_observed_at_unix_us: observedAtUnixUs,
        elapsed_ms: elapsedMs,
        observed_at_unix_us: observedAtUnixUs,
        occurrences: 0,
        voice_latency_aggregate: true,
      });
      if (!recorded) return { handled: true, recorded: null };
    }
    recorded.elapsed_ms = elapsedMs;
    recorded.observed_at_unix_us = observedAtUnixUs;
    recorded.last_event = detail.event || recorded.last_event || null;
    recorded.occurrences += 1;
    if (stage === "chat_stream_route_decision") {
      recorded.same_live_conversation_true = (recorded.same_live_conversation_true || 0)
        + (detail.same_live_conversation ? 1 : 0);
      recorded.owns_live_voice_true = (recorded.owns_live_voice_true || 0)
        + (detail.owns_live_voice ? 1 : 0);
      recorded.last_live_request_id = detail.live_request_id || null;
    } else if (stage === "native_chat_stream_event") {
      recorded.total_delta_bytes = (recorded.total_delta_bytes || 0) + Math.max(0, Number(detail.delta_bytes) || 0);
      recorded.last_kind = detail.kind || recorded.last_kind || null;
      const nativeTime = Number(detail.native_emitted_at_unix_us);
      if (Number.isFinite(nativeTime)) {
        recorded.first_native_emitted_at_unix_us ??= nativeTime;
        recorded.last_native_emitted_at_unix_us = nativeTime;
      }
    } else if (stage === "playback_packet_enqueued" || stage === "playback_packet_admitted") {
      const frames = Math.max(0, Number(detail.frames) || 0);
      recorded.total_frames = (recorded.total_frames || 0) + frames;
      recorded.packet_count = (recorded.packet_count || 0) + 1;
      recorded.first_packet_index ??= Number(detail.packet_index) || 0;
      recorded.first_sequence ??= Number(detail.sequence) || 0;
      recorded.first_sample_offset ??= Number(detail.sample_offset) || 0;
      recorded.last_packet_index = Number(detail.packet_index) || 0;
      recorded.last_sequence = Number(detail.sequence) || 0;
      recorded.last_sample_offset = Number(detail.sample_offset) || 0;
      const audioContextMs = Number(detail.audio_context_ms);
      if (Number.isFinite(audioContextMs)) {
        recorded.first_audio_context_ms ??= audioContextMs;
        recorded.last_audio_context_ms = audioContextMs;
      }
      const queued = Number(detail.queued_frames);
      const buffered = Number(detail.buffered_frames);
      const delay = Number(detail.enqueue_to_admission_ms);
      if (Number.isFinite(queued)) recorded.max_queued_frames = Math.max(recorded.max_queued_frames || 0, queued);
      if (Number.isFinite(buffered)) recorded.max_buffered_frames = Math.max(recorded.max_buffered_frames || 0, buffered);
      if (Number.isFinite(delay)) recorded.max_enqueue_to_admission_ms = Math.max(recorded.max_enqueue_to_admission_ms || 0, delay);
      if (detail.operation_owner_missing) recorded.operation_owner_missing_packets = (recorded.operation_owner_missing_packets || 0) + 1;
    }
    return { handled: true, recorded };
  };

  const compactVoiceLatencySnapshot = (snapshot, summary) => {
    const serialize = () => JSON.stringify({ ...snapshot, summary });
    const envelopeBytes = () => voiceLatencyUtf8Bytes(JSON.stringify({ ...snapshot, stages: [], summary }));
    const stageBytes = () => snapshot.stages.map((entry) => voiceLatencyUtf8Bytes(JSON.stringify(entry)));
    const estimatedBytes = () => {
      const sizes = stageBytes();
      return envelopeBytes() + sizes.reduce((total, bytes) => total + bytes, 0) + Math.max(0, sizes.length - 1);
    };

    // High-volume detail has already been aggregated by owner. Under receipt
    // pressure discard aggregate buckets first, then noncritical diagnostics,
    // without serializing the full trace once per discarded stage.
    let sizes = stageBytes();
    const dropSelected = (predicate) => {
      const kept = [];
      let removedRecords = 0;
      let removedEvents = 0;
      snapshot.stages.forEach((entry, index) => {
        if (predicate(entry)) {
          removedRecords += 1;
          removedEvents += Number(entry?.occurrences) || 1;
        } else {
          kept.push(entry);
        }
      });
      if (removedRecords) {
        snapshot.stages = kept;
        noteVoiceLatencyCompaction(snapshot, "byte_limit_records_omitted", removedRecords);
        noteVoiceLatencyCompaction(snapshot, "byte_limit_events_omitted", removedEvents);
      }
    };
    if (estimatedBytes() > VOICE_LATENCY_TRACE_BYTE_LIMIT) {
      dropSelected((entry) => entry.voice_latency_aggregate === true);
    }
    if (estimatedBytes() > VOICE_LATENCY_TRACE_BYTE_LIMIT) {
      sizes = stageBytes();
      let bytesToRemove = estimatedBytes() - VOICE_LATENCY_TRACE_BYTE_LIMIT;
      let removeBytes = 0;
      let removeCount = 0;
      for (let index = 0; index < snapshot.stages.length && removeBytes < bytesToRemove; index += 1) {
        if (!isVoiceLatencyCriticalRecord(snapshot.stages[index])) {
          removeBytes += (sizes[index] || 0) + 1;
          removeCount += 1;
        }
      }
      let remaining = removeCount;
      dropSelected((entry) => !isVoiceLatencyCriticalRecord(entry) && remaining-- > 0);
    }
    if (estimatedBytes() > VOICE_LATENCY_TRACE_BYTE_LIMIT) {
      // Preserve lifecycle identity/timing while discarding only verbose
      // metric fields. These receipts explicitly mark that detail was omitted.
      const keep = new Set([
        "stage", "elapsed_ms", "observed_at_unix_us", "native_emitted_at_unix_us",
        "first_elapsed_ms", "first_observed_at_unix_us", "request_id", "operation_id",
        "conversation_id", "event", "owner_type", "owner_id", "occurrences",
      ]);
      let fieldsRemoved = 0;
      for (const entry of snapshot.stages) {
        for (const key of Object.keys(entry)) {
          if (!keep.has(key)) {
            delete entry[key];
            fieldsRemoved += 1;
          }
        }
      }
      if (fieldsRemoved) noteVoiceLatencyCompaction(snapshot, "byte_limit_detail_fields_omitted", fieldsRemoved);
    }
    if (estimatedBytes() > VOICE_LATENCY_TRACE_BYTE_LIMIT && snapshot.stages.length > 1) {
      // As a last pressure step retain the latest lifecycle milestone.
      sizes = stageBytes();
      let bytesToRemove = estimatedBytes() - VOICE_LATENCY_TRACE_BYTE_LIMIT;
      let removeCount = 0;
      for (let index = 0; index < snapshot.stages.length - 1 && bytesToRemove > 0; index += 1) {
        bytesToRemove -= (sizes[index] || 0) + 1;
        removeCount += 1;
      }
      if (removeCount) {
        const removed = snapshot.stages.splice(0, removeCount);
        noteVoiceLatencyCompaction(snapshot, "byte_limit_records_omitted", removed.length);
        noteVoiceLatencyCompaction(snapshot, "byte_limit_events_omitted",
          removed.reduce((total, entry) => total + (Number(entry?.occurrences) || 1), 0));
      }
    }

    if (Object.values(snapshot.compaction || {}).some((count) => Number(count) > 0)) {
      summary.incomplete = true;
    }
    let serialized = serialize();
    if (voiceLatencyUtf8Bytes(serialized) > VOICE_LATENCY_TRACE_BYTE_LIMIT) {
      // The fixed summary is normally small. If a malformed/custom summary
      // itself exceeds the receipt budget, retain core scalar outcomes and
      // make the summary loss explicit rather than looping or dropping it.
      const compactSummary = {};
      for (const key of [
        "silence_to_first_content_ms", "silence_to_first_token_ms", "silence_to_tts_pcm_ms",
        "generation_first_token_ms", "generation_finish_ms", "capture_to_first_sample_ms",
        "barge_in_playback_stop_ms", "call_activity_guard_wait_ms",
        "call_history_recovery_suspend_wait_ms", "playback_underrun_total_gap_ms",
        "playback_underrun_max_gap_ms", "playback_renderer_metrics_complete",
        "playback_renderer_underruns", "playback_renderer_pcm_signal_frames",
        "playback_renderer_pcm_signal_rms", "playback_renderer_pcm_signal_peak",
        "playback_renderer_pcm_signal_min_quantum_rms",
        "playback_renderer_pcm_signal_max_quantum_rms",
        "playback_renderer_pcm_low_energy_threshold_rms",
        "playback_renderer_pcm_low_energy_frames",
        "playback_renderer_pcm_low_energy_quanta",
        "playback_renderer_pcm_max_low_energy_frames",
        "playback_renderer_recoveries", "playback_renderer_rebuffered_frames",
        "playback_renderer_max_buffered_frames", "playback_renderer_max_queued_frames",
        "playback_renderer_min_buffered_frames", "playback_renderer_first_receive_context_frame",
        "playback_renderer_first_render_context_frame", "tts_segments", "tts_total_ms",
      ]) {
        const value = summary?.[key];
        if (value === null || typeof value === "number" || typeof value === "boolean") compactSummary[key] = value;
      }
      compactSummary.incomplete = true;
      snapshot.compaction ||= {};
      snapshot.compaction.summary_fields_omitted = Object.keys(summary || {}).length - Object.keys(compactSummary).length + 1;
      const lastStage = snapshot.stages.at(-1);
      const minimal = {
        id: snapshot.id,
        mode: snapshot.mode,
        began_at: snapshot.began_at,
        checkpoint: snapshot.checkpoint,
        compaction: snapshot.compaction,
        stages: lastStage ? [{
          stage: lastStage.stage,
          elapsed_ms: lastStage.elapsed_ms,
          observed_at_unix_us: lastStage.observed_at_unix_us,
          request_id: lastStage.request_id,
          operation_id: lastStage.operation_id,
          occurrences: lastStage.occurrences,
        }] : [],
        summary: compactSummary,
      };
      serialized = JSON.stringify(minimal);
      if (voiceLatencyUtf8Bytes(serialized) > VOICE_LATENCY_TRACE_BYTE_LIMIT) {
        // This final form contains only bounded generated identifiers and
        // scalar fields; it is a defensive guarantee against a future schema
        // accidentally making the summary or trace envelope unbounded.
        serialized = JSON.stringify({
          id: String(snapshot.id || "").slice(0, 64),
          mode: String(snapshot.mode || "").slice(0, 64),
          checkpoint: Number(snapshot.checkpoint) || 0,
          compaction: { receipt_fallback: true, records_omitted: snapshot.compaction?.byte_limit_records_omitted || 0 },
          stages: [],
          summary: { incomplete: true },
        });
      }
    }
    return serialized;
  };

  const markVoiceLatencyIn = (trace, stage, detail = {}) => {
    if (!trace) return;
    const markStarted = performance.now();
    const elapsedMs = Math.round(performance.now() - trace.started);
    const timeOrigin = Number(performance.timeOrigin);
    const observedAtUnixUs = Number.isFinite(timeOrigin)
      ? Math.round((timeOrigin + markStarted) * 1000)
      : Date.now() * 1000;
    let recorded = null;
    let firstOccurrence = true;
    const aggregation = aggregateVoiceLatencyStage(trace, stage, detail, elapsedMs, observedAtUnixUs);
    recorded = aggregation.recorded || null;
    if (aggregation.handled) {
      firstOccurrence = false;
    } else if (stage === "playback_consumed") {
      let consumed = trace.stages.find((entry) => entry.stage === stage);
      const frames = Math.max(0, Number(detail.frames) || 0);
      const buffered = Math.max(0, Number(detail.buffered_frames) || 0);
      if (!consumed) {
        consumed = appendVoiceLatencyStage(trace, {
          stage, elapsed_ms: elapsedMs, observed_at_unix_us: observedAtUnixUs,
          first_elapsed_ms: elapsedMs, first_observed_at_unix_us: observedAtUnixUs,
          occurrences: 0, total_frames: 0,
          min_buffered_frames: buffered, max_buffered_frames: buffered,
        });
      }
      if (consumed) {
        consumed.elapsed_ms = elapsedMs;
        consumed.observed_at_unix_us = observedAtUnixUs;
        consumed.occurrences += 1;
        consumed.total_frames += frames;
        consumed.min_buffered_frames = Math.min(consumed.min_buffered_frames, buffered);
        consumed.max_buffered_frames = Math.max(consumed.max_buffered_frames, buffered);
        consumed.rendered_frames = Number(detail.rendered_frames) || 0;
        consumed.queued_frames = Number(detail.queued_frames) || 0;
        consumed.audio_context_ms = Number(detail.audio_context_ms) || 0;
      }
      recorded = consumed;
    } else if (stage === "playback_underrun") {
      let underrun = trace.stages.find((entry) => entry.stage === stage);
      if (!underrun) {
        underrun = appendVoiceLatencyStage(trace, {
          stage, elapsed_ms: elapsedMs, occurrences: 0, total_gap_ms: 0, max_gap_ms: 0,
        });
      }
      if (!underrun) return;
      const gapMs = Math.max(0, Number(detail.gap_ms) || 0);
      underrun.elapsed_ms = elapsedMs;
      underrun.occurrences += 1;
      underrun.total_gap_ms = Math.round((underrun.total_gap_ms + gapMs) * 100) / 100;
      underrun.max_gap_ms = Math.max(underrun.max_gap_ms, gapMs);
      recorded = underrun;
    } else if (COALESCED_VOICE_LATENCY_STAGES.has(stage)) {
      recorded = trace.stages.find((entry) => entry.stage === stage);
      if (recorded) {
        firstOccurrence = false;
        const firstElapsedMs = recorded.first_elapsed_ms ?? recorded.elapsed_ms;
        const firstObservedAtUnixUs = recorded.first_observed_at_unix_us ?? recorded.observed_at_unix_us;
        Object.assign(recorded, detail, {
          elapsed_ms: elapsedMs,
          observed_at_unix_us: observedAtUnixUs,
          first_elapsed_ms: firstElapsedMs,
          first_observed_at_unix_us: firstObservedAtUnixUs,
          occurrences: (recorded.occurrences || 1) + 1,
        });
      } else {
        recorded = { stage, elapsed_ms: elapsedMs, observed_at_unix_us: observedAtUnixUs,
          first_elapsed_ms: elapsedMs, first_observed_at_unix_us: observedAtUnixUs,
          occurrences: 1, ...detail };
        recorded = appendVoiceLatencyStage(trace, recorded);
      }
    } else {
      recorded = { stage, elapsed_ms: elapsedMs, observed_at_unix_us: observedAtUnixUs, ...detail };
      recorded = appendVoiceLatencyStage(trace, recorded);
    }
    const lifecycleStreamEvent = (stage === "native_chat_stream_event"
      || stage === "chat_stream_event_received" || stage === "chat_stream_route_decision")
      && VOICE_LATENCY_LIFECYCLE_EVENTS.has(detail.event);
    const checkpoint = VOICE_LATENCY_CHECKPOINT_STAGES.has(stage) || lifecycleStreamEvent;
    if (checkpoint && (firstOccurrence || !COALESCED_VOICE_LATENCY_STAGES.has(stage))) {
      trace.checkpoint = (trace.checkpoint || 0) + 1;
      queueVoiceLatencyCheckpoint(trace);
    }
    if (recorded) recorded.instrumentation_us = Math.round((performance.now() - markStarted) * 1000);
  };

  const markVoiceLatency = (stage, detail = {}) => {
    if (!voiceLatencyTrace) {
      voiceLatencyTrace = {
        id: crypto.randomUUID(), began_at: Date.now(), started: performance.now(), stages: [],
      };
    }
    markVoiceLatencyIn(voiceLatencyTrace, stage, detail);
  };

  const startVoiceLatency = (mode, initialStage = "capture_started") => {
    voiceLatencyTrace = { id: crypto.randomUUID(), mode, began_at: Date.now(), started: performance.now(), stages: [] };
    markVoiceLatency(initialStage);
    markVoiceLatency("runtime_warm_state", runtimeWarmState);
    markVoiceLatency("pcm_worklet_warm_state", pcmWorkletWarmState);
  };

  const takeLiveSpeechChunk = (
    text,
    flush = false,
    first = false,
    speechMode = "call",
  ) => {
    const words = [...text.matchAll(/\S+/g)];
    if (!words.length) return null;
    const splitAt = (index, boundary) => {
      const end = words[index].index + words[index][0].length;
      return { chunk: text.slice(0, end).trim(), rest: text.slice(end).trimStart(), boundary };
    };
    if (speechMode === "call") {
      // Each Qwen request starts a fresh acoustic trajectory. Let it see a
      // complete sentence; audio runway controls dispatch, not text phrasing.
      // A completed-word cap bounds exceptionally long/unpunctuated output.
      const maximum = 64;
      const completeWords = words.length - (!flush && !/\s$/.test(text) ? 1 : 0);
      const limit = Math.min(completeWords, maximum);
      let clause = -1;
      for (let index = 0; index < limit; index += 1) {
        const word = words[index][0];
        const abbreviation = /^(?:[A-Za-z]\.)+$/.test(word)
          || /^(?:Mr|Mrs|Ms|Dr|Prof|Sr|Jr|St|vs|etc|e\.g|i\.e)\.$/i.test(word)
          || /^\d+\.$/.test(word);
        if (/[.!?…][\]"')”’]*$/u.test(word) && !abbreviation) {
          return splitAt(index, "sentence");
        }
        if (index >= 15 && /[;:,][\]"')”’]*$/u.test(word)) clause = index;
      }
      if (completeWords >= maximum) {
        return splitAt(clause >= 0 ? clause : maximum - 1,
          clause >= 0 ? "limit_clause" : "limit_words");
      }
      return flush ? splitAt(words.length - 1, "end") : null;
    }
    // Voice notes join short sentences into a phrase before starting Qwen.
    const maximum = first ? 20 : 28;
    const minimum = first ? 10 : 12;
    const limit = Math.min(words.length, maximum);
    for (let index = minimum - 1; index < limit; index += 1) {
      if (/[.!?;:,][\]"')]*$/.test(words[index][0])) return splitAt(index, "phrase");
    }
    if (words.length >= maximum || flush) {
      if (!flush && words.length === maximum && !/\s$/.test(text)) return null;
      return splitAt(Math.min(words.length, maximum) - 1, flush ? "end" : "limit_words");
    }
    return null;
  };

  const beginLiveVoiceTts = ({
    conversation,
    mode,
    requestId = null,
    clientRequest = null,
    button: initialButton = null,
    callEpoch = null,
    deferPlayback = false,
    onFirstChunk = null,
    onPlaybackStarted = null,
    onFailure = null,
  }) => {
    const voiceNotePublicationReservation = mode === "voice_note" ? {} : null;
    if (voiceNotePublicationReservation) {
      activeVoiceNotePublications.add(voiceNotePublicationReservation);
    }
    const releaseVoiceNotePublicationReservation = () => {
      if (voiceNotePublicationReservation && !durableVoiceStorePromise) {
        activeVoiceNotePublications.delete(voiceNotePublicationReservation);
      }
    };
    if (requestId) rememberVoiceStreamRequest(requestId);
    let pendingText = "";
    let streamedText = "";
    let running = false;
    let closed = false;
    let stopped = false;
    let interruptedForSpeech = false;
    let firstRequest = true;
    let activeOperation = null;
    let activeSynthesis = null;
    let cancelPcmDelivery = null;
    let nextCallPrefill = null;
    let generationSettlement = Promise.resolve();
    let callTranscriptResidentCheck = null;
    let nextCallPrefillRetryPending = false;
    let retryHook = null;
    let nextCallPrefillHandedOff = false;
    let callCaptureStarted = false;
    let playbackAuthorized = !deferPlayback;
    let settleNextCallPrefillGate = null;
    const nextCallPrefillGate = mode === "call" ? new Promise((resolve) => {
      settleNextCallPrefillGate = resolve;
    }) : null;
    let segmentIndex = 0;
    let ttsSynthesisMs = 0;
    let ttsOutputAudioMs = 0;
    let ttsPcmPackets = 0;
    let ttsPcmAudioMs = 0;
    let ttsFirstPacketAudioMs = null;
    let ttsFirstPacketAt = null;
    let ttsPreviousPacketAt = null;
    let ttsMaxPacketGapMs = 0;
    let ttsMinimumProducerLeadMs = null;
    let audioProgressWatchdog = null;
    let watchdogActive = false;
    let schedulerWakeTimer = null;
    let schedulerWaitStartedAt = null;
    let semanticBackpressureDesired = false;
    let semanticBackpressureActive = false;
    let semanticBackpressureReconciling = false;
    let semanticBackpressureStartedAt = null;
    let semanticBackpressureSwitches = 0;
    let semanticBackpressurePauseMs = 0;
    let semanticBackpressureCommandMs = 0;
    let semanticBackpressureYields = 0;
    let semanticBackpressureYieldMs = 0;
    let pump = () => {};
    let button = initialButton;
    let firstPcmSeen = false;
    let voiceNoteComplete = false;
    let durablePending = false;
    let voiceAudioReadySettled = false;
    let resolveVoiceAudioReady;
    const voiceAudioReady = new Promise((resolve) => { resolveVoiceAudioReady = resolve; });
    const settleVoiceAudioReady = (ready) => {
      if (voiceAudioReadySettled) return;
      voiceAudioReadySettled = true;
      resolveVoiceAudioReady(ready);
    };
    let durableVoiceTarget = null;
    let durableVoiceStorePromise = null;
    let durableVoiceAttachmentId = null;
    let durableVoiceStoreError = null;
    let resolveFinished;
    const finished = new Promise((resolve) => { resolveFinished = resolve; });
    const ttsLatencyTrace = voiceLatencyTrace;
    const markTtsLatency = (stage, detail = {}) => {
      markVoiceLatencyIn(ttsLatencyTrace, stage, detail);
    };
    const bindDurableVoiceReply = (target) => {
      if (mode !== "voice_note") return Promise.resolve(null);
      const exactTarget = {
        conversation: typeof target?.conversation === "string" ? target.conversation : "",
        message: typeof target?.message === "string" ? target.message : "",
        text: typeof target?.text === "string" ? target.text : "",
      };
      if (!exactTarget.conversation || !exactTarget.message || !exactTarget.text.trim()) {
        return Promise.reject(new Error("voice_reply_attachment_target_missing"));
      }
      if (durableVoiceTarget && (durableVoiceTarget.conversation !== exactTarget.conversation
        || durableVoiceTarget.message !== exactTarget.message
        || durableVoiceTarget.text !== exactTarget.text)) {
        return Promise.reject(new Error("voice_reply_attachment_target_conflict"));
      }
      durableVoiceTarget ||= exactTarget;
      if (durableVoiceAttachmentId) return Promise.resolve(durableVoiceAttachmentId);
      if (durableVoiceStorePromise) return durableVoiceStorePromise;
      if (voiceNotePublicationReservation) {
        activeVoiceNotePublications.add(voiceNotePublicationReservation);
      }
      let storeAttempted = false;
      durableVoiceStorePromise = (async () => {
        if (!await voiceAudioReady) {
          throw new Error("voice_reply_audio_incomplete: The reply audio did not finish.");
        }
        const bytes = controller.wavBytes();
        if (!isCanonicalPcmWav(bytes)) {
          throw new Error("voice_reply_audio_invalid: The completed reply audio is unavailable.");
        }
        storeAttempted = true;
        const response = await invoke("mom_llama_voice_reply_store", {
          conversation: exactTarget.conversation,
          message: exactTarget.message,
          text: exactTarget.text,
          bytes: Array.from(bytes),
        });
        const attachmentId = response?.result?.attachment?.id;
        if (response?.status !== "contracted" || !attachmentId) {
          const blocker = response?.blocker;
          throw new Error(blocker
            ? `${blocker.code}: ${blocker.message}`
            : "voice_reply_attachment_uncommitted: The reply audio was not saved.");
        }
        durableVoiceAttachmentId = attachmentId;
        durableVoiceStoreError = null;
        if (voiceNotePublicationReservation) {
          activeVoiceNotePublications.delete(voiceNotePublicationReservation);
        }
        failedVoiceReplyOwners.delete(voiceReplyPersistenceKey(
          exactTarget.conversation, exactTarget.message,
        ));
        if (button) {
          button.dataset.committedAttachment = attachmentId;
          delete button.dataset.audioPersistence;
          button.title = "Play voice message";
        }
        markTtsLatency("durable_voice_audio_ready", {
          attachment_id: attachmentId,
          wav_bytes: bytes.byteLength,
        });
        refreshVoiceReplyProjectionIfNeeded(exactTarget);
        return attachmentId;
      })().catch((error) => {
        durableVoiceStorePromise = null;
        durableVoiceStoreError = storeAttempted ? error : null;
        const key = voiceReplyPersistenceKey(exactTarget.conversation, exactTarget.message);
        if (storeAttempted) {
          failedVoiceReplyOwners.set(key, playback);
          if (button) {
            button.dataset.audioPersistence = "failed";
            button.title = "Audio is available in this session; retry saving";
          }
          markTtsLatency("durable_voice_audio_failed", { message: errorMessage(error) });
          reportError(error);
          refreshVoiceReplyProjectionIfNeeded(exactTarget);
        } else {
          markTtsLatency("durable_voice_audio_not_stored", { message: errorMessage(error) });
        }
        releaseVoiceNotePublicationReservation();
        return null;
      });
      return durableVoiceStorePromise;
    };
    const refreshVoiceReplyProjectionIfNeeded = (target) => {
      if (selectedConversation() !== target.conversation || button?.isConnected) return;
      let attempts = 0;
      const refreshWhenIdle = () => {
        if (selectedConversation() !== target.conversation) return;
        if (activeDispatchConversation === target.conversation && attempts++ < 40) {
          window.setTimeout(refreshWhenIdle, 50);
          return;
        }
        if (activeDispatchConversation === target.conversation) return;
        void retainComposerDraft().then(() => {
          if (selectedConversation() === target.conversation
              && activeDispatchConversation !== target.conversation) {
            return refreshChat();
          }
          return null;
        }).catch(reportError);
      };
      window.setTimeout(refreshWhenIdle, 0);
    };
    const synthesisPurpose = mode === "call" ? "call" : "voice_note";
    const calibrationCache = voiceLeadCalibration || (voiceLeadCalibration =
      new globalThis.MomVoicePipelineScheduler.VoiceLeadCalibrationCache());
    const leadController = new globalThis.MomVoicePipelineScheduler.AdaptiveVoiceLeadController(
      calibrationCache.get(synthesisPurpose)
        || { initialBufferComplete: true, initialProbePending: true },
    );
    const semanticLeadController = new globalThis.MomVoicePipelineScheduler.SemanticLeadController();
    const mountVoiceReplyPlayer = () => {
      if (mode !== "voice_note" || button || !firstPcmSeen) return button;
      const row = document.querySelector(
        ".message-row.voice-reply-row.voice-reply-pending",
      );
      const card = row?.querySelector?.(".message-card");
      if (!card) return null;
      appendMessageSpeakerAvatar(row);
      row.classList.add("voice-reply-has-audio");
      const player = pendingVoiceReplyPlayer();
      card.replaceChildren(player);
      button = player.querySelector(".voice-reply-play");
      if (button) voiceReplyPlayback.set(button, playback);
      return button;
    };
    const pendingWordCount = () => pendingText.match(/\S+/g)?.length || 0;
    const semanticBackpressureSnapshot = () => ({
      active: semanticBackpressureActive,
      switches: semanticBackpressureSwitches,
      pause_ms: Math.round(semanticBackpressurePauseMs * 100) / 100,
      command_ms: Math.round(semanticBackpressureCommandMs * 100) / 100,
      generation_yields: semanticBackpressureYields,
      generation_yield_ms: Math.round(semanticBackpressureYieldMs * 100) / 100,
      ...semanticLeadController.snapshot(),
    });
    const reconcileSemanticBackpressure = async () => {
      if (semanticBackpressureReconciling || !requestId) return;
      semanticBackpressureReconciling = true;
      try {
        while (semanticBackpressureActive !== semanticBackpressureDesired && requestId) {
          const active = semanticBackpressureDesired;
          const started = performance.now();
          const receipt = await invoke("mom_llama_voice_generation_backpressure", {
            request: requestId,
            active,
          });
          semanticBackpressureCommandMs += performance.now() - started;
          semanticBackpressureActive = active;
          semanticBackpressureSwitches += 1;
          if (active) {
            semanticBackpressureStartedAt = performance.now();
            markTtsLatency("semantic_backpressure_started", {
              queued_words: pendingWordCount(),
              ...semanticBackpressureSnapshot(),
            });
          } else {
            if (semanticBackpressureStartedAt !== null) {
              semanticBackpressurePauseMs += performance.now() - semanticBackpressureStartedAt;
              semanticBackpressureStartedAt = null;
            }
            semanticBackpressureYields += Number(receipt?.generation_yields) || 0;
            semanticBackpressureYieldMs += (Number(receipt?.generation_yield_us) || 0) / 1000;
            markTtsLatency("semantic_backpressure_finished", {
              queued_words: pendingWordCount(),
              ...semanticBackpressureSnapshot(),
            });
          }
        }
      } catch (error) {
        // A command response can be lost after native code applied it. Issue
        // an idempotent release before forgetting local state so an ambiguous
        // transport failure cannot leave this exact generation paused.
        const failedRequest = requestId;
        if (failedRequest) {
          void invoke("mom_llama_voice_generation_backpressure", {
            request: failedRequest,
            active: false,
          }).catch(() => {});
        }
        semanticBackpressureDesired = false;
        semanticBackpressureActive = false;
        semanticBackpressureStartedAt = null;
        markTtsLatency("semantic_backpressure_failed", { message: errorMessage(error) });
      } finally {
        semanticBackpressureReconciling = false;
        if (requestId && semanticBackpressureActive !== semanticBackpressureDesired) {
          queueMicrotask(reconcileSemanticBackpressure);
        }
      }
    };
    const updateSemanticBackpressure = ({ terminal = false } = {}) => {
      const words = pendingWordCount();
      // Eight words of hysteresis gives Gemma a useful decode quantum between
      // handoffs. Never pause unless the queued text contains a chunk the TTS
      // pump can actually consume. A partial sentence must keep Gemma running
      // until punctuation (or the bounded long-sentence fallback) arrives.
      const readyChunk = takeLiveSpeechChunk(pendingText, false, firstRequest, mode);
      semanticBackpressureDesired = semanticLeadController.shouldPause({
        queuedWords: words,
        readyChunk: Boolean(readyChunk),
        terminal: terminal || stopped,
      });
      void reconcileSemanticBackpressure();
    };
    const panel = document.createElement("section");
    panel.className = "speech-playback";
    const startNextCallPrefill = (trigger = "buffered_playback_after_tts") => {
      if (nextCallPrefill || mode !== "call" || !voiceCallActive || voiceCallEpoch !== callEpoch) {
        return nextCallPrefill;
      }
      const requestedAt = performance.now();
      let startedAt = requestedAt;
      // Barge-in can stop speech before the confirmed reply is saved. Keep
      // capture running, but do not snapshot the next prompt until that exact
      // native operation has settled. Otherwise the new turn becomes a sibling
      // of the interrupted reply and subsequent turns follow the wrong branch.
      nextCallPrefill = generationSettlement.then(() => {
        if ((stopped && !nextCallPrefillHandedOff)
            || !voiceCallActive || voiceCallEpoch !== callEpoch) return null;
        startedAt = performance.now();
        markTtsLatency("next_turn_history_settled", {
          wait_ms: Math.round((startedAt - requestedAt) * 100) / 100,
        });
        markTtsLatency("next_turn_prefill_started", {
          overlap: trigger,
          buffered_runway_ms: controller.bufferedMs(),
        });
        return startVoicePrefill(conversation, true, ttsLatencyTrace);
      }).then(async (live) => {
        if (trigger === "transcript_committed") nextCallPrefillRetryPending = false;
        if ((stopped && !nextCallPrefillHandedOff)
            || !voiceCallActive || voiceCallEpoch !== callEpoch) {
          if (live?.session) {
            await invoke("mom_llama_voice_audio_abort", { session: live.session }).catch(() => {});
          }
          return null;
        }
        markTtsLatency("next_turn_prefill_ready", {
          duration_ms: Math.round((performance.now() - startedAt) * 100) / 100,
          buffered_runway_ms: controller.bufferedMs(),
        });
        if (retryCallPrefillAfterTranscript === retryHook) retryCallPrefillAfterTranscript = null;
        return live;
      }).catch((error) => {
        if (trigger === "transcript_committed") nextCallPrefillRetryPending = false;
        const message = errorMessage(error);
        markTtsLatency("next_turn_prefill_failed", { message });
        if (message.includes("voice_audio_pending_history_requires_complete_media_fallback")) {
          retryHook = (settledConversation) => {
            const recording = callCaptureStarted ? voiceRecording : null;
            const decision = globalThis.MomVoiceCapturePolicy.callPrefillRetryDecision({
              expectedConversation: conversation, settledConversation,
              callActive: voiceCallActive, expectedEpoch: callEpoch,
              currentEpoch: voiceCallEpoch, captureStarted: callCaptureStarted, recording,
            });
            if (decision === "stale") return;
            if (retryCallPrefillAfterTranscript === retryHook) retryCallPrefillAfterTranscript = null;
            if (decision === "capture_advanced") {
              markTtsLatency("next_turn_prefill_retry_skipped", { reason: "capture_advanced" });
              return;
            }
            nextCallPrefill = null;
            nextCallPrefillRetryPending = true;
            const retry = startNextCallPrefill("transcript_committed");
            if (recording) {
              // Failed admission consumed no microphone PCM. Rebind before
              // onset; the capture's pre-roll remains available to the retry.
              recording.prefillEpoch += 1;
              const epoch = recording.prefillEpoch;
              recording.prefillFailed = false;
              recording.prefillPending = true;
              recording.prefillRetryPending = true;
              recording.prefillSession = retry.then((replacement) => {
                if (recording.prefillEpoch !== epoch || recording.sending
                    || voiceRecording !== recording) {
                  if (replacement?.session) {
                    void invoke("mom_llama_voice_audio_abort", { session: replacement.session }).catch(() => {});
                  }
                  return null;
                }
                recording.prefillPending = false;
                recording.prefillRetryPending = false;
                if (!replacement?.session) {
                  recording.prefillFailed = true;
                }
                return replacement;
              });
              markVoiceLatencyIn(recording.trace, "audio_prefill_retry_after_transcript");
            }
          };
          retryCallPrefillAfterTranscript = retryHook;
        }
        return null;
      });
      settleNextCallPrefillGate?.(nextCallPrefill);
      settleNextCallPrefillGate = null;
      return nextCallPrefill;
    };
    const startCallCapture = () => {
      if (!playbackAuthorized || callCaptureStarted || mode !== "call" || !voiceCallActive
          || voiceCallEpoch !== callEpoch) return;
      if (voiceRecording) {
        markTtsLatency("call_capture_start_skipped", {
          reason: "capture_owned",
          capture_trace_id: voiceRecording.trace?.id || null,
        });
        return;
      }
      callCaptureStarted = true;
      nextCallPrefillHandedOff = true;
      const deferPrefill = pendingCallTranscriptJobs.length > 0 || Boolean(activeCallTranscriptJob);
      void beginVoiceCapture({
        call: true,
        prefillSession: deferPrefill ? null : (nextCallPrefill || nextCallPrefillGate),
        prefillRetryPending: nextCallPrefillRetryPending,
        deferPrefill,
        onVoiceOnset: (recording) => preemptCallTranscriptForSpeech(recording, startNextCallPrefill),
      }).then(() => {
        // The transcript slot requires an attached live capture with no Gemma
        // session. Recheck after asynchronous microphone admission completes.
        startBufferedNextCallPrefill();
      }).catch((error) => {
        callCaptureStarted = false;
        reportError(error);
      });
    };
    const abortUnclaimedCallPrefill = () => {
      if (nextCallPrefillHandedOff) {
        if (!voiceCallActive || voiceCallEpoch !== callEpoch) {
          settleNextCallPrefillGate?.(null);
          settleNextCallPrefillGate = null;
        }
        return;
      }
      settleNextCallPrefillGate?.(null);
      settleNextCallPrefillGate = null;
      if (!nextCallPrefill) return;
      const pending = nextCallPrefill;
      nextCallPrefill = null;
      void pending.then((live) => live?.session
        ? invoke("mom_llama_voice_audio_abort", { session: live.session })
        : null).catch(() => {});
    };
    const controller = createPcmPlaybackBuffer({
      autoplay: !deferPlayback && (mode === "call" || voiceReplyPlayRequested),
      retainHistory: mode !== "call",
      startupBufferMs: mode === "call" ? () => leadController.startupRunwayMs() : 120,
      startupDelayMs: mode === "call" ? () => leadController.startupDelayMs() : 0,
      waitForCompletion: () => mode === "call" && leadController.shouldBufferCompleteReply(),
      onFirstChunk: (firstPcm) => {
        firstPcmSeen = true;
        markTtsLatency("tts_first_pcm", {
          pcm_bytes: firstPcm?.bytes || 0,
          pcm_frames: firstPcm?.frames || 0,
          sample_rate: firstPcm?.sampleRate || 0,
          channels: firstPcm?.channels || 0,
        });
        onFirstChunk?.(firstPcm);
      },
      onPlayable: () => {
        // This threshold proves only that transient PCM can begin rendering.
        // Keep a voice note as the ordinary typing bubble until synthesis has
        // produced and validated the complete replayable WAV. Call playback
        // may still start immediately.
        if (mode === "voice_note") markTtsLatency("live_pcm_playable");
      },
      onPlaybackStarted: (metrics = {}) => {
        markTtsLatency("playback_started", metrics);
        onPlaybackStarted?.(metrics);
        if (mode === "call" && voiceCallActive && voiceCallEpoch === callEpoch) {
          activeCallPlayback = playback;
        }
        // Calls retain one WebKit microphone route with acoustic echo
        // cancellation. Begin processing it as soon as speech is audible so
        // the caller can interrupt without a device reopen or a dead interval.
        if (mode === "call" && voiceCallActive && voiceCallEpoch === callEpoch) {
          startCallCapture();
          startBufferedNextCallPrefill();
        }
        if (button) {
          button.classList.remove("is-loading");
          button.dataset.audioState = "playing";
          button.setAttribute("aria-label", "Pause voice message");
          button.removeAttribute("aria-pressed");
        }
      },
      onPlaybackPaused: () => {
        if (button) {
          button.dataset.audioState = "ready";
          button.setAttribute("aria-label", "Play voice message");
          button.removeAttribute("aria-pressed");
          button.closest?.(".voice-message-player")?.classList.remove("is-playing");
        }
      },
      onPlaybackError: (error) => fail(error),
      onUnderrun: (gapMs) => {
        leadController.noteUnderrun(gapMs);
        calibrationCache.set(synthesisPurpose, leadController.calibration());
        markTtsLatency("playback_underrun", {
          gap_ms: Math.round(gapMs),
          scheduler: leadController.snapshot(),
        });
      },
      onBufferLevel: () => {
        if (schedulerWaitStartedAt !== null && !schedulerWakeTimer) queueMicrotask(pump);
        startBufferedNextCallPrefill();
      },
      onRendererEvent: (event) => markTtsLatency(`playback_${event.type}`, event),
      onRendererMetrics: (metrics) => markTtsLatency("playback_renderer", metrics),
      onEnded: () => {
        if (activeCallPlayback === playback) activeCallPlayback = null;
        if (button) {
          button.dataset.audioState = "ready";
          button.setAttribute("aria-label", "Play voice message");
          button.removeAttribute("aria-pressed");
          button.closest?.(".voice-message-player")?.classList.remove("is-playing");
        }
        if (mode === "voice_note" && durablePending && button) {
          durablePending = false;
          settleInMemoryVoiceReply(button, controller, ttsLatencyTrace);
        }
        if (mode === "call" && voiceCallActive && voiceCallEpoch === callEpoch) {
          // Playback may end before the start callback on an exceptionally
          // short terminal packet. Keep a single idempotent fallback here.
          startCallCapture();
          startBufferedNextCallPrefill();
        }
        if (mode === "call" && liveVoiceTts === playback) liveVoiceTts = null;
      },
    });

    const startBufferedNextCallPrefill = () => {
      const bufferedPlaybackFinished = !controller.playing && controller.bufferedMs() <= 0;
      if (mode !== "call" || !closed || stopped
          || (!controller.playing && !bufferedPlaybackFinished)
          || running || pendingText.trim()) return;
      // The completed Gemma reply has released its worker. Begin the next
      // microphone prefix during the final PCM tail only after Qwen has
      // released its active segment. Competing prefill can otherwise turn a
      // sub-real-time TTS segment into repeated playback underruns.
      if (!bufferedPlaybackFinished && !leadController.shouldPrepareNextCall({
        generationComplete: closed,
        playing: controller.playing,
        bufferedMs: controller.bufferedMs(),
      })) return;
      if (activeCallTranscriptTask) return;
      if (pendingCallTranscriptJobs.length) {
        const recording = voiceRecording;
        if (!recording || recording.prefillDeferred !== true) return;
        if (recording?.voiceActivity?.heardVoice || recording?.sending || recording?.speculation) return;
        if (voiceCallTranscriptSlotsDisabled) {
          markTtsLatency("call_transcript_slot_skipped", { reason: "slow_cancel_or_cold_model" });
          void startNextCallPrefill("transcript_slot_disabled").then((live) => {
            if (recording?.prefillDeferred && live?.session) {
              return recording.resumeDeferredPrefill(live);
            }
            return false;
          }).catch((error) => reportError(error));
          return;
        }
        if (callTranscriptResidentCheck) return;
        callTranscriptResidentCheck = invoke("mom_llama_speech_parakeet_resident").then(async (status) => {
          if (status?.status === "blocked" || status?.result !== true) {
            markTtsLatency("call_transcript_slot_skipped", { reason: "parakeet_not_resident" });
            const live = await startNextCallPrefill("transcript_model_not_resident");
            if (recording?.prefillDeferred && live?.session) await recording.resumeDeferredPrefill(live);
            return;
          }
          // Text EOF can now precede the native history commit. Compressing
          // older turns must wait too, or that commit could overwrite the
          // transcript slot's updated conversation snapshot.
          await generationSettlement;
          if (!voiceCallActive || voiceCallEpoch !== callEpoch || recording?.sending
              || recording?.speculation || recording?.voiceActivity?.heardVoice) return;
          const playbackSafe = bufferedPlaybackFinished || leadController.shouldPrepareNextCall({
            generationComplete: closed,
            playing: controller.playing,
            bufferedMs: controller.bufferedMs(),
          });
          if (!globalThis.MomVoicePipelineScheduler.shouldRunCallTranscriptSlot({
            generationComplete: closed,
            playbackSafe,
            qwenIdle: !running && !pendingText.trim(),
            captureUnattached: recording.prefillDeferred === true,
            callerVoiced: Boolean(recording?.voiceActivity?.heardVoice),
            parakeetResident: true,
            transcriptSlotActive: Boolean(activeCallTranscriptTask),
            disabled: voiceCallTranscriptSlotsDisabled,
          })) return;
        markTtsLatency("call_transcript_slot_started", {
          queued: pendingCallTranscriptJobs.length,
          buffered_runway_ms: controller.bufferedMs(),
          capture_unprepared: Boolean(recording?.call && recording.prefillDeferred),
        });
        const transcriptTask = startPendingCallTranscriptSlot();
        if (transcriptTask) {
          const transcriptJob = activeCallTranscriptJob;
          await transcriptTask.then(async (outcome) => {
            if (outcome.cancelledByCaller) return;
            if (!voiceCallActive || voiceCallEpoch !== callEpoch || recording?.sending) return;
            const live = await startNextCallPrefill(outcome.committed
              ? "transcript_committed" : "transcript_unavailable");
            if (recording?.prefillDeferred && live?.session) {
              const replayed = await recording.resumeDeferredPrefill(live);
              markTtsLatency("call_transcript_slot_prefill_replayed", {
                committed: outcome.committed,
                replayed,
                samples: recording.capturedSamples,
              });
            }
          }).catch((error) => {
            markTtsLatency("call_transcript_slot_failed", { message: errorMessage(error) });
            reportError(error);
          }).finally(() => {
            // Keep this slot reserved through the post-transcript handoff so
            // buffer callbacks cannot overlap Parakeet with Gemma preparation.
            if (activeCallTranscriptJob === transcriptJob) activeCallTranscriptJob = null;
            if (activeCallTranscriptTask === transcriptTask) activeCallTranscriptTask = null;
            activeCallTranscriptOperation = null;
          });
        }
        }).catch((error) => {
          markTtsLatency("call_transcript_slot_skipped", {
            reason: "parakeet_residency_check_failed",
            message: errorMessage(error),
          });
          return startNextCallPrefill("transcript_residency_check_failed").then((live) => {
            if (recording?.prefillDeferred && live?.session) {
              return recording.resumeDeferredPrefill(live);
            }
            return false;
          });
        }).finally(() => { callTranscriptResidentCheck = null; });
        return;
      }
      startNextCallPrefill("generation_complete_buffered_playback");
    };

    const fail = (error) => {
      if (stopped) return;
      stopped = true;
      cancelPcmDelivery?.();
      if (activeCallPlayback === playback) activeCallPlayback = null;
      updateSemanticBackpressure({ terminal: true });
      window.clearTimeout(audioProgressWatchdog);
      audioProgressWatchdog = null;
      window.clearTimeout(schedulerWakeTimer);
      schedulerWakeTimer = null;
      const operation = activeOperation;
      const synthesis = activeSynthesis;
      activeOperation = null;
      const continueCall = playbackAuthorized && !onFailure && mode === "call"
        && voiceCallActive && voiceCallEpoch === callEpoch;
      if (!continueCall) abortUnclaimedCallPrefill();
      const releaseProducer = Promise.allSettled([
        controller.stop(),
        operation ? invoke("mom_llama_speech_stop", { operation }) : Promise.resolve(),
      ]).then(() => synthesis ? Promise.allSettled([synthesis]) : null);
      if (button) voiceReplyPlayback.delete(button);
      if (button) settleUnavailableVoiceReply(button);
      else if (mode === "voice_note") {
        document.querySelector(
          ".message-row.voice-reply-row.voice-reply-pending",
        )?.remove();
      }
      if (mode === "voice_note") settleVoiceAudioReady(false);
      releaseVoiceNotePublicationReservation();
      if (onFailure) {
        onFailure(error);
      } else if (continueCall) {
        // Preserve the single capture owner. Once the failed Qwen producer has
        // released shared compute, attach the next resident Gemma session to
        // that capture and replay its retained PCM exactly once.
        void releaseProducer.then(() => {
          if (!voiceCallActive || voiceCallEpoch !== callEpoch) return;
          startCallCapture();
          if (callCaptureStarted) startNextCallPrefill();
        });
      }
      markTtsLatency("failed", { component: "tts", message: errorMessage(error) });
      if (mode === "voice_note") settleVoiceAudioReady(false);
      if (!onFailure) reportError(error);
      resolveFinished(false);
    };

    const armAudioProgressWatchdog = () => {
      if (stopped) return;
      watchdogActive = true;
      window.clearTimeout(audioProgressWatchdog);
      audioProgressWatchdog = window.setTimeout(() => {
        fail(new Error("voice_reply_timeout: Voice synthesis stopped making progress."));
      }, leadController.progressTimeoutMs());
    };

    // Model generation owns its own bounded lifetime. Do not call a reply a
    // *synthesis* timeout before any text has reached synthesis: cold model
    // preemption and queued first-token work can legitimately outlive this
    // shorter audio-progress deadline. The first text delta (or an explicitly
    // activated native silence gate) arms the watchdog; every real PCM packet
    // then renews it.

    pump = () => {
      if (running || stopped) return;
      const bufferedRunwayMs = controller.bufferedMs();
      const next = takeLiveSpeechChunk(pendingText, closed, firstRequest, mode);
      if (!next) {
        if (closed) {
          window.clearTimeout(audioProgressWatchdog);
          audioProgressWatchdog = null;
          // The buffered-playback and caller-onset paths may have started the
          // next Gemma session already. If neither did, the final PCM tail
          // provides one last opportunity to prepare before playback ends.
          const aggregateRtf = ttsOutputAudioMs > 0
            ? Math.round((ttsSynthesisMs / ttsOutputAudioMs) * 10_000) / 10_000
            : null;
          markTtsLatency("tts_all_segments_finished", {
            segments: segmentIndex,
            synthesis_ms: Math.round(ttsSynthesisMs * 100) / 100,
            output_audio_ms: Math.round(ttsOutputAudioMs * 100) / 100,
            production_rtf: aggregateRtf,
            pcm_packets: ttsPcmPackets,
            pcm_audio_ms: Math.round(ttsPcmAudioMs * 100) / 100,
            first_packet_audio_ms: ttsFirstPacketAudioMs,
            max_packet_gap_ms: Math.round(ttsMaxPacketGapMs * 100) / 100,
            minimum_producer_lead_ms: ttsMinimumProducerLeadMs === null
              ? null : Math.round(ttsMinimumProducerLeadMs * 100) / 100,
            buffered_runway_ms: controller.bufferedMs(),
            scheduler: leadController.snapshot(),
            semantic_backpressure: semanticBackpressureSnapshot(),
          });
          startNextCallPrefill();
          controller.finish();
          if (mode === "voice_note") {
            voiceNoteComplete = true;
            settleVoiceAudioReady(true);
            const voiceButton = mountVoiceReplyPlayer();
            if (voiceButton) {
              if (controller.playing) durablePending = true;
              else settleInMemoryVoiceReply(voiceButton, controller, ttsLatencyTrace);
            }
          }
          resolveFinished(true);
        }
        return;
      }
      if (!next.chunk) {
        pendingText = next.rest;
        pump();
        return;
      }
      if (mode === "call" && !leadController.shouldDispatch({
        first: firstRequest,
        playing: controller.playing,
        bufferedMs: bufferedRunwayMs,
      })) {
        if (schedulerWaitStartedAt === null) {
          schedulerWaitStartedAt = performance.now();
          markTtsLatency("tts_scheduler_wait_started", {
            buffered_runway_ms: bufferedRunwayMs,
            ...leadController.snapshot(),
          });
        }
        const remainingMs = Math.max(0, bufferedRunwayMs - leadController.targetRunwayMs());
        window.clearTimeout(schedulerWakeTimer);
        schedulerWakeTimer = window.setTimeout(() => {
          schedulerWakeTimer = null;
          pump();
        }, Math.min(250, Math.max(16, remainingMs)));
        return;
      }
      if (schedulerWaitStartedAt !== null) {
        const waitedMs = performance.now() - schedulerWaitStartedAt;
        schedulerWaitStartedAt = null;
        leadController.noteWait(waitedMs);
        markTtsLatency("tts_scheduler_wait_finished", {
          duration_ms: Math.round(waitedMs * 100) / 100,
          buffered_runway_ms: bufferedRunwayMs,
          ...leadController.snapshot(),
        });
      }
      pendingText = next.rest;
      updateSemanticBackpressure();
      running = true;
      // The audio watchdog covers only time owned by an active synthesis
      // request. Language-model token gaps between speech chunks have their
      // own native generation deadline and must not be mislabeled as a stuck
      // Qwen producer.
      armAudioProgressWatchdog();
      const operation = speechOperationToken();
      activeOperation = operation;
      const segment = ++segmentIndex;
      // Tauri delivers large binary channel packets through asynchronous
      // fetches. The command result can overtake that channel's last packets.
      // Keep this segment's owner until both native success and validated PCM
      // EOF arrive; otherwise the next segment discards the old audio tail.
      let pcmEndSeen = false;
      let settlePcmDelivery;
      const pcmDelivered = new Promise((resolve) => { settlePcmDelivery = resolve; });
      const cancelThisPcmDelivery = () => settlePcmDelivery(false);
      cancelPcmDelivery = cancelThisPcmDelivery;
      const segmentDispatchedAt = performance.now();
      markTtsLatency("tts_segment_dispatched", {
        segment, operation_id: operation, request_id: requestId,
      });
      const wordCount = next.chunk.match(/\S+/g)?.length || 1;
      const maxSegmentFrames = Math.ceil(Math.min(40, Math.max(2.5, wordCount / 2 + 1)) * 24000);
      let segmentFrames = 0;
      let segmentPcmPackets = 0;
      let segmentPcmAudioMs = 0;
      let segmentFirstPacketAudioMs = null;
      let segmentFirstPacketAt = null;
      let segmentPreviousPacketAt = null;
      let segmentMaxPacketGapMs = 0;
      let segmentMinimumProducerLeadMs = null;
      // Bounded cadence evidence, published once at segment completion. Each
      // pair is milliseconds since first PCM and cumulative source frames;
      // no microphone samples or conversation text enter this diagnostic.
      const segmentPacketTimeline = [];
      let segmentRequiredStartupDelayMs = 0;
      if (firstRequest) {
        firstRequest = false;
        markTtsLatency("tts_first_chunk_ready", {
          overlap: "content_delta",
          characters: next.chunk.length,
          words: wordCount,
          segmentation_boundary: next.boundary,
        });
        markTtsLatency("tts_started", { overlap: "content_delta" });
      }
      const stream = pcmChannel((frame) => {
        if (stopped || activeOperation !== operation) return;
        try {
          if (pcmEndSeen) {
            throw new Error("speech_stream_after_end: PCM arrived after its segment ended.");
          }
          const decoded = decodePcmFrame(frame);
          const packetFrames = decoded.data.byteLength / (2 * decoded.channels);
          segmentFrames += packetFrames;
          if (segmentFrames > maxSegmentFrames) {
            throw new Error("speech_stream_runaway: Qwen exceeded the bounded audio duration for this text segment.");
          }
          if (packetFrames > 0) {
            const packetAt = performance.now();
            if (segmentFirstPacketAt === null) {
              markTtsLatency("tts_segment_first_pcm", {
                segment, operation_id: operation, request_id: requestId,
                dispatch_to_pcm_ms: Math.round(packetAt - segmentDispatchedAt),
              });
            }
            const packetAudioMs = packetFrames * 1000 / decoded.sampleRate;
            segmentPcmPackets += 1;
            segmentPcmAudioMs += packetAudioMs;
            ttsPcmPackets += 1;
            ttsPcmAudioMs += packetAudioMs;
            segmentFirstPacketAudioMs ??= Math.round(packetAudioMs * 100) / 100;
            ttsFirstPacketAudioMs ??= Math.round(packetAudioMs * 100) / 100;
            segmentFirstPacketAt ??= packetAt;
            ttsFirstPacketAt ??= packetAt;
            if (segmentPacketTimeline.length < 256) {
              segmentPacketTimeline.push([
                Math.round((packetAt - segmentFirstPacketAt) * 100) / 100,
                segmentFrames,
              ]);
            }
            segmentRequiredStartupDelayMs = Math.max(segmentRequiredStartupDelayMs,
              packetAt - segmentFirstPacketAt - (segmentPcmAudioMs - packetAudioMs));
            const packetGapMs = segmentPreviousPacketAt === null
              ? null : packetAt - segmentPreviousPacketAt;
            const aggregatePacketGapMs = ttsPreviousPacketAt === null
              ? null : packetAt - ttsPreviousPacketAt;
            const segmentBoundary = segmentPreviousPacketAt === null
              && ttsPreviousPacketAt !== null;
            if (segmentPreviousPacketAt !== null) {
              segmentMaxPacketGapMs = Math.max(segmentMaxPacketGapMs, packetGapMs);
            }
            if (ttsPreviousPacketAt !== null) {
              ttsMaxPacketGapMs = Math.max(ttsMaxPacketGapMs, packetAt - ttsPreviousPacketAt);
            }
            segmentPreviousPacketAt = packetAt;
            ttsPreviousPacketAt = packetAt;
            // Segment-local cadence omits the model-switching gap between the
            // last PCM packet of one request and the first packet of the next.
            // That boundary is the gap that can starve playback, so feed the
            // aggregate producer cadence back into both this turn's runway and
            // the next turn's calibration.
            const startupDeficitMs = Math.max(
              0,
              packetAt - ttsFirstPacketAt - (ttsPcmAudioMs - packetAudioMs),
            );
            leadController.notePacket({
              audioMs: packetAudioMs,
              gapMs: aggregatePacketGapMs,
              boundary: segmentBoundary,
              startupDeficitMs,
            });
            calibrationCache.set(synthesisPurpose, leadController.calibration());
            const segmentLead = segmentPcmAudioMs - (packetAt - segmentFirstPacketAt);
            const aggregateLead = ttsPcmAudioMs - (packetAt - ttsFirstPacketAt);
            segmentMinimumProducerLeadMs = segmentMinimumProducerLeadMs === null
              ? segmentLead : Math.min(segmentMinimumProducerLeadMs, segmentLead);
            ttsMinimumProducerLeadMs = ttsMinimumProducerLeadMs === null
              ? aggregateLead : Math.min(ttsMinimumProducerLeadMs, aggregateLead);
          }
          controller.pushSegment(frame, operation);
          if (decoded.end) {
            pcmEndSeen = true;
            markTtsLatency("tts_segment_pcm_complete", {
              segment, operation_id: operation, request_id: requestId,
              frames: segmentFrames, packets: segmentPcmPackets,
            });
            settlePcmDelivery(true);
          }
          // Transport heartbeats and terminal empty packets are not synthesis
          // progress. Only actual PCM earns more time; otherwise an unhealthy
          // producer can keep a pending bubble alive forever without audio.
          if (decoded.data.byteLength > 0) {
            playback.lastProgressAt = performance.now();
            armAudioProgressWatchdog();
          }
        } catch (error) { fail(error); }
      });
      if (segment === 1) markTtsLatency("tts_native_dispatched", { characters: next.chunk.length });
      const synthesis = invoke("mom_llama_speech_precommit_synthesize", {
        conversation,
        operation,
        text: next.chunk,
        purpose: synthesisPurpose,
        pcmStream: stream,
      });
      activeSynthesis = synthesis;
      synthesis.then(async (response) => {
        if (stopped || activeOperation !== operation) return;
        if (response?.status === "blocked" || !response?.result) {
          throw new Error(response?.blocker?.message || "Voice synthesis stopped.");
        }
        const receiptAt = performance.now();
        markTtsLatency("tts_segment_receipt_received", {
          segment, operation_id: operation, request_id: requestId,
          pcm_complete: pcmEndSeen,
        });
        // The existing audio-progress watchdog also bounds a missing channel
        // EOF. Stop/failure settles this delivery wait immediately, while
        // activeSynthesis separately tracks the actual native producer.
        if (!await pcmDelivered || stopped || activeOperation !== operation) return;
        const binding = response.result.binding || {};
        const synthesisMs = Number(binding.tts_total_ms);
        const outputAudioMs = Number(binding.duration_ms);
        if (Number.isFinite(synthesisMs)) ttsSynthesisMs += synthesisMs;
        if (Number.isFinite(outputAudioMs)) ttsOutputAudioMs += outputAudioMs;
        leadController.noteSegment({
          synthesisMs,
          audioMs: outputAudioMs,
          firstAudioMs: Number(binding.tts_time_to_first_audio_ms),
          words: wordCount,
        });
        calibrationCache.set(synthesisPurpose, leadController.calibration());
        // Completion of the cold probe can release playback without waiting
        // for another PCM packet (or the entire reply) to wake the player.
        if (mode === "call") void controller.reconsiderStartup();
        markTtsLatency("tts_segment_finished", {
          segment,
          operation_id: operation,
          request_id: requestId,
          backend_id: binding.backend_id ?? null,
          model_id: binding.model_id ?? null,
          voice_id: binding.voice_id ?? null,
          transport_drain_ms: Math.round(performance.now() - receiptAt),
          characters: next.chunk.length,
          words: wordCount,
          segmentation_boundary: next.boundary,
          queue_ms: binding.tts_queue_ms ?? null,
          queue_us: binding.tts_queue_us ?? null,
          host_route_us: binding.tts_host_route_us ?? null,
          host_queue_us: binding.tts_host_queue_us ?? null,
          host_dispatch_us: binding.tts_host_dispatch_us ?? null,
          host_admission_us: binding.tts_host_admission_us ?? null,
          model_load_ms: binding.tts_model_load_ms ?? null,
          background_warm_active: binding.tts_background_warm_active ?? null,
          background_warm_completed: binding.tts_background_warm_completed ?? null,
          background_warm_total: binding.tts_background_warm_total ?? null,
          reference_admission_us: binding.tts_reference_admission_us ?? null,
          reference_cache: binding.tts_reference_cache ?? null,
          reference_us: binding.tts_reference_us ?? null,
          reference_identity_us: binding.tts_reference_identity_us ?? null,
          reference_memory_lookup_us: binding.tts_reference_memory_lookup_us ?? null,
          reference_disk_lookup_us: binding.tts_reference_disk_lookup_us ?? null,
          reference_source_read_us: binding.tts_reference_source_read_us ?? null,
          reference_source_hash_us: binding.tts_reference_source_hash_us ?? null,
          reference_compute_us: binding.tts_reference_compute_us ?? null,
          reference_persist_us: binding.tts_reference_persist_us ?? null,
          generation_segments: binding.tts_generation_segments ?? null,
          packet_timeline: segmentPacketTimeline,
          packet_timeline_truncated: segmentPcmPackets > segmentPacketTimeline.length,
          required_startup_delay_ms: Math.round(segmentRequiredStartupDelayMs * 100) / 100,
          helper_first_pcm_us: binding.tts_helper_first_pcm_us ?? null,
          voice_lookup_us: binding.tts_voice_lookup_us ?? null,
          descriptor_snapshot_us: binding.tts_descriptor_snapshot_us ?? null,
          route_selection_us: binding.tts_route_selection_us ?? null,
          operation_admission_us: binding.tts_operation_admission_us ?? null,
          postflight_voice_lookup_us: binding.tts_postflight_voice_lookup_us ?? null,
          compute_lease_acquisitions: binding.tts_compute_lease_acquisitions ?? null,
          compute_lease_releases: binding.tts_compute_lease_releases ?? null,
          generation_yields: binding.tts_generation_yields ?? null,
          generation_yield_us: binding.tts_generation_yield_us ?? null,
          generation_fair_passes: binding.tts_generation_fair_passes ?? null,
          compute_lease_held_us: binding.tts_compute_lease_held_us ?? null,
          compute_lease_released_us: binding.tts_compute_lease_released_us ?? null,
          compute_minimum_lead_ms: binding.tts_compute_minimum_lead_ms ?? null,
          compute_maximum_lead_ms: binding.tts_compute_maximum_lead_ms ?? null,
          compute_low_water_ms: binding.tts_compute_low_water_ms ?? null,
          compute_high_water_ms: binding.tts_compute_high_water_ms ?? null,
          time_to_first_audio_ms: binding.tts_time_to_first_audio_ms ?? null,
          total_ms: binding.tts_total_ms ?? null,
          output_audio_ms: binding.duration_ms ?? null,
          pcm_packets: segmentPcmPackets,
          pcm_audio_ms: Math.round(segmentPcmAudioMs * 100) / 100,
          first_packet_audio_ms: segmentFirstPacketAudioMs,
          max_packet_gap_ms: Math.round(segmentMaxPacketGapMs * 100) / 100,
          minimum_producer_lead_ms: segmentMinimumProducerLeadMs === null
            ? null : Math.round(segmentMinimumProducerLeadMs * 100) / 100,
          production_rtf: Number.isFinite(synthesisMs) && Number.isFinite(outputAudioMs)
              && outputAudioMs > 0
            ? Math.round((synthesisMs / outputAudioMs) * 10_000) / 10_000
            : null,
          buffered_runway_ms: controller.bufferedMs(),
          scheduler: leadController.snapshot(),
        });
        startBufferedNextCallPrefill();
      }).catch(fail).finally(() => {
        window.clearTimeout(audioProgressWatchdog);
        audioProgressWatchdog = null;
        watchdogActive = false;
        if (activeOperation === operation) activeOperation = null;
        if (activeSynthesis === synthesis) activeSynthesis = null;
        if (cancelPcmDelivery === cancelThisPcmDelivery) cancelPcmDelivery = null;
        running = false;
        pump();
      });
    };

    const playback = {
      conversation,
      mode,
      trace: ttsLatencyTrace,
      firstDeltaSeen: false,
      lastProgressAt: performance.now(),
      get requestId() { return requestId; },
      clientRequest,
      acceptsRequest(candidate, eventName = "", candidateClientRequest = null) {
        if (stopped || !candidate) return false;
        if (requestId) return requestId === candidate;
        if (!clientRequest || candidateClientRequest !== clientRequest
            || eventName !== "started" || voiceStreamRequestIds.has(candidate)) return false;
        requestId = candidate;
        rememberVoiceStreamRequest(candidate);
        void reconcileSemanticBackpressure();
        return true;
      },
      trackGeneration(operation) {
        // Observe the native operation itself, never a renderer timeout or
        // cancellation request: only settlement establishes that its history
        // commit can no longer race preparation of the next turn.
        generationSettlement = operation.then(() => {}, () => {});
        return operation;
      },
      get button() { return button; },
      get audible() { return controller.playing; },
      get interruptedForSpeech() { return interruptedForSpeech; },
      interruptForSpeech() {
        interruptedForSpeech = true;
        return this.stop();
      },
      bindPendingRow() {
        if (!voiceNoteComplete) return button;
        const voiceButton = mountVoiceReplyPlayer();
        if (voiceButton && !controller.playing) {
          settleInMemoryVoiceReply(voiceButton, controller, ttsLatencyTrace);
        }
        return voiceButton;
      },
      bindDurableVoiceReply(target) { return bindDurableVoiceReply(target); },
      retryDurableVoiceReply() {
        if (!durableVoiceTarget || !durableVoiceStoreError) {
          return Promise.resolve(durableVoiceAttachmentId);
        }
        return bindDurableVoiceReply(durableVoiceTarget);
      },
      get durableVoiceAttachmentId() { return durableVoiceAttachmentId; },
      get durableVoiceStoreError() { return durableVoiceStoreError; },
      bindButton(nextButton) {
        if (!nextButton || stopped) return false;
        if (button && button !== nextButton) voiceReplyPlayback.delete(button);
        button = nextButton;
        voiceReplyPlayback.set(button, this);
        if (controller.playing) {
          button.dataset.audioState = "playing";
          button.classList.remove("is-loading");
          button.setAttribute("aria-label", "Stop voice reply");
        }
        return true;
      },
      controller,
      activate() {
        // Native silence release authorizes playback; synthesis timing begins
        // only when `pump` dispatches an actual text segment.
      },
      get text() { return streamedText; },
      push(delta) {
        if (closed || stopped || !delta) return;
        this.lastProgressAt = performance.now();
        pendingText += delta;
        streamedText += delta;
        pump();
        updateSemanticBackpressure();
      },
      finish(finalText = "") {
        if (closed || stopped) return finished;
        if (finalText.trim()) {
          if (!streamedText.trim()) {
            pendingText += finalText;
          } else if (finalText.startsWith(streamedText)) {
            // The authoritative result repairs any terminal deltas lost at an
            // IPC or ownership boundary without replaying text already queued.
            pendingText += finalText.slice(streamedText.length);
          }
        }
        closed = true;
        updateSemanticBackpressure({ terminal: true });
        startBufferedNextCallPrefill();
        pump();
        return finished;
      },
      async play() {
        playbackAuthorized = true;
        markTtsLatency("playback_requested", { pcm_ready: controller.ready });
        button?.classList.add("is-loading");
        let started = false;
        try {
          started = await controller.play();
        } catch (error) {
          button?.classList.remove("is-loading");
          fail(error);
          return false;
        }
        if (!started) {
          // An early tap only expresses playback intent. There is no work for
          // the button itself to await until the startup buffer fills, so clear
          // its transient acknowledgement instead of presenting an unbounded
          // spinner while generation continues in the background. The PCM
          // controller retains the playback intent and starts automatically.
          window.setTimeout(() => button?.classList.remove("is-loading"), 220);
        }
        return started;
      },
      async pause() {
        return controller.pause();
      },
      unavailable(error) {
        fail(error);
        return finished;
      },
      async cancel() {
        if (stopped) return;
        stopped = true;
        if (mode === "voice_note") settleVoiceAudioReady(false);
        releaseVoiceNotePublicationReservation();
        cancelPcmDelivery?.();
        if (activeCallPlayback === playback) activeCallPlayback = null;
        updateSemanticBackpressure({ terminal: true });
        window.clearTimeout(audioProgressWatchdog);
        audioProgressWatchdog = null;
        window.clearTimeout(schedulerWakeTimer);
        schedulerWakeTimer = null;
        abortUnclaimedCallPrefill();
        const synthesis = activeSynthesis;
        const operation = activeOperation;
        await controller.stop();
        if (operation) await invoke("mom_llama_speech_stop", { operation }).catch(() => {});
        if (synthesis) await Promise.allSettled([synthesis]);
        if (button) {
          voiceReplyPlayback.delete(button);
          settleUnavailableVoiceReply(button);
        } else if (mode === "voice_note") {
          document.querySelector(
            ".message-row.voice-reply-row.voice-reply-pending",
          )?.remove();
        }
        if (mode === "call" && nextCallPrefillHandedOff
            && voiceCallActive && voiceCallEpoch === callEpoch) {
          startNextCallPrefill();
        }
        resolveFinished(false);
      },
      async stop() {
        if (stopped) return;
        stopped = true;
        if (mode === "voice_note") settleVoiceAudioReady(false);
        releaseVoiceNotePublicationReservation();
        cancelPcmDelivery?.();
        if (activeCallPlayback === playback) activeCallPlayback = null;
        updateSemanticBackpressure({ terminal: true });
        window.clearTimeout(audioProgressWatchdog);
        audioProgressWatchdog = null;
        window.clearTimeout(schedulerWakeTimer);
        schedulerWakeTimer = null;
        const operation = activeOperation;
        const synthesis = activeSynthesis;
        activeOperation = null;
        abortUnclaimedCallPrefill();
        // Silence the speaker first. Native cancellation can take longer and
        // must never extend audible playback past hang-up.
        const stoppedController = controller.stop();
        if (operation) {
          await Promise.allSettled([
            stoppedController,
            invoke("mom_llama_speech_stop", { operation }),
          ]);
        } else {
          await stoppedController;
        }
        // Cancellation acknowledgement is not backend completion. The next
        // Qwen request must not enter its bounded queue until this producer
        // has actually released the backend lease.
        if (synthesis) await Promise.allSettled([synthesis]);
        if (button) {
          voiceReplyPlayback.delete(button);
          settleUnavailableVoiceReply(button);
        } else if (mode === "voice_note") {
          document.querySelector(
            ".message-row.voice-reply-row.voice-reply-pending",
          )?.remove();
        }
        if (mode === "call" && nextCallPrefillHandedOff
            && voiceCallActive && voiceCallEpoch === callEpoch) {
          startNextCallPrefill();
        }
        resolveFinished(false);
      },
    };
    // Once a retained voice-note reply has settled, its button owns this
    // controller through voiceReplyPlayback. Do not let the next dispatch's
    // live-controller cleanup stop or invalidate an already completed reply.
    if (mode !== "call") {
      void finished.finally(() => {
        if (liveVoiceTts === playback) liveVoiceTts = null;
      });
    }
    return playback;
  };

  const chatRequestIsCancellable = (busyLeases, streamLeases) => (
    [...streamLeases].some((lease) => (
      lease.startsWith("chat-request:") && busyLeases.has(lease)
    ))
  );

  const renderChatBusyState = () => {
    const busy = chatBusyLeases.size > 0;
    const cancellable = chatRequestIsCancellable(chatBusyLeases, activeDispatchStreamLeases);
    const form = document.getElementById("chat-form");
    if (!form) return;
    const voice = form.querySelector?.(".voice-message-button");
    const stop = form.querySelector?.(".stop-button");
    const skipReasoning = form.querySelector?.(".skip-reasoning-button");
    const message = formField(form, "message");
    const attachmentImport = form.querySelector?.("[data-action='attachment-import']");
    const attachmentRemovers = form.querySelectorAll?.("[data-action='draft-attachment-remove']") || [];
    if (message) message.readOnly = busy;
    if (attachmentImport) attachmentImport.disabled = busy;
    attachmentRemovers.forEach((button) => { button.disabled = busy; });
    if (voice) {
      const disabled = busy || voice.dataset.modelReady === "false";
      voice.disabled = disabled;
      voice.setAttribute("aria-disabled", String(disabled));
    }
    if (stop) {
      stop.disabled = !cancellable;
      stop.classList.toggle("is-hidden", !cancellable);
    }
    if (skipReasoning && !busy) {
      skipReasoning.disabled = true;
      skipReasoning.classList.add("is-hidden");
    }
    form.dataset.busy = busy ? "true" : "false";
  };

  const setVoiceRecordingVisible = (visible) => {
    const form = document.getElementById("chat-form");
    const recorder = document.getElementById("voice-message-recorder");
    form?.classList.toggle("voice-recording", visible);
    if (recorder) {
      recorder.hidden = !visible;
      recorder.classList.toggle("is-hidden", !visible);
    }
  };

  const mergeFloat32 = (chunks) => {
    const length = chunks.reduce((total, chunk) => total + chunk.length, 0);
    const merged = new Float32Array(length);
    let offset = 0;
    chunks.forEach((chunk) => { merged.set(chunk, offset); offset += chunk.length; });
    return merged;
  };

  const trimVoiceSilence = (samples, sampleRate = 16000) => {
    const frame = Math.max(1, Math.round(sampleRate * .02));
    const rms = [];
    for (let start = 0; start < samples.length; start += frame) {
      let power = 0;
      const end = Math.min(samples.length, start + frame);
      for (let index = start; index < end; index += 1) power += samples[index] * samples[index];
      rms.push(Math.sqrt(power / Math.max(1, end - start)));
    }
    const sorted = [...rms].sort((a, b) => a - b);
    const noiseFloor = sorted[Math.floor(sorted.length * .2)] || 0;
    const threshold = globalThis.MomVoiceCapturePolicy.speechThreshold(noiseFloor);
    const active = rms.map((value) => value >= threshold);
    const first = active.indexOf(true);
    const last = active.lastIndexOf(true);
    if (first < 0) return new Float32Array();
    const marginFrames = 8;
    const from = Math.max(0, first - marginFrames);
    const to = Math.min(active.length - 1, last + marginFrames);
    const kept = [];
    let cursor = from;
    while (cursor <= to) {
      if (active[cursor]) {
        kept.push(cursor);
        cursor += 1;
        continue;
      }
      let end = cursor;
      while (end <= to && !active[end]) end += 1;
      const run = end - cursor;
      const edge = cursor === from || end > to;
      const keep = edge || run <= 30 ? run : 16;
      for (let offset = 0; offset < keep; offset += 1) {
        const frameIndex = run <= keep ? cursor + offset : (offset < 8 ? cursor + offset : end - (keep - offset));
        kept.push(frameIndex);
      }
      cursor = end;
    }
    const output = new Float32Array(kept.length * frame);
    let write = 0;
    kept.forEach((frameIndex) => {
      const slice = samples.subarray(frameIndex * frame, Math.min(samples.length, (frameIndex + 1) * frame));
      output.set(slice, write);
      write += slice.length;
    });
    return output.subarray(0, write);
  };

  const encodePcmWav = (samples, sampleRate = 16000) => {
    const buffer = new ArrayBuffer(44 + samples.length * 2);
    const view = new DataView(buffer);
    const text = (offset, value) => [...value].forEach((character, index) => view.setUint8(offset + index, character.charCodeAt(0)));
    text(0, "RIFF"); view.setUint32(4, 36 + samples.length * 2, true); text(8, "WAVE");
    text(12, "fmt "); view.setUint32(16, 16, true); view.setUint16(20, 1, true);
    view.setUint16(22, 1, true); view.setUint32(24, sampleRate, true);
    view.setUint32(28, sampleRate * 2, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true);
    text(36, "data"); view.setUint32(40, samples.length * 2, true);
    samples.forEach((sample, index) => view.setInt16(44 + index * 2, Math.round(Math.max(-1, Math.min(1, sample)) * 32767), true));
    return new Uint8Array(buffer);
  };

  // Live inference and its retained derivative share the same integer PCM16
  // samples; the policy module owns the canonical quantizer/encoder.
  const { quantizeInferencePcm16, truncateInferencePcm16, encodeInferencePcm16Wav,
    appendInferenceSourceRange, sliceInferenceSourceRanges, inferenceSourceOffset }
    = globalThis.MomVoiceCapturePolicy;

  const voiceModePrompt = (call) => call
    ? "[Phone call] Reply naturally in at most two short sentences. No headings, lists, preamble, or extended explanation."
    : "[Voice note] Reply naturally and briefly, as a conversational voice message. No headings, lists, or preamble.";

  const enqueueVoicePrefill = (recording, samples, flush = false,
    sourceStart = recording.capturedSamples - (samples?.length || 0)) => {
    if (recording.prefillFailed || (!samples?.length && !flush)) return;
    if (samples?.length) {
      if (!Number.isSafeInteger(sourceStart) || sourceStart < recording.prefillLastSourceEnd
          || sourceStart + samples.length > recording.capturedSamples) {
        throw new Error("Voice prefill does not match the captured source.");
      }
      if (recording.prefillInputStartSample === null) recording.prefillInputStartSample = sourceStart;
      appendInferenceSourceRange(recording.prefillSourceRanges, sourceStart, samples.length);
      recording.prefillLastSourceEnd = sourceStart + samples.length;
      recording.prefillParts.push(samples);
      recording.prefillSamples += samples.length;
    }
    // The native consumer has a 30-second ring. Bound the browser IPC queue
    // and deferred caller input together; keep the physical archive separate.
    const queueLimitSamples = 16_000 * 30;
    if (recording.prefillQueuedSamples + recording.prefillSamples > queueLimitSamples) {
      recording.prefillFailed = true;
      recording.modelInputReliable = false;
      recording.prefillEpoch += 1;
      markVoiceLatencyIn(recording.trace, "audio_prefill_producer_overflow", {
        queued_samples: recording.prefillQueuedSamples,
        incoming_samples: recording.prefillSamples,
        limit_samples: queueLimitSamples,
      });
      void recording.prefillSession.then((live) => live?.session
        ? invoke("mom_llama_voice_audio_abort", { session: live.session })
        : null).catch(() => {});
      return;
    }
    // Queue only VAD-admitted caller input during compute handoff. Replaying
    // all captured PCM here used to insert playback and idle room audio, then
    // lose the exact derivative when its guessed contiguous origin disagreed.
    if (recording.prefillDeferred || (!flush && recording.prefillSamples < 640)
        || !recording.prefillSamples) return;
    const block = mergeFloat32(recording.prefillParts);
    const sourceRanges = recording.prefillSourceRanges;
    recording.prefillParts = [];
    recording.prefillSourceRanges = [];
    recording.prefillSamples = 0;
    let blockOffset = 0;
    const blocks = globalThis.MomVoiceCapturePolicy.splitPrefillPcm(block).map((chunk) => {
      const quantized = quantizeInferencePcm16(chunk);
      const ranges = sliceInferenceSourceRanges(sourceRanges, blockOffset, chunk.length);
      blockOffset += chunk.length;
      return { ...quantized, ranges };
    });
    const queuedSamples = block.length;
    recording.prefillQueuedSamples += queuedSamples;
    recording.prefillQueueHighWaterSamples = Math.max(
      recording.prefillQueueHighWaterSamples, recording.prefillQueuedSamples);
    const epoch = recording.prefillEpoch;
    const session = recording.prefillSession;
    recording.pushChain = recording.pushChain.then(async () => {
      const live = await session;
      if (!live?.session || recording.prefillFailed || recording.prefillEpoch !== epoch) return;
      for (const chunk of blocks) {
        if (recording.prefillEpoch !== epoch) return;
        const pushStarted = performance.now();
        const accepted = await invoke("mom_llama_voice_audio_push", {
          session: live.session,
          samples: Array.from(chunk.decoded),
        });
        if (recording.prefillEpoch !== epoch) return;
        recording.modelInputPcm16Parts.push(chunk.pcm16);
        recording.modelInputSampleCount += chunk.pcm16.length;
        for (const range of chunk.ranges) {
          appendInferenceSourceRange(recording.modelInputSourceRanges,
            range.start, range.end_exclusive - range.start);
        }
        recording.prefillPushes += 1;
        recording.prefillAcceptedSamples += chunk.pcm16.length;
        const pushMs = performance.now() - pushStarted;
        recording.prefillIpcMs += pushMs;
        recording.prefillMaxIpcMs = Math.max(recording.prefillMaxIpcMs, pushMs);
        recording.prefillHighWaterSamples = Math.max(
          recording.prefillHighWaterSamples,
          Number(accepted?.buffered_samples || 0),
        );
        if (!recording.firstPrefillChunkAccepted) {
          recording.firstPrefillChunkAccepted = true;
          markVoiceLatencyIn(recording.trace, "audio_prefill_first_chunk_accepted", {
            samples: chunk.pcm16.length,
            native_buffered_samples: accepted?.buffered_samples,
          });
        }
      }
    }).catch(async (error) => {
      if (recording.prefillEpoch !== epoch) return;
      recording.prefillFailed = true;
      recording.modelInputReliable = false;
      markVoiceLatencyIn(recording.trace, "audio_prefill_push_failed", {
        message: errorMessage(error),
        producer_pushes: recording.prefillPushes || 0,
        producer_accepted_samples: recording.prefillAcceptedSamples || 0,
      });
      const live = await session.catch(() => null);
      if (live?.session) {
        await invoke("mom_llama_voice_audio_abort", { session: live.session }).catch(() => {});
      }
    }).finally(() => {
      if (recording.prefillEpoch === epoch) {
        recording.prefillQueuedSamples = Math.max(0, recording.prefillQueuedSamples - queuedSamples);
      }
    });
  };

  const prepareVoicePrefill = async (recording) => {
    // Capture is already running and retaining PCM. Foreground model work
    // waits for the exact archival ASR terminal, never its stop request alone.
    const releaseHistoryRecovery = recording.call ? null
      : await suspendVoiceHistoryRecoveryForForeground("voice_capture");
    try {
      if (recording.discarded) return null;
      const sourceConversation = recording.call && voiceCallConversation
        ? voiceCallConversation
        : recording.sourceConversation;
      let conversation = sourceConversation;
      if (sourceConversation !== "default" && recording.sourceKind === "persona_template") {
        const instantiated = await instantiatePersona(sourceConversation);
        if (instantiated?.status === "blocked" || !instantiated?.result?.id) return null;
        conversation = instantiated.result.id;
        if (selectedConversation() === sourceConversation) {
          chat().dataset.currentConversation = conversation;
          chat().dataset.conversationKind = "chat";
        }
      }
      return await startVoicePrefill(conversation, recording.call, recording.trace);
    } finally {
      // Dispatch completion or confirmed cancellation resumes recovery. Do
      // not restart ASR between capture stop and submission of retained PCM.
      releaseHistoryRecovery?.({ resume: false });
    }
  };

  let pendingCallTranscriptJobs = [];
  let activeCallTranscriptJob = null;
  let activeCallTranscriptTask = null;
  let activeCallTranscriptOperation = null;
  const idleVoiceTranscriptJobs = new Set();

  const indexVoiceTurn = (conversation, output, fallbackAttachment = null, sourceCapture = null,
    sourceTrace = null, { allowActiveCall = false, callJob = null } = {}) => {
    const attachment = output?.voice_attachment_id || fallbackAttachment;
    const messageId = output?.user_message_id;
    // Playback may have opened the next microphone turn already. Backfill
    // stages belong to the physical turn that produced this recording.
    const trace = sourceTrace || sourceCapture?.trace || voiceLatencyTrace;
    // A prepared conversation snapshot stays fixed for its current turn.
    // Saved recordings may be indexed in a bounded idle slot while the next
    // Gemma session is still unattached; idle recovery handles any remainder.
    if ((voiceCallActive || voiceCallPreparing) && !allowActiveCall) {
      if (voiceCallActive && attachment && messageId) {
        const intent = globalThis.MomVoicePipelineScheduler.createCallTranscriptIntent({
          conversation,
          output,
          fallbackAttachment,
          sourceTrace: trace,
        });
        if (globalThis.MomVoicePipelineScheduler.enqueueCallTranscriptIntent(
          pendingCallTranscriptJobs, intent, 64)) {
          markVoiceLatencyIn(trace, "voice_transcript_queued_for_idle_playback", {
            queued: pendingCallTranscriptJobs.length,
          });
        } else {
          markVoiceLatencyIn(trace, "voice_transcript_queue_deferred_overflow", {
            limit: 64,
            queued: pendingCallTranscriptJobs.length,
          });
        }
      } else {
        markVoiceLatencyIn(trace, "voice_transcript_deferred");
      }
      return Promise.resolve();
    }
    // Ordinary dispatch holds a recovery suspension through its speech drain.
    // Its release will revisit this saved recording. Never start a second
    // archival reader alongside recovery or a new microphone turn.
    if (!callJob && (voiceHistoryRecoverySuspensions > 0 || voiceRecording || voiceHistoryRecovery)) {
      markVoiceLatencyIn(trace, "voice_transcript_deferred", { reason: "foreground_or_recovery" });
      return Promise.resolve();
    }
    const idleJob = callJob ? null : { operation: null, task: null, cancelledByForeground: false };
    if (idleJob) idleVoiceTranscriptJobs.add(idleJob);
    const preempted = () => callJob?.cancelledByCaller || idleJob?.cancelledByForeground;
    const started = performance.now();
    markVoiceLatencyIn(trace, "voice_transcript_started", {
      source: output?.voice_attachment_id ? "live" : "fallback",
    });
    const task = (async () => {
      if (!attachment || !messageId) {
        throw new Error("voice_transcript_source_missing: The saved voice turn has no recording identity.");
      }
      // Speculative generation can finish before the first audible packet.
      // Its recording becomes authoritative only after confirmation commits.
      const physicalCapture = await Promise.resolve(sourceCapture);
      if (preempted() || (sourceCapture && !physicalCapture)) return false;
      const catalog = attachmentPreviewResult(await invoke("mom_llama_attachment_preview", {
        attachment,
      }));
      if (preempted()) return false;
      const primary = catalog.primary;
      if (!primary || primary.kind !== "audio") {
        throw new Error("voice_transcript_source_missing: The recorded voice could not be read for conversation memory.");
      }
      markVoiceLatencyIn(trace, "voice_transcript_source_ready", {
        duration_ms: Math.round((performance.now() - started) * 100) / 100,
      });
      const inferenceStarted = performance.now();
      markVoiceLatencyIn(trace, "voice_transcript_inference_started");
      let transcribed = null;
      if (!callJob && physicalCapture?.transcription && typeof physicalCapture.samples === "function") {
        try {
          const live = await physicalCapture.transcription.finished;
          const samples = physicalCapture.samples();
          const identity = physicalCapture.transcription.metrics();
          const source = live?.source;
          // The same resampled PCM chunks are retained in capturedPcm and
          // appended to the native transcription producer. Its completed
          // receipt binds that accepted stream to this capture identity/count.
          // Hash the archival WAV below to bind the durable attachment; avoid
          // rescanning the same PCM buffer in JavaScript.
          const archivedWavSha256 = await sha256Hex(encodePcmWav(samples));
          if (source?.identity?.turn !== identity.turn
              || source?.identity?.capture !== identity.capture
              || source?.identity?.conversation !== conversation
              || source?.samples !== samples.length
              || catalog.root_sha256 !== archivedWavSha256) {
            throw new Error("live_transcription_source_mismatch");
          }
          transcribed = { status: "contracted", result: { transcript: live.transcript } };
          markVoiceLatencyIn(trace, "voice_transcript_live_verified", {
            samples: source.samples,
            ...(source.metrics || {}),
          });
        } catch (error) {
          // The saved WAV remains authoritative. A failed parallel reader may
          // be retried from that recording without delaying the reply.
          markVoiceLatencyIn(trace, "voice_transcript_live_failed", { message: errorMessage(error) });
        }
      }
      if (!transcribed) {
        if (preempted()) return false;
        const operation = speechOperationToken();
        if (idleJob) idleJob.operation = operation;
        if (callJob && voiceCallActive && activeCallTranscriptJob === callJob) {
          activeCallTranscriptOperation = operation;
        }
        try {
          transcribed = await invoke("mom_llama_speech_transcribe_attachment", {
            conversation,
            ...attachmentPreviewAnchor(catalog, primary),
            operation,
          });
        } finally {
          if (activeCallTranscriptOperation === operation) activeCallTranscriptOperation = null;
          if (idleJob?.operation === operation) idleJob.operation = null;
        }
      }
      if (preempted()) return false;
      markVoiceLatencyIn(trace, "voice_transcript_inference_finished", {
        duration_ms: Math.round((performance.now() - inferenceStarted) * 100) / 100,
        ...(transcribed?.result?.timing || {}),
      });
      if (transcribed?.status === "blocked" || !transcribed?.result) {
        throw new Error(`${transcribed?.blocker?.code || "voice_transcript_blocked"}: ${transcribed?.blocker?.message || "The recorded voice could not be transcribed."}`);
      }
      const transcript = transcribed?.result?.transcript?.trim();
      if (!transcript) {
        // Archival enrichment is optional. Silence or an ASR miss leaves the
        // original recording authoritative for the next prompt and idle retry.
        markVoiceLatencyIn(trace, "voice_transcript_skipped", {
          reason: "empty_transcript",
          recording_retained: true,
        });
        return false;
      }
      const recordStarted = performance.now();
      markVoiceLatencyIn(trace, "voice_transcript_record_started");
      const recorded = await invoke("mom_llama_message_record_voice_transcript", {
        conversation, message: messageId, attachment, transcript,
      });
      markVoiceLatencyIn(trace, "voice_transcript_record_finished", {
        duration_ms: Math.round((performance.now() - recordStarted) * 100) / 100,
      });
      if (recorded?.status === "blocked" || !recorded?.result) {
        throw new Error(`${recorded?.blocker?.code || "voice_transcript_record_failed"}: ${recorded?.blocker?.message || "The voice transcript could not be saved."}`);
      }
      if (callJob) callJob.committed = true;
      return true;
    })().finally(() => {
      if (idleJob) idleVoiceTranscriptJobs.delete(idleJob);
    });
    if (idleJob) idleJob.task = task;
    void task.then((committed) => {
      if (committed !== true) return;
      markVoiceLatencyIn(trace, "voice_transcript_finished", {
        duration_ms: Math.round((performance.now() - started) * 100) / 100,
      });
      retryCallPrefillAfterTranscript?.(conversation);
    }, (error) => {
      // Transcription enriches history when it succeeds. A failed or slow
      // transcriber must never hold the live listener or the next reply.
      markVoiceLatencyIn(trace, "voice_transcript_failed", {
        duration_ms: Math.round((performance.now() - started) * 100) / 100,
        message: errorMessage(error),
      });
      if (!preempted()) reportError(error);
    });
    return task;
  };

  const finishVoiceReplyAndIndexTurn = async (callTurn, index, finish, persist = null) => {
    if (callTurn) index();
    const persistence = persist
      ? Promise.resolve().then(persist).catch((error) => {
        reportError(error);
        return null;
      })
      : null;
    const synthesized = await finish();
    if (persistence) await persistence;
    if (synthesized && !callTurn) index();
    return synthesized;
  };

  const startPendingCallTranscriptSlot = () => {
    if (!voiceCallActive || activeCallTranscriptTask || !pendingCallTranscriptJobs.length) return null;
    const job = pendingCallTranscriptJobs.shift();
    job.cancelledByCaller = false;
    activeCallTranscriptJob = job;
    const task = indexVoiceTurn(job.conversation, job.output, job.fallbackAttachment,
      job.sourceCapture, job.sourceTrace, { allowActiveCall: true, callJob: job });
    const slotTask = task.then(
      () => ({ committed: job.committed === true, cancelledByCaller: job.cancelledByCaller === true }),
      () => ({ committed: job.committed === true, cancelledByCaller: job.cancelledByCaller === true }),
    );
    activeCallTranscriptTask = slotTask;
    return slotTask;
  };

  const preemptCallTranscriptForSpeech = async (recording, startNextCallPrefill) => {
    const job = activeCallTranscriptJob;
    const operation = activeCallTranscriptOperation;
    if (job) {
      job.cancelledByCaller = true;
      if (operation) {
        const elapsed = await stopSpeechOperationAndWait(operation, "caller_speech_onset");
        if (globalThis.MomVoicePipelineScheduler.shouldDisableCallTranscriptSlotsAfterStop(elapsed)) {
          voiceCallTranscriptSlotsDisabled = true;
        }
      }
      await activeCallTranscriptTask?.catch(() => {});
      if (!job.committed) pendingCallTranscriptJobs.unshift(job);
      if (activeCallTranscriptJob === job) activeCallTranscriptJob = null;
      if (activeCallTranscriptTask) activeCallTranscriptTask = null;
      activeCallTranscriptOperation = null;
    }
    const live = await startNextCallPrefill("caller_voice_onset");
    if (recording?.prefillDeferred && live?.session) {
      await recording.resumeDeferredPrefill(live);
    }
  };

  const stopCallTranscriptForClose = async () => {
    const operation = activeCallTranscriptOperation;
    const job = activeCallTranscriptJob;
    if (job) job.cancelledByCaller = true;
    let stopError = null;
    if (operation) {
      try {
        await stopSpeechOperationAndWait(operation, "call_closed");
      } catch (error) {
        stopError = error;
      }
    }
    await activeCallTranscriptTask?.catch(() => {});
    if (activeCallTranscriptJob === job) activeCallTranscriptJob = null;
    activeCallTranscriptTask = null;
    activeCallTranscriptOperation = null;
    // The saved recording remains the source of truth; idle recovery will
    // rediscover any turn whose transcript did not commit before hang-up.
    pendingCallTranscriptJobs = [];
    if (stopError) throw stopError;
  };

  // Saved recordings are authoritative until a complete transcript replaces
  // their model-context representation. A crashed/older client can leave more
  // pending recordings than one Gemma request may admit, so resume that work
  // from the active chain without ever dropping or truncating its history.
  let voiceHistoryRecovery = null;
  let voiceHistoryRecoveryOperation = null;
  let voiceHistoryRecoveryStage = "idle";
  let voiceHistoryRecoveryColdInitialization = false;
  let voiceHistoryRecoverySuspensions = 0;
  const canContinueVoiceHistoryRecovery = () => globalThis.MomVoicePipelineScheduler.canContinueVoiceHistoryRecovery({
    suspended: voiceHistoryRecoverySuspensions > 0,
    foregroundDispatch: activeDispatchConversation !== null,
    callActive: voiceCallActive,
    callPreparing: voiceCallPreparing,
    recording: Boolean(voiceRecording) || idleVoiceTranscriptJobs.size > 0,
  });
  const recoverPendingVoiceHistory = () => {
    if (!canContinueVoiceHistoryRecovery()) return Promise.resolve();
    // Selection or call teardown can arrive while an earlier conversation is
    // still being examined. Revisit the current selection after that pass;
    // otherwise its recordings can remain forever on the slow media path.
    if (voiceHistoryRecovery) return voiceHistoryRecovery.then(() => recoverPendingVoiceHistory());
    voiceHistoryRecovery = (async () => {
      voiceHistoryRecoveryStage = "listing";
      const listed = await invoke("mom_llama_conversation_list");
      if (!canContinueVoiceHistoryRecovery()) return;
      if (listed?.status === "blocked" || !Array.isArray(listed?.result)) return;
      const selected = selectedConversation();
      for (const conversation of listed.result.filter((item) => item.id === selected)) {
        const messages = new Map((conversation.messages || []).map((message) => [message.id, message]));
        const active = [];
        const seen = new Set();
        let id = conversation.active_leaf_message_id;
        while (id && !seen.has(id) && messages.has(id)) {
          seen.add(id);
          const message = messages.get(id);
          active.push(message);
          id = message.parent_id;
        }
        for (const message of active.reverse()) {
          if (!canContinueVoiceHistoryRecovery()) return;
          if (message.role !== "user"
              || ![voiceModePrompt(true), voiceModePrompt(false)].includes(message.content)
              || message.attachment_ids?.length !== 1) continue;
          // Live turns take priority. Recovery only uses idle time and never
          // holds generation or the microphone on Parakeet.
          if (voiceCallActive || voiceCallPreparing || voiceRecording) return;
          const attachment = message.attachment_ids[0];
          try {
            voiceHistoryRecoveryStage = "metadata";
            markVoiceLatencyIn(voiceLatencyTrace, "voice_history_recovery_started", {
              conversation: conversation.id, message: message.id, attachment,
            });
            const catalog = attachmentPreviewResult(await invoke("mom_llama_attachment_preview", {
              attachment,
            }));
            if (!canContinueVoiceHistoryRecovery()) return;
            if (catalog.primary?.kind !== "audio") {
              markVoiceLatencyIn(voiceLatencyTrace, "voice_history_recovery_skipped", {
                message: message.id, reason: "not_audio",
              });
              continue;
            }
            if (voiceCallActive || voiceCallPreparing || voiceRecording) return;
            const residency = await invoke("mom_llama_speech_parakeet_resident");
            if (!canContinueVoiceHistoryRecovery()) return;
            voiceHistoryRecoveryColdInitialization = residency?.result !== true;
            const operation = speechOperationToken();
            voiceHistoryRecoveryOperation = operation;
            voiceHistoryRecoveryStage = "transcription";
            let transcribed;
            try {
              transcribed = await invoke("mom_llama_speech_transcribe_attachment", {
                conversation: conversation.id,
                ...attachmentPreviewAnchor(catalog, catalog.primary),
                operation,
              });
            } finally {
              if (voiceHistoryRecoveryOperation === operation) voiceHistoryRecoveryOperation = null;
            }
            if (!canContinueVoiceHistoryRecovery()) return;
            if (transcribed?.status === "blocked" || !transcribed?.result?.transcript?.trim()) {
              markVoiceLatencyIn(voiceLatencyTrace, "voice_history_recovery_skipped", {
                message: message.id,
                reason: transcribed?.blocker?.code || "empty_transcript",
              });
              continue;
            }
            if (!canContinueVoiceHistoryRecovery()) return;
            voiceHistoryRecoveryStage = "recording_commit";
            const recorded = await invoke("mom_llama_message_record_voice_transcript", {
              conversation: conversation.id,
              message: message.id,
              attachment,
              transcript: transcribed.result.transcript.trim(),
            });
            if (!canContinueVoiceHistoryRecovery()) return;
            if (recorded?.status === "blocked" || !recorded?.result) {
              markVoiceLatencyIn(voiceLatencyTrace, "voice_history_recovery_skipped", {
                message: message.id,
                reason: recorded?.blocker?.code || "record_failed",
              });
              continue;
            }
            markVoiceLatencyIn(voiceLatencyTrace, "voice_history_recovery_finished", {
              message: message.id,
            });
          } catch (error) {
            markVoiceLatencyIn(voiceLatencyTrace, "voice_history_recovery_failed", {
              message: message.id, reason: errorMessage(error),
            });
            console.warn("Voice history recovery paused", errorMessage(error));
            return;
          }
        }
      }
    })().finally(() => {
      voiceHistoryRecovery = null;
      voiceHistoryRecoveryOperation = null;
      voiceHistoryRecoveryStage = "idle";
      voiceHistoryRecoveryColdInitialization = false;
    });
    return voiceHistoryRecovery;
  };

  const suspendVoiceHistoryRecoveryForForeground = async (reason) => {
    voiceHistoryRecoverySuspensions += 1;
    try {
      const idleJobs = [...idleVoiceTranscriptJobs];
      // Mark every owner before awaiting anything: jobs still reading source
      // metadata must not admit a new backend operation after this snapshot.
      for (const job of idleJobs) job.cancelledByForeground = true;
      for (const job of idleJobs) {
        if (job.operation) {
          await stopSpeechOperationAndWait(job.operation, reason, {
            allowColdTranscriptionSettlement: true,
          });
        }
      }
      // A transcript commit that already began must settle before Gemma reads
      // the next history. Failed/cancelled readers keep their source recording.
      await Promise.all(idleJobs.map((job) => job.task?.catch(() => {})));
      const operation = voiceHistoryRecoveryOperation;
      const coldInitialization = voiceHistoryRecoveryColdInitialization;
      if (operation) {
        const elapsed = await stopSpeechOperationAndWait(operation, reason, {
          allowColdTranscriptionSettlement: coldInitialization,
        });
        if (globalThis.MomVoicePipelineScheduler.shouldDisableCallTranscriptSlotsAfterStop(elapsed)) {
          voiceCallTranscriptSlotsDisabled = true;
        }
      }
      if (globalThis.MomVoicePipelineScheduler.shouldWaitForVoiceHistoryRecovery({
        operation,
        stage: voiceHistoryRecoveryStage,
      })) {
        await voiceHistoryRecovery?.catch(() => {});
      }
    } catch (error) {
      voiceHistoryRecoverySuspensions = Math.max(0, voiceHistoryRecoverySuspensions - 1);
      throw error;
    }
    let released = false;
    return ({ resume = true } = {}) => {
      if (released) return;
      released = true;
      voiceHistoryRecoverySuspensions = Math.max(0, voiceHistoryRecoverySuspensions - 1);
      if (resume && voiceHistoryRecoverySuspensions === 0 && canContinueVoiceHistoryRecovery()) {
        void recoverPendingVoiceHistory().catch(() => {});
      }
    };
  };

  const startVoicePrefill = async (conversation, call, trace) => {
    markVoiceLatencyIn(trace, "audio_prefill_begin_requested", { conversation, call });
    const live = await invoke("mom_llama_voice_audio_begin", {
      conversation,
      message: voiceModePrompt(call),
    });
    markVoiceLatencyIn(trace, "audio_prefill_session_ready", { begin: live?.begin_metrics || null });
    return { ...live, conversation };
  };

  const abortStoppedVoicePrefill = (recording, abortPrefill) => {
    const cancelled = recording.speculation && !recording.speculation.confirmed
      ? abortCallSpeculation(recording)
      : abortPrefill ? recording.prefillSession.then((live) => live?.session
        ? invoke("mom_llama_voice_audio_abort", { session: live.session }) : null)
        : Promise.resolve();
    // Keep the gesture responsive, but do not restart archival compute until
    // the cancelled model owner has acknowledged the stop.
    void Promise.resolve(cancelled).then(() => {
      if (!recording.call) void recoverPendingVoiceHistory().catch(() => {});
    }, (error) => {
      markVoiceLatencyIn(recording.trace, "voice_capture_prefill_abort_failed", {
        message: errorMessage(error),
      });
    });
  };

  const stopVoiceCapture = async ({ discard = false, abortPrefill = true,
    flushPrefill = true, reason = "sent" } = {}) => {
    const recording = voiceRecording;
    voiceRecording = null;
    if (!recording) return null;
    if (discard) recording.discarded = true;
    markVoiceLatencyIn(recording.trace, "capture_stopped", {
      reason, discard, producer: recording.native ? "native_ring" : "browser_capture",
    });
    clearTimeout(recording.limit);
    if (recording.native) {
      setVoiceRecordingVisible(false);
      await recording.startPromise;
      if (discard) {
        abortStoppedVoicePrefill(recording, abortPrefill);
        await invoke("mom_llama_voice_capture_cancel", { capture: recording.capture });
        return null;
      }
      await recording.targetChain.catch(() => null);
      const stopped = await invoke("mom_llama_voice_capture_stop", { capture: recording.capture });
      const wavBytes = ipcBinaryBytes(stopped?.wav, 16 * 1024 * 1024, "Native voice recording");
      const modelInputWavBytes = stopped?.inference_wav == null
        ? null : ipcBinaryBytes(stopped.inference_wav, 16 * 1024 * 1024, "Native model-input recording");
      return {
        heardVoice: stopped?.heard_voice === true,
        metrics: stopped?.metrics || recording.nativeMetrics || {},
        wavBytes,
        modelInputWavBytes,
        sourceSampleCount: stopped?.inference_source_sample_count,
        modelInputSampleCount: stopped?.inference_accepted_sample_count,
        modelInputSourceRanges: stopped?.inference_source_ranges,
        trace: recording.trace,
      };
    }
    recording.processor.onaudioprocess = null;
    if (!recording.sharedCallGraph) {
      recording.stream.getTracks().forEach((track) => track.stop());
      recording.source.disconnect();
      recording.processor.disconnect();
      recording.silence.disconnect();
      await recording.context.close().catch(() => {});
    }
    setVoiceRecordingVisible(false);
    const captureMetrics = {
      callbacks: recording.captureCallbacks,
      detector_subframes: recording.detectorSubframes,
      callback_cpu_ms: Math.round(recording.captureCpuMs * 100) / 100,
      callback_max_ms: Math.round(recording.captureMaxCpuMs * 100) / 100,
      detector_cpu_ms: Math.round(recording.detectorCpuMs * 100) / 100,
      resample_cpu_ms: Math.round(recording.resampleCpuMs * 100) / 100,
      echo_guard_callbacks: recording.echoGuardCallbacks || 0,
      echo_guard_suppressed_callbacks: recording.echoGuardSuppressedCallbacks || 0,
      echo_guard_peak_rms: recording.echoGuardPeakRms || 0,
      echo_guard_threshold: recording.echoGuardThreshold || 0,
      echo_reference_callbacks: recording.echoReferenceCallbacks || 0,
      echo_matched_callbacks: recording.echoMatchedCallbacks || 0,
      prefill_ipc_queued_samples: recording.prefillQueuedSamples || 0,
      prefill_ipc_queue_high_water_samples: recording.prefillQueueHighWaterSamples || 0,
      prefill_suppressed_silence_samples: recording.prefillSuppressedSilenceSamples || 0,
      prefill_failed: recording.prefillFailed === true,
    };
    markVoiceLatencyIn(recording.trace, "capture_processing", { ...captureMetrics, reason, discard });
    if (discard) {
      recording.transcription?.cancel();
      abortStoppedVoicePrefill(recording, abortPrefill);
      return null;
    }
    // Physical stop seals the transcript independently. Never await Parakeet
    // here, and never seal it at speculative Gemma end-of-input.
    recording.transcription?.seal();
    if (flushPrefill) {
      enqueueVoicePrefill(recording, new Float32Array(), true);
    }
    await recording.pushChain;
    const modelInputWavBytes = recording.modelInputReliable && !recording.prefillFailed
      && recording.modelInputSourceRanges.length > 0
      ? encodeInferencePcm16Wav(recording.modelInputPcm16Parts, recording.modelInputSampleCount)
      : null;
    markVoiceLatencyIn(recording.trace, "voice_inference_archive", {
      source_samples: recording.capturedSamples,
      accepted_samples: recording.modelInputSampleCount,
      source_ranges: recording.modelInputSourceRanges.length,
      exact_derivative: Boolean(modelInputWavBytes),
    });
    return {
      heardVoice: recording.heardVoice,
      metrics: captureMetrics,
      transcription: recording.transcription,
      trace: recording.trace,
      modelInputWavBytes,
      modelInputSourceRanges: recording.modelInputSourceRanges,
      modelInputSampleCount: recording.modelInputSampleCount,
      sourceSampleCount: recording.capturedSamples,
      // The saved turn is the physical microphone stream. Editing it here
      // would make the archive differ from Parakeet's parallel PCM consumer.
      samples: () => mergeFloat32(recording.capturedPcm),
    };
  };

  const interruptCallPlaybackForSpeech = (recording, detail = {}) => {
    const reply = recording?.call ? activeCallPlayback : null;
    // The prearmed speculative reply is also the live TTS owner while the
    // caller is speaking, but it has no audible PCM yet. Treating the caller's
    // initial voice onset as a barge-in stopped that empty controller and left
    // the later Gemma result with nowhere to send audio.
    if (!reply?.audible) return;
    const startedAt = performance.now();
    const replyTrace = reply.trace;
    const replyTraceId = replyTrace?.id || null;
    const replyRequestId = reply.requestId || null;
    const captureTraceId = recording.trace?.id || null;
    const replyOwnership = {
      reply_trace_id: replyTraceId,
      reply_request_id: replyRequestId,
    };
    const captureOwnership = {
      ...replyOwnership,
      capture_trace_id: captureTraceId,
    };
    markVoiceLatencyIn(recording.trace, "barge_in_detected", {
      ...detail,
      ...replyOwnership,
    });
    if (replyTrace && replyTrace !== recording.trace) {
      markVoiceLatencyIn(replyTrace, "barge_in_detected", captureOwnership);
    }
    void reply.interruptForSpeech().then(() => {
      const stopped = {
        ...replyOwnership,
        duration_ms: Math.round((performance.now() - startedAt) * 100) / 100,
      };
      markVoiceLatencyIn(recording.trace, "barge_in_playback_stopped", stopped);
      if (replyTrace && replyTrace !== recording.trace) {
        markVoiceLatencyIn(replyTrace, "barge_in_playback_stopped", {
          ...stopped,
          capture_trace_id: captureTraceId,
        });
      }
    }).catch((error) => {
      const failure = {
        ...replyOwnership,
        message: errorMessage(error),
      };
      markVoiceLatencyIn(recording.trace, "barge_in_playback_stop_failed", failure);
      if (replyTrace && replyTrace !== recording.trace) {
        markVoiceLatencyIn(replyTrace, "barge_in_playback_stop_failed", {
          ...failure,
          capture_trace_id: captureTraceId,
        });
      }
    });
  };

  const beginBrowserVoiceCapture = async ({ call = false, prefillSession = null,
    prefillRetryPending = false, transcription = null, onVoiceOnset = null,
    deferPrefill = false } = {}) => {
    if (voiceRecording) throw new Error("Voice capture is already active.");
    // The call controller opens the next microphone turn as playback starts so
    // speech can stop the speaker locally. Its model target is attached when
    // the playback buffer has runway, or immediately when real caller speech
    // takes priority over the contact's remaining synthesis.
    // The streamed request lease can outlive audio by one terminal event, so the
    // ordinary composer busy gate must not reject this owned handoff.
    if (!call && chatBusyLeases.size) throw new Error("Voice capture is waiting for the current reply.");
    if (call && !voiceCallActive) throw new Error("The call has ended.");
    startVoiceLatency(call ? "call" : "voice_note");
    const sharedCallStream = call && voiceCallMicrophoneStream?.active
      ? voiceCallMicrophoneStream
      : null;
    const stream = sharedCallStream || await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
    const sharedCallGraph = call && sharedCallStream
      && voiceCallCaptureGraph?.stream === sharedCallStream
      && voiceCallCaptureGraph.context.state !== "closed"
      ? voiceCallCaptureGraph
      : null;
    const context = sharedCallGraph?.context || new AudioContext();
    const source = sharedCallGraph?.source || context.createMediaStreamSource(stream);
    // WebKit's smallest ScriptProcessor quantum is 256 samples (~5.3 ms at
    // 48 kHz). The detector subdivides it into 64-sample windows, giving the
    // squelch ~1.3 ms resolution without allocating per subframe.
    const processor = sharedCallGraph?.processor || context.createScriptProcessor(256, 1, 1);
    const silence = sharedCallGraph?.silence || context.createGain();
    if (!sharedCallGraph) silence.gain.value = 0;
    const capturedPcm = [];
    const recording = {
      stream, context, source, processor, silence, capturedPcm,
      trace: voiceLatencyTrace,
      // Optional staged tap. Default callers remain unchanged until the native
      // owner/command and derived-input source binding are implemented.
      transcription,
      capturedSamples: 0, prefillInputStartSample: null,
      modelInputPcm16Parts: [], modelInputSampleCount: 0, modelInputReliable: true,
      modelInputSourceRanges: [], prefillSourceRanges: [], prefillLastSourceEnd: 0,
      sharedCallStream: stream === voiceCallMicrophoneStream,
      sharedCallGraph: Boolean(sharedCallGraph),
      resampler: globalThis.MomVoiceCapturePolicy.createStreamingMonoResampler(context.sampleRate),
      limit: null, call,
      sourceConversation: selectedConversation(), sourceKind: selectedConversationKind(),
      heardVoice: false, silentMs: 0, sending: false,
      voiceActivity: globalThis.MomVoiceCapturePolicy.initialVoiceActivity(),
      preRoll: [], preRollSamples: 0, prefillParts: [], prefillSamples: 0,
      pushChain: Promise.resolve(), prefillFailed: false, prefillSession: null,
      prefillPending: deferPrefill,
      prefillDeferred: deferPrefill,
      prefillRetryPending,
      prefillEpoch: 0,
      firstPrefillChunkAccepted: false, speculation: null, speculationStarting: false,
      // Give a caller a natural pause before spending compute on a reply.
      // Keep VAD onset/resume fast so speech still cancels speculation promptly.
      speculationReleaseMs: 350,
      prefillPushes: 0, prefillAcceptedSamples: 0, prefillHighWaterSamples: 0,
      prefillQueuedSamples: 0, prefillQueueHighWaterSamples: 0,
      prefillSuppressedSilenceSamples: 0,
      prefillIpcMs: 0, prefillMaxIpcMs: 0,
      captureCallbacks: 0, captureCpuMs: 0, captureMaxCpuMs: 0,
      detectorCpuMs: 0, detectorSubframes: 0, resampleCpuMs: 0,
      echoGuard: globalThis.MomVoiceCapturePolicy.initialPlaybackEchoGuard(),
      echoMatcher: globalThis.MomVoiceCapturePolicy.createPlaybackEchoMatcher(),
      echoGuardCallbacks: 0,
      echoGuardSuppressedCallbacks: 0, echoGuardPeakRms: 0,
      echoGuardThreshold: 0,
      echoReferenceCallbacks: 0, echoMatchedCallbacks: 0, echoEvidence: null,
      detectorSilence: new Float32Array(processor.bufferSize),
    };
    // Transcription is an idle archival operation. The live microphone has
    // one inference consumer, so a slower Mac cannot accumulate parallel ASR
    // work or rewrite history under the prepared Gemma request.
    const initialPrefillEpoch = recording.prefillEpoch;
    recording.prefillSession = deferPrefill
      ? Promise.resolve(null)
      : (prefillSession || prepareVoicePrefill(recording)).then((live) => {
        if (recording.prefillEpoch !== initialPrefillEpoch) {
          if (live?.session) {
            void invoke("mom_llama_voice_audio_abort", { session: live.session }).catch(() => {});
          }
          return null;
        }
        recording.prefillPending = false;
        recording.prefillRetryPending = false;
        if (!live?.session) {
          recording.prefillFailed = true;
          markVoiceLatencyIn(recording.trace, "audio_prefill_begin_failed", {
            message: "The live audio session was unavailable; the saved recording will carry this turn.",
          });
        }
        return live;
      }).catch((error) => {
        if (recording.prefillEpoch === initialPrefillEpoch) {
          recording.prefillPending = false;
          recording.prefillRetryPending = false;
          recording.prefillFailed = true;
          markVoiceLatencyIn(recording.trace, "audio_prefill_begin_failed", { message: errorMessage(error) });
        }
        return null;
      });
    recording.resumeDeferredPrefill = (live) => {
      if (recording.deferredResume) return recording.deferredResume;
      if (!recording.prefillDeferred || !live?.session || voiceRecording !== recording) {
        return Promise.resolve(false);
      }
      const resume = (async () => {
        recording.prefillPending = true;
        recording.prefillFailed = false;
        recording.prefillSession = Promise.resolve(live);
        // The deferred queue contains only source ranges admitted by the
        // detector. Attach it synchronously before awaiting IPC so later
        // callbacks append in order, without replaying the physical archive.
        recording.prefillDeferred = false;
        recording.prefillPending = false;
        recording.prefillRetryPending = false;
        const replayedSamples = recording.prefillSamples;
        enqueueVoicePrefill(recording, new Float32Array(), true);
        await recording.pushChain;
        if (recording.prefillFailed) return false;
        markVoiceLatencyIn(recording.trace, "audio_prefill_deferred_pcm_replayed", {
          samples: replayedSamples,
          captured_samples: recording.capturedSamples,
        });
        return true;
      })();
      recording.deferredResume = resume;
      void resume.then(() => {
        if (recording.deferredResume === resume) recording.deferredResume = null;
      }, () => {
        if (recording.deferredResume === resume) recording.deferredResume = null;
      });
      return resume;
    };
    processor.onaudioprocess = (event) => {
      const callbackStarted = performance.now();
      const chunk = event.inputBuffer.getChannelData(0);
      const previousActivity = recording.voiceActivity;
      const audibleReply = recording.call ? activeCallPlayback : null;
      const playbackAudible = Boolean(audibleReply?.audible);
      let detectorChunk = chunk;
      if (playbackAudible) {
        let power = 0;
        for (let index = 0; index < chunk.length; index += 1) power += chunk[index] * chunk[index];
        const rawRms = Math.sqrt(power / Math.max(1, chunk.length));
        const durationMs = chunk.length * 1000 / context.sampleRate;
        const reference = audibleReply?.controller?.echoReference();
        if (reference) recording.echoReferenceCallbacks += 1;
        const matchedEcho = reference ? recording.echoMatcher.process(chunk, context.sampleRate,
          reference.samples, reference.sampleRate) : null;
        const echo = globalThis.MomVoiceCapturePolicy.updatePlaybackEchoGuard(
          recording.echoGuard,
          matchedEcho ? matchedEcho.residualRms : rawRms,
          durationMs,
          { matchedEcho: Boolean(matchedEcho) },
        );
        if (matchedEcho) recording.echoMatchedCallbacks += 1;
        recording.echoEvidence = {
          reference_available: Boolean(reference),
          matched: Boolean(matchedEcho),
          correlation: matchedEcho?.correlation ?? null,
          residual_rms: matchedEcho?.residualRms ?? null,
          raw_rms: rawRms,
          guard_threshold: echo.threshold,
          suppressed: echo.suppressed,
          source_rate_hz: context.sampleRate,
          reference_rate_hz: reference?.sampleRate ?? null,
          matcher: recording.echoMatcher.snapshot(),
          callback_lag_ms: Number.isFinite(event.playbackTime)
            ? (context.currentTime - event.playbackTime) * 1000 : null,
        };
        recording.echoGuard = echo.guard;
        recording.echoGuardCallbacks += 1;
        recording.echoGuardPeakRms = Math.max(recording.echoGuardPeakRms, rawRms);
        recording.echoGuardThreshold = echo.threshold;
        if (echo.suppressed) {
          recording.echoGuardSuppressedCallbacks += 1;
          detectorChunk = recording.detectorSilence;
        }
      } else if (recording.echoPlaybackActive) {
        recording.echoGuard = globalThis.MomVoiceCapturePolicy.initialPlaybackEchoGuard();
        recording.echoMatcher = globalThis.MomVoiceCapturePolicy.createPlaybackEchoMatcher();
      }
      recording.echoPlaybackActive = playbackAudible;
      const detectorStarted = performance.now();
      const analysis = globalThis.MomVoiceCapturePolicy.analyzeVoiceSamples(
        previousActivity,
        detectorChunk,
        context.sampleRate,
      );
      recording.detectorCpuMs += performance.now() - detectorStarted;
      recording.detectorSubframes += analysis.subframes;
      const { activity, rms, threshold } = analysis;
      recording.voiceActivity = activity;
      recording.heardVoice = activity.heardVoice;
      recording.silentMs = activity.silentMs;
      if (analysis.hadVoiced && recording.speculation
          && !recording.speculation.confirmed && !recording.speculation.aborting) {
        void abortCallSpeculation(recording, { restartPrefill: true }).catch((error) => {
          reportError(error);
          if (recording.call && voiceCallActive) void closeVoiceCallSurface();
        });
      }
      const resampleStarted = performance.now();
      const pcm = recording.resampler.process(chunk);
      recording.resampleCpuMs += performance.now() - resampleStarted;
      if (pcm.length) {
        recording.capturedPcm.push(pcm);
        recording.capturedSamples += pcm.length;
        // Source-side only: this executes during speculation as well. The
        // Gemma replay/retarget path never feeds this independent cursor.
        recording.transcription?.append(pcm);
      }
      // A speculative decode has consumed its native prefill session. Keep
      // recording locally; on resumed speech, restore its prepared turn and
      // replay only the PCM beyond the native audio checkpoint.
      if (!recording.speculation && analysis.hadVoiced) {
        if (!previousActivity.heardVoice) {
          interruptCallPlaybackForSpeech(recording, {
            producer: "browser_capture",
            onset_sample: analysis.voiceOnsetSample,
            echo: recording.echoEvidence,
          });
          // A real caller takes priority over finishing the contact's voice.
          // Release the prefetched-session gate at onset so PCM enters Gemma
          // while the caller is speaking, even if Qwen still has a tail.
          if (onVoiceOnset) {
            queueMicrotask(() => {
              void Promise.resolve(onVoiceOnset(recording)).catch((error) => {
                recording.computePreemptionBlocked = true;
                recording.prefillPending = true;
                recording.prefillFailed = true;
                markVoiceLatencyIn(recording.trace, "voice_onset_prefill_failed", {
                  message: errorMessage(error),
                });
                reportError(error);
              });
            });
          }
          let preRollStart = recording.capturedSamples - pcm.length - recording.preRollSamples;
          recording.preRoll.forEach((part) => {
            enqueueVoicePrefill(recording, part, false, preRollStart);
            preRollStart += part.length;
          });
          recording.preRoll = [];
          recording.preRollSamples = 0;
          markVoiceLatencyIn(recording.trace, "voice_activity_first_detected", {
            onset_sample: analysis.voiceOnsetSample,
            rms,
            threshold,
          });
        }
        enqueueVoicePrefill(recording, pcm);
      } else if (!recording.speculation && activity.heardVoice) {
        // The first silence edge has already flushed the model input. Further
        // silence belongs only to the physical recording; pushing it after
        // speculative decode starts can race native end-of-input.
        if (!recording.speculationStarting && activity.silentMs <= 600) {
          enqueueVoicePrefill(recording, pcm);
        } else {
          recording.prefillSuppressedSilenceSamples += pcm.length;
        }
      } else if (!recording.speculation) {
        recording.preRoll.push(pcm);
        recording.preRollSamples += pcm.length;
        while (recording.preRollSamples > 3200 && recording.preRoll.length > 1) {
          recording.preRollSamples -= recording.preRoll.shift().length;
        }
      }
      if (activity.heardVoice && !activity.voiced
          && activity.silentMs >= (recording.call ? recording.speculationReleaseMs : 20)
          && voiceRecording === recording
          && !recording.sending && !recording.speculation && !recording.speculationStarting
          && !recording.prefillFailed && !recording.prefillPending) {
        markVoiceLatencyIn(recording.trace, "silence_onset", {
          silence_ms: activity.silentMs,
          onset_sample: analysis.silenceOnsetSample,
          rms,
          threshold,
        });
        void startVoiceSpeculation(recording).catch(reportError);
      } else if ((recording.prefillFailed || recording.prefillPending)
          && !recording.computePreemptionBlocked
          && activity.heardVoice
          && activity.silentMs >= 900 && voiceRecording === recording && !recording.sending) {
        if (recording.prefillPending) {
          const retry = recording.prefillRetryPending;
          recording.prefillPending = false;
          recording.prefillRetryPending = false;
          recording.prefillFailed = true;
          recording.prefillEpoch += 1;
          markVoiceLatencyIn(recording.trace, "audio_prefill_wait_expired", {
            silence_ms: activity.silentMs, retry,
          });
        }
        // The physical recording can use the complete-media chat path without
        // waiting for Parakeet or leaving the turn stuck at a silence edge.
        void sendVoiceCapture(recording).catch(reportError);
      } else if (!recording.call && activity.heardVoice && activity.silentMs >= 900
          && voiceRecording === recording && !recording.sending) {
        if (recording.speculation && !recording.speculation.aborting) {
          void confirmVoiceNoteSpeculation(recording, recording.speculation).catch(reportError);
        } else if (!recording.speculationStarting) {
          void sendVoiceCapture(recording).catch(reportError);
        }
      }
      const callbackMs = performance.now() - callbackStarted;
      recording.captureCallbacks += 1;
      recording.captureCpuMs += callbackMs;
      recording.captureMaxCpuMs = Math.max(recording.captureMaxCpuMs, callbackMs);
    };
    if (!sharedCallGraph) {
      source.connect(processor); processor.connect(silence); silence.connect(context.destination);
    }
    voiceRecording = recording;
    if (!call) setVoiceRecordingVisible(true);
    return recording;
  };

  const beginNativeVoiceCapture = async ({
    call = false,
    prefillSession = null,
    recoveryAttempt = 0,
    preserveLatencyTrace = false,
  } = {}) => {
    if (voiceRecording) throw new Error("Voice capture is already active.");
    if (!call && chatBusyLeases.size) throw new Error("Voice capture is waiting for the current reply.");
    if (call && !voiceCallActive) throw new Error("The call has ended.");
    if (!preserveLatencyTrace) startVoiceLatency(call ? "call" : "voice_note");
    const recording = {
      native: true,
      trace: voiceLatencyTrace,
      capture: crypto.randomUUID(),
      call,
      sourceConversation: selectedConversation(),
      sourceKind: selectedConversationKind(),
      heardVoice: false,
      silentMs: 0,
      sending: false,
      voiceActivity: { voiced: false, heardVoice: false },
      prefillFailed: false,
      prefillSession: null,
      targetChain: Promise.resolve(),
      startPromise: null,
      speculation: null,
      speculationStarting: false,
      discarded: false,
      nativeMetrics: null,
      recoveryAttempt,
      limit: null,
    };
    voiceRecording = recording;
    if (!call) setVoiceRecordingVisible(true);
    // Admit the microphone before starting any model preparation. Both are
    // asynchronous, but native long-operation admission preserves request
    // order: starting Gemma first delayed the latest packaged capture by
    // 2.96 seconds while its 1,643-token prefix was restored. Capture retains
    // this head in fixed native storage and retarget replays it exactly once,
    // so listening can begin immediately while preparation runs in parallel.
    markVoiceLatencyIn(recording.trace, "native_capture_requested");
    recording.startPromise = invoke("mom_llama_voice_capture_start", {
      capture: recording.capture,
      call,
    });
    recording.prefillSession = (prefillSession || prepareVoicePrefill(recording)).catch((error) => {
      recording.prefillFailed = true;
      markVoiceLatencyIn(recording.trace, "audio_prefill_begin_failed", {
        producer: "native_ring",
        message: errorMessage(error),
      });
      return null;
    });
    recording.targetChain = recording.prefillSession.then(async (live) => {
      if (!live?.session || recording.discarded || recording.speculation) return;
      await recording.startPromise;
      const targetedAt = performance.now();
      const targeted = await invoke("mom_llama_voice_capture_retarget", {
        capture: recording.capture,
        session: live.session,
      });
      markVoiceLatencyIn(recording.trace, "audio_prefill_native_targeted", {
        session: live.session,
        round_trip_ms: Math.round((performance.now() - targetedAt) * 100) / 100,
        native_elapsed_us: targeted?.native_elapsed_us || null,
      });
      // Arm the native silence gate as soon as the model ring is attached.
      // The command waits off the capture thread, so the first silence edge
      // can release generation without a renderer round trip.
      queueMicrotask(() => {
        if (voiceRecording === recording && !recording.discarded
            && (!recording.call || voiceCallActive)) {
          void startVoiceSpeculation(recording, { prearm: true }).catch(reportError);
        }
      });
      return targeted;
    }).catch((error) => {
      recording.prefillFailed = true;
      markVoiceLatencyIn(recording.trace, "audio_prefill_target_failed", {
        producer: "native_ring",
        message: errorMessage(error),
      });
      return null;
    });
    try {
      await recording.startPromise;
      markVoiceLatencyIn(recording.trace, "native_capture_started");
    } catch (error) {
      recording.discarded = true;
      if (voiceRecording === recording) voiceRecording = null;
      setVoiceRecordingVisible(false);
      abortStoppedVoicePrefill(recording, true);
      throw error;
    }
    return recording;
  };

  const beginVoiceCapture = async (options = {}) => {
    if (!options.call && (failedVoiceReplyOwners.size >= MAX_FAILED_VOICE_REPLY_OWNERS
        || activeVoiceNotePublications.size >= MAX_FAILED_VOICE_REPLY_OWNERS)) {
      throw new Error("voice_reply_save_retry_required: Save the previous voice reply before recording another.");
    }
    // This runs from the user's microphone/call gesture. Compile and suspend
    // the audio-thread renderer while capture and model prefill are already
    // in flight, so first PCM never waits for AudioWorklet setup.
    void preparePcmWorklet(24_000);
    // WebKit owns call capture because its voice-processing route can subtract
    // this app's playback. Native CPAL remains the lower-overhead voice-note
    // path, where there is no simultaneous speaker output to cancel.
    if (options.call) return beginBrowserVoiceCapture(options);
    try {
      return await beginNativeVoiceCapture(options);
    } catch (error) {
      // Older or restricted hosts retain the existing Web Audio path. A
      // native build should never reach this branch during ordinary capture.
      markVoiceLatency("native_capture_unavailable", { message: errorMessage(error) });
      return beginBrowserVoiceCapture(options);
    }
  };

  const resumeBrowserVoicePrefill = (recording, replacement) => {
    if (voiceRecording !== recording || (recording.call && !voiceCallActive)) return;
    const resumeOffset = Number(replacement?.resume_sample_offset);
    if (recording.native || !replacement?.session
        || !Number.isSafeInteger(resumeOffset) || resumeOffset < 0
        || (replacement.resume_reused !== true && resumeOffset !== 0)) {
      throw new Error("The live audio checkpoint could not be resumed.");
    }
    recording.prefillEpoch = (recording.prefillEpoch || 0) + 1;
    const captured = mergeFloat32(recording.capturedPcm);
    const sourceOffset = recording.modelInputSourceRanges.length
      ? inferenceSourceOffset(recording.modelInputSourceRanges, resumeOffset)
      : (resumeOffset === 0 ? recording.prefillInputStartSample : null);
    if (!Number.isSafeInteger(sourceOffset) || sourceOffset < 0 || sourceOffset > captured.length
        || resumeOffset > recording.modelInputSampleCount) {
      throw new Error("The voice checkpoint is outside the acknowledged audio.");
    }
    truncateInferencePcm16(recording, resumeOffset);
    const priorAudio = captured.subarray(sourceOffset);
    recording.prefillParts = [];
    recording.prefillSourceRanges = [];
    recording.prefillLastSourceEnd = sourceOffset;
    recording.prefillSamples = 0;
    recording.prefillQueuedSamples = 0;
    recording.prefillQueueHighWaterSamples = 0;
    recording.pushChain = Promise.resolve();
    recording.prefillFailed = false;
    recording.firstPrefillChunkAccepted = false;
    recording.prefillPushes = 0;
    recording.prefillAcceptedSamples = 0;
    recording.prefillHighWaterSamples = 0;
    recording.prefillIpcMs = 0;
    recording.prefillMaxIpcMs = 0;
    recording.prefillSession = Promise.resolve({
      session: replacement.session,
      request_id: replacement.request_id,
      conversation: replacement.conversation_id || voiceCallConversation || recording.sourceConversation,
      begin_metrics: replacement.begin_metrics || null,
    });
    enqueueVoicePrefill(recording, priorAudio, true, sourceOffset);
    markVoiceLatencyIn(recording.trace, "speculation_handoff_replay_enqueued", {
      checkpoint_samples: resumeOffset,
      replay_samples: priorAudio.length,
      captured_samples: captured.length,
    });
  };

  const abortCallSpeculation = async (recording, { restartPrefill = false } = {}) => {
    const speculation = recording?.speculation;
    if (!speculation || speculation.confirmed || speculation.aborting) return;
    speculation.aborting = true;
    speculation.resolveCapture?.(null);
    markVoiceLatencyIn(recording.trace, "speculation_aborted", { reason: restartPrefill ? "speech_resumed" : "capture_stopped" });
    if (!recording.native && recording.call && restartPrefill) {
      // A short intra-utterance pause can cross the first speculative edge.
      // Repeated belays are direct evidence that this caller needs a longer
      // endpoint; cap the added wait so a genuine reply remains responsive.
      recording.speculationReleaseMs = Math.min(600, recording.speculationReleaseMs + 140);
    }

    // Stop the speaker immediately, but never serialize microphone recovery
    // behind WebAudio/native TTS teardown. `stop()` closes local playback
    // synchronously before its first await; the remaining cleanup can settle
    // independently while native capture is retargeted.
    const ttsStopAt = performance.now();
    void speculation.tts.stop().catch(() => {}).finally(() => {
      markVoiceLatencyIn(recording.trace, "speculation_tts_stopped", {
        duration_ms: Math.round((performance.now() - ttsStopAt) * 100) / 100,
      });
    });

    let replacement = null;
    let replacementError = null;
    const nativeAbortAt = performance.now();
    if (restartPrefill) {
      markVoiceLatencyIn(recording.trace, "speculation_handoff_started");
      try {
        await withDeadline(speculation.admissionReady, 2_000,
          "voice_speculation_admission_timeout: The call turn could not be prepared for interruption.");
        replacement = await invoke("mom_llama_voice_audio_speculation_abort_and_retarget", {
          capture: recording.native ? recording.capture : null,
          speculation: speculation.id,
          conversation: speculation.conversation,
          message: voiceModePrompt(recording.call),
        });
        const live = {
          session: replacement.session,
          request_id: replacement.request_id,
          conversation: replacement.conversation_id || speculation.conversation,
          begin_metrics: replacement.begin_metrics || null,
        };
        recording.prefillFailed = false;
        recording.prefillSession = Promise.resolve(live);
        recording.targetChain = Promise.resolve(replacement);
        markVoiceLatencyIn(recording.trace, "speculation_handoff_ready", {
          round_trip_ms: Math.round((performance.now() - nativeAbortAt) * 100) / 100,
          abort_ms: Math.round(Number(replacement.abort_us || 0) / 10) / 100,
          begin_ms: Math.round(Number(replacement.begin_us || 0) / 10) / 100,
          prepared_reuse: replacement.begin_metrics?.prepared_reuse === true,
          request_clone_us: Number(replacement.begin_metrics?.request_clone_us || 0),
          operation_register_us: Number(replacement.begin_metrics?.operation_register_us || 0),
          generation_admission_us: Number(replacement.begin_metrics?.generation_admission_us || 0),
          resume_wait_us: Number(replacement.resume_wait_us || 0),
          resume_reused: replacement.resume_reused === true,
          resume_sample_offset: Number(replacement.resume_sample_offset || 0),
          resume_resident_max_sequences: Number(replacement.resume_resident_max_sequences || 0),
          resume_checkpoint_supported: replacement.resume_checkpoint_supported === true,
          resume_checkpoint_available: replacement.resume_checkpoint_available === true,
          resume_key_matched: replacement.resume_key_matched === true,
          resume_request_matched: replacement.resume_request_matched === true,
          resume_binding_us: Number(replacement.resume_binding_us || 0),
          resume_restore_us: Number(replacement.resume_restore_us || 0),
          retarget_ms: Math.round(Number(replacement.retarget_us || 0) / 10) / 100,
          replay: replacement.replay || null,
          native_total_ms: Math.round(Number(replacement.total_us || 0) / 10) / 100,
          begin: replacement.begin_metrics || null,
        });
      } catch (error) {
        replacementError = error;
        recording.prefillFailed = true;
        markVoiceLatencyIn(recording.trace, "speculation_handoff_failed", { message: errorMessage(error) });
      }
    } else {
      await invoke("mom_llama_voice_audio_speculation_abort", {
        speculation: speculation.id,
        conversation: speculation.conversation,
      }).catch((error) => { replacementError = error; });
      markVoiceLatencyIn(recording.trace, "speculation_native_abort_complete", {
        duration_ms: Math.round((performance.now() - nativeAbortAt) * 100) / 100,
      });
    }
    if (recording.speculation === speculation) recording.speculation = null;
    if (activeVoiceSpeculation === speculation) activeVoiceSpeculation = null;
    if (liveVoiceTts === speculation.tts) liveVoiceTts = null;
    if (activeDispatchOwner === speculation.tts) {
      activeDispatchOwner = null;
      if (activeDispatchConversation === speculation.conversation) activeDispatchConversation = null;
      if (callReplyConversation === speculation.conversation) callReplyConversation = null;
      if (voiceReplyConversation === speculation.conversation) voiceReplyConversation = null;
    }
    if (replacement && !recording.native && voiceRecording === recording) {
      try {
        resumeBrowserVoicePrefill(recording, replacement);
      } catch (error) {
        void invoke("mom_llama_voice_audio_abort", { session: replacement.session }).catch(() => {});
        replacementError = error;
        recording.prefillFailed = true;
        markVoiceLatencyIn(recording.trace, "speculation_handoff_failed", { message: errorMessage(error) });
        replacement = null;
      }
    }
    if (replacement && voiceRecording !== recording) {
      void invoke("mom_llama_voice_audio_abort", { session: replacement.session }).catch(() => {});
      return;
    }
    if (replacement && voiceRecording === recording && !recording.sending
        && (!recording.call || voiceCallActive)) {
      // Silence can begin while the replacement Gemma session is restoring.
      // That edge has already passed by the time retargeting completes, so no
      // future capture event is guaranteed to start another attempt. Re-arm
      // the native gate immediately; it observes the capture's current state
      // and releases at once if silence is already in progress.
      queueMicrotask(() => {
        if (voiceRecording === recording && !recording.sending && !recording.speculation
            && (!recording.call || voiceCallActive)) {
          if (!recording.native && (recording.voiceActivity.voiced
              || recording.silentMs < recording.speculationReleaseMs)) return;
          markVoiceLatencyIn(recording.trace, "speculation_handoff_rearmed", {
            silent_ms: recording.silentMs || 0,
          });
          void startVoiceSpeculation(recording,
            { prearm: recording.native }).catch(reportError);
        }
      });
    }
    // Never rebuild an utterance from its first sample after belaying a
    // speculative reply. That would replay seconds of valid projector work
    // and make each interruption progressively slower.
    if (restartPrefill && !replacement) recording.prefillFailed = true;
    if (replacementError) throw replacementError;
  };

  const sealedVoiceRecordingBytes = (capture) => {
    if (!capture?.heardVoice) throw new Error("The voice turn has no complete recording.");
    const bytes = capture.wavBytes || encodePcmWav(capture.samples());
    if (bytes.byteLength <= 44) throw new Error("The voice turn has no complete recording.");
    return Array.from(bytes);
  };

  const sealedVoiceInferenceRecording = (capture) => {
    const bytes = capture?.modelInputWavBytes;
    if (!(bytes instanceof Uint8Array) || bytes.byteLength <= 44) return null;
    const sourceSampleCount = Number(capture.sourceSampleCount);
    const acceptedSampleCount = Number(capture.modelInputSampleCount);
    if (!Number.isSafeInteger(sourceSampleCount) || sourceSampleCount <= 0
        || !Number.isSafeInteger(acceptedSampleCount) || acceptedSampleCount <= 0) return null;
    const sourceRanges = Array.isArray(capture.modelInputSourceRanges)
      ? capture.modelInputSourceRanges.map((range) => ({
        start: Number(range?.start),
        end_exclusive: Number(range?.end_exclusive),
      }))
      : null;
    const sourceStartSample = Number(capture.modelInputStartSample);
    if (sourceRanges) {
      let previousEnd = 0;
      let rangeSamples = 0;
      if (sourceRanges.length === 0) return null;
      for (const range of sourceRanges) {
        if (!Number.isSafeInteger(range.start) || !Number.isSafeInteger(range.end_exclusive)
            || range.start < previousEnd || range.start >= range.end_exclusive
            || range.end_exclusive > sourceSampleCount) return null;
        rangeSamples += range.end_exclusive - range.start;
        previousEnd = range.end_exclusive;
      }
      if (rangeSamples !== acceptedSampleCount) return null;
    } else if (!Number.isSafeInteger(sourceStartSample) || sourceStartSample < 0
        || sourceStartSample + acceptedSampleCount > sourceSampleCount) return null;
    return {
      recording: Array.from(bytes),
      sourceSampleCount,
      acceptedSampleCount,
      ...(sourceRanges ? { sourceRanges } : { sourceStartSample }),
    };
  };

  const confirmRecordedSpeculation = (speculation, stoppedCapture) => stoppedCapture
    .then((capture) => {
      const modelInput = sealedVoiceInferenceRecording(capture);
      return invoke("mom_llama_voice_audio_speculation_confirm", {
        speculation: speculation.id,
        recording: sealedVoiceRecordingBytes(capture),
        ...(modelInput ? { modelInputRecording: modelInput.recording,
          sourceSampleCount: modelInput.sourceSampleCount,
          acceptedSampleCount: modelInput.acceptedSampleCount,
          ...(modelInput.sourceRanges
            ? { sourceRanges: modelInput.sourceRanges }
            : { sourceStartSample: modelInput.sourceStartSample }) } : {}),
      });
    })
    .catch(async (error) => {
      await speculation.tts.stop().catch(() => {});
      await invoke("mom_llama_voice_audio_speculation_abort", {
        speculation: speculation.id,
        conversation: speculation.conversation,
      }).catch(() => {});
      throw error;
    });

  const confirmCallSpeculation = async (recording, speculation) => {
    if (recording.speculation !== speculation || speculation.confirmed
        || speculation.aborting || recording.voiceActivity.voiced
        || !voiceCallActive || voiceRecording !== recording) {
      await abortCallSpeculation(recording, { restartPrefill: voiceCallActive });
      return;
    }
    speculation.confirmed = true;
    markVoiceLatencyIn(recording.trace, "speculation_confirmed");
    // Sever microphone capture synchronously at the commit boundary. The
    // native confirmation IPC must not leave a window in which resumed speech
    // is accidentally attributed to the reply that is already audible.
    const stoppedCapture = stopVoiceCapture({ abortPrefill: false, flushPrefill: false,
      reason: "call_speculation_confirmed" });
    speculation.capturePromise = stoppedCapture;
    const confirmed = confirmRecordedSpeculation(speculation, stoppedCapture);
    speculation.resolveCapture?.(Promise.all([stoppedCapture, confirmed]).then(([capture]) => capture));
    // Local capture is already severed before any promise is awaited. Start
    // the buffered reply now; native commit and recorder drain are bookkeeping
    // that can complete concurrently and must not extend time to first sound.
    const playback = speculation.tts.play();
    await Promise.all([playback, confirmed, stoppedCapture]);
  };

  const confirmVoiceNoteSpeculation = async (recording, speculation) => {
    if (recording.speculation !== speculation || speculation.confirmed
        || speculation.aborting || voiceRecording !== recording) return false;
    speculation.confirmed = true;
    recording.sending = true;
    setVoiceRecordingVisible(false);
    markVoiceLatencyIn(recording.trace, "speculation_confirmed", { mode: "voice_note" });

    // The reply has remained private while the user could still resume
    // speaking. Publish only after the send boundary, while capture drain and
    // WAV encoding continue concurrently with the native commit latch.
    const outgoing = appendOptimisticVoiceMessage();
    const pendingReply = appendLiveVoiceReply("live-voice-reply-pending");
    if (outgoing?.dataset) outgoing.dataset.voiceSpeculationId = speculation.id;
    if (pendingReply?.dataset) pendingReply.dataset.voiceSpeculationId = speculation.id;
    const pendingButton = document.querySelector(
      "#live-voice-reply-pending [data-action='voice-reply-pending-play']",
    );
    speculation.tts.bindButton(pendingButton);
    speculation.tts.bindPendingRow();
    voiceReplyConversation = speculation.conversation;
    liveVoiceTts = speculation.tts;

    const stoppedCapture = stopVoiceCapture({ abortPrefill: false, flushPrefill: false,
      reason: "voice_note_speculation_confirmed" });
    const confirmed = confirmRecordedSpeculation(speculation, stoppedCapture);
    speculation.resolveCapture?.(Promise.all([stoppedCapture, confirmed]).then(([capture]) => capture));
    let capture = null;
    let captureWavBytes = null;
    let captureWavAttempted = false;
    const materializeCaptureWav = () => {
      if (captureWavAttempted) return captureWavBytes;
      captureWavAttempted = true;
      captureWavBytes = capture?.wavBytes
        || (typeof capture?.samples === "function" ? encodePcmWav(capture.samples()) : null);
      return captureWavBytes;
    };
    try {
      [capture] = await Promise.all([stoppedCapture, confirmed]);
      captureWavBytes = materializeCaptureWav();
      if (captureWavBytes?.byteLength > 44) {
        if (outgoing?.dataset?.voiceSpeculationId === speculation.id) {
          hydrateOptimisticVoiceMessage(outgoing, captureWavBytes);
        }
      } else {
        if (outgoing?.dataset?.voiceSpeculationId === speculation.id) {
          outgoing.querySelector(".message-card")?.replaceChildren(unavailableVoiceMessage());
        }
      }
      return true;
    } catch (error) {
      // Confirmation can fail after the optimistic rows are visible (most
      // commonly because the stopped capture contains no speech). Settle only
      // these exact nodes and this speculation's controller: a later reply in
      // the same conversation may already own the global routing state.
      capture ??= await stoppedCapture.catch(() => null);
      let captureWavError = null;
      try {
        materializeCaptureWav();
      } catch (wavError) {
        captureWavError = wavError;
      }
      const hasRecording = capture?.heardVoice === true
        && captureWavBytes?.byteLength > 44;
      const ownsOutgoing = outgoing?.dataset?.voiceSpeculationId === speculation.id;
      const ownsPendingReply = pendingReply?.dataset?.voiceSpeculationId === speculation.id;
      if (hasRecording) {
        if (ownsOutgoing) {
          try {
            hydrateOptimisticVoiceMessage(outgoing, captureWavBytes);
          } catch (hydrateError) {
            markVoiceLatencyIn(recording.trace, "voice_note_recording_hydration_failed", {
              message: errorMessage(hydrateError),
            });
            outgoing.querySelector(".message-card")?.replaceChildren(unavailableVoiceMessage());
          }
        }
        if (ownsPendingReply) {
          const card = pendingReply.querySelector(".message-card");
          if (card) card.replaceChildren(unavailableVoiceMessage());
          pendingReply.hidden = false;
          pendingReply.classList.remove("voice-reply-pending");
        }
      } else {
        if (ownsOutgoing) {
          if (outgoing.dataset.objectUrl) URL.revokeObjectURL(outgoing.dataset.objectUrl);
          outgoing.remove();
        }
        if (ownsPendingReply) pendingReply.remove();
      }
      if (liveVoiceTts === speculation.tts) {
        liveVoiceTts = null;
        if (voiceReplyConversation === speculation.conversation) {
          voiceReplyConversation = null;
        }
      }
      markVoiceLatencyIn(recording.trace, "voice_note_confirmation_failed", {
        heard_voice: hasRecording,
        message: errorMessage(error),
        ...(captureWavError ? { recording_encode_error: errorMessage(captureWavError) } : {}),
      });
      throw error;
    }
  };

  const startVoiceSpeculation = async (recording, { prearm = false } = {}) => {
    if (voiceRecording !== recording || recording.speculation
        || recording.speculationStarting || recording.sending
        || (recording.call && !voiceCallActive)) return;
    recording.speculationStarting = true;
    markVoiceLatencyIn(recording.trace, "prefill_flush_started");
    if (recording.native) {
      await recording.targetChain;
    } else {
      enqueueVoicePrefill(recording, new Float32Array(), true);
      try {
        // This is a snapshot of pushes already queued at the silence edge.
        // A stalled IPC/ring producer must not strand the entire call.
        await withDeadline(recording.pushChain, recording.call ? 720 : 1_200,
          "voice_audio_prefill_drain_timeout: The microphone audio stopped reaching the model.",
          "voice_audio_prefill_drain_timeout");
      } catch (error) {
        recording.speculationStarting = false;
        if (recording.voiceActivity.voiced) {
          markVoiceLatencyIn(recording.trace, "audio_prefill_drain_deferred", {
            message: errorMessage(error), reason: "speech_resumed",
          });
          return;
        }
        if (voiceRecording === recording) {
          recording.prefillFailed = true;
          recording.prefillEpoch += 1;
          markVoiceLatencyIn(recording.trace, "audio_prefill_drain_failed", {
            message: errorMessage(error),
            producer_pushes: recording.prefillPushes || 0,
            producer_accepted_samples: recording.prefillAcceptedSamples || 0,
          });
          void recording.prefillSession.then((live) => live?.session
            ? invoke("mom_llama_voice_audio_abort", { session: live.session })
            : null).catch(() => {});
        }
        return;
      }
    }
    markVoiceLatencyIn(recording.trace, "prefill_flush_accepted", {
      producer: recording.native ? "native_ring" : "renderer_ipc",
      producer_pushes: recording.prefillPushes || 0,
      producer_ipc_ms: Math.round(recording.prefillIpcMs || 0),
      producer_max_ipc_ms: Math.round(recording.prefillMaxIpcMs || 0),
    });
    const live = await recording.prefillSession.catch(() => null);
    if (!live?.session && voiceRecording === recording && !recording.prefillFailed) {
      recording.prefillFailed = true;
      markVoiceLatencyIn(recording.trace, "audio_prefill_begin_failed", {
        message: "The live audio session was unavailable; the saved recording will carry this turn.",
      });
    }
    if (!live?.session || recording.prefillFailed || voiceRecording !== recording
        || (!prearm && recording.voiceActivity.voiced)
        || (recording.call && !voiceCallActive)) {
      recording.speculationStarting = false;
      if (recording.native && live?.session && voiceRecording === recording
          && (!recording.call || voiceCallActive)) {
        recording.targetChain = invoke("mom_llama_voice_capture_retarget", {
          capture: recording.capture,
          session: live.session,
        });
        await recording.targetChain;
      }
      return;
    }
    const speculation = {
      id: crypto.randomUUID(),
      conversation: live.conversation,
      tts: null,
      confirmed: false,
      aborting: false,
      generationReleased: null,
      releaseGeneration: null,
      admissionReady: null,
      releaseAdmission: null,
    };
    speculation.admissionReady = new Promise((resolve) => {
      speculation.releaseAdmission = resolve;
    });
    speculation.generationReleased = new Promise((resolve) => {
      speculation.releaseGeneration = resolve;
    });
    speculation.captureReady = new Promise((resolve) => {
      speculation.resolveCapture = resolve;
    });
    const tts = beginLiveVoiceTts({
      conversation: live.conversation,
      mode: recording.call ? "call" : "voice_note",
      requestId: live.request_id,
      callEpoch: voiceCallEpoch,
      deferPlayback: true,
      onFirstChunk: recording.call
        ? () => { void confirmCallSpeculation(recording, speculation).catch(reportError); }
        : null,
    });
    speculation.tts = tts;
    recording.speculation = speculation;
    activeVoiceSpeculation = speculation;
    recording.speculationStarting = false;
    liveVoiceTts = tts;
    if (recording.call) callReplyConversation = live.conversation;
    else voiceReplyConversation = live.conversation;
    activeDispatchConversation = live.conversation;
    activeDispatchOwner = tts;
    markVoiceLatencyIn(recording.trace, prearm ? "speculation_prearmed" : "speculation_started", {
      speculation: speculation.id,
      producer: recording.native ? "native_ring" : "renderer_ipc",
      producer_pushes: recording.prefillPushes || 0,
      producer_accepted_samples: recording.prefillAcceptedSamples || 0,
      producer_high_water_samples: recording.prefillHighWaterSamples || 0,
      producer_ipc_queue_high_water_samples: recording.prefillQueueHighWaterSamples || 0,
      producer_ipc_ms: Math.round(recording.prefillIpcMs || 0),
      producer_max_ipc_ms: Math.round(recording.prefillMaxIpcMs || 0),
    });
    if (!prearm) markVoiceLatencyIn(recording.trace, "generation_dispatched", {
      speculative: true,
      speculation: speculation.id,
    });
    // Native send_ready may race the async prefill/session handoff. Re-check
    // the already accumulated silence after publishing the speculation owner
    // so an edge delivered during startup cannot strand the note indefinitely.
    if (!recording.call && recording.silentMs >= 900 && voiceRecording === recording) {
      queueMicrotask(() => {
        if (voiceRecording === recording && recording.speculation === speculation
            && !recording.sending && !speculation.aborting) {
          void confirmVoiceNoteSpeculation(recording, speculation).catch(reportError);
        }
      });
    }
    try {
      markVoiceLatencyIn(recording.trace, "speculation_dispatch_started", {
        speculation: speculation.id,
        producer: recording.native ? "native_ring" : "renderer_ipc",
      });
      const generationTimeoutMs = 30_000;
      const rendererFailsafeMs = generationTimeoutMs + 2_000;
      const waitForNativeSilence = Boolean(prearm && recording.native);
      const generation = tts.trackGeneration(invoke("mom_llama_voice_audio_speculate", {
        session: live.session,
        speculation: speculation.id,
        capture: recording.native ? recording.capture : null,
        waitForSilence: waitForNativeSilence,
        generationTimeoutMs,
      }));
      // A prearmed request may wait while the caller speaks for any length of
      // time. Start its deadline only when native VAD releases the silence
      // gate. The unguarded branch still propagates an admission error that
      // occurs before that release.
      const response = await (waitForNativeSilence
        ? Promise.race([
          generation,
          speculation.generationReleased.then(() => withProgressDeadline(
            generation,
            rendererFailsafeMs,
            () => tts.lastProgressAt,
            "voice_generation_timeout: The contact stopped making progress while preparing a reply.",
          )),
        ])
        : withProgressDeadline(
          generation,
          rendererFailsafeMs,
          () => tts.lastProgressAt,
          "voice_generation_timeout: The contact stopped making progress while preparing a reply.",
        ));
      if (recording.speculation !== speculation || speculation.aborting) return;
      if (response?.status === "blocked" || !response?.result) {
        throw new Error(response?.blocker?.message || "The speculative call reply failed.");
      }
      markVoiceLatencyIn(recording.trace, "generation_finished", {
        speculative: true,
        speculation: speculation.id,
        native_duration_ms: response.result.duration_ms,
        first_token_ms: response.result.first_token_ms,
        cache_metrics: response.result.cache_metrics || null,
        voice_audio: response.result.voice_audio_metrics,
      });
      const synthesized = await finishVoiceReplyAndIndexTurn(recording.call,
        () => indexVoiceTurn(live.conversation, response.result, null,
          speculation.captureReady, recording.trace),
        () => tts.finish(response.result.assistant_text || tts.text),
        recording.call ? null : () => tts.bindDurableVoiceReply({
          conversation: live.conversation,
          message: response.result.assistant_message_id,
          text: response.result.assistant_text || tts.text,
        }));
      if (!synthesized) {
        throw new Error("voice_reply_incomplete: The completed reply did not produce complete audio.");
      }
      if (!recording.call) {
        void Promise.all([refreshSidebar(), refreshConversationSettingsProjection()]).catch(reportError);
      }
    } catch (error) {
      if (error?.code === "voice_generation_timeout") {
        markVoiceLatencyIn(recording.trace, "generation_timeout", {
          speculation: speculation.id,
          phase: recording.call ? "call_turn" : "voice_note",
          timeout_ms: 32_000,
        });
      }
      if (speculation.confirmed && !speculation.aborting) {
        await tts.unavailable(error);
      } else if (!speculation.aborting && voiceRecording === recording
          && (!recording.call || voiceCallActive)) {
        // Only resumed speech may replace a speculative session. A terminal
        // generation failure while the caller remains silent has no new PCM
        // to justify another turn; restarting here immediately re-releases on
        // the same silence, races the failed owner, and can strand the single
        // resident Gemma worker for the next call.
        const resumedSpeech = recording.voiceActivity.voiced === true;
        if (recording.call && !resumedSpeech && voiceCallActive) reportError(error);
        await abortCallSpeculation(recording, { restartPrefill: resumedSpeech });
        if (recording.call && !resumedSpeech && voiceCallActive) {
          const closing = closeVoiceCallSurface();
          await closing;
        }
        throw error;
      }
    } finally {
      if (!speculation.confirmed) speculation.resolveCapture?.(null);
      // All speculative attempts for one call share a conversation. Do not
      // let a superseded attempt clear the routing markers published by its
      // replacement after a resumed-speech handoff.
      const stillOwnsDispatch = activeDispatchOwner === tts;
      if (stillOwnsDispatch && activeDispatchConversation === live.conversation) {
        activeDispatchConversation = null;
      }
      if (stillOwnsDispatch) activeDispatchOwner = null;
      if (stillOwnsDispatch && callReplyConversation === live.conversation) {
        callReplyConversation = null;
      }
      if (stillOwnsDispatch && !speculation.confirmed
          && voiceReplyConversation === live.conversation) {
        voiceReplyConversation = null;
      }
      if (activeVoiceSpeculation === speculation) activeVoiceSpeculation = null;
      if (!recording.call && stillOwnsDispatch) {
        void recoverPendingVoiceHistory().catch(() => {});
      }
    }
  };

  const sendVoiceCapture = async (expectedRecording = voiceRecording) => {
    if (!expectedRecording || voiceRecording !== expectedRecording || expectedRecording.sending) return;
    // Resumed speech atomically replaces the consumed speculative audio
    // session. A send gesture can arrive while that native handoff is still
    // running; wait for the replacement identity instead of submitting the
    // already-consumed session and reporting that live voice disappeared.
    if (expectedRecording.speculationRecovery) {
      await expectedRecording.speculationRecovery;
      if (voiceRecording !== expectedRecording) return;
    }
    if (expectedRecording.speculationRecoveryError) {
      const error = expectedRecording.speculationRecoveryError;
      expectedRecording.speculationRecoveryError = null;
      throw error;
    }
    if (!expectedRecording.call && expectedRecording.speculation
        && !expectedRecording.speculation.confirmed && !expectedRecording.speculation.aborting) {
      await confirmVoiceNoteSpeculation(expectedRecording, expectedRecording.speculation);
      return;
    }
    expectedRecording.sending = true;
    // A send tap must commit immediately. Native prefill draining, audio-context
    // shutdown, encoding, and persistence continue after the recorder is gone.
    setVoiceRecordingVisible(false);
    const call = Boolean(voiceRecording?.call);
    const capture = await stopVoiceCapture({ flushPrefill: !expectedRecording.prefillFailed });
    if (!capture?.heardVoice) {
      report({ status: "blocked", blocker: { code: "voice_message_silent", message: "No speech was detected." } });
      if (voiceCallActive) await beginVoiceCapture({ call: true });
      return;
    }
    const form = document.getElementById("chat-form");
    // Once a failed or expired incremental attempt chose recorded-audio
    // fallback, its session may still be starting. It cannot hold this turn.
    const live = expectedRecording.prefillFailed
      ? null : await expectedRecording.prefillSession.catch(() => null);
    const sourceConversation = call && voiceCallConversation
      ? voiceCallConversation
      : expectedRecording.sourceConversation;
    const conversation = live?.conversation || sourceConversation;
    pendingVoiceFallbackAttachment = null;
    if (live?.session && !expectedRecording.prefillFailed) {
      pendingVoiceAudioSession = {
        session: live.session,
        conversation,
        requestId: live.request_id,
        recording: sealedVoiceRecordingBytes(capture),
        modelInput: sealedVoiceInferenceRecording(capture),
        sourceCapture: capture,
      };
    } else {
      const wav = capture.wavBytes || encodePcmWav(capture.samples());
      markVoiceLatencyIn(expectedRecording.trace, "fallback_capture_encoded", { wav_bytes: wav.byteLength });
      // Preserve the attachment if incremental audio admission failed. This is
      // an explicit degraded path; the normal path persists it concurrently
      // inside voice_audio_finish.
      const imported = await invoke("mom_llama_voice_message_import", {
        conversation,
        bytes: Array.from(wav),
      });
      markVoiceLatencyIn(expectedRecording.trace, "fallback_import_finished");
      report(imported);
      if (imported?.status === "blocked") {
        if (voiceCallActive) await beginVoiceCapture({ call: true });
        return;
      }
      pendingVoiceAudioSession = null;
      pendingVoiceFallbackAttachment = imported.result?.attachment?.id ? {
        conversation,
        attachment: imported.result.attachment.id,
        sourceCapture: capture,
      } : null;
    }
    if (call) callReplyConversation = conversation;
    const currentForm = document.getElementById("chat-form");
    pendingVoiceDispatchPrompt = voiceModePrompt(call);
    cancelComposerAutocomplete({ native: true, announce: false });
    if (liveVoiceTts) await liveVoiceTts.stop().catch(() => {});
    voiceReplyPlayRequested = false;
    if (!call) {
      // The just-recorded samples are already resident. Materialize their
      // small local replay file before mounting the row so the outgoing play
      // affordance is never shown in a disabled, misleading state.
      const replayStarted = performance.now();
      const replayBytes = capture.wavBytes || encodePcmWav(capture.samples());
      markVoiceLatencyIn(expectedRecording.trace, "local_replay_encoded", {
        duration_ms: Math.round((performance.now() - replayStarted) * 100) / 100,
        wav_bytes: replayBytes.byteLength,
      });
      appendOptimisticVoiceMessage(replayBytes);
      appendLiveVoiceReply("live-voice-reply-pending");
      // Set this only after the pre-dispatch refresh. Setting it sooner made
      // that refresh mistake every historical reply for the pending one.
      voiceReplyConversation = conversation;
    }
    const pendingButton = document.querySelector(
      "#live-voice-reply-pending [data-action='voice-reply-pending-play']",
    );
    liveVoiceTts = beginLiveVoiceTts({
      conversation,
      mode: call ? "call" : "voice_note",
      requestId: live?.request_id || null,
      clientRequest: live?.request_id ? null : speechOperationToken(),
      button: pendingButton,
      callEpoch: call ? voiceCallEpoch : null,
    });
    pendingVoiceTtsOwner = liveVoiceTts;
    if (pendingButton && liveVoiceTts) voiceReplyPlayback.set(pendingButton, liveVoiceTts);
    currentForm?.requestSubmit();
    // Dispatch is already in flight; projections are presentation work and
    // must never sit on the response-critical path.
    void Promise.all([refreshSidebar(), refreshConversationSettingsProjection()]).catch(reportError);
  };

  const acquireChatBusy = (scope) => {
    const lease = `${scope}:${++chatBusyLeaseSerial}`;
    chatBusyLeases.add(lease);
    renderChatBusyState();
    return lease;
  };

  const acquireStableChatBusy = (lease) => {
    if (!lease) return null;
    chatBusyLeases.add(lease);
    if (activeDispatchConversation !== null) {
      activeDispatchStreamLeases.add(lease);
      activeDispatchStreamLeaseOwners.set(lease, activeDispatchOwner);
    }
    renderChatBusyState();
    return lease;
  };

  const releaseChatBusy = (lease) => {
    if (!lease) return;
    chatBusyLeases.delete(lease);
    activeDispatchStreamLeases.delete(lease);
    activeDispatchStreamLeaseOwners.delete(lease);
    renderChatBusyState();
  };

  const releaseMentionInvocationBusy = (invocation) => {
    if (!invocation) return;
    const prefix = `mention-request:${invocation}:`;
    [...chatBusyLeases]
      .filter((lease) => lease.startsWith(prefix))
      .forEach((lease) => chatBusyLeases.delete(lease));
    renderChatBusyState();
  };

  // A canceled voice request can emit a late terminal or delta after its
  // controller is gone. Retain its identity so that event can neither create
  // an ordinary text bubble nor enter the next physical turn's latency trace.
  const voiceStreamRequestIds = new Set();
  const rememberVoiceStreamRequest = (requestId) => {
    if (!requestId) return;
    voiceStreamRequestIds.delete(requestId);
    voiceStreamRequestIds.add(requestId);
    if (voiceStreamRequestIds.size > 512) {
      voiceStreamRequestIds.delete(voiceStreamRequestIds.values().next().value);
    }
  };

  const chatRequestLease = (payload) => payload.request_id
    ? `chat-request:${payload.request_id}`
    : null;

  const onChatEvent = (event) => {
    const payload = event.payload || event;
    // A resumed-speech abort and its replacement use the same conversation.
    // The older speculation can finish after the replacement has started and
    // clear the conversation-wide dispatch marker. Request identity is the
    // durable authority here: route the stream to the exact live controller
    // even when that stale cleanup has already run.
    const sameLiveConversation = liveVoiceTts?.conversation === payload.conversation_id;
    const ownsLiveVoice = sameLiveConversation
      && liveVoiceTts.acceptsRequest(payload.request_id, payload.event, payload.client_request);
    const eventTrace = ownsLiveVoice ? liveVoiceTts.trace : null;
    if (eventTrace) {
      markVoiceLatencyIn(eventTrace, "chat_stream_event_received", {
        event: payload.event,
        request_id: payload.request_id,
        conversation_id: payload.conversation_id,
      });
      markVoiceLatencyIn(eventTrace, "chat_stream_route_decision", {
        event: payload.event,
        request_id: payload.request_id,
        conversation_id: payload.conversation_id,
        live_request_id: liveVoiceTts?.requestId || null,
        live_conversation_id: liveVoiceTts?.conversation || null,
        same_live_conversation: sameLiveConversation,
        owns_live_voice: ownsLiveVoice,
      });
    }
    const voiceReply = ownsLiveVoice && liveVoiceTts.mode === "voice_note";
    const callReply = ownsLiveVoice && liveVoiceTts.mode === "call";
    if ((sameLiveConversation || voiceStreamRequestIds.has(payload.request_id)) && !ownsLiveVoice) {
      if (["completed", "cancelled", "failed"].includes(payload.event)) {
        releaseChatBusy(chatRequestLease(payload));
      }
      return;
    }
    if ((voiceReply || callReply) && ["prefill_started", "decode_started"].includes(payload.event)) {
      markVoiceLatencyIn(eventTrace, payload.event, { observed_at_unix_us: payload.emitted_at_unix_us });
    }
    if (payload.event === "started") {
      acquireStableChatBusy(chatRequestLease(payload));
      if (callReply) {
        document.getElementById("live-voice-reply-pending")?.remove();
        return;
      }
      if (voiceReply) return;
      document.getElementById("live-voice-reply-pending")?.remove();
      appendLiveMessage("assistant", "", `live-assistant-${payload.request_id}`);
    }
    if (payload.event === "delta" && !voiceReply && !callReply) {
      const content = document.querySelector(`#live-assistant-${CSS.escape(payload.request_id)} .live-content`);
      appendLiveDelta(content, payload.delta);
    }
    if (payload.event === "delta" && (voiceReply || callReply) && !liveVoiceTts.firstDeltaSeen) {
      liveVoiceTts.firstDeltaSeen = true;
      markVoiceLatencyIn(eventTrace, "first_content_delta", { observed_at_unix_us: payload.emitted_at_unix_us });
      markVoiceLatencyIn(eventTrace, "first_content_delta_received");
    }
    if (payload.event === "delta" && (voiceReply || callReply)) {
      liveVoiceTts?.push(payload.delta || "");
    }
    if (payload.event === "text_completed" && (voiceReply || callReply)) {
      // Native text EOF precedes recording confirmation and history commit.
      // Flush the last sentence now so its first PCM can confirm the recording;
      // the tracked native operation still gates preparation of the next turn.
      liveVoiceTts.lastProgressAt = performance.now();
      markVoiceLatencyIn(eventTrace, "text_completed", {
        observed_at_unix_us: payload.emitted_at_unix_us,
      });
      void liveVoiceTts.finish(payload.delta || liveVoiceTts.text);
    }
    if (payload.event === "reasoning_delta" && !voiceReply && !callReply) {
      const reasoning = document.querySelector(`#live-assistant-${CSS.escape(payload.request_id)} .live-reasoning`);
      const content = reasoning?.querySelector(".reasoning-content");
      if (settingEnabled("showThoughtInProgress")) reasoning?.classList.remove("is-hidden");
      if (content) {
        content.textContent += payload.delta || "";
        keepLiveTailVisible(content);
      }
      const skipReasoning = document.querySelector("#chat-form .skip-reasoning-button");
      if (skipReasoning) {
        skipReasoning.disabled = false;
        skipReasoning.classList.remove("is-hidden");
      }
    }
    if (["completed", "cancelled", "failed"].includes(payload.event)) {
      const voiceOwner = (voiceReply || callReply) ? liveVoiceTts : null;
      if (payload.event === "failed" && voiceOwner) {
        void voiceOwner.unavailable(new Error(
          payload.message || "Voice reply generation ended before audio was available.",
        ));
      } else if (payload.event === "cancelled" && voiceOwner) {
        void voiceOwner.cancel();
      }
      finishLiveMessage(document.getElementById(`live-assistant-${payload.request_id}`));
      releaseChatBusy(chatRequestLease(payload));
    }
  };

  const mentionLiveId = (payload) => `live-mention-${payload.invocation_id}-${payload.target_id}`;
  const appendMentionMessage = (payload, voiceReply = false) => {
    const id = mentionLiveId(payload);
    const card = voiceReply
      ? (appendLiveVoiceReply(id), document.querySelector(`#${CSS.escape(id)} .message-card`))
      : appendLiveMessage("assistant", "", id);
    const row = card?.closest(".message-row");
    if (!card || !row) return card;
    const byline = document.createElement("p");
    byline.className = "message-attribution";
    const name = document.createElement("strong");
    name.textContent = payload.label || payload.handle;
    const handle = document.createElement("span");
    handle.textContent = `@${payload.handle}`;
    const stop = createCommandElement("button", DYNAMIC_CONTROL_SPECS.mentionCancel);
    stop.type = "button";
    stop.className = "mention-stop";
    stop.textContent = "Stop";
    stop.dataset.action = "mention-cancel";
    stop.dataset.invocation = payload.invocation_id;
    stop.dataset.target = payload.target_id;
    byline.append(name, handle, stop);
    card.prepend(byline);
    return card;
  };

  const onDispatchEvent = (event) => {
    const envelope = event.payload || event;
    // Ordinary multi-recipient dispatches arrive in a tagged envelope, while
    // the latency-critical native voice path emits ChatStreamEvent directly.
    // Both are the same chat stream contract once unwrapped.
    const chatEvent = envelope.kind === "chat"
      ? envelope.event
      : (envelope.request_id && envelope.conversation_id ? envelope : null);
    if (chatEvent) {
      const clientRequest = envelope.client_request ?? chatEvent.client_request;
      onChatEvent({ payload: clientRequest === undefined
        ? chatEvent
        : { ...chatEvent, client_request: clientRequest } });
      return;
    }
    const payload = envelope.event || envelope;
    const voiceReply = voiceReplyConversation === activeDispatchConversation;
    const callReply = callReplyConversation === activeDispatchConversation;
    const lease = payload.invocation_id && payload.target_id
      ? `mention-request:${payload.invocation_id}:${payload.target_id}`
      : null;
    if (payload.event === "started") {
      acquireStableChatBusy(lease);
      if (!callReply && !voiceReply) {
        document.getElementById("live-voice-reply-pending")?.remove();
        appendMentionMessage(payload, false);
      }
    }
    const row = document.getElementById(mentionLiveId(payload));
    if (payload.event === "delta" && !voiceReply && !callReply) {
      const content = row?.querySelector(".live-content");
      appendLiveDelta(content, payload.delta);
    }
    const terminal = payload.state && ["completed", "cancelled", "failed"].includes(payload.state);
    if (["completed", "cancelled", "failed"].includes(payload.event) || terminal) {
      releaseChatBusy(lease);
      finishLiveMessage(row);
      row?.querySelector(".mention-stop")?.remove();
      row?.setAttribute("data-state", payload.state || payload.event);
    }
  };

  let mentionSearchSerial = 0;
  const mentionTokenAtCursor = (textarea) => {
    const before = textarea.value.slice(0, textarea.selectionStart);
    const match = before.match(/(?:^|\s)@([\w-]*)$/);
    if (!match) return null;
    return { query: match[1], start: textarea.selectionStart - match[1].length - 1, end: textarea.selectionStart };
  };

  const composerTextarea = () => (
    document.querySelector("#chat-form textarea[name='message']")
  );

  const syncMentionAria = () => {
    const textarea = composerTextarea();
    const list = document.getElementById("mention-candidates");
    const options = [...list?.querySelectorAll(".mention-candidate") || []];
    const open = composerState.kind === "mention"
      && composerState.optionCount === options.length
      && !list?.classList.contains("is-hidden");
    const activeIndex = open ? composerState.activeIndex : -1;
    options.forEach((option, index) => {
      const active = index === activeIndex;
      option.classList.toggle("active", active);
      option.setAttribute("aria-selected", active ? "true" : "false");
    });
    textarea?.setAttribute("aria-expanded", open ? "true" : "false");
    if (open && options[activeIndex]?.id) {
      textarea?.setAttribute("aria-activedescendant", options[activeIndex].id);
    } else {
      textarea?.removeAttribute("aria-activedescendant");
    }
  };

  const hideMentionPresentation = () => {
    mentionSearchSerial += 1;
    const list = document.getElementById("mention-candidates");
    list?.classList.add("is-hidden");
    list?.replaceChildren();
    list?.setAttribute("aria-busy", "false");
    syncMentionAria();
  };

  const closeMentions = () => {
    transitionComposer({ type: "mention_close" });
    hideMentionPresentation();
  };

  const insertMention = (textarea, handle) => {
    const token = mentionTokenAtCursor(textarea);
    if (!token) return;
    textarea.setRangeText(`@${handle} `, token.start, token.end, "end");
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    closeMentions();
    textarea.focus();
  };

  const updateMentionCandidates = async (textarea) => {
    const token = mentionTokenAtCursor(textarea);
    if (!token) { closeMentions(); return; }
    const conversation = selectedConversation();
    const serial = ++mentionSearchSerial;
    const list = document.getElementById("mention-candidates");
    transitionComposer({ type: "mention_close" });
    list?.classList.add("is-hidden");
    list?.replaceChildren();
    list?.setAttribute("aria-busy", "true");
    syncMentionAria();
    const response = await invoke("mom_llama_mention_candidates", {
      query: token.query,
      conversation,
    });
    if (serial !== mentionSearchSerial) return;
    const currentToken = mentionTokenAtCursor(textarea);
    const tokenIsCurrent = currentToken
      && currentToken.query === token.query
      && currentToken.start === token.start
      && currentToken.end === token.end;
    if (!textarea.isConnected || selectedConversation() !== conversation || !tokenIsCurrent) {
      closeMentions();
      return;
    }
    const candidates = response?.result || [];
    if (!list || !candidates.length) { closeMentions(); return; }
    list.replaceChildren(...candidates.map((candidate, index) => {
      const button = createCommandElement("button", DYNAMIC_CONTROL_SPECS.mentionCandidates);
      button.type = "button";
      button.id = `mention-candidate-${serial}-${index}`;
      button.className = `mention-candidate${index === 0 ? " active" : ""}`;
      button.setAttribute("role", "option");
      button.setAttribute("aria-selected", index === 0 ? "true" : "false");
      button.dataset.handle = candidate.handle;
      button.dataset.kind = candidate.kind;
      button.dataset.action = "mention-insert";
      const icon = document
        .querySelector(`[data-mention-icon="${candidate.kind}"] svg`)
        ?.cloneNode(true);
      const copy = document.createElement("span");
      copy.className = "mention-candidate-copy";
      const label = document.createElement("strong");
      label.textContent = `@${candidate.handle}`;
      const detail = document.createElement("span");
      detail.textContent = `${candidate.label} · ${candidate.detail}`;
      copy.append(label, detail);
      button.append(...(icon ? [icon, copy] : [copy]));
      return button;
    }));
    const mentionEffects = transitionComposer({
      type: "mention_open",
      optionCount: candidates.length,
    });
    if (mentionEffects.some((effect) => effect.kind === "ai_cancel")) {
      cancelComposerAutocomplete({ forceNative: true });
    }
    if (composerState.kind !== "mention") {
      hideMentionPresentation();
      return;
    }
    list.setAttribute("aria-busy", "false");
    list.classList.remove("is-hidden");
    syncMentionAria();
  };

  const onToolLoopEvent = (event) => {
    const payload = event.payload || event;
    const live = document.getElementById("tool-loop-live");
    const events = document.getElementById("tool-loop-live-events");
    const state = document.getElementById("tool-loop-live-state");
    if (!live || !events) return;
    live.classList.remove("is-hidden");
    live.dataset.requestId = payload.request_id || "";
    if (state) state.textContent = (payload.event || "running").replaceAll("_", " ");

    if (payload.event === "model_delta") {
      const turn = String(payload.turn || 1);
      let row = events.querySelector(`.tool-loop-model-delta[data-turn="${CSS.escape(turn)}"]`);
      if (!row) {
        row = document.createElement("article");
        row.className = "tool-loop-live-event tool-loop-model-delta";
        row.dataset.turn = turn;
        const label = document.createElement("strong");
        label.textContent = `Model · turn ${turn}`;
        const content = document.createElement("pre");
        row.append(label, content);
        events.appendChild(row);
      }
      const content = row.querySelector("pre");
      if (content) content.textContent += payload.delta || "";
      return;
    }

    if (["started", "model_state", "warning", "completed"].includes(payload.event)) {
      if (payload.event === "completed" && state) state.textContent = "completed";
      return;
    }
    if (!["tool_call_started", "tool_call_requested", "tool_result"].includes(payload.event)) return;

    const row = document.createElement("article");
    row.className = `tool-loop-live-event ${payload.event || ""}`;
    const label = document.createElement("strong");
    const turn = payload.turn ? ` · turn ${payload.turn}` : "";
    label.textContent = payload.event === "tool_result"
      ? `${payload.tool || "Tool"} result${turn}`
      : `${payload.tool || "Tool"} call${turn}`;
    const body = document.createElement("pre");
    const value = payload.event === "tool_result" ? payload.result : payload.arguments;
    body.textContent = JSON.stringify(value ?? {}, null, 2);
    row.append(label, body);
    events.appendChild(row);
  };

  const armDestructiveAction = (button) => {
    if (button.dataset.confirmArmed === "true") return true;
    button.dataset.confirmArmed = "true";
    setButtonStateLabel(button, "Delete?");
    window.setTimeout(() => {
      button.dataset.confirmArmed = "false";
      setButtonStateLabel(button, "Delete");
    }, 3500);
    return false;
  };

  const inlineEdit = (button) => {
    const row = button.closest(".message-row");
    const card = row?.querySelector(".message-card");
    if (!card || card.querySelector("textarea")) return;
    const textarea = createCommandElement("textarea", DYNAMIC_CONTROL_SPECS.messageEdit);
    textarea.value = button.dataset.messageContent || card.textContent;
    textarea.rows = 5;
    textarea.className = "inline-message-editor";
    const save = createCommandElement("button", DYNAMIC_CONTROL_SPECS.messageEdit);
    save.type = "button";
    save.className = "small-button";
    save.textContent = "Save";
    save.dataset.action = "message-edit-save";
    save.dataset.message = button.dataset.message;
    const cancel = createCommandElement("button", DYNAMIC_CONTROL_SPECS.conversationSelect);
    cancel.type = "button";
    cancel.className = "small-button";
    cancel.textContent = "Cancel";
    cancel.dataset.action = "message-edit-cancel";
    const actions = document.createElement("div");
    actions.className = "inline-message-actions";
    actions.append(save, cancel);
    card.replaceChildren(textarea, actions);
    textarea.focus();
  };

  const MCP_PROCESS_ACTIONS = new Set([
    "mcp-status",
    "mcp-command-browse",
    "mcp-configure",
    "mcp-list-servers",
    "mcp-list-tools",
    "mcp-call-tool",
    "mcp-list-resources",
    "mcp-read-resource",
    "mcp-list-prompts",
    "mcp-get-prompt",
    "tool-loop-prepare",
    "tool-loop-run",
    "tool-loop-cancel",
    "tool-permission-list",
    "tool-permission-set",
    "tool-permission-revoke",
    "mention-tool-approval-open",
    "mention-tool-approve",
    "mention-tool-deny",
  ]);

  const selectAndLoadModel = async (button, path) => {
    if (!path) return null;
    const picker = button.closest("[data-model-picker]");
    const previousState = picker?.dataset.state || "empty";
    if (picker) {
      const inlineError = picker.querySelector(".model-picker-error");
      if (inlineError) {
        inlineError.textContent = "";
        inlineError.hidden = true;
      }
      picker.dataset.state = "loading";
      picker.removeAttribute("open");
    }
    try {
      const result = await invoke("mom_llama_model_select", {
        modelPath: path,
        conversation: button.dataset.conversation || null,
      });
      if (result?.status === "blocked") {
        recordCommandResult(result);
        const inlineError = picker?.querySelector(".model-picker-error");
        if (inlineError) {
          inlineError.textContent = result?.blocker?.message || "That model could not be loaded.";
          inlineError.hidden = false;
          picker.setAttribute("open", "");
        }
      } else {
        report(result);
        await Promise.all([refreshChat(), refreshSettings("general")]);
      }
      return result;
    } finally {
      if (picker?.isConnected && picker.dataset.state === "loading") {
        picker.dataset.state = previousState;
      }
    }
  };

  const retainComposerDraft = async () => {
    const form = document.getElementById("chat-form");
    if (form && form.dataset.busy !== "true") await persistDraftNow(formField(form, "message")?.value || "", draftAttachmentIds(form));
  };

  const openNewDraft = async (persona = null) => {
    const revision = ++conversationNavigationRevision;
    void retainComposerDraft().catch(reportError);
    const result = await mutateConversationSelection(
      () => invoke("mom_llama_conversation_draft_open", { persona }),
    );
    if (revision !== conversationNavigationRevision) return result;
    report(result);
    if (result?.status !== "blocked") {
      closeSettings();
      await refreshConversationProjection();
      const draftText = document.querySelector("#chat-form textarea[name='message']")?.value || "";
      if (!persona && !draftText && document.getElementById("recipient-query")) document.getElementById("recipient-query").focus();
      else focusComposer();
    }
    return result;
  };

  const openPersonaConversation = (persona) => openNewDraft(persona);

  let recipientMutation = Promise.resolve();
  const draftRecipientIds = () => Array.from(document.querySelectorAll('.recipients-header[data-unsent="true"] [data-recipient-id]'), (chip) => chip.dataset.recipientId);
  const updateRecipients = (mutate) => {
    const source = chat();
    recipientMutation = recipientMutation.catch(() => {}).then(async () => {
      if (source !== chat() || selectedConversation() !== "default") return;
      await retainComposerDraft();
      if (source !== chat() || selectedConversation() !== "default") return;
      const ids = mutate(draftRecipientIds());
      const name = document.getElementById("recipient-group-name")?.value || null;
      const response = await invoke("mom_llama_conversation_draft_recipients_update", { recipientIds: ids, name });
      report(response);
      if (response?.status !== "blocked" && source === chat()) {
        await refreshChat();
        document.getElementById("recipient-query")?.focus();
      }
    });
    return recipientMutation;
  };
  const normalizedRecipientQuery = (value) => value.trim().toLowerCase().replace(/^@+/, "");
  const exactRecipientOption = (value) => {
    const query = normalizedRecipientQuery(value);
    if (!query) return null;
    const matches = Array.from(document.querySelectorAll("#recipient-options .recipient-option")).filter((option) =>
      option.dataset.recipientName === query || option.dataset.recipientHandle === query,
    );
    return matches.length === 1 ? matches[0] : null;
  };
  const recipientOptionMatches = (option, query) => {
    // Parenthetical role/protocol qualifiers describe the contact but are not
    // part of their addressable name. Excluding them prevents queries such as
    // `@ad` from matching "Addiction Protocol".
    const name = (option.dataset.recipientName || "").split(/\s+\(/, 1)[0];
    const handle = option.dataset.recipientHandle || "";
    return name.startsWith(query)
      || name.split(/\s+/).some((part) => part.startsWith(query))
      || handle.startsWith(query);
  };
  const visibleRecipientOptions = () => Array.from(
    document.querySelectorAll("#recipient-options .recipient-option:not([hidden])"),
  );
  const activeRecipientOption = () => visibleRecipientOptions().find((option) =>
    option.getAttribute("aria-selected") === "true",
  ) || visibleRecipientOptions()[0] || null;
  const setActiveRecipientOption = (active) => {
    const input = document.getElementById("recipient-query");
    for (const [index, option] of visibleRecipientOptions().entries()) {
      if (!option.id) option.id = `recipient-option-${index}`;
      option.setAttribute("aria-selected", String(option === active));
    }
    if (active?.id) input?.setAttribute("aria-activedescendant", active.id);
    else input?.removeAttribute("aria-activedescendant");
  };
  const filterRecipientOptions = () => {
    const input = document.getElementById("recipient-query");
    const list = document.getElementById("recipient-options");
    if (!input || !list) return;
    // The UI presents handles with a leading @, while the canonical stored
    // handle deliberately omits it. Treat the display sigil as query syntax so
    // typing the handle exactly as shown never empties the picker.
    const query = normalizedRecipientQuery(input.value);
    const open = query.length > 0;
    list.hidden = !open;
    input.setAttribute("aria-expanded", String(open));
    input.removeAttribute("aria-invalid");
    for (const option of list.querySelectorAll(".recipient-option")) {
      option.hidden = !open || !recipientOptionMatches(option, query);
      option.setAttribute("aria-selected", "false");
    }
    setActiveRecipientOption(visibleRecipientOptions()[0] || null);
  };
  document.addEventListener("input", (event) => {
    if (event.target.id === "recipient-query" && !event.isComposing) filterRecipientOptions();
  });
  document.addEventListener("focusin", (event) => {
    if (event.target.id === "recipient-query") filterRecipientOptions();
  });
  document.addEventListener("pointerdown", (event) => {
    // Keep the combobox focused until the option's click commits it. Allowing
    // pointerdown to move focus fires focusout first and hides the option before
    // the click can select it.
    if (event.target.closest?.("#recipient-options .recipient-option")) event.preventDefault();
  });
  document.addEventListener("keydown", (event) => {
    if (event.isComposing) return;
    if (event.target.matches?.("#recipient-options .recipient-option")) {
      if (event.key === "Tab" || event.key === "Enter") {
        event.preventDefault();
        event.target.click();
      }
      return;
    }
    if (event.target.id !== "recipient-query") return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      const options = visibleRecipientOptions();
      const current = options.indexOf(activeRecipientOption());
      setActiveRecipientOption(options[Math.min(options.length - 1, current + 1)] || options[0] || null);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      const options = visibleRecipientOptions();
      const current = options.indexOf(activeRecipientOption());
      setActiveRecipientOption(options[Math.max(0, current - 1)] || options[0] || null);
    } else if (event.key === "Escape") {
      document.getElementById("recipient-options").hidden = true;
      event.target.setAttribute("aria-expanded", "false");
    } else if (event.key === "Enter" || event.key === "Tab") {
      const option = event.target.value.trim() ? activeRecipientOption() : null;
      if (!option) return;
      event.preventDefault();
      option.click();
    }
  });
  document.addEventListener("focusout", (event) => {
    const header = event.target.closest?.(".recipients-header");
    if (header && !header.contains(event.relatedTarget)) {
      const list = document.getElementById("recipient-options");
      if (list) list.hidden = true;
      document.getElementById("recipient-query")?.setAttribute("aria-expanded", "false");
    }
  });
  document.addEventListener("change", (event) => {
    if (event.target.id === "recipient-group-name") updateRecipients((ids) => ids).catch(reportError);
  });

  const actionHandlers = {
    "recipient-add": async (button) => updateRecipients((ids) => ids.includes(button.dataset.recipient) ? ids : [...ids, button.dataset.recipient]),
    "recipient-remove": async (button) => updateRecipients((ids) => ids.filter((id) => id !== button.dataset.recipient)),
    "sidebar-section-toggle": async (button) => {
      const section = button.dataset.sidebarSection;
      const list = document.getElementById(button.getAttribute("aria-controls"));
      if (!section || !list) return;
      const expanding = button.getAttribute("aria-expanded") !== "true";
      if (!expanding) {
        button.setAttribute("aria-expanded", "false");
        button.setAttribute("aria-label", `Expand ${button.dataset.sidebarLabel || section}`);
        list.hidden = true;
        collapsedSidebarSections.add(section);
        return;
      }
      collapsedSidebarSections.delete(section);
      try {
        const replacement = await refreshSidebar();
        replacement
          ?.querySelector(`[data-action="sidebar-section-toggle"][data-sidebar-section="${CSS.escape(section)}"]`)
          ?.focus();
      } catch (error) {
        collapsedSidebarSections.add(section);
        applySidebarSectionState();
        throw error;
      }
    },
    "settings-open": async () => {
      await invoke("mom_llama_settings_get");
      if (document.getElementById("settings-sidebar")?.hidden) openSettings();
      else closeSettings();
    },
    "settings-close": async () => { await invoke("mom_llama_settings_get"); closeSettings(); },
    "settings-section": async (button) => switchSettingsSection(button.dataset.section || "general"),
    "mention-insert": async (button) => {
      const textarea = document.querySelector("#chat-form textarea[name='message']");
      if (!textarea) return;
      const handle = button.dataset.handle || "";
      const response = await invoke("mom_llama_mention_candidates", {
        query: handle,
        conversation: selectedConversation(),
      });
      const current = (response?.result || []).some((candidate) => (
        String(candidate.handle || "").toLowerCase() === handle.toLowerCase()
      ));
      if (response?.status === "blocked" || !current) {
        report(response?.status === "blocked" ? response : {
          status: "blocked",
          blocker: {
            code: "mention_target_not_found",
            message: "That mention target is no longer available.",
          },
        });
        closeMentions();
        return;
      }
      insertMention(textarea, handle);
    },
    "mention-cancel": async (button) => report(await invoke("mom_llama_mention_cancel", {
      invocation: button.dataset.invocation,
      target: button.dataset.target || null,
    })),
    "mention-synthesize": async (button) => {
      const lease = acquireChatBusy("mention-synthesis");
      try {
        const result = await invoke("mom_llama_mention_synthesize", {
          invocation: button.dataset.invocation,
        });
        report(result);
        await refreshConversationProjection();
      } finally {
        releaseChatBusy(lease);
      }
    },
    "mention-tool-approval-open": async (button) => {
      openPersonaToolApproval(JSON.parse(button.dataset.approvalJson || "{}"));
    },
    "mention-tool-approve": async () => decidePersonaToolApproval("approve"),
    "mention-tool-deny": async () => decidePersonaToolApproval("deny"),
    "conversation-new": async () => {
      await openNewDraft();
    },
    "conversation-list": async () => refreshConversationProjection(),
    "conversation-search-open": async () => { setSearchMode(true); await search(); },
    "conversation-search-close": async () => setSearchMode(false),
    "voice-message-start": async () => beginVoiceCapture(),
    "voice-message-cancel": async () => {
      await stopVoiceCapture({ discard: true, reason: "voice_note_cancelled" });
      focusComposer();
    },
    "voice-message-send": async () => sendVoiceCapture(),
    "voice-message-pending-play": async (button) => {
      const preview = button.closest(".voice-message-bubble");
      if (!preview) return;
      button.classList.add("is-loading");
      button.textContent = "";
      await ensureAttachmentPreviewLoaded(preview);
      const ready = preview.querySelector(".voice-message-play");
      if (ready && ready !== button && !ready.disabled) ready.click();
      if (button.isConnected) {
        button.classList.remove("is-loading");
        button.textContent = "";
      }
    },
    "voice-reply-pending-play": async (button) => {
      const playback = voiceReplyPlayback.get(button);
      if (!playback || playback.button !== button) {
        settleUnavailableVoiceReply(button);
        return;
      }
      if (playback.durableVoiceStoreError) {
        void playback.retryDurableVoiceReply();
      }
      voiceReplyPlayRequested = true;
      if (playback.controller.playing) {
        await playback.pause();
      } else {
        button.classList.add("is-loading");
        await playback.play();
      }
    },
    "voice-call-open": async () => openVoiceCallSurface(),
    "voice-call-close": async () => closeVoiceCallSurface(),
    "conversation-select": async (button, event) => {
      if (button.dataset.conversation === selectedConversation()) {
        if (event?.target?.closest?.(".conversation-title")) {
          beginConversationRename(button.closest?.("[data-conversation-menu-target]") || button);
        }
      } else await selectConversation(button.dataset.conversation);
    },
    "chat-cancel": async () => report(await invoke("mom_llama_chat_cancel", { conversation: activeDispatchConversation || selectedConversation() })),
    "chat-skip-reasoning": async (button) => {
      const result = await invoke("mom_llama_chat_skip_reasoning", { conversation: selectedConversation() });
      report(result);
      if (result?.status !== "blocked") {
        button.disabled = true;
        button.classList.add("is-hidden");
      }
    },
    "chat-regenerate": async () => { report(await invoke("mom_llama_chat_regenerate", { conversation: selectedConversation() })); await refreshConversationProjection(); },
    "chat-continue": async () => { report(await invoke("mom_llama_chat_continue", { conversation: selectedConversation() })); await refreshConversationProjection(); },
    "message-copy": async (button) => {
      const result = await invoke("mom_llama_message_copy", { conversation: selectedConversation(), message: button.dataset.message });
      if (result?.result?.content) {
        await navigator.clipboard.writeText(attachmentCopyText(result.result.content));
      }
      report(result);
    },
    "message-read-aloud": async (button) => {
      if (activeSpeechPlayback?.button === button) {
        if (activeSpeechPlayback.controller?.ready
            && !activeSpeechPlayback.controller.playing
            && !activeSpeechPlayback.controller.starting) {
          commandMetadata(button, DYNAMIC_CONTROL_SPECS.readAloudStop);
          button.title = "Stop reading";
          button.setAttribute("aria-pressed", "true");
          button.querySelector(".sr-only")?.replaceChildren("Stop reading");
          await activeSpeechPlayback.controller.play();
          return;
        }
        await stopSpeechPlayback();
        return;
      }
      if (button.dataset.playback) {
        const playback = button.dataset.playback;
        restoreReadAloudButton(button);
        await invoke("mom_llama_speech_stop", { playback });
        return;
      }
      if (button.dataset.operation) {
        const operation = button.dataset.operation;
        restoreReadAloudButton(button);
        await invoke("mom_llama_speech_stop", { operation });
        return;
      }
      const stopped = stopSpeechPlayback();
      const epoch = speechPlaybackEpoch;
      await stopped;
      if (epoch !== speechPlaybackEpoch || !button.isConnected) return;
      const conversation = selectedConversation();
      const message = button.dataset.message;
      const operation = speechOperationToken();
      button.dataset.operation = operation;
      const panel = document.createElement("section");
      panel.className = "speech-playback";
      let retainedPlayback = null;
      let controller = null;
      let endedBeforeCommit = false;
      // Explicit reading shares the call renderer and measured packet runway,
      // but keeps its own calibration: reading a long document must not change
      // the next live call's startup policy.
      const leadController = new globalThis.MomVoicePipelineScheduler.AdaptiveVoiceLeadController();
      const trace = { id: crypto.randomUUID(), mode: "read_aloud", began_at: Date.now(),
        started: performance.now(), stages: [] };
      const markReadLatency = (stage, detail = {}) => markVoiceLatencyIn(trace, stage, detail);
      let firstPacketAt = null;
      let previousPacketAt = null;
      let audioMs = 0;
      let packetCount = 0;
      try {
        markReadLatency("tts_started");
        controller = createPcmPlaybackBuffer({
          autoplay: true,
          startupBufferMs: () => leadController.startupRunwayMs(),
          startupDelayMs: () => leadController.startupDelayMs(),
          onUnderrun: (gapMs) => {
            leadController.noteUnderrun(gapMs);
            markReadLatency("playback_underrun", { gap_ms: gapMs, scheduler: leadController.snapshot() });
          },
          onFirstChunk: () => {
            if (activeSpeechPlayback?.controller !== controller || !button.isConnected) return;
            markReadLatency("tts_first_pcm");
            const row = button.closest(".message-row");
            const card = row?.querySelector(".message-card");
            card?.append(panel);
            commandMetadata(button, DYNAMIC_CONTROL_SPECS.readAloudStop);
            button.title = "Stop reading";
            button.setAttribute("aria-pressed", "true");
            button.querySelector(".sr-only")?.replaceChildren("Stop reading");
          },
          onPlaybackStarted: (metrics) => {
            markReadLatency("playback_started", metrics);
          },
          onRendererEvent: (event) => {
            markReadLatency(`playback_${event.type}`, event);
          },
          onRendererMetrics: (metrics) => {
            markReadLatency("playback_renderer", metrics);
          },
          onEnded: () => {
            if (activeSpeechPlayback?.controller !== controller) return;
            if (!activeSpeechPlayback.id) {
              endedBeforeCommit = true;
              void controller.stop().catch(reportError);
              restoreReadAloudButton(button);
              return;
            }
            void stopSpeechPlayback().catch(reportError);
          },
        });
        const stream = pcmChannel((frame) => {
          if (activeSpeechPlayback?.controller !== controller || epoch !== speechPlaybackEpoch) return;
          try {
            const decoded = decodePcmFrame(frame);
            if (decoded.data.byteLength > 0) {
              const now = performance.now();
              const packetMs = decoded.data.byteLength * 1000 / (2 * decoded.channels * decoded.sampleRate);
              firstPacketAt ??= now;
              audioMs += packetMs;
              packetCount += 1;
              leadController.notePacket({ audioMs: packetMs,
                gapMs: previousPacketAt === null ? null : now - previousPacketAt,
                startupDeficitMs: Math.max(0, now - firstPacketAt - (audioMs - packetMs)) });
              previousPacketAt = now;
            }
            controller.push(frame);
            if (decoded.end) markReadLatency("tts_pcm_complete", {
              pcm_packets: packetCount, output_audio_ms: audioMs, scheduler: leadController.snapshot(),
            });
          } catch (error) { reportError(error); }
        });
        activeSpeechPlayback = { id: null, operation, controller, panel, button };
        const response = await invoke("mom_llama_speech_read_aloud", {
          conversation,
          message,
          operation,
          requirePersonaVoice: false,
          pcmStream: stream,
        });
        // A cancelled request may still finish natively and return a retained
        // playback. Capture that handle first so `finally` can release it, but
        // do not publish the cancellation response or mutate the row.
        retainedPlayback = response?.result?.playback_id || null;
        if (activeSpeechPlayback?.controller !== controller || epoch !== speechPlaybackEpoch) return;
        if (response?.result?.binding) {
          markReadLatency("tts_backend_complete", {
            backend_id: response.result.binding.backend_id ?? null,
            model_id: response.result.binding.model_id ?? null,
            voice_id: response.result.binding.voice_id ?? null,
            backend_ms: response.result.binding.tts_time_to_first_audio_ms,
            generation_segments: response.result.binding.tts_generation_segments ?? null,
          });
        }
        markReadLatency("tts_finished");
        report(response);
        if (response?.status === "blocked" || !response?.result) return;
        const result = response.result;
        if (activeSpeechPlayback?.controller !== controller || epoch !== speechPlaybackEpoch) return;
        delete button.dataset.operation;
        button.dataset.playback = result.playback_id;
        if (
          selectedConversation() !== conversation
          || !button.isConnected
          || button.closest(".message-row")?.dataset.messageId !== result.binding.message_id
        ) {
          await invoke("mom_llama_speech_stop", { playback: result.playback_id });
          retainedPlayback = null;
          return;
        }
        activeSpeechPlayback.id = result.playback_id;
        activeSpeechPlayback.operation = null;
        panel.dataset.playback = result.playback_id;
        retainedPlayback = null;
        if (endedBeforeCommit) await stopSpeechPlayback();
      } finally {
        if (retainedPlayback) {
          await invoke("mom_llama_speech_stop", { playback: retainedPlayback }).catch(() => {});
        }
        if (activeSpeechPlayback?.controller === controller && !controller?.ready) {
          await stopSpeechPlayback().catch(() => {});
        }
        if (activeSpeechPlayback?.button !== button
          && (button.dataset.operation === operation
            || (retainedPlayback && button.dataset.playback === retainedPlayback))) {
          restoreReadAloudButton(button);
        }
      }
    },
    "attachment-transcribe": async (button) => {
      if (button.dataset.operation) {
        const operation = button.dataset.operation;
        delete button.dataset.operation;
        commandMetadata(button, DYNAMIC_CONTROL_SPECS.attachmentTranscribe);
        button.textContent = "Transcribe audio";
        await invoke("mom_llama_speech_stop", { operation });
        return;
      }
      const preview = button.closest("[data-attachment-preview]");
      const conversation = selectedConversation();
      const operation = speechOperationToken();
      button.dataset.operation = operation;
      commandMetadata(button, DYNAMIC_CONTROL_SPECS.attachmentTranscriptionStop);
      button.textContent = "Stop transcription";
      try {
        const response = await invoke("mom_llama_speech_transcribe_attachment", {
          conversation,
          attachment: button.dataset.attachment,
          rootSha256: button.dataset.rootSha256,
          artifact: button.dataset.artifact,
          policyFingerprint: button.dataset.policyFingerprint,
          operation,
        });
        report(response);
        const result = response?.result;
        if (response?.status === "blocked" || !result) return;
        const provenance = result.provenance;
        if (
          !preview?.isConnected
          || button.dataset.operation !== operation
          || selectedConversation() !== conversation
          || provenance.conversation_id !== conversation
          || preview.dataset.attachmentPreview !== provenance.attachment_id
          || button.dataset.rootSha256 !== provenance.root_sha256
          || button.dataset.artifact !== provenance.artifact_id
          || button.dataset.policyFingerprint !== provenance.policy_fingerprint
        ) return;
        renderAttachmentTranscript(button, result);
      } finally {
        if (button.dataset.operation === operation) {
          delete button.dataset.operation;
          commandMetadata(button, DYNAMIC_CONTROL_SPECS.attachmentTranscribe);
          button.textContent = "Transcribe audio";
        }
      }
    },
    "speech-transcript-insert": async (button) => {
      const preview = button.closest("[data-attachment-preview]");
      const source = preview?.querySelector("[data-action='attachment-transcribe']");
      const result = preview && attachmentTranscripts.get(preview);
      const textarea = document.querySelector("#chat-form textarea[name='message']");
      if (
        !result
        || !source
        || !textarea
        || selectedConversation() !== result.provenance.conversation_id
        || preview.dataset.attachmentPreview !== result.provenance.attachment_id
        || source.dataset.rootSha256 !== result.provenance.root_sha256
        || source.dataset.artifact !== result.provenance.artifact_id
        || source.dataset.policyFingerprint !== result.provenance.policy_fingerprint
      ) {
        throw new Error("speech_transcription_stale: The transcript target is no longer current.");
      }
      const transcript = result.transcript || "";
      const start = textarea.selectionStart;
      const end = textarea.selectionEnd;
      const prefix = start > 0 && !/\s$/.test(textarea.value.slice(0, start)) ? " " : "";
      const suffix = end < textarea.value.length && !/^\s/.test(textarea.value.slice(end)) ? " " : "";
      textarea.setRangeText(`${prefix}${transcript}${suffix}`, start, end, "end");
      textarea.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: transcript }));
      textarea.focus();
    },
    "message-raw-toggle": async (button) => {
      const result = await invoke("mom_llama_message_copy", {
        conversation: selectedConversation(),
        message: button.dataset.message,
      });
      const card = button.closest(".message-row")?.querySelector(".message-card");
      const formatted = card?.querySelector(":scope > .markdown-content");
      const raw = card?.querySelector(":scope > .raw-message-content");
      const showRaw = raw?.classList.contains("is-hidden") === true;
      formatted?.classList.toggle("is-hidden", showRaw);
      raw?.classList.toggle("is-hidden", !showRaw);
      setButtonStateLabel(button, showRaw ? "Formatted" : "Raw");
      button.setAttribute("aria-pressed", showRaw ? "true" : "false");
      report(result);
    },
    "tool-details-toggle": async (button) => {
      const result = await invoke("mom_llama_message_copy", {
        conversation: selectedConversation(),
        message: button.dataset.message,
      });
      if (result?.status === "blocked") {
        report(result);
        return;
      }
      const card = button.closest(".tool-result-card");
      const details = card?.querySelectorAll(":scope > .tool-result-details") || [];
      const show = [...details].some((detail) => detail.classList.contains("is-hidden"));
      details.forEach((detail) => detail.classList.toggle("is-hidden", !show));
      setButtonStateLabel(button, show ? "Hide details" : "Details");
      button.setAttribute("aria-expanded", show ? "true" : "false");
      report(result);
    },
    "message-edit": async (button) => inlineEdit(button),
    "persona-freeze": async (button) => openPersonaFreeze(button),
    "persona-freeze-close": async () => setModalVisibility("persona-freeze-modal", false),
    "persona-freeze-save": async () => {
      const modal = document.getElementById("persona-freeze-modal");
      const history = modal?.querySelector('[name="freeze_history"]:checked')?.value || "full";
      const result = await invoke("mom_llama_persona_freeze", {
        conversation: modal.dataset.conversation,
        message: formValue(modal, "freeze_message"),
        name: formValue(modal, "freeze_name"),
        handle: formValue(modal, "freeze_handle"),
        history,
      });
      report(result);
      if (result?.status !== "blocked") {
        setModalVisibility("persona-freeze-modal", false);
        await Promise.all([refreshSettings("personas"), refreshSidebar()]);
      }
    },
    "persona-edit": async (button) => openPersonaProfile(button.dataset.persona),
    "persona-profile-open": async (button) => openPersonaProfile(button.dataset.persona),
    "persona-menu-open": async (button) => openPersonaMenu(button),
    "persona-draft-open": async (button) => openPersonaConversation(button.dataset.persona),
    "persona-menu-start": async () => {
      const persona = document.getElementById("persona-context-menu")?.dataset.persona;
      closePersonaMenu(false);
      await openPersonaConversation(persona);
    },
    "persona-menu-edit": async () => {
      const persona = document.getElementById("persona-context-menu")?.dataset.persona;
      closePersonaMenu(false);
      if (await openPersonaProfile(persona)) {
        formField(document.getElementById("persona-editor"), "persona_name")?.focus();
      }
    },
    "persona-menu-removal-preview": async () => {
      const persona = document.getElementById("persona-context-menu")?.dataset.persona;
      await openPersonaRemoval(persona);
    },
    "persona-removal-close": async () => setModalVisibility("persona-removal-modal", false),
    "persona-removal-commit": async () => commitPersonaRemoval(),
    "persona-instantiate": async (button) => openNewDraft(button.dataset.persona),
    "persona-update": async () => {
      const result = await invoke("mom_llama_persona_update", { profile: personaProfileFromEditor() });
      report(result);
      if (result?.status !== "blocked") {
        await Promise.all([
          refreshSettings("personas"),
          refreshConversationProjection(),
        ]);
      }
    },
    "persona-group-new": async () => setPersonaGroupEditor(),
    "persona-group-edit": async (button) => setPersonaGroupEditor(JSON.parse(button.dataset.groupJson || "{}")),
    "persona-group-save": async () => {
      const editor = document.getElementById("persona-group-editor");
      const group = formValue(editor, "persona_group_id");
      const personas = Array.from({ length: 4 }, (_, index) => formValue(editor, `persona_group_member_${index}`)).filter(Boolean);
      const payload = {
        name: formValue(editor, "persona_group_name"),
        handle: formValue(editor, "persona_group_handle"),
        personas,
      };
      const result = group
        ? await invoke("mom_llama_persona_group_update", { group, ...payload })
        : await invoke("mom_llama_persona_group_create", payload);
      report(result);
      if (result?.status !== "blocked") {
        await Promise.all([refreshSettings("consult"), refreshSidebar()]);
      }
    },
    "persona-group-delete": async (button) => {
      if (!armDestructiveAction(button)) return;
      report(await invoke("mom_llama_persona_group_delete", { group: button.dataset.group }));
      await Promise.all([refreshSettings("consult"), refreshSidebar()]);
    },
    "message-edit-save": async (button) => {
      const content = button.closest(".message-card")?.querySelector("textarea")?.value || "";
      report(await invoke("mom_llama_message_edit", { conversation: selectedConversation(), message: button.dataset.message, content }));
      await refreshChat();
    },
    "message-edit-cancel": async () => {
      report(await invoke("mom_llama_conversation_select", { conversation: selectedConversation() }));
      await refreshChat();
    },
    "message-delete": async (button) => {
      if (!armDestructiveAction(button)) return;
      report(await invoke("mom_llama_message_delete", { conversation: selectedConversation(), message: button.dataset.message }));
      await refreshConversationProjection();
    },
    "message-branch-step": async (button) => {
      const conversation = selectedConversation();
      const branches = await invoke("mom_llama_message_branches", {
        conversation,
        message: button.dataset.message,
      });
      if (branches?.status === "blocked") {
        report(branches);
        return;
      }
      const siblings = branches?.result?.siblings || [];
      const current = siblings.findIndex((sibling) => sibling.message_id === button.dataset.message);
      const direction = Number(button.dataset.direction || 0);
      const target = siblings[current + direction];
      if (!target) return;
      report(await invoke("mom_llama_message_branch_select", {
        conversation,
        message: target.message_id,
      }));
      await refreshChat();
    },
    "conversation-fork": async (button) => {
      report(await invoke("mom_llama_conversation_fork", { conversation: selectedConversation(), message: button.dataset.message }));
      await refreshConversationProjection();
    },
    "conversation-siblings": async () => report(await invoke("mom_llama_conversation_siblings", { conversation: selectedConversation() })),
    "conversation-export": async () => {
      const result = await invoke("mom_llama_conversation_export", { conversation: selectedConversation(), format: "markdown" });
      if (result?.result?.content) await navigator.clipboard.writeText(result.result.content);
      report(result);
    },
    "conversation-import": async () => {
      const path = await pickFile("conversation");
      if (!path) return;
      report(await invoke("mom_llama_conversation_import", { path }));
      await refreshConversationProjection();
    },
    "attachment-import": async () => {
      const form = document.getElementById("chat-form");
      const message = formField(form, "message")?.value || "";
      const path = await pickFile("attachment");
      if (!path) return;
      const sourceConversation = selectedConversation();
      let conversation = sourceConversation;
      if (sourceConversation !== "default" && selectedConversationKind() === "persona_template") {
        const instantiated = await instantiatePersona(sourceConversation);
        if (instantiated?.status === "blocked" || !instantiated?.result?.id) return;
        conversation = instantiated.result.id;
        chat().dataset.currentConversation = conversation;
        chat().dataset.conversationKind = "chat";
        await invoke("mom_llama_draft_update", {
          conversation: sourceConversation,
          message: "",
          attachmentIds: [],
        });
      }
      await persistDraftNow(message, draftAttachmentIds(form), conversation);
      const result = await invoke("mom_llama_attachment_import", { conversation, path });
      report(result);
      if (result?.status !== "blocked") await refreshConversationProjection();
    },
    "draft-attachment-remove": async (button) => {
      const form = document.getElementById("chat-form");
      const attachmentIds = draftAttachmentIds(form)
        .filter((attachment) => attachment !== button.dataset.attachment);
      const result = await persistDraftNow(
        formField(form, "message")?.value || "",
        attachmentIds,
      );
      if (result?.status === "blocked") report(result);
      else report(null);
      await refreshChat();
    },
    "attachment-library-open": async (button) => openAttachmentLibrary(button),
    "attachment-library-close": async () => setModalVisibility("attachment-library-modal", false),
    "attachment-library-preview": async () => previewAttachmentLibrary(),
    "attachment-library-commit": async () => commitAttachmentLibrary(),
    "information-alexandria-pick": async () => {
      const grant = await invoke("mom_llama_information_pick_alexandria");
      const field = document.getElementById("information-path-grant");
      if (field) field.value = grant.grant_id;
      setText(
        "information-path-grant-status",
        `Opaque ${grant.profile} grant ready; the renderer has no source path.`,
      );
      const register = document.getElementById("information-alexandria-register");
      if (register) {
        register.disabled = !document.getElementById("information-rights-confirmed")?.checked;
      }
    },
    "information-alexandria-register": async () => {
      const grantId = document.getElementById("information-path-grant")?.value || "";
      if (!grantId) throw new Error("Choose an Alexandria database first.");
      const registration = await invoke("mom_llama_information_register_alexandria", {
        grantId,
        decision: {
          confirmed: Boolean(document.getElementById("information-rights-confirmed")?.checked),
          allow_model_context: Boolean(
            document.getElementById("information-model-context-allowed")?.checked,
          ),
        },
      });
      document.getElementById("information-path-grant").value = "";
      document.getElementById("information-alexandria-register").disabled = true;
      setText(
        "information-path-grant-status",
        `Registered ${registration.profile} immutable read-only · ${registration.source_bytes} bytes · SHA-256 ${registration.source_sha256}.`,
      );
      await refreshInformationLibraries();
    },
    "information-libraries-refresh": async () => {
      const libraries = await refreshInformationLibraries();
      report({ libraries: libraries.length, path_free: true });
    },
    "information-search": async () => {
      const installationId = selectedInformationLibrary();
      if (!installationId) throw new Error("Register and select one Alexandria library first.");
      const text = document.getElementById("information-search-query")?.value || "";
      const result = await invoke("mom_llama_information_search", { installationId, text });
      renderInformationSearch(result);
    },
    "information-open-citation": async (button) => {
      const evidence = await invoke("mom_llama_information_open_citation", {
        citation: JSON.parse(button.dataset.citation || "{}"),
      });
      const context = document.createElement("pre");
      context.className = "information-citation-context";
      context.textContent = evidence.context;
      button.closest(".information-result")?.append(context);
      button.disabled = true;
    },
    "information-model-grant": async (button) => {
      const conversationId = selectedConversation();
      const installationId = selectedInformationLibrary();
      if (!installationId) throw new Error("Select an Alexandria library first.");
      if (conversationId === "default") throw new Error("Start a chat before granting model context.");
      const grant = await invoke("mom_llama_information_grant_model_context", {
        conversationId,
        installationId,
        confirmed: true,
      });
      const section = button.closest(".information-search");
      section.dataset.modelGrantId = grant.grant_id;
      section.dataset.modelGrantConversation = grant.conversation_id;
      section.dataset.modelGrantTarget = [
        grant.resource_id,
        grant.release_id,
        grant.representation_id,
        grant.source_sha256,
      ].join("|");
      setButtonStateLabel(button, "Allowed for this chat");
    },
    "information-chat-send": async (button) => {
      const section = button.closest(".information-search");
      const conversationId = selectedConversation();
      if (
        !section?.dataset.modelGrantId
        || section.dataset.modelGrantConversation !== conversationId
      ) {
        throw new Error("Grant this exact library to the current chat first.");
      }
      const query = document.getElementById("information-search-query")?.value || "";
      const message = document.getElementById("information-chat-message")?.value || "";
      const result = await invoke("mom_llama_information_chat_send", {
        conversationId,
        grantId: section.dataset.modelGrantId,
        query,
        message,
      });
      report(result);
      closeSettings();
      await refreshConversationProjection();
    },
    "information-managed-refresh": async () => {
      const records = await refreshManagedAttachments();
      report({ managed_attachments: records.length, projection_only: true });
    },
    "information-search-managed": async (button) => {
      const article = button.closest(".information-managed-item");
      const text = article?.querySelector("input[type='search']")?.value || "";
      const hits = await invoke("mom_llama_information_search_managed_attachment", {
        materializationId: article?.dataset.materialization || "",
        text,
      });
      renderManagedEvidence(article.querySelector(".information-managed-results"), hits);
    },
    "information-open-managed-citation": async (button) => {
      const evidence = await invoke("mom_llama_information_open_managed_citation", {
        citation: JSON.parse(button.dataset.citation || "{}"),
      });
      const exact = document.createElement("pre");
      exact.className = "information-citation-context";
      exact.textContent = evidence.snippet;
      button.closest(".information-managed-hit")?.append(exact);
      button.disabled = true;
    },
    "information-managed-remove-preview": async (button) => {
      const article = button.closest(".information-managed-item");
      const selection = article?.dataset.retainedOperation
        ? { kind: "retained", operation_id: article.dataset.retainedOperation }
        : { kind: "active", materialization_id: article?.dataset.materialization || "" };
      const confirmation = await invoke("mom_llama_information_managed_removal_preview", { selection });
      const preview = confirmation.preview;
      const output = article.querySelector(".information-managed-results");
      output.replaceChildren();
      const impact = document.createElement("p");
      impact.textContent = `Remove ${preview.observed_managed_bytes} managed bytes only · content ${preview.content_sha256} · database ${preview.database_sha256} · external source removed: ${preview.external_source_bytes_removed}.`;
      const commit = createCommandElement(
        "button",
        DYNAMIC_CONTROL_SPECS.informationManagedRemovalCommit,
      );
      commit.type = "button";
      commit.className = "primary-button danger-button";
      commit.dataset.action = "information-managed-remove-commit";
      commit.dataset.confirmation = JSON.stringify(confirmation);
      setInformationRemovalControl(commit, "commit", "Remove", "Confirm this exact removal; original source bytes are preserved.");
      output.append(impact, commit);
    },
    "information-managed-remove-commit": async (button) => {
      const result = await invoke("mom_llama_information_managed_removal_commit", {
        confirmation: JSON.parse(button.dataset.confirmation || "{}"),
      });
      if (result.external_source_bytes_removed) {
        throw new Error("Information reported an invalid external-source removal.");
      }
      report(result);
      await refreshManagedAttachments();
    },
    "settings-get": async () => report(await invoke("mom_llama_settings_get")),
    "settings-reset": async () => { report(await invoke("mom_llama_settings_reset")); await refreshSettings("general"); },
    "settings-retry": async () => {
      const failures = [...autosaveQueues.entries()]
        .filter(([, queue]) => (
          queue.failure
          && !queue.running
          && !queue.pending
          && queue.failure.job.revision === queue.latestRevision
        ))
        .map(([key, queue]) => ({ key, failure: queue.failure }));
      failures.forEach(({ key, failure }) => queueAutosave(key, failure.job, 0));
    },
    "model-list": async () => {
      report(await invoke("mom_llama_model_list"));
      await Promise.all([refreshChat(), refreshSettings("general")]);
    },
    "model-select": async (button) => {
      await selectAndLoadModel(button, button.dataset.modelPath || "");
    },
    "model-browse": async (button) => {
      const path = await pickFile("model");
      if (path) await selectAndLoadModel(button, path);
    },
    "mcp-status": async () => { report(await invoke("mom_llama_mcp_status")); openSettings("mcp"); },
    "mcp-command-browse": async () => {
      const path = await pickFile("mcp");
      if (path) formField(document.getElementById("settings-form"), "mcp_command").value = path;
    },
    "mcp-configure": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_mcp_configure", {
        name: formValue(form, "mcp_server"),
        command: formValue(form, "mcp_command"),
        args: [],
        enabled: true,
      }));
    },
    "mcp-list-servers": async () => report(await invoke("mom_llama_mcp_list_servers")),
    "mcp-list-tools": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_mcp_list_tools", { server: formValue(form, "mcp_server") }));
    },
    "mcp-call-tool": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_mcp_call_tool", {
        server: formValue(form, "mcp_server"),
        tool: formValue(form, "mcp_tool"),
        arguments: jsonField(form, "mcp_arguments"),
      }));
    },
    "mcp-list-resources": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_mcp_list_resources", { server: formValue(form, "mcp_server") }));
    },
    "mcp-read-resource": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_mcp_read_resource", {
        server: formValue(form, "mcp_server"), uri: formValue(form, "mcp_uri"),
      }));
    },
    "mcp-list-prompts": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_mcp_list_prompts", { server: formValue(form, "mcp_server") }));
    },
    "mcp-get-prompt": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_mcp_get_prompt", {
        server: formValue(form, "mcp_server"),
        prompt: formValue(form, "mcp_prompt"),
        arguments: jsonField(form, "mcp_arguments"),
      }));
    },
    "tool-loop-prepare": async () => {
      const form = document.getElementById("settings-form");
      const result = await invoke("mom_llama_tool_loop_prepare", {
        conversation: selectedConversation(),
        prompt: formValue(form, "tool_loop_prompt"),
        server: formValue(form, "mcp_server"),
        tool: formValue(form, "mcp_tool"),
        arguments: jsonField(form, "mcp_arguments"),
        maxTurns: Math.max(1, Math.min(8, Math.trunc(settingNumber("agenticMaxTurns", 4)))),
      });
      report(result);
      if (result?.status !== "blocked") {
        openToolApproval(result?.result);
        if (result?.result?.requires_confirmation === false) {
          await actionHandlers["tool-loop-run"]();
        }
      }
    },
    "tool-approval-close": async () => closeToolApproval(),
    "tool-loop-run": async () => {
      const modal = document.getElementById("tool-approval-modal");
      if (!modal?.dataset.approvalId) throw new Error("Prepare and review the tool call first.");
      const approve = modal.querySelector('[data-action="tool-loop-run"]');
      const cancel = modal.querySelector('[data-action="tool-loop-cancel"]');
      const close = modal.querySelector('[data-action="tool-approval-close"]');
      modal.dataset.running = "true";
      if (approve) approve.disabled = true;
      if (cancel) cancel.disabled = false;
      if (close) close.disabled = true;
      try {
        const result = await invoke("mom_llama_tool_loop_run", {
          input: {
            conversation: modal.dataset.conversation || selectedConversation(),
            prompt: modal.dataset.prompt || "",
            server: modal.dataset.server || "",
            tool: modal.dataset.tool || "",
            arguments: JSON.parse(modal.dataset.arguments || "{}"),
            maxTurns: Number(modal.dataset.maxTurns || 1),
            approvalId: modal.dataset.approvalId,
          },
        });
        report(result);
      } finally {
        modal.dataset.running = "false";
        closeToolApproval();
        closeSettings();
        await refreshConversationProjection();
      }
    },
    "tool-loop-cancel": async () => {
      const modal = document.getElementById("tool-approval-modal");
      const cancel = modal?.querySelector('[data-action="tool-loop-cancel"]');
      if (cancel) cancel.disabled = true;
      let result;
      for (let attempt = 0; attempt < 20; attempt += 1) {
        result = await invoke("mom_llama_tool_loop_cancel", {
          conversation: modal?.dataset.conversation || selectedConversation(),
        });
        if (result?.blocker?.code !== "no_active_tool_loop") break;
        await wait(50);
      }
      report(result);
      if (result?.blocker?.code === "no_active_tool_loop" && cancel) {
        cancel.disabled = false;
      }
    },
    "tool-permission-list": async () => {
      report(await invoke("mom_llama_tool_permission_list"));
    },
    "tool-permission-set": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_tool_permission_set", {
        server: formValue(form, "permission_server"),
        tool: formValue(form, "permission_tool"),
        policy: formValue(form, "permission_policy"),
      }));
    },
    "tool-permission-revoke": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_tool_permission_revoke", {
        server: formValue(form, "permission_server"),
        tool: formValue(form, "permission_tool"),
      }));
    },
  };

  document.addEventListener("click", async (event) => {
    document.querySelectorAll("details.model-picker[open]").forEach((picker) => {
      if (!picker.contains(event.target)) picker.removeAttribute("open");
    });
    if (!event.target.closest("#persona-context-menu, .persona-menu-trigger")) {
      closePersonaMenu(false);
    }
    const button = event.target.closest("[data-action]");
    if (!button || button.disabled) return;
    if (MCP_PROCESS_ACTIONS.has(button.dataset.action) && !requireMcpProcessUi()) return;
    const handler = actionHandlers[button.dataset.action];
    if (!handler) return;
    event.preventDefault();
    try { await handler(button, event); } catch (error) { reportError(error); }
  });

  const popupRowMenu = async (target, point = null) => {
    const Menu = window.__TAURI__?.menu?.Menu;
    if (!Menu) throw new Error("Native contextual menus are unavailable.");
    const safeAction = (action) => async () => {
      try { await action(); } catch (error) { reportError(error); }
    };
    let items;
    if (target.dataset.personaMenuTarget) {
      const persona = target.dataset.persona;
      items = [
        { id: "start", text: "Chat", action: safeAction(() => openPersonaConversation(persona)) },
        { id: "edit", text: "Edit", action: safeAction(() => openPersonaProfile(persona)) },
        { id: "remove", text: "Remove", action: safeAction(() => openPersonaRemoval(persona)) },
      ];
    } else {
      const conversation = target.dataset.conversation;
      items = [
        { id: "rename", text: "Rename…", action: safeAction(() => beginConversationRename(target)) },
        { id: "delete", text: "Delete", action: safeAction(async () => {
          await retainComposerDraft();
          report(await invoke("mom_llama_conversation_delete", { conversation }));
          await refreshConversationProjection();
        }) },
        { id: "save-persona", text: "Save as Persona…", enabled: !!target.dataset.message,
          action: safeAction(async () => {
            if (selectedConversation() !== conversation) await selectConversation(conversation);
            openPersonaFreeze(target);
          }) },
      ];
    }
    const menu = await Menu.new({ items });
    const rect = target.getBoundingClientRect();
    const anchor = point || { x: rect.left, y: rect.bottom };
    await menu.popup(new window.__TAURI__.dpi.LogicalPosition(anchor.x, anchor.y));
    // Tauri callbacks outlive popup(), which returns as soon as it is shown.
    // Retain this resource until another menu replaces it.
    if (popupRowMenu.previous) await popupRowMenu.previous.close();
    popupRowMenu.previous = menu;
  };

  document.addEventListener("contextmenu", (event) => {
    const target = event.target.closest('[data-persona-menu-target="true"], [data-conversation-menu-target="true"]');
    if (!target) return;
    event.preventDefault();
    popupRowMenu(target, { x: event.clientX, y: event.clientY }).catch(reportError);
  });

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && event.target.closest("#conversation-search-form")) {
      event.preventDefault();
      setSearchMode(false);
    }
    if (event.key === "Escape" && event.target.closest("#voice-call-modal")) {
      event.preventDefault();
      void closeVoiceCallSurface().then(() => {
        document.querySelector('[data-action="voice-call-open"]')?.focus();
      }).catch(reportError);
    }
    if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
      const target = event.target.closest('[data-persona-menu-target="true"], [data-conversation-menu-target="true"]');
      if (target) {
        event.preventDefault();
        popupRowMenu(target).catch(reportError);
      }
    }
    if (event.target.matches(".sidebar-resizer") && ["ArrowLeft", "ArrowRight"].includes(event.key)) {
      event.preventDefault();
      resizeSidebar(document.querySelector(".sidebar").getBoundingClientRect().width + (event.key === "ArrowRight" ? 16 : -16));
    }
  });

  const resizeSidebar = (width) => {
    const clamped = Math.max(180, Math.min(width, 480, window.innerWidth - 300));
    document.documentElement.style.setProperty("--sidebar-width", `${clamped}px`);
    document.querySelector(".sidebar-resizer")?.setAttribute("aria-valuenow", String(Math.round(clamped)));
    localStorage.setItem("mom-sidebar-width", String(clamped));
  };
  const storedSidebarWidth = Number(localStorage.getItem("mom-sidebar-width"));
  if (storedSidebarWidth) resizeSidebar(storedSidebarWidth);
  window.addEventListener("resize", () => resizeSidebar(document.querySelector(".sidebar")?.getBoundingClientRect().width || 256));
  document.addEventListener("pointerdown", (event) => {
    if (!event.target.matches(".sidebar-resizer")) return;
    event.preventDefault();
    const separator = event.target;
    separator.setPointerCapture(event.pointerId);
    separator.onpointermove = (move) => resizeSidebar(move.clientX);
    separator.onpointerup = separator.onpointercancel = () => {
      separator.onpointermove = null;
      separator.releasePointerCapture(event.pointerId);
    };
  });

  document.addEventListener("submit", async (event) => {
    event.preventDefault();
    const form = event.target;
    try {
      if (form.id === "chat-form") {
        if (autocompleteAccepting) return;
        if (form.dataset.busy === "true") return;
        const recipientQuery = document.getElementById("recipient-query");
        let exactRecipient = null;
        if (recipientQuery?.value.trim()) {
          filterRecipientOptions();
          exactRecipient = exactRecipientOption(recipientQuery.value);
          if (!exactRecipient) {
            recipientQuery.focus();
            recipientQuery.setAttribute("aria-invalid", "true");
            return;
          }
        }
        cancelComposerAutocomplete();
        // Voice prompts are transport metadata, never composer content. The
        // synchronous submit event consumes this one-shot value before the
        // caller returns, so an exception cannot strand it in visible UI.
        const privateVoicePrompt = pendingVoiceDispatchPrompt;
        pendingVoiceDispatchPrompt = null;
        const message = privateVoicePrompt || formValue(form, "message");
        const voiceDispatch = isInternalVoicePrompt(message);
        const attachmentIds = draftAttachmentIds(form);
        if (!message && !attachmentIds.length) return;
        if (voiceDispatch) {
          form.querySelector("#composer-attachments")?.remove();
        }
        // Acquire the UI-side dispatch gate before the first await. Without
        // this, rapid Enter presses can race persona instantiation or draft
        // persistence and dispatch the same turn more than once.
        const dispatchLease = acquireChatBusy("chat-dispatch");
        const dispatchOwner = {};
        const sourceConversation = selectedConversation();
        let conversation = sourceConversation;
        let dispatchVoiceOwner = null;
        activeDispatchConversation = sourceConversation;
        activeDispatchOwner = dispatchOwner;
        const textarea = formField(form, "message");
        let releaseHistoryRecovery = null;
        try {
          releaseHistoryRecovery = await suspendVoiceHistoryRecoveryForForeground("chat_send");
          if (exactRecipient) {
            await updateRecipients((ids) => ids.includes(exactRecipient.dataset.recipient)
              ? ids
              : [...ids, exactRecipient.dataset.recipient]);
          }
          await recipientMutation;
          if (sourceConversation === "default") {
            await persistDraftNow(message, attachmentIds, sourceConversation);
            const created = await invoke("mom_llama_conversation_draft_submit");
            report(created);
            if (created?.status === "blocked" || !created?.result?.id) return;
            conversation = created.result.id;
            chat().dataset.currentConversation = conversation;
            chat().dataset.conversationKind = "chat";
          } else if (selectedConversationKind() === "persona_template") {
            if (attachmentIds.length) {
              report({
                status: "blocked",
                blocker: {
                  code: "persona_template_staged_attachments",
                  message: "Start a chat before adding attachments to a Persona.",
                },
              });
              return;
            }
            const instantiated = await instantiatePersona(sourceConversation);
            if (instantiated?.status === "blocked" || !instantiated?.result?.id) return;
            conversation = instantiated.result.id;
            chat().dataset.currentConversation = conversation;
            chat().dataset.conversationKind = "chat";
          }
          if (activeDispatchOwner === dispatchOwner) {
            activeDispatchConversation = conversation;
          }
          if (!voiceDispatch) {
            await persistDraftNow(message, attachmentIds, conversation);
          }
          if (conversation !== sourceConversation && sourceConversation !== "default") {
            await invoke("mom_llama_draft_update", {
              conversation: sourceConversation,
              message: "",
              attachmentIds: [],
            });
          }
          if (selectedConversation() === conversation) {
            if (textarea) textarea.value = "";
            sizeComposer();
            if (message && !message.startsWith("[Voice note]") && !message.startsWith("[Phone call]")) {
              appendLiveMessage("user", message, `live-user-${Date.now()}`);
            }
          }
          closeMentions();
          // Capture the controller that owns this exact dispatch. A later UI
          // action may replace the global controller while native generation
          // is still finishing; the authoritative result must still settle
          // the bubble created for this turn.
          const voiceTtsOwner = voiceDispatch
            && pendingVoiceTtsOwner?.conversation === conversation
            ? pendingVoiceTtsOwner
            : (voiceDispatch && liveVoiceTts?.conversation === conversation ? liveVoiceTts : null);
          dispatchVoiceOwner = voiceTtsOwner;
          if (voiceDispatch && pendingVoiceTtsOwner === voiceTtsOwner) pendingVoiceTtsOwner = null;
          let result;
          let voiceTranscriptAttachment = null;
          let liveVoice = null;
          let fallbackVoice = null;
          let voiceDispatchTrace = voiceDispatch
            ? (voiceTtsOwner?.trace || voiceLatencyTrace) : null;
          try {
            liveVoice = voiceDispatch
              && pendingVoiceAudioSession?.conversation === conversation
              ? pendingVoiceAudioSession
              : null;
            fallbackVoice = voiceDispatch
              && pendingVoiceFallbackAttachment?.conversation === conversation
              ? pendingVoiceFallbackAttachment
              : null;
            voiceDispatchTrace = (liveVoice || fallbackVoice)?.sourceCapture?.trace
              || voiceDispatchTrace;
            if (voiceDispatch) markVoiceLatencyIn(voiceDispatchTrace, "generation_dispatched");
            if (liveVoice) pendingVoiceAudioSession = null;
            if (fallbackVoice) pendingVoiceFallbackAttachment = null;
            voiceTranscriptAttachment = fallbackVoice?.attachment || null;
            if (liveVoice) {
              const operation = invoke("mom_llama_voice_audio_finish", {
                session: liveVoice.session,
                recording: liveVoice.recording,
                ...(liveVoice.modelInput ? {
                  modelInputRecording: liveVoice.modelInput.recording,
                  sourceSampleCount: liveVoice.modelInput.sourceSampleCount,
                  acceptedSampleCount: liveVoice.modelInput.acceptedSampleCount,
                  ...(liveVoice.modelInput.sourceRanges
                    ? { sourceRanges: liveVoice.modelInput.sourceRanges }
                    : { sourceStartSample: liveVoice.modelInput.sourceStartSample }),
                } : {}),
              });
              const finished = await (voiceTtsOwner
                ? voiceTtsOwner.trackGeneration(operation) : operation);
              result = finished?.result ? {
                ...finished,
                result: { kind: "direct", conversation_id: conversation, output: finished.result },
              } : finished;
            } else {
              // The degraded import path stages the captured audio in the
              // native draft. Bind the private voice prompt to that exact
              // attachment before dispatch so an older saved composer draft
              // cannot win the admission race.
              if (fallbackVoice) {
                await persistDraftNow(message, [fallbackVoice.attachment], conversation);
              }
              const operation = invoke("mom_llama_chat_dispatch", {
                conversation, message,
                ...(voiceTtsOwner?.clientRequest ? { clientRequest: voiceTtsOwner.clientRequest } : {}),
              });
              result = await (voiceTtsOwner
                ? voiceTtsOwner.trackGeneration(operation) : operation);
            }
            if (voiceDispatch) {
              const output = result?.result?.output;
              markVoiceLatencyIn(voiceDispatchTrace, "generation_finished", {
                native_duration_ms: output?.duration_ms,
                first_token_ms: output?.first_token_ms,
                cache_metrics: output?.cache_metrics || null,
                voice_audio: output?.voice_audio_metrics,
              });
            }
          } catch (error) {
            // Rust retains the saved draft until commit. Recovery must never
            // write through the renderer's possibly changed selection.
            if (!voiceDispatch && selectedConversation() === conversation && textarea && !textarea.value) textarea.value = message;
            if (voiceDispatch) {
              await invoke("mom_llama_draft_update", {
                conversation,
                message: "",
                attachmentIds: [],
              }).catch(reportError);
            }
            await refreshChat().catch(reportError);
            throw error;
          }
          report(result);
          releaseMentionInvocationBusy(result?.result?.invocation?.id);
          const pendingApproval = result?.result?.invocation?.tool_approvals?.find(
            (approval) => approval.state === "pending",
          );
          if (pendingApproval && mcpProcessUiSupported()) {
            openPersonaToolApproval(pendingApproval);
          }
          if (!voiceDispatch && result?.status === "blocked" && selectedConversation() === conversation && textarea && !textarea.value) {
            textarea.value = message;
          }
          if (voiceDispatch) {
            await invoke("mom_llama_draft_update", {
              conversation,
              message: "",
              attachmentIds: [],
            }).catch(reportError);
            voiceReplyMessageId = result?.status !== "blocked"
              ? result?.result?.output?.assistant_message_id || null
              : null;
            // The presence of a direct output is the authoritative shape
            // discriminator. Mention dispatches have no `output` field. Do
            // not make voice synthesis depend on an adjacent serde enum tag:
            // the attachment fallback can commit valid assistant text while
            // that presentation-only tag is unavailable at this boundary.
            const output = result?.result?.output || null;
            if (result?.status !== "blocked" && output?.assistant_text) {
              markVoiceLatencyIn(voiceDispatchTrace, "tts_text_admitted", {
                characters: output.assistant_text.length,
                owner_bound: Boolean(voiceTtsOwner),
              });
              if (!voiceTtsOwner) {
                throw new Error("voice_reply_owner_missing: The completed reply lost its speech owner.");
              }
              // A successful model result transfers this turn to the speech
              // drain. Keep that exact owner alive until every text segment
              // has either produced PCM or failed explicitly.
              const synthesized = await finishVoiceReplyAndIndexTurn(
                callReplyConversation === conversation,
                () => {
                  if (result?.result?.kind === "direct") {
                    // Keep call transcripts queued before the next capture;
                    // voice-note backfill waits until durable speech succeeds.
                    indexVoiceTurn(conversation, output, voiceTranscriptAttachment,
                      (liveVoice || fallbackVoice)?.sourceCapture || null, voiceDispatchTrace);
                  }
                },
                () => voiceTtsOwner.finish(output.assistant_text),
                callReplyConversation === conversation ? null : () => voiceTtsOwner.bindDurableVoiceReply({
                  conversation,
                  message: output.assistant_message_id,
                  text: output.assistant_text,
                }),
              );
              if (!synthesized) {
                throw new Error("voice_reply_incomplete: The completed reply did not produce complete audio.");
              }
            } else {
              await voiceTtsOwner?.stop().catch(() => {});
              if (callReplyConversation === conversation && voiceCallActive) {
                await beginVoiceCapture({ call: true });
              }
            }
          }
          if (!voiceDispatch) {
            document.querySelectorAll(".optimistic-voice-message").forEach((row) => {
              if (row.dataset.objectUrl) URL.revokeObjectURL(row.dataset.objectUrl);
              row.remove();
            });
            await refreshConversationProjection();
          } else {
            // Keep the live audio rows mounted while PCM is buffered and
            // played. Replacing the chat here exposed the completed text and
            // destroyed the stream controller before its first audio frame.
            await Promise.all([refreshSidebar(), refreshConversationSettingsProjection()]);
          }
          if (activeDispatchOwner === dispatchOwner) {
            if (callReplyConversation === conversation) {
              callReplyConversation = null;
            }
            if (voiceReplyConversation === conversation && result?.status !== "blocked") {
              voiceReplyConversation = null;
              const row = document.getElementById("live-voice-reply-pending");
              if (row && voiceReplyMessageId) {
                row.id = `message-${voiceReplyMessageId}`;
                row.dataset.messageId = voiceReplyMessageId;
              }
              voiceReplyMessageId = null;
            }
          }
        } finally {
          // A rejected native batch can return without a terminal stream event
          // or an invocation result. Its started events must not strand Stop.
          [...activeDispatchStreamLeaseOwners]
            .filter(([, owner]) => owner === dispatchOwner)
            .forEach(([lease]) => releaseChatBusy(lease));
          if (activeDispatchOwner === dispatchOwner) {
            activeDispatchOwner = null;
            if (activeDispatchConversation === conversation) activeDispatchConversation = null;
            if (voiceReplyConversation === conversation) voiceReplyConversation = null;
            voiceReplyMessageId = null;
            if (callReplyConversation === conversation) callReplyConversation = null;
            if (dispatchVoiceOwner && pendingVoiceTtsOwner === dispatchVoiceOwner) pendingVoiceTtsOwner = null;
          }
          releaseChatBusy(dispatchLease);
          releaseHistoryRecovery?.();
        }
      }
      if (form.id === "settings-form") {
        scheduleSettingsAutosave(0);
      }
      if (form.id === "conversation-search-form") await search();
    } catch (error) { reportError(error); }
  });

  document.addEventListener("compositionstart", (event) => {
    if (!event.target.matches("#chat-form textarea[name='message']")) return;
    if (autocompleteAccepting) return;
    cancelComposerAutocomplete();
    transitionComposer({ type: "composition_start" });
    hideMentionPresentation();
  });

  document.addEventListener("compositionend", (event) => {
    if (!event.target.matches("#chat-form textarea[name='message']")) return;
    transitionComposer({ type: "composition_end" });
    scheduleDraft(event.target.value);
    updateMentionCandidates(event.target).catch(reportError);
    scheduleComposerAutocomplete(event.target);
  });

  document.addEventListener("input", (event) => {
    if (event.target.matches("#attachment-library-item-title, #attachment-library-rights")) {
      const modal = document.getElementById("attachment-library-modal");
      if (modal) clearAttachmentLibraryPreview(modal);
      return;
    }
    if (event.target.matches("[data-model-search]")) {
      const query = event.target.value.trim().toLocaleLowerCase();
      event.target.closest(".model-picker-popover")
        ?.querySelectorAll("[data-model-search-value]")
        .forEach((option) => {
          option.hidden = Boolean(query)
            && !option.dataset.modelSearchValue.includes(query);
        });
    }
    if (event.target.matches("#chat-form textarea[name='message']")) {
      sizeComposer();
      renderChatBusyState();
      if (autocompleteAccepting) return;
      if (event.isComposing || composerState.kind === "composing") return;
      const committedAutocomplete = event.target.dataset.autocompleteCommittedDraft;
      const alreadyPersisted = committedAutocomplete === event.target.value;
      delete event.target.dataset.autocompleteCommittedDraft;
      cancelComposerAutocomplete();
      if (!alreadyPersisted) scheduleDraft(event.target.value);
      updateMentionCandidates(event.target).catch(reportError);
      if (!alreadyPersisted) scheduleComposerAutocomplete(event.target);
    }
    if (event.target.matches('[name="freeze_name"]') && !formValue(document.getElementById("persona-freeze-modal"), "freeze_handle")) {
      formField(document.getElementById("persona-freeze-modal"), "freeze_handle").value = slugHandle(event.target.value);
    }
    if (event.target.matches('[name="persona_chat_template_policy"]')) {
      document.querySelector("#persona-editor .persona-template-source")?.classList.toggle("is-hidden", event.target.value !== "frozen_source");
    }
    if (event.target.matches(
      "#settings-form textarea[data-setting-key], #settings-form input[type='text'][data-setting-key], #settings-form input[type='password'][data-setting-key]",
    )) {
      scheduleSettingsAutosave();
    }
    if (event.target.matches("#settings-form [data-chat-setting='system_message']")) {
      scheduleChatInstructionsAutosave(event.target);
    }
    if (event.target.matches("#conversation-search-form input[name='query']")) search().catch(reportError);
  });

  document.addEventListener("scroll", (event) => {
    if (event.target.matches?.("#chat-form textarea[name='message']")) {
      syncComposerAutocompleteScroll(event.target);
      return;
    }
    const stream = event.target;
    if (!(stream instanceof Element) || !stream.matches(".message-stream")) return;
    const distanceFromTail = stream.scrollHeight - stream.scrollTop - stream.clientHeight;
    stream.dataset.followTail = distanceFromTail <= 96 ? "true" : "false";
  }, true);

  document.addEventListener("paste", async (event) => {
    if (!event.target.matches("#chat-form textarea[name='message']")) return;
    if (autocompleteAccepting) {
      event.preventDefault();
      return;
    }
    const threshold = Math.max(0, Math.trunc(settingNumber("pasteLongTextToFileLen", 2500)));
    const text = event.clipboardData?.getData("text/plain") || "";
    if (threshold === 0 || text.length < threshold) return;
    event.preventDefault();
    try {
      await persistDraftNow(event.target.value, draftAttachmentIds(event.target.form));
      const result = await invoke("mom_llama_attachment_import_paste", {
        conversation: selectedConversation(),
        text,
      });
      report(result);
      if (result?.status !== "blocked") await refreshConversationProjection();
    } catch (error) { reportError(error); }
  });

  document.addEventListener("keydown", (event) => {
    if (event.target.matches("[data-model-search]")) {
      if (event.key === "Enter") {
        event.preventDefault();
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        const picker = event.target.closest("details.model-picker");
        picker?.removeAttribute("open");
        picker?.querySelector("summary")?.focus();
        return;
      }
    }
    const personaMenu = document.getElementById("persona-context-menu");
    if (event.target.matches(".persona-menu-trigger") && event.key === "ArrowDown") {
      event.preventDefault();
      openPersonaMenu(event.target);
      return;
    }
    if (personaMenu && !personaMenu.hidden && event.target.closest("#persona-context-menu")) {
      const items = [...personaMenu.querySelectorAll('[role="menuitem"]')];
      const current = items.indexOf(event.target);
      if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
        event.preventDefault();
        const next = event.key === "Home" ? 0
          : event.key === "End" ? items.length - 1
            : (current + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
        items[next]?.focus();
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        closePersonaMenu(true);
        return;
      }
      if (event.key === "Tab") closePersonaMenu(false);
    }
    if (event.key === "Escape" && !document.getElementById("persona-removal-modal")?.hidden) {
      event.preventDefault();
      setModalVisibility("persona-removal-modal", false);
      return;
    }
    if (event.key === "Escape" && !document.getElementById("tool-approval-modal")?.hidden) {
      event.preventDefault();
      closeToolApproval();
      return;
    }
    if (event.key === "Escape" && event.target.matches(".inline-message-editor")) {
      event.preventDefault();
      refreshChat().catch(reportError);
      return;
    }
    if (event.key === "Escape" && event.target.closest("#settings-sidebar")) {
      event.preventDefault();
      closeSettings();
      return;
    }
    if (!event.target.matches("#chat-form textarea[name='message']")) return;
    const mentionList = document.getElementById("mention-candidates");
    const mentionOptions = [...mentionList?.querySelectorAll(".mention-candidate") || []];
    const mentionPresentationOpen = mentionOptions.length > 0
      && !mentionList?.classList.contains("is-hidden");
    if (composerState.kind === "mention" && (
      !mentionPresentationOpen || composerState.optionCount !== mentionOptions.length
    )) {
      transitionComposer({ type: "mention_close" });
    } else if (composerState.kind !== "mention" && mentionPresentationOpen) {
      transitionComposer({ type: "mention_open", optionCount: mentionOptions.length });
      syncMentionAria();
    }
    const keyEffects = transitionComposer({
      type: "key_down",
      key: event.key,
      keyCode: event.keyCode,
      isComposing: event.isComposing,
      shiftKey: event.shiftKey,
      altKey: event.altKey,
      metaKey: event.metaKey,
      ctrlKey: event.ctrlKey,
      sendOnEnter: document.querySelector('[data-setting-key="sendOnEnter"]')?.checked !== false,
    });
    for (const keyEffect of keyEffects) {
      if (keyEffect.kind === "mention_active_changed") {
        event.preventDefault();
        syncMentionAria();
      }
      if (keyEffect.kind === "mention_accept") {
        event.preventDefault();
        const option = mentionOptions[keyEffect.activeIndex];
        if (option) insertMention(event.target, option.dataset.handle || "");
      }
      if (keyEffect.kind === "mention_dismiss") {
        event.preventDefault();
        hideMentionPresentation();
      }
      if (keyEffect.kind === "ai_accept") {
        event.preventDefault();
        acceptComposerAutocomplete(event.target, keyEffect).catch(() => {
          cancelComposerAutocomplete({ native: false });
          refreshChat().catch(() => {});
        });
      }
      if (keyEffect.kind === "ai_dismiss") {
        event.preventDefault();
        cancelComposerAutocomplete({ native: false, announce: true });
      }
      if (keyEffect.kind === "ai_cancel") {
        cancelComposerAutocomplete({ forceNative: true });
      }
      if (keyEffect.kind === "submit") {
        event.preventDefault();
        event.target.form?.requestSubmit();
      }
    }
  });

  document.addEventListener("selectionchange", () => {
    if (autocompleteAccepting) return;
    const textarea = composerTextarea();
    if (document.activeElement !== textarea) return;
    if (!presentedAutocomplete) return;
    if (autocompleteAnchorKey(composerLogicalAnchor(textarea)) !== autocompleteAnchorKey(presentedAutocomplete.anchor)) {
      cancelComposerAutocomplete({ native: false });
    }
  });

  document.addEventListener("focusout", (event) => {
    if (autocompleteAccepting) return;
    if (event.target.matches?.("#chat-form textarea[name='message']")) {
      cancelComposerAutocomplete();
    }
  });

  document.addEventListener("change", async (event) => {
    if (event.target.matches("#information-rights-confirmed")) {
      const register = document.getElementById("information-alexandria-register");
      const hasGrant = Boolean(document.getElementById("information-path-grant")?.value);
      if (register) register.disabled = !(hasGrant && event.target.checked);
      return;
    }
    if (event.target.matches("#information-library-select")) {
      const section = event.target.closest(".information-search");
      delete section?.dataset.modelGrantId;
      delete section?.dataset.modelGrantConversation;
      delete section?.dataset.modelGrantTarget;
      return;
    }
    if (event.target.matches("#attachment-library-item-title, #attachment-library-rights")) {
      const modal = document.getElementById("attachment-library-modal");
      if (modal) clearAttachmentLibraryPreview(modal);
      return;
    }
    if (event.target.matches("#settings-form [data-setting-core], #settings-form [data-setting-key]")) {
      scheduleSettingsAutosave(0);
      return;
    }
    if (event.target.matches("#settings-form [data-chat-setting='system_message']")) {
      scheduleChatInstructionsAutosave(event.target, 0);
      return;
    }
  });

  const recoverFailedNativeCapture = async (recording, message) => {
    if (voiceRecording !== recording) return;
    voiceRecording = null;
    recording.discarded = true;
    clearTimeout(recording.limit);
    setVoiceRecordingVisible(false);
    markVoiceLatency("native_capture_failed", {
      message,
      recovery_attempt: recording.recoveryAttempt || 0,
    });

    await invoke("mom_llama_voice_capture_cancel", { capture: recording.capture }).catch(() => {});
    if (recording.speculation && !recording.speculation.confirmed) {
      await abortCallSpeculation(recording).catch(() => {});
    } else {
      const live = await recording.prefillSession.catch(() => null);
      if (live?.session) {
        await invoke("mom_llama_voice_audio_abort", { session: live.session }).catch(() => {});
      }
    }

    if (recording.call && voiceCallActive && (recording.recoveryAttempt || 0) < 1) {
      markVoiceLatency("native_capture_recovery_started");
      try {
        await beginNativeVoiceCapture({
          call: true,
          recoveryAttempt: (recording.recoveryAttempt || 0) + 1,
          preserveLatencyTrace: true,
        });
        markVoiceLatency("native_capture_recovery_ready");
        return;
      } catch (error) {
        markVoiceLatency("native_capture_recovery_failed", { message: errorMessage(error) });
      }
    }

    const failure = new Error(message);
    reportError(failure);
    if (recording.call && voiceCallActive) await closeVoiceCallSurface();
  };

  const onNativeVoiceCaptureEvent = ({ payload }) => {
    const recording = voiceRecording;
    if (!recording?.native || payload?.capture !== recording.capture) return;
    const emittedAtMs = Number(payload.emitted_at_unix_us || 0) / 1000;
    const nativeTiming = {
      audio_clock_ms: Math.round(Number(payload.sample_offset || 0) / 16),
      native_capture_elapsed_ms: Math.round(Number(payload.capture_elapsed_us || 0) / 1000),
      native_emitted_at_unix_us: Number(payload.emitted_at_unix_us || 0) || null,
      event_delivery_ms: emittedAtMs > 0
        ? Math.max(0, Math.round((Date.now() - emittedAtMs) * 100) / 100)
        : null,
    };
    recording.silentMs = Number(payload.silence_ms || 0);
    if (payload.event === "started") {
      markVoiceLatency("native_capture_event_started", nativeTiming);
      return;
    }
    if (payload.event === "first_sample") {
      markVoiceLatency("native_capture_first_sample", nativeTiming);
      return;
    }
    if (payload.event === "voice_onset") {
      recording.heardVoice = true;
      recording.voiceActivity = { heardVoice: true, voiced: true };
      interruptCallPlaybackForSpeech(recording, {
        producer: "native_ring",
        onset_sample: payload.sample_offset,
        ...nativeTiming,
      });
      markVoiceLatency("voice_activity_first_detected", {
        onset_sample: payload.sample_offset,
        rms: payload.rms,
        threshold: payload.threshold,
        close_threshold: payload.close_threshold,
        noise_floor: payload.noise_floor,
        producer: "native_ring",
        ...nativeTiming,
      });
      return;
    }
    if (payload.event === "silence_onset") {
      recording.voiceActivity = { heardVoice: recording.heardVoice, voiced: false };
      markVoiceLatency("silence_onset", {
        speculation: recording.speculation?.id || null,
        silence_ms: payload.silence_ms,
        onset_sample: payload.sample_offset,
        capture_elapsed_us: payload.capture_elapsed_us,
        rms: payload.rms,
        threshold: payload.threshold,
        close_threshold: payload.close_threshold,
        noise_floor: payload.noise_floor,
        producer: "native_ring",
        ...nativeTiming,
      });
      if (!recording.sending && !recording.speculation
          && !recording.speculationStarting) {
        void startVoiceSpeculation(recording).catch(reportError);
      }
      return;
    }
    if (payload.event === "speech_resumed") {
      recording.voiceActivity = { heardVoice: true, voiced: true };
      markVoiceLatency("speech_resumed", {
        onset_sample: payload.sample_offset,
        capture_elapsed_us: payload.capture_elapsed_us,
        rms: payload.rms,
        threshold: payload.threshold,
        close_threshold: payload.close_threshold,
        noise_floor: payload.noise_floor,
        producer: "native_ring",
        ...nativeTiming,
      });
      if (recording.speculation && !recording.speculation.confirmed
          && !recording.speculation.aborting) {
        recording.speculationRecovery = abortCallSpeculation(
          recording,
          { restartPrefill: true },
        ).catch((error) => {
          recording.speculationRecoveryError = error;
          reportError(error);
          return null;
        }).finally(() => {
          recording.speculationRecovery = null;
        });
      }
      return;
    }
    if (payload.event === "send_ready" && !recording.call && !recording.sending) {
      if (recording.speculation && !recording.speculation.aborting) {
        void confirmVoiceNoteSpeculation(recording, recording.speculation).catch(reportError);
      } else if (!recording.speculationStarting) {
        void sendVoiceCapture(recording).catch(reportError);
      }
      return;
    }
    if (payload.event === "failed") {
      void recoverFailedNativeCapture(
        recording,
        payload.message || "Native voice capture failed.",
      );
    }
  };

  const listen = async () => {
    const events = tauri() && tauri().event;
    if (!events || typeof events.listen !== "function") return;
    await events.listen("mom_llama_settings_open", async () => {
      try { await invoke("mom_llama_settings_get"); openSettings(); } catch (error) { reportError(error); }
    });
    await events.listen("mom_llama_chat_stream", onChatEvent);
    await events.listen("mom_llama_chat_dispatch_stream", onDispatchEvent);
    await events.listen("mom_llama_tool_loop_stream", onToolLoopEvent);
    await events.listen("mom_llama_voice_capture_event", onNativeVoiceCaptureEvent);
    await events.listen("mom_llama_voice_speculation_stage", ({ payload }) => {
      const speculation = voiceRecording?.speculation || activeVoiceSpeculation;
      const knownAttempt = voiceLatencyTrace?.stages?.some(
        (entry) => entry.speculation === payload?.speculation,
      );
      if (!payload?.stage
          || (!knownAttempt && (!speculation || payload.speculation !== speculation.id))) return;
      const emittedAtMs = Number(payload.emitted_at_unix_us || 0) / 1000;
      markVoiceLatency(payload.stage, {
        speculation: payload.speculation,
        native_command_elapsed_ms: Math.round(Number(payload.command_elapsed_us || 0) / 1000 * 100) / 100,
        native_emitted_at_unix_us: Number(payload.emitted_at_unix_us || 0) || null,
        event_delivery_ms: emittedAtMs > 0 ? Math.max(0, Math.round((Date.now() - emittedAtMs) * 100) / 100) : null,
        ...(payload.detail && typeof payload.detail === "object" ? payload.detail : {}),
      });
      if (payload.stage === "native_cancellation_registered"
          && speculation?.id === payload.speculation) {
        speculation.releaseAdmission?.();
        speculation.releaseAdmission = null;
      }
      if (payload.stage === "native_silence_released"
          && speculation?.id === payload.speculation) {
        speculation.releaseGeneration?.();
        speculation.releaseGeneration = null;
        speculation.tts?.activate?.();
        markVoiceLatency("generation_dispatched", {
          speculative: true,
          speculation: payload.speculation,
          producer: "native_silence_gate",
        });
      }
    });
    await events.listen("mom_llama_voice_capture_metrics", ({ payload }) => {
      if (voiceRecording?.native && payload?.capture === voiceRecording.capture) {
        voiceRecording.nativeMetrics = payload.metrics || {};
      }
      markVoiceLatency("capture_processing", {
        producer: "native_ring",
        ...(payload?.metrics || {}),
        wav_bytes: payload?.wav_bytes,
      });
    });
    await events.listen("mom_llama_speech_quiescing", () => {
      void stopSpeechPlayback({ notifyNative: false });
    });
  };

  const RESIZE_EDGE_PX = 14;
  const resizeHitForPoint = (clientX, clientY) => {
    const west = clientX <= RESIZE_EDGE_PX;
    const east = clientX >= window.innerWidth - RESIZE_EDGE_PX;
    const north = clientY <= RESIZE_EDGE_PX;
    const south = clientY >= window.innerHeight - RESIZE_EDGE_PX;
    if (north && west) return { direction: "NorthWest", cursor: "nwseResize", css: "nwse-resize" };
    if (north && east) return { direction: "NorthEast", cursor: "neswResize", css: "nesw-resize" };
    if (south && west) return { direction: "SouthWest", cursor: "neswResize", css: "nesw-resize" };
    if (south && east) return { direction: "SouthEast", cursor: "nwseResize", css: "nwse-resize" };
    if (north) return { direction: "North", cursor: "nsResize", css: "ns-resize" };
    if (south) return { direction: "South", cursor: "nsResize", css: "ns-resize" };
    if (east) return { direction: "East", cursor: "ewResize", css: "ew-resize" };
    if (west) return { direction: "West", cursor: "ewResize", css: "ew-resize" };
    return null;
  };
  let activeResizeCursor = "default";
  const presentResizeCursor = (hit) => {
    const cursor = hit?.cursor || "default";
    document.documentElement.style.cursor = hit?.css || "";
    if (cursor === activeResizeCursor) return;
    activeResizeCursor = cursor;
    const nativeWindow = tauri()?.window?.getCurrentWindow();
    if (nativeWindow) void nativeWindow.setCursorIcon(cursor).catch(reportError);
  };

  document.addEventListener("mousedown", (event) => {
    if (event.button !== 0) return;
    const nativeWindow = tauri()?.window?.getCurrentWindow();
    if (!nativeWindow) return;
    const resizeRegion = event.target.closest?.("[data-resize-direction]");
    const resizeHit = resizeHitForPoint(event.clientX, event.clientY);
    const resizeDirection = resizeHit?.direction || resizeRegion?.dataset.resizeDirection;
    if (resizeDirection) {
      event.preventDefault();
      void nativeWindow.startResizeDragging(resizeDirection).catch(reportError);
      return;
    }
    const toolbar = event.target.closest?.(".chrome");
    const control = event.target.closest?.("button, input, textarea, select, a, [contenteditable], form");
    if (toolbar && !control) {
      event.preventDefault();
      void nativeWindow.startDragging().catch(reportError);
    }
  });

  document.addEventListener("pointermove", (event) => {
    presentResizeCursor(resizeHitForPoint(event.clientX, event.clientY));
  });
  document.addEventListener("pointerleave", () => presentResizeCursor(null));

  const boot = async () => {
    const status = document.getElementById("startup-status");
    const retry = document.getElementById("startup-retry");
    const details = document.getElementById("startup-details");
    const diagnostic = document.getElementById("startup-error");
    const card = document.getElementById("startup-card");
    const greeting = document.getElementById("startup-greeting");
    const setupProgress = document.getElementById("startup-progress");
    const friends = document.getElementById("startup-friends");
    const friendPortraits = [
      ["bessel_van_der_kolk", "Bessel van der Kolk"],
      ["gabor_mate", "Gabor Maté"],
      ["peter_levine", "Peter Levine"],
      ["judith_herman", "Judith Herman"],
      ["richard_schwartz", "Richard Schwartz"],
      ["janina_fisher", "Janina Fisher"],
      ["ad_de_jongh", "Ad de Jongh"],
      ["christine_courtois", "Christine Courtois"],
      ["robert_miller", "Robert Miller"],
      ["arnold_popky", "Arnold Popky"],
      ["jim_knipe", "Jim Knipe"],
      ["francine_shapiro", "Francine Shapiro"],
      ["shirley_jean_schmidt", "Shirley Jean Schmidt"],
      ["dolores_mosquera", "Dolores Mosquera"],
    ];
    if (friends) {
      for (const [index, [id, name]] of friendPortraits.entries()) {
        const portrait = document.createElement("span");
        portrait.className = "startup-friend";
        portrait.dataset.personaId = id;
        portrait.setAttribute("role", "img");
        portrait.setAttribute("aria-label", name);
        portrait.title = name;
        portrait.style.backgroundPosition = `${(index % 4) * 100 / 3}% ${Math.floor(index / 4) * 100 / 3}%`;
        friends.append(portrait);
      }
    }
    let firstRunSetup = false;
    const initialize = async () => {
      if (status) {
        status.classList.remove("startup-error");
        status.textContent = "Opening";
        status.removeAttribute("title");
        status.removeAttribute("aria-label");
      }
      if (details) { details.hidden = true; details.open = false; }
      if (diagnostic) diagnostic.textContent = "";
      if (retry) {
        retry.hidden = true;
        retry.disabled = true;
      }
      const events = tauri()?.event;
      const revealSetup = () => {
        firstRunSetup = true;
        card?.classList.add("first-run");
        if (greeting) greeting.hidden = false;
        if (friends) friends.hidden = false;
        if (setupProgress) setupProgress.hidden = false;
      };
      const artifactLabel = (artifact) => ({
        gemma: "language model",
        projector: "listening model",
        mtp: "response accelerator",
        "qwen-weights": "voice model",
        "pocket-weights": "call voice",
        "qwen-codec-weights": "speech decoder",
      })[artifact] || "voice support";
      const stopSetupState = await events?.listen?.("mom_llama_setup_state", ({ payload }) => {
        if (!payload?.first_run) return;
        revealSetup();
        if (payload.phase === "ready" && status) status.textContent = "Ready";
      });
      const stopModelProgress = await events?.listen?.("mom_llama_model_acquisition_progress", ({ payload }) => {
        if (!firstRunSetup || !payload) return;
        const total = Number(payload.total_bytes || 0);
        const completed = Number(payload.completed_bytes || 0);
        const percent = total > 0 ? Math.max(0, Math.min(100, Math.floor((completed / total) * 100))) : 0;
        if (setupProgress) setupProgress.value = percent;
        if (status) status.textContent = payload.phase === "downloading"
          ? `Getting Blessenger ready · ${artifactLabel(payload.artifact)} · ${percent}%`
          : `Checking ${artifactLabel(payload.artifact)} · ${percent}%`;
      });
      const stopVoiceProgress = await events?.listen?.("mom_llama_voice_initialization_progress", ({ payload }) => {
        if (!firstRunSetup || !payload) return;
        if (status) status.textContent = `Checking ${payload.voice}`;
        for (const portrait of friends?.children || []) {
          portrait.classList.toggle("active", portrait.dataset.personaId === payload.persona_id);
        }
        if (setupProgress) setupProgress.value = Math.floor((Number(payload.completed || 0) / Number(payload.total || 1)) * 100);
      });
      const stopCachePreparationProgress = await events?.listen?.("mom_llama_cache_preparation_progress", ({ payload }) => {
        if (!firstRunSetup || !payload) return;
        if (status) status.textContent = `Preparing ${payload.persona}`;
        for (const portrait of friends?.children || []) {
          portrait.classList.toggle("active", portrait.dataset.personaId === payload.persona_id);
        }
        if (setupProgress) {
          const total = Math.max(1, Number(payload.total) || 1);
          const completed = Math.max(0, Math.min(total, Number(payload.completed) || 0));
          setupProgress.value = Math.floor((completed / total) * 100);
        }
      });
      try {
        await invoke("mom_llama_runtime_initialize");
        const root = document.getElementById("app");
        root.innerHTML = await invokeMarkup("mom_llama_render_app");
        const bootComposer = document.getElementById("chat-form");
        const interruptedVoiceDispatch = bootComposer?.dataset.internalVoiceDraft !== undefined
          || isInternalVoicePrompt(formField(bootComposer, "message")?.value || "");
        if (interruptedVoiceDispatch) {
          document.getElementById("chat-form")?.setAttribute("hidden", "");
          const cleared = await invoke("mom_llama_draft_update", {
            conversation: selectedConversation(),
            message: "",
            attachmentIds: [],
          });
          if (cleared?.status === "blocked") report(cleared);
          root.innerHTML = await invokeMarkup("mom_llama_render_app");
        }
        // Start restoring and compiling the resident audio model as soon as
        // the authoritative shell exists. It is supervised launch work rather
        // than disposable speculation: ordinary startup commands must not
        // cancel it and move compilation onto the caller's first utterance.
        const warmStarted = performance.now();
        runtimeWarmState = { status: "running" };
        runtimeWarmPromise = invoke("mom_llama_runtime_warm").then((response) => {
          runtimeWarmState = {
            status: response?.status === "blocked" ? "blocked" : "ready",
            total_ms: Math.round((performance.now() - warmStarted) * 100) / 100,
            model_loaded: Boolean(response?.result?.model_loaded),
            audio_projector_warmed: Boolean(response?.result?.audio_projector_warmed),
            audio_projector_warm_us: Number(response?.result?.audio_projector_warm_us || 0),
            persona_prefixes_ready: Number(response?.result?.persona_prefixes_ready || 0),
            persona_prefixes_reused: Number(response?.result?.persona_prefixes_reused || 0),
            persona_prefix_warm_us: Number(response?.result?.persona_prefix_warm_us || 0),
            persona_prefix_warm_status: response?.result?.persona_prefix_warm_status || "unknown",
            settings_us: Number(response?.result?.settings_us || 0),
            model_acquire_us: Number(response?.result?.model_acquire_us || 0),
            model_load: response?.result?.model_load || null,
          };
          if (voiceLatencyTrace) markVoiceLatency("runtime_warm_completed", runtimeWarmState);
          return runtimeWarmState;
        }).catch((error) => {
          runtimeWarmState = {
            status: "failed",
            total_ms: Math.round((performance.now() - warmStarted) * 100) / 100,
            message: errorMessage(error),
          };
          if (voiceLatencyTrace) markVoiceLatency("runtime_warm_failed", runtimeWarmState);
          return runtimeWarmState;
        });
        sizeComposer();
        await initializeWindowChrome();
        applyCustomCss(shell()?.dataset.customCss || "");
        await hydrateAttachmentPreviews(root);
        restoreChatViewport(null, chat());

        sizeComposer();
        await listen();
        // Recovery is opportunistic and has no place on the call admission
        // critical path. The installed CPU listener can work while Gemma is
        // resident; a failed turn keeps its exact WAV for a later retry.
        void runtimeWarmPromise.then(() => recoverPendingVoiceHistory()).catch((error) => {
          console.warn("Voice history recovery unavailable", errorMessage(error));
        });
      } catch (error) {
        if (status) {
          status.classList.add("startup-error");
          status.textContent = "Unavailable";
          status.title = String(error);
          status.setAttribute("aria-label", String(error));
        }
        if (diagnostic) diagnostic.textContent = String(error);
        if (details) details.hidden = false;
        if (retry) {
          retry.hidden = false;
          retry.disabled = false;
        }
      } finally {
        if (typeof stopModelProgress === "function") stopModelProgress();
        if (typeof stopVoiceProgress === "function") stopVoiceProgress();
        if (typeof stopCachePreparationProgress === "function") stopCachePreparationProgress();
        if (typeof stopSetupState === "function") stopSetupState();
      }
    };
    retry?.addEventListener("click", initialize);
    await initialize();
  };

  boot();
})();

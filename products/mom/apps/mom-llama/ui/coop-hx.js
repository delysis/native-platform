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

  const report = (value) => {
    const output = document.getElementById("command-output");
    if (output) output.value = JSON.stringify(value);
    const status = document.getElementById("command-status");
    if (status) {
      const blocker = value?.blocker?.message;
      const label = blocker
        || value?.result?.message
        || (value?.status === "blocked" ? "That action could not be completed." : null);
      if (!label) {
        status.classList.add("is-hidden");
        return;
      }
      status.textContent = String(label).replaceAll("_", " ");
      status.classList.toggle("blocked", Boolean(blocker) || value?.status === "blocked");
      status.classList.remove("is-hidden");
      window.clearTimeout(status.hideTimer);
      status.hideTimer = window.setTimeout(() => status.classList.add("is-hidden"), 5000);
    }
  };

  const reportError = (error) => {
    const message = error && error.message ? error.message : String(error);
    report({ status: "blocked", blocker: { code: "view_command_failed", message } });
    if (!document.getElementById("command-status")) {
      const root = document.getElementById("app");
      if (root) {
        const state = document.createElement("section");
        state.className = "boot boot-error";
        const heading = document.createElement("h1");
        heading.textContent = "Mom Llama could not open";
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

  const swap = async (selector, command) => {
    const current = document.querySelector(selector);
    if (!current) return null;
    const replacement = parseFragment(await invokeMarkup(command));
    if (!replacement) throw new Error(`Renderer ${command} returned no element.`);
    if (activeSpeechPlayback?.panel && current.contains(activeSpeechPlayback.panel)) {
      await stopSpeechPlayback();
    }
    releaseAttachmentObjectUrls(current);
    current.replaceWith(replacement);
    return replacement;
  };

  const rawAttachmentBytes = (response) => {
    if (response instanceof ArrayBuffer) return new Uint8Array(response);
    if (ArrayBuffer.isView(response)) {
      return new Uint8Array(response.buffer, response.byteOffset, response.byteLength);
    }
    throw new Error("Attachment preview returned serialized JSON instead of raw IPC bytes.");
  };

  const speechOperationToken = () => {
    const value = globalThis.crypto?.randomUUID?.();
    if (!value) throw new Error("Secure opaque speech operation IDs are unavailable.");
    return value;
  };
  const sha256Hex = async (bytes) => {
    const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
    return [...new Uint8Array(digest)]
      .map((byte) => byte.toString(16).padStart(2, "0"))
      .join("");
  };
  const attachmentTranscripts = new WeakMap();
  let activeSpeechPlayback = null;

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

  const stopSpeechPlayback = async ({ notifyNative = true } = {}) => {
    const playback = activeSpeechPlayback;
    activeSpeechPlayback = null;
    if (!playback) return;
    playback.audio.pause();
    playback.audio.removeAttribute("src");
    playback.audio.load();
    URL.revokeObjectURL(playback.url);
    playback.panel.remove();
    restoreReadAloudButton(playback.button);
    if (notifyNative) {
      await invoke("mom_llama_speech_stop", { playback: playback.id }).catch(reportError);
    }
  };

  const renderSpeechPlayback = async (button, result, bytes) => {
    const binding = result.binding;
    if (await sha256Hex(bytes) !== binding.wav_sha256) {
      throw new Error("speech_provenance_mismatch: Complete WAV bytes changed at IPC.");
    }
    await stopSpeechPlayback();
    const row = button.closest(".message-row");
    if (
      !row?.isConnected
      || row.dataset.messageId !== binding.message_id
      || selectedConversation() !== binding.conversation_id
    ) {
      await invoke("mom_llama_speech_stop", { playback: result.playback_id });
      return;
    }
    const panel = document.createElement("section");
    panel.className = "speech-playback";
    panel.dataset.playback = result.playback_id;
    const audio = createCommandElement("audio", DYNAMIC_CONTROL_SPECS.readAloudStop);
    audio.controls = true;
    audio.autoplay = false;
    audio.preload = "auto";
    const url = URL.createObjectURL(new Blob([bytes], { type: "audio/wav" }));
    audio.src = url;
    const progress = document.createElement("output");
    progress.className = "speech-playback-progress";
    progress.textContent = "Ready";
    const notice = document.createElement("small");
    notice.textContent = "Stop suppresses playback immediately. Apple’s inner synthesis call is non-preemptive and Quit joins it before exit.";
    panel.append(audio, progress, notice);
    row.querySelector(".message-card")?.append(panel);
    commandMetadata(button, DYNAMIC_CONTROL_SPECS.readAloudStop);
    button.title = "Stop reading";
    button.setAttribute("aria-pressed", "true");
    button.querySelector(".sr-only")?.replaceChildren("Stop reading");
    activeSpeechPlayback = {
      id: result.playback_id,
      audio,
      url,
      panel,
      button,
    };
    audio.addEventListener("timeupdate", () => {
      const current = Number.isFinite(audio.currentTime) ? audio.currentTime : 0;
      const duration = Number.isFinite(audio.duration) ? audio.duration : 0;
      progress.textContent = `${current.toFixed(1)} / ${duration.toFixed(1)} seconds`;
    });
    audio.addEventListener("ended", () => void stopSpeechPlayback());
    await audio.play().catch(() => {});
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
      const bytes = rawAttachmentBytes(response);
      const url = URL.createObjectURL(new Blob([bytes], { type: primary.media_type }));
      const media = primary.kind === "image"
        ? document.createElement("img")
        : primary.kind === "audio"
          ? createCommandElement("audio", DYNAMIC_CONTROL_SPECS.attachmentPreview)
          : createCommandElement("video", DYNAMIC_CONTROL_SPECS.attachmentPreview);
      media.src = url;
      if (primary.kind === "image") {
        media.alt = preview.querySelector("figcaption strong")?.textContent || "Local attachment";
      } else {
        media.controls = true;
        media.autoplay = false;
        media.preload = "metadata";
      }
      body.replaceChildren(media);
      appendAttachmentPreviewNotices(body, catalog.notices);
      appendAttachmentTranscriptionControl(preview, catalog, primary);
      attachmentPreviewMedia.set(preview, media);
      attachmentObjectUrls.set(media, { preview, url });
      preview.dataset.previewHydrated = "true";
      enforceAttachmentPreviewLimit();
    } catch (error) {
      if (!preview.isConnected || preview.dataset.previewReleased === "true") return;
      preview.dataset.previewHydrated = "blocked";
      preview.title = errorMessage(error);
      attachmentPreviewObserver?.unobserve(preview);
    }
  };

  const drainAttachmentPreviewQueue = () => {
    while (
      attachmentPreviewLoads < ATTACHMENT_PREVIEW_CONCURRENCY
      && attachmentPreviewQueue.length
    ) {
      const preview = attachmentPreviewQueue.shift();
      if (!preview?.isConnected || preview.dataset.previewReleased === "true") continue;
      attachmentPreviewLoads += 1;
      loadAttachmentPreview(preview)
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
    attachmentPreviewsWithin(root).forEach((preview) => {
      delete preview.dataset.previewReleased;
      rememberAttachmentPreviewFallback(preview);
      if (attachmentPreviewObserver) {
        attachmentPreviewObserver.observe(preview);
      } else {
        queueAttachmentPreview(preview);
      }
    });
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
    if (replacement) await hydrateAttachmentPreviews(replacement);
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
    applySidebarSectionState(replacement);
    return replacement;
  };
  const refreshSettings = async (section = "general") => {
    const wasOpen = !document.getElementById("settings-modal")?.hidden;
    const modal = await swap("#settings-modal", "mom_llama_render_settings_fragment");
    refreshAutosaveStatus();
    if (wasOpen && modal) {
      modal.hidden = false;
      modal.classList.remove("is-hidden");
      modal.setAttribute("aria-hidden", "false");
      switchSettingsSection(section);
    }
    return modal;
  };

  const refreshCacheInspector = async (action, result) => {
    report(result);
    await refreshSettings("developer");
    const status = document.getElementById("cache-action-status");
    const inspector = window.MomLlamaCacheInspector;
    if (!status || !inspector) return result;
    status.textContent = inspector.actionMessage(action, result);
    status.dataset.state = inspector.actionState(result);
    return result;
  };

  const refreshConversationProjection = async () => {
    await Promise.all([refreshChat(), refreshSidebar()]);
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
    // Empty strings are explicit clears; omitted or null values mean unchanged.
    modelPath: formValue(form, "model_path"),
    mmprojPath: formValue(form, "mmproj_path"),
    device: formValue(form, "native_device") || null,
    contextTokens: numberOrNull(formValue(form, "context_tokens")),
    batchTokens: numberOrNull(formValue(form, "batch_tokens")),
    maxParallelSequences: numberOrNull(formValue(form, "max_parallel_sequences")),
    memoryBudgetMib: numberOrNull(formValue(form, "memory_budget_mib")),
    temperature: numberOrNull(formValue(form, "temperature")),
    topP: numberOrNull(formValue(form, "top_p")),
    maxTokens: numberOrNull(formValue(form, "max_tokens")),
    kvCachePolicy: formValue(form, "kv_cache_policy") || null,
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
    const modal = field.closest("#settings-modal");
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
    const modal = document.getElementById("settings-modal");
    if (!modal) return;
    modal.hidden = false;
    modal.classList.remove("is-hidden");
    modal.setAttribute("aria-hidden", "false");
    switchSettingsSection(section);
  };

  const closeSettings = () => {
    const modal = document.getElementById("settings-modal");
    if (!modal) return;
    modal.hidden = true;
    modal.classList.add("is-hidden");
    modal.setAttribute("aria-hidden", "true");
  };

  const setModalVisibility = (id, visible) => {
    const modal = document.getElementById(id);
    if (!modal) return;
    modal.hidden = !visible;
    modal.classList.toggle("is-hidden", !visible);
    modal.setAttribute("aria-hidden", visible ? "false" : "true");
  };

  const slugHandle = (value) => String(value || "")
    .trim().toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "");

  const openPersonaFreeze = (button) => {
    const modal = document.getElementById("persona-freeze-modal");
    if (!modal) return;
    formField(modal, "freeze_message").value = button.dataset.message || "";
    formField(modal, "freeze_name").value = "";
    formField(modal, "freeze_handle").value = "";
    setModalVisibility("persona-freeze-modal", true);
    formField(modal, "freeze_name")?.focus();
  };

  const personaTools = (value) => {
    const bindings = String(value || "").split(/\r?\n/)
      .map((line) => line.trim()).filter(Boolean).map((line) => {
        const [server, ...tool] = line.split("/");
        return { server: server.trim(), tool: tool.join("/").trim() };
      }).filter((binding) => binding.server && binding.tool);
    if (bindings.length > MAX_PERSONA_TOOL_BINDINGS) {
      throw new Error(`A Persona may attach at most ${MAX_PERSONA_TOOL_BINDINGS} tools.`);
    }
    return bindings;
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
    formField(editor, "persona_model_path").value = profile.model_path || "";
    formField(editor, "persona_mmproj_path").value = profile.mmproj_path || "";
    formField(editor, "persona_system_message").value = profile.system_message || "";
    formField(editor, "persona_source_tokens").value = profile.source_history_tokens ?? 4096;
    formField(editor, "persona_host_tokens").value = profile.host_context_tokens ?? 2048;
    const toolsField = formField(editor, "persona_tools");
    if (toolsField) {
      toolsField.value = (profile.tool_bindings || [])
        .map((binding) => `${binding.server}/${binding.tool}`).join("\n");
    }
    const frozen = profile.chat_template && typeof profile.chat_template === "object"
      ? profile.chat_template.frozen_source : null;
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

  const persistCurrentDraftBeforeNavigation = async () => {
    const conversation = selectedConversation();
    const form = document.getElementById("chat-form");
    const draft = {
      conversation,
      message: formValue(form, "message"),
      attachmentIds: draftAttachmentIds(form),
    };
    const persisted = await persistDraftNow(draft.message, draft.attachmentIds, conversation);
    report(persisted);
    return persisted?.status === "blocked" ? null : draft;
  };

  const seedNewChatDraft = async (conversation, draft, prefix = "") => {
    const transferLandingDraft = draft.conversation === "default";
    if (!transferLandingDraft && !prefix) return true;
    const seeded = await invoke("mom_llama_draft_update", {
      conversation,
      message: transferLandingDraft ? `${prefix}${draft.message}` : prefix,
      attachmentIds: transferLandingDraft ? draft.attachmentIds : [],
    });
    report(seeded);
    if (seeded?.status === "blocked") return false;
    if (transferLandingDraft) {
      const cleared = await invoke("mom_llama_draft_update", {
        conversation: draft.conversation,
        message: "",
        attachmentIds: [],
      });
      report(cleared);
    }
    return true;
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
    const template = formValue(editor, "persona_chat_template_policy") === "frozen_source"
      ? { frozen_source: formValue(editor, "persona_chat_template") }
      : "model_default";
    return {
      persona_id: formValue(editor, "persona_id"),
      name: formValue(editor, "persona_name"),
      mention_handle: formValue(editor, "persona_handle"),
      model_path: formValue(editor, "persona_model_path") || null,
      mmproj_path: formValue(editor, "persona_mmproj_path") || null,
      system_message: formValue(editor, "persona_system_message") || null,
      sampling: profile.sampling || null,
      chat_template: template,
      tool_bindings: mcpProcessUiSupported()
        ? personaTools(formValue(editor, "persona_tools"))
        : (profile.tool_bindings || []),
      source_history_tokens: Math.max(0, Math.trunc(Number(formValue(editor, "persona_source_tokens") || 4096))),
      host_context_tokens: Math.max(0, Math.trunc(Number(formValue(editor, "persona_host_tokens") || 2048))),
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
    const modal = document.getElementById("settings-modal");
    if (modal) modal.dataset.activeSection = section;
    document.querySelectorAll(".section-tab[data-section]").forEach((tab) => {
      const active = tab.dataset.section === section;
      tab.classList.toggle("active", active);
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
    if (enabled) form?.querySelector("input[name='query']")?.focus();
  };

  const commandMetadata = (element, spec) => {
    element.dataset.affordance = spec.affordance;
    element.dataset.command = spec.command;
    element.dataset.tauriCommand = spec.tauri;
    element.dataset.cli = spec.cli;
    element.dataset.effect = spec.effect;
  };

  const DYNAMIC_CONTROL_SPECS = Object.freeze({
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
      const button = createCommandElement("button", DYNAMIC_CONTROL_SPECS.conversationSelect);
      button.type = "button";
      button.className = "conversation-item search-hit";
      button.dataset.action = "conversation-select";
      button.dataset.conversation = hit.conversation_id;
      const title = document.createElement("span");
      title.textContent = hit.title || hit.conversation_id;
      const detail = document.createElement("small");
      detail.textContent = hit.snippet || `${hit.message_count || 0} messages`;
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
        if (response?.status !== "passed") {
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
      if (acceptance?.status !== "passed") {
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

  const appendLiveMessage = (role, content, id) => {
    const stream = ensureMessageStream();
    if (!stream) return null;
    const article = document.createElement("article");
    article.id = id;
    article.className = `message-row ${role}`;
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
    visibleContent.textContent = content;
    card.appendChild(visibleContent);
    article.appendChild(card);
    stream.appendChild(article);
    if (!settingEnabled("disableAutoScroll")) {
      stream.dataset.followTail = "true";
      stream.scrollTop = stream.scrollHeight;
    }
    return card;
  };

  const keepLiveTailVisible = (element) => {
    if (settingEnabled("disableAutoScroll")) return;
    const stream = element?.closest(".message-stream");
    if (!stream || stream.dataset.followTail === "false") return;
    stream.scrollTop = stream.scrollHeight;
  };

  const chatBusyLeases = new Set();
  let chatBusyLeaseSerial = 0;

  const renderChatBusyState = () => {
    const busy = chatBusyLeases.size > 0;
    const form = document.getElementById("chat-form");
    if (!form) return;
    const send = form.querySelector("button[type='submit']");
    const stop = form.querySelector(".stop-button");
    const skipReasoning = form.querySelector(".skip-reasoning-button");
    const message = formField(form, "message");
    const attachmentImport = form.querySelector("[data-action='attachment-import']");
    const attachmentRemovers = form.querySelectorAll("[data-action='draft-attachment-remove']");
    if (message) message.readOnly = busy;
    if (attachmentImport) attachmentImport.disabled = busy;
    attachmentRemovers.forEach((button) => { button.disabled = busy; });
    if (send) {
      send.disabled = busy;
      send.classList.toggle("is-hidden", busy);
    }
    if (stop) {
      stop.disabled = !busy;
      stop.classList.toggle("is-hidden", !busy);
    }
    if (skipReasoning && !busy) {
      skipReasoning.disabled = true;
      skipReasoning.classList.add("is-hidden");
    }
    form.dataset.busy = busy ? "true" : "false";
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
    renderChatBusyState();
    return lease;
  };

  const releaseChatBusy = (lease) => {
    if (!lease) return;
    chatBusyLeases.delete(lease);
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

  const chatRequestLease = (payload) => payload.request_id
    ? `chat-request:${payload.request_id}`
    : null;

  const onChatEvent = (event) => {
    const payload = event.payload || event;
    if (payload.event === "started") {
      acquireStableChatBusy(chatRequestLease(payload));
      appendLiveMessage("assistant", "", `live-assistant-${payload.request_id}`);
    }
    if (payload.event === "delta") {
      const content = document.querySelector(`#live-assistant-${CSS.escape(payload.request_id)} .live-content`);
      if (content) {
        content.textContent += payload.delta || "";
        keepLiveTailVisible(content);
      }
    }
    if (payload.event === "reasoning_delta") {
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
      releaseChatBusy(chatRequestLease(payload));
    }
  };

  const mentionLiveId = (payload) => `live-mention-${payload.invocation_id}-${payload.target_id}`;
  const appendMentionMessage = (payload) => {
    const card = appendLiveMessage("assistant", "", mentionLiveId(payload));
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
    if (envelope.kind === "chat") {
      onChatEvent({ payload: envelope.event });
      return;
    }
    const payload = envelope.event || envelope;
    const lease = payload.invocation_id && payload.target_id
      ? `mention-request:${payload.invocation_id}:${payload.target_id}`
      : null;
    if (payload.event === "started") {
      acquireStableChatBusy(lease);
      appendMentionMessage(payload);
    }
    const row = document.getElementById(mentionLiveId(payload));
    if (payload.event === "delta") {
      const content = row?.querySelector(".live-content");
      if (content) {
        content.textContent += payload.delta || "";
        keepLiveTailVisible(content);
      }
    }
    const terminal = payload.state && ["completed", "cancelled", "failed"].includes(payload.state);
    if (["completed", "cancelled", "failed"].includes(payload.event) || terminal) {
      releaseChatBusy(lease);
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

  const setSkillForm = (skill = null) => {
    const form = document.getElementById("skill-form");
    if (!form) return;
    formField(form, "skill_id").value = skill?.id || "";
    formField(form, "name").value = skill?.name || "";
    formField(form, "description").value = skill?.description || "";
    formField(form, "prompt_template").value = skill?.prompt || "";
    formField(form, "cache_policy").value = skill?.cache || "none";
    const submit = form.querySelector('[data-action="skill-create"]');
    setButtonStateLabel(submit, skill ? "Save changes" : "Save Skill");
    form.querySelector('[data-action="skill-edit-cancel"]')?.classList.toggle("is-hidden", !skill);
    if (skill) formField(form, "name")?.focus();
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

  const actionHandlers = {
    "sidebar-toggle": async () => {
      await invoke("mom_llama_conversation_list");
      shell()?.classList.toggle("sidebar-open");
    },
    "sidebar-section-toggle": async (button) => {
      const section = button.dataset.sidebarSection;
      const list = document.getElementById(button.getAttribute("aria-controls"));
      if (!section || !list) return;
      const command = {
        conversations: "mom_llama_conversation_list",
        personas: "mom_llama_persona_list",
        "consult-groups": "mom_llama_persona_group_list",
      }[section];
      const listed = command ? await invoke(command) : null;
      if (listed?.status === "blocked") {
        report(listed);
        return;
      }
      const expanded = button.getAttribute("aria-expanded") !== "true";
      button.setAttribute("aria-expanded", String(expanded));
      button.setAttribute(
        "aria-label",
        `${expanded ? "Collapse" : "Expand"} ${button.dataset.sidebarLabel || section}`,
      );
      list.hidden = !expanded;
      if (expanded) collapsedSidebarSections.delete(section);
      else collapsedSidebarSections.add(section);
    },
    "sidebar-persona-start": async (button) => {
      const draft = await persistCurrentDraftBeforeNavigation();
      if (!draft) return;
      const result = await instantiatePersona(button.dataset.persona);
      if (result?.status === "blocked" || !result?.result?.id) return;
      try {
        await seedNewChatDraft(result.result.id, draft);
      } catch (error) {
        await refreshConversationProjection().catch(reportError);
        throw error;
      }
      await refreshConversationProjection();
      focusComposer();
    },
    "sidebar-consult-group-start": async (button) => {
      const listed = await invoke("mom_llama_persona_group_list");
      const group = (listed?.result || []).find((candidate) => (
        candidate.id === button.dataset.group
        && String(candidate.mention_handle || "").toLowerCase()
          === String(button.dataset.handle || "").toLowerCase()
      ));
      if (listed?.status === "blocked" || !group) {
        report(listed?.status === "blocked" ? listed : {
          status: "blocked",
          blocker: {
            code: "persona_group_not_found",
            message: "That consult group is no longer available.",
          },
        });
        await refreshSidebar();
        return;
      }
      const draft = await persistCurrentDraftBeforeNavigation();
      if (!draft) return;
      const created = await invoke("mom_llama_conversation_new", { title: group.name });
      report(created);
      if (created?.status === "blocked" || !created?.result?.id) return;
      try {
        await seedNewChatDraft(created.result.id, draft, `@${group.mention_handle} `);
      } catch (error) {
        await refreshConversationProjection().catch(reportError);
        throw error;
      }
      await refreshConversationProjection();
      focusComposer();
    },
    "settings-open": async () => { await invoke("mom_llama_settings_get"); openSettings(); },
    "settings-close": async () => { await invoke("mom_llama_settings_get"); closeSettings(); },
    "settings-section": async (button) => switchSettingsSection(button.dataset.section || "general"),
    "skills-open": async () => { await invoke("mom_llama_skill_list"); openSettings("general"); },
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
      const result = await invoke("mom_llama_conversation_new", { title: "New chat" });
      report(result); await refreshConversationProjection();
    },
    "conversation-list": async () => refreshConversationProjection(),
    "conversation-search-open": async () => { setSearchMode(true); await search(); },
    "conversation-search-close": async () => setSearchMode(false),
    "conversation-select": async (button) => {
      const result = await invoke("mom_llama_conversation_select", { conversation: button.dataset.conversation });
      report(result); await refreshConversationProjection();
    },
    "chat-cancel": async () => report(await invoke("mom_llama_chat_cancel", { conversation: selectedConversation() })),
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
        await stopSpeechPlayback();
        return;
      }
      if (button.dataset.playback) {
        const playback = button.dataset.playback;
        delete button.dataset.playback;
        await invoke("mom_llama_speech_stop", { playback });
        restoreReadAloudButton(button);
        return;
      }
      if (button.dataset.operation) {
        const operation = button.dataset.operation;
        delete button.dataset.operation;
        await invoke("mom_llama_speech_stop", { operation });
        restoreReadAloudButton(button);
        return;
      }
      await stopSpeechPlayback();
      const conversation = selectedConversation();
      const message = button.dataset.message;
      const operation = speechOperationToken();
      button.dataset.operation = operation;
      commandMetadata(button, DYNAMIC_CONTROL_SPECS.readAloudStop);
      button.title = "Stop reading";
      button.setAttribute("aria-pressed", "true");
      button.querySelector(".sr-only")?.replaceChildren("Stop reading");
      let retainedPlayback = null;
      try {
        const response = await invoke("mom_llama_speech_read_aloud", {
          conversation,
          message,
          operation,
        });
        report(response);
        if (response?.status === "blocked" || !response?.result) return;
        const result = response.result;
        retainedPlayback = result.playback_id;
        if (button.dataset.operation === operation) delete button.dataset.operation;
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
        const bytes = rawAttachmentBytes(await invoke("mom_llama_speech_audio", {
          playback: result.playback_id,
          message: result.binding.message_id,
          textSha256: result.binding.text_sha256,
          backendDescriptorSha256: result.binding.backend_descriptor_sha256,
        }));
        if (button.dataset.playback !== result.playback_id) return;
        await renderSpeechPlayback(button, result, bytes);
        if (activeSpeechPlayback?.id === result.playback_id) retainedPlayback = null;
      } finally {
        if (retainedPlayback) {
          await invoke("mom_llama_speech_stop", { playback: retainedPlayback }).catch(() => {});
        }
        if (button.dataset.operation === operation) delete button.dataset.operation;
        if (activeSpeechPlayback?.button !== button) restoreReadAloudButton(button);
      }
    },
    "attachment-transcribe": async (button) => {
      if (button.dataset.operation) {
        const operation = button.dataset.operation;
        delete button.dataset.operation;
        await invoke("mom_llama_speech_stop", { operation });
        commandMetadata(button, DYNAMIC_CONTROL_SPECS.attachmentTranscribe);
        button.textContent = "Transcribe audio";
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
        if (button.dataset.operation === operation) delete button.dataset.operation;
        commandMetadata(button, DYNAMIC_CONTROL_SPECS.attachmentTranscribe);
        button.textContent = "Transcribe audio";
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
        conversation: selectedConversation(),
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
    "persona-menu-start": async () => {
      const persona = document.getElementById("persona-context-menu")?.dataset.persona;
      closePersonaMenu(false);
      const result = await instantiatePersona(persona);
      if (result?.status !== "blocked") {
        closeSettings();
        await refreshConversationProjection();
      }
    },
    "persona-menu-edit": async () => {
      const persona = document.getElementById("persona-context-menu")?.dataset.persona;
      closePersonaMenu(false);
      await openPersonaProfile(persona);
    },
    "persona-menu-removal-preview": async () => {
      const persona = document.getElementById("persona-context-menu")?.dataset.persona;
      await openPersonaRemoval(persona);
    },
    "persona-removal-close": async () => setModalVisibility("persona-removal-modal", false),
    "persona-removal-commit": async () => commitPersonaRemoval(),
    "persona-instantiate": async (button) => {
      const result = await instantiatePersona(button.dataset.persona);
      if (result?.status !== "blocked") {
        closeSettings();
        await refreshConversationProjection();
      }
    },
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
      if (selectedConversationKind() === "persona_template") {
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
      await persistDraftNow(message, [], conversation);
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
      report(result);
      await refreshChat();
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
    "engine-check": async () => { report(await invoke("mom_llama_engine_check")); await refreshSettings("general"); },
    "model-list": async () => report(await invoke("mom_llama_model_list")),
    "model-select": async (button) => {
      const form = document.getElementById("settings-form");
      const path = button.dataset.modelPath || formValue(form, "model_path");
      report(await invoke("mom_llama_model_select", { modelPath: path }));
      await Promise.all([refreshChat(), refreshSettings("general")]);
    },
    "model-browse": async () => {
      const path = await pickFile("model");
      if (path) {
        const field = formField(document.getElementById("settings-form"), "model_path");
        field.value = path;
        field.dispatchEvent(new Event("change", { bubbles: true }));
      }
    },
    "mmproj-browse": async () => {
      const path = await pickFile("mmproj");
      if (path) {
        const field = formField(document.getElementById("settings-form"), "mmproj_path");
        field.value = path;
        field.dispatchEvent(new Event("change", { bubbles: true }));
      }
    },
    "persona-model-browse": async () => {
      const path = await pickFile("model");
      if (path) formField(document.getElementById("persona-editor"), "persona_model_path").value = path;
    },
    "persona-mmproj-browse": async () => {
      const path = await pickFile("mmproj");
      if (path) formField(document.getElementById("persona-editor"), "persona_mmproj_path").value = path;
    },
    "skill-create": async () => document.getElementById("skill-form")?.requestSubmit(),
    "skill-edit": async (button) => setSkillForm({
      id: button.dataset.skill,
      name: button.dataset.skillName,
      description: button.dataset.skillDescription,
      prompt: button.dataset.skillPrompt,
      cache: button.dataset.skillCache,
    }),
    "skill-edit-cancel": async () => setSkillForm(),
    "skill-apply": async (button) => {
      report(await invoke("mom_llama_skill_apply", { conversation: selectedConversation(), skill: button.dataset.skill }));
      await refreshSettings("general");
    },
    "kv-status": async () => refreshCacheInspector(
      "refresh",
      await invoke("mom_llama_kv_cache_status"),
    ),
    "kv-clear": async (button) => {
      if (!armDestructiveAction(button)) return;
      await refreshCacheInspector("clear", await invoke("mom_llama_kv_cache_clear"));
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
    "resident-model-browse": async () => {
      const path = await pickFile("model");
      if (path) formField(document.getElementById("settings-form"), "resident_model_path").value = path;
    },
    "resident-slots": async () => report(await invoke("mom_llama_model_slot_list")),
    "resident-slot-load": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_model_slot_load", {
        slot: numberOrNull(formValue(form, "resident_slot")) || 0,
        modelPath: formValue(form, "resident_model_path"),
      }));
      await Promise.all([refreshChat(), refreshSettings("developer")]);
    },
    "resident-slot-unload": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_model_slot_unload", {
        slot: numberOrNull(formValue(form, "resident_slot")) || 0,
      }));
      await Promise.all([refreshChat(), refreshSettings("developer")]);
    },
  };

  document.addEventListener("click", async (event) => {
    if (!event.target.closest("#persona-context-menu, .persona-menu-trigger")) {
      closePersonaMenu(false);
    }
    const button = event.target.closest("[data-action]");
    if (!button || button.disabled) return;
    if (MCP_PROCESS_ACTIONS.has(button.dataset.action) && !requireMcpProcessUi()) return;
    const handler = actionHandlers[button.dataset.action];
    if (!handler) return;
    event.preventDefault();
    try { await handler(button); } catch (error) { reportError(error); }
  });

  document.addEventListener("contextmenu", (event) => {
    const target = event.target.closest('[data-persona-menu-target="true"]');
    if (!target) return;
    event.preventDefault();
    openPersonaMenu(target, { x: event.clientX, y: event.clientY });
  });

  document.addEventListener("submit", async (event) => {
    event.preventDefault();
    const form = event.target;
    try {
      if (form.id === "chat-form") {
        if (autocompleteAccepting) return;
        if (form.dataset.busy === "true") return;
        cancelComposerAutocomplete();
        const message = formValue(form, "message");
        const attachmentIds = draftAttachmentIds(form);
        if (!message && !attachmentIds.length) return;
        // Acquire the UI-side dispatch gate before the first await. Without
        // this, rapid Enter presses can race persona instantiation or draft
        // persistence and dispatch the same turn more than once.
        const dispatchLease = acquireChatBusy("chat-dispatch");
        const sourceConversation = selectedConversation();
        let conversation = sourceConversation;
        const textarea = formField(form, "message");
        try {
          if (sourceConversation === "default") {
            const created = await invoke("mom_llama_conversation_new", { title: "New chat" });
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
          await persistDraftNow(textarea?.value || message, attachmentIds, conversation);
          if (conversation !== sourceConversation) {
            await invoke("mom_llama_draft_update", {
              conversation: sourceConversation,
              message: "",
              attachmentIds: [],
            });
          }
          if (textarea) textarea.value = "";
          if (message) appendLiveMessage("user", message, `live-user-${Date.now()}`);
          closeMentions();
          let result;
          try {
            result = await invoke("mom_llama_chat_dispatch", { conversation, message });
          } catch (error) {
            if (textarea && !textarea.value) textarea.value = message;
            await persistDraftNow(message, attachmentIds).catch(reportError);
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
          if (result?.status === "blocked" && textarea && !textarea.value) {
            textarea.value = message;
            await persistDraftNow(message, attachmentIds);
          }
          await refreshConversationProjection();
        } finally {
          releaseChatBusy(dispatchLease);
        }
      }
      if (form.id === "settings-form") {
        scheduleSettingsAutosave(0);
      }
      if (form.id === "skill-form") {
        const skill = formValue(form, "skill_id");
        const payload = {
          name: formValue(form, "name"),
          description: formValue(form, "description"),
          promptTemplate: formValue(form, "prompt_template"),
          usageHint: "Use this perspective when it helps the current conversation.",
          cachePolicy: formValue(form, "cache_policy") || "none",
        };
        const result = skill
          ? await invoke("mom_llama_skill_update", { skill, ...payload })
          : await invoke("mom_llama_skill_create", payload);
        report(result);
        setSkillForm();
        await refreshSettings("general");
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
    if (event.target.matches("#chat-form textarea[name='message']")) {
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
    if (event.target.matches("#settings-form [data-setting-core], #settings-form [data-setting-key]")) {
      scheduleSettingsAutosave(0);
      return;
    }
    if (event.target.matches("#settings-form [data-chat-setting='system_message']")) {
      scheduleChatInstructionsAutosave(event.target, 0);
      return;
    }
  });

  const listen = async () => {
    const events = tauri() && tauri().event;
    if (!events || typeof events.listen !== "function") return;
    await events.listen("mom_llama_chat_stream", onChatEvent);
    await events.listen("mom_llama_chat_dispatch_stream", onDispatchEvent);
    await events.listen("mom_llama_tool_loop_stream", onToolLoopEvent);
    await events.listen("mom_llama_speech_quiescing", () => {
      void stopSpeechPlayback({ notifyNative: false });
    });
  };

  const boot = async () => {
    const status = document.getElementById("startup-status");
    const retry = document.getElementById("startup-retry");
    const initialize = async () => {
      if (status) {
        status.classList.remove("startup-error");
        status.textContent = "Unlocking Mom Llama's encrypted local data…";
      }
      if (retry) {
        retry.hidden = true;
        retry.disabled = true;
      }
      try {
        await invoke("mom_llama_runtime_initialize");
        const root = document.getElementById("app");
        root.innerHTML = await invokeMarkup("mom_llama_render_app");
        applyCustomCss(shell()?.dataset.customCss || "");
        await hydrateAttachmentPreviews(root);
        restoreChatViewport(null, chat());
        const alwaysShowSidebar = shell()?.dataset.alwaysShowSidebar === "true";
        if (window.innerWidth >= 1180 && alwaysShowSidebar) shell()?.classList.add("sidebar-open");
        await listen();
      } catch (error) {
        if (status) {
          status.classList.add("startup-error");
          status.textContent = `${String(error)} Approve Keychain access, then retry.`;
        }
        if (retry) {
          retry.hidden = false;
          retry.disabled = false;
        }
      }
    };
    retry?.addEventListener("click", initialize);
    await initialize();
  };

  boot();
})();

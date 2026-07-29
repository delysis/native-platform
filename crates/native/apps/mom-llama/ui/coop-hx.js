(() => {
  "use strict";

  const tauri = () => window.__TAURI__;
  const invoke = (command, payload = {}) => {
    const core = tauri() && tauri().core;
    if (!core || typeof core.invoke !== "function") {
      return Promise.reject(new Error("Tauri IPC is unavailable."));
    }
    return core.invoke(command, payload);
  };

  const shell = () => document.querySelector(".llama-ui-shell");
  const chat = () => document.getElementById("chat");
  const consult = () => document.getElementById("consult-view");
  const selectedConversation = () =>
    (chat() && chat().dataset.currentConversation) || "default";
  const settingEnabled = (key, fallback = false) => {
    const field = document.querySelector(`[data-setting-key="${key}"]`);
    return field ? Boolean(field.checked) : fallback;
  };
  const applyTheme = (theme) => {
    const root = shell();
    if (root) root.dataset.theme = theme || "system";
  };

  const report = (value) => {
    const output = document.getElementById("command-output");
    if (output) output.value = JSON.stringify(value);
    const status = document.getElementById("command-status");
    if (status) {
      const blocker = value?.blocker?.message;
      const label = blocker || value?.result?.message || value?.readiness || value?.status || "Done";
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
    console.error(message);
  };

  const parseFragment = (markup) => {
    const template = document.createElement("template");
    template.innerHTML = markup.trim();
    return template.content.firstElementChild;
  };

  const swap = async (selector, command) => {
    const current = document.querySelector(selector);
    if (!current) return null;
    const replacement = parseFragment(await invoke(command));
    if (!replacement) throw new Error(`Renderer ${command} returned no element.`);
    current.replaceWith(replacement);
    return replacement;
  };

  const refreshChat = () => swap("#chat", "mom_llama_render_chat_fragment");
  const refreshSidebar = () => swap(".sidebar", "mom_llama_render_sidebar_fragment");
  const refreshSettings = async (section = "general") => {
    const wasOpen = !document.getElementById("settings-modal")?.hidden;
    const modal = await swap("#settings-modal", "mom_llama_render_settings_fragment");
    if (wasOpen && modal) {
      modal.hidden = false;
      modal.classList.remove("is-hidden");
      modal.setAttribute("aria-hidden", "false");
      switchSettingsSection(section);
    }
    return modal;
  };

  const refreshConversationProjection = async () => {
    await Promise.all([refreshChat(), refreshSidebar()]);
  };

  const formField = (form, name) => form && form.elements.namedItem(name);
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
  const pickFile = (kind) => invoke("mom_llama_pick_file", { kind });

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

  const switchSettingsSection = (section) => {
    const title = document.getElementById("settings-section-title");
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
      const button = document.createElement("button");
      button.type = "button";
      button.className = "conversation-item search-hit";
      button.dataset.action = "conversation-select";
      button.dataset.conversation = hit.conversation_id;
      commandMetadata(button, {
        affordance: "conversation.select",
        command: "mom_llama.conversation_select",
        tauri: "mom_llama_conversation_select",
        cli: "mom-llama conversation select --conversation <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
      });
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
  const scheduleDraft = (message) => {
    window.clearTimeout(draftTimer);
    const conversation = selectedConversation();
    draftTimer = window.setTimeout(() => {
      invoke("mom_llama_draft_update", {
        conversation,
        message,
        attachmentIds: [],
      }).catch(reportError);
    }, 300);
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
    card.textContent = content;
    article.appendChild(card);
    stream.appendChild(article);
    if (!settingEnabled("disableAutoScroll")) stream.scrollTop = stream.scrollHeight;
    return card;
  };

  const setChatBusy = (busy) => {
    const form = document.getElementById("chat-form");
    if (!form) return;
    const send = form.querySelector("button[type='submit']");
    const stop = form.querySelector(".stop-button");
    if (send) {
      send.disabled = busy;
      send.classList.toggle("is-hidden", busy);
    }
    if (stop) {
      stop.disabled = !busy;
      stop.classList.toggle("is-hidden", !busy);
    }
    form.dataset.busy = busy ? "true" : "false";
  };

  const onChatEvent = (event) => {
    const payload = event.payload || event;
    if (payload.event === "started") {
      setChatBusy(true);
      appendLiveMessage("assistant", "", `live-assistant-${payload.request_id}`);
    }
    if (payload.event === "delta") {
      const card = document.querySelector(`#live-assistant-${CSS.escape(payload.request_id)} .message-card`);
      if (card) card.textContent += payload.delta || "";
    }
    if (["completed", "cancelled", "warning"].includes(payload.event)) {
      if (payload.event !== "warning") setChatBusy(false);
    }
  };

  const openConsult = () => {
    chat()?.classList.add("is-hidden");
    const view = consult();
    if (view) {
      view.classList.remove("is-hidden");
      view.querySelector("textarea")?.focus();
    }
  };

  const closeConsult = () => {
    consult()?.classList.add("is-hidden");
    chat()?.classList.remove("is-hidden");
  };

  const resetConsultSeats = () => {
    document.querySelectorAll(".consult-seat").forEach((seat) => {
      seat.dataset.state = "generating";
      const status = seat.querySelector(".seat-state");
      const output = seat.querySelector(".seat-output");
      const stop = seat.querySelector(".seat-stop");
      if (status) status.textContent = "Starting";
      if (output) output.replaceChildren();
      if (stop) stop.disabled = false;
    });
    const synth = document.getElementById("consult-synthesize-button");
    if (synth) synth.disabled = true;
    const section = document.getElementById("consult-synthesis");
    if (section) section.classList.add("is-hidden");
  };

  const onConsultEvent = (event) => {
    const payload = event.payload || event;
    const view = consult();
    if (!view) return;
    view.dataset.runId = payload.run_id;
    const seat = view.querySelector(`.consult-seat[data-seat='${CSS.escape(payload.seat_id)}']`);
    if (!seat) return;
    const output = seat.querySelector(".seat-output");
    const status = seat.querySelector(".seat-state");
    if (payload.event === "delta" && output) output.textContent += payload.delta || "";
    if (payload.state) {
      seat.dataset.state = payload.state;
      if (status) status.textContent = payload.state.replaceAll("_", " ");
      if (["completed", "cancelled", "failed"].includes(payload.state)) {
        const stop = seat.querySelector(".seat-stop");
        if (stop) stop.disabled = true;
      }
    }
  };

  const finalizeConsult = (response) => {
    const run = response?.result;
    if (!run) return;
    const view = consult();
    if (view) view.dataset.runId = run.id;
    (run.seats || []).forEach((result) => {
      const seat = view?.querySelector(`.consult-seat[data-seat='${CSS.escape(result.seat_id)}']`);
      if (!seat) return;
      seat.dataset.state = result.state;
      const output = seat.querySelector(".seat-output");
      const status = seat.querySelector(".seat-state");
      const stop = seat.querySelector(".seat-stop");
      if (output) output.textContent = result.text;
      if (status) status.textContent = result.state.replaceAll("_", " ");
      if (stop) stop.disabled = true;
    });
    const completed = (run.seats || []).some((seat) => seat.state === "completed");
    const synth = document.getElementById("consult-synthesize-button");
    if (synth) synth.disabled = !completed;
  };

  const setSkillForm = (skill = null) => {
    const form = document.getElementById("skill-form");
    if (!form) return;
    formField(form, "skill_id").value = skill?.id || "";
    formField(form, "name").value = skill?.name || "";
    formField(form, "description").value = skill?.description || "";
    formField(form, "prompt_template").value = skill?.prompt || "";
    formField(form, "cache_policy").value = skill?.cache || "none";
    const submit = form.querySelector('[data-action="skill-create"] span');
    if (submit) submit.textContent = skill ? "Save changes" : "Save Skill";
    form.querySelector('[data-action="skill-edit-cancel"]')?.classList.toggle("is-hidden", !skill);
    if (skill) formField(form, "name")?.focus();
  };

  const armDestructiveAction = (button) => {
    if (button.dataset.confirmArmed === "true") return true;
    button.dataset.confirmArmed = "true";
    button.textContent = "Delete?";
    window.setTimeout(() => {
      button.dataset.confirmArmed = "false";
      button.textContent = "Delete";
    }, 3500);
    return false;
  };

  const inlineEdit = (button) => {
    const row = button.closest(".message-row");
    const card = row?.querySelector(".message-card");
    if (!card || card.querySelector("textarea")) return;
    const textarea = document.createElement("textarea");
    textarea.value = button.dataset.messageContent || card.textContent;
    textarea.rows = 5;
    textarea.className = "inline-message-editor";
    const save = document.createElement("button");
    save.type = "button";
    save.className = "small-button";
    save.textContent = "Save";
    save.dataset.action = "message-edit-save";
    save.dataset.message = button.dataset.message;
    commandMetadata(save, {
      affordance: "message.edit",
      command: "mom_llama.message_edit",
      tauri: "mom_llama_message_edit",
      cli: "mom-llama message edit --conversation <id> --message <id> --content <text> --json",
      effect: "mom_llama.effects.conversation_store.v1",
    });
    card.replaceChildren(textarea, save);
    textarea.focus();
  };

  const actionHandlers = {
    "sidebar-toggle": async () => {
      await invoke("mom_llama_conversation_list");
      shell()?.classList.toggle("sidebar-open");
    },
    "settings-open": async () => { await invoke("mom_llama_settings_get"); openSettings(); },
    "settings-close": async () => { await invoke("mom_llama_settings_get"); closeSettings(); },
    "settings-section": async (button) => switchSettingsSection(button.dataset.section || "general"),
    "skills-open": async () => { await invoke("mom_llama_skill_list"); openSettings("general"); },
    "consult-open": async () => { await invoke("mom_llama_consult_panel_list"); openConsult(); },
    "consult-close": async () => { await invoke("mom_llama_conversation_list"); closeConsult(); },
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
    "chat-regenerate": async () => { report(await invoke("mom_llama_chat_regenerate", { conversation: selectedConversation() })); await refreshConversationProjection(); },
    "chat-continue": async () => { report(await invoke("mom_llama_chat_continue", { conversation: selectedConversation() })); await refreshConversationProjection(); },
    "message-copy": async (button) => {
      const result = await invoke("mom_llama_message_copy", { conversation: selectedConversation(), message: button.dataset.message });
      if (result?.result?.content) await navigator.clipboard.writeText(result.result.content);
      report(result);
    },
    "message-edit": async (button) => inlineEdit(button),
    "message-edit-save": async (button) => {
      const content = button.parentElement?.querySelector("textarea")?.value || "";
      report(await invoke("mom_llama_message_edit", { conversation: selectedConversation(), message: button.dataset.message, content }));
      await refreshChat();
    },
    "message-delete": async (button) => {
      if (!armDestructiveAction(button)) return;
      report(await invoke("mom_llama_message_delete", { conversation: selectedConversation(), message: button.dataset.message }));
      await refreshConversationProjection();
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
      const path = await pickFile("attachment");
      if (!path) return;
      report(await invoke("mom_llama_attachment_import", { conversation: selectedConversation(), path }));
      await refreshConversationProjection();
    },
    "settings-get": async () => report(await invoke("mom_llama_settings_get")),
    "settings-reset": async () => { report(await invoke("mom_llama_settings_reset")); await refreshSettings("general"); },
    "settings-update": async () => document.getElementById("settings-form")?.requestSubmit(),
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
      if (path) formField(document.getElementById("settings-form"), "model_path").value = path;
    },
    "mmproj-browse": async () => {
      const path = await pickFile("mmproj");
      if (path) formField(document.getElementById("settings-form"), "mmproj_path").value = path;
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
    "kv-status": async () => report(await invoke("mom_llama_kv_cache_status")),
    "kv-save": async () => { report(await invoke("mom_llama_kv_cache_save", { skill: null })); await refreshSettings("developer"); },
    "kv-restore": async () => { report(await invoke("mom_llama_kv_cache_restore", { cache: null })); await refreshSettings("developer"); },
    "kv-clear": async (button) => {
      if (!armDestructiveAction(button)) return;
      report(await invoke("mom_llama_kv_cache_clear")); await refreshSettings("developer");
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
    "tool-loop-run": async () => {
      const form = document.getElementById("settings-form");
      report(await invoke("mom_llama_tool_loop_run", {
        conversation: selectedConversation(),
        prompt: formValue(form, "tool_loop_prompt"),
        server: formValue(form, "mcp_server"),
        tool: formValue(form, "mcp_tool"),
        arguments: jsonField(form, "mcp_arguments"),
        maxTurns: 1,
      }));
      await refreshConversationProjection();
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
    "consult-cancel": async (button) => {
      const view = consult();
      report(await invoke("mom_llama_consult_cancel", { run: view?.dataset.runId || "", seat: button.dataset.seat }));
    },
    "consult-synthesize": async () => {
      const view = consult();
      const result = await invoke("mom_llama_consult_synthesize", { run: view?.dataset.runId || "", seats: [] });
      report(result);
      const section = document.getElementById("consult-synthesis");
      const output = section?.querySelector(".synthesis-output");
      if (section && output && result?.result) {
        output.textContent = result.result.text;
        section.classList.remove("is-hidden");
      }
    },
  };

  document.addEventListener("click", async (event) => {
    const button = event.target.closest("[data-action]");
    if (!button || button.disabled) return;
    const handler = actionHandlers[button.dataset.action];
    if (!handler) return;
    event.preventDefault();
    try { await handler(button); } catch (error) { reportError(error); }
  });

  document.addEventListener("submit", async (event) => {
    event.preventDefault();
    const form = event.target;
    try {
      if (form.id === "chat-form") {
        if (form.dataset.busy === "true") return;
        const message = formValue(form, "message");
        if (!message) return;
        const conversation = selectedConversation();
        appendLiveMessage("user", message, `live-user-${Date.now()}`);
        setChatBusy(true);
        const result = await invoke("mom_llama_chat_send", { conversation, message });
        report(result);
        await invoke("mom_llama_draft_clear", { conversation });
        setChatBusy(false);
        await refreshConversationProjection();
      }
      if (form.id === "consult-form") {
        const prompt = formValue(form, "prompt");
        if (!prompt) return;
        resetConsultSeats();
        const result = await invoke("mom_llama_consult_start", {
          conversation: selectedConversation(),
          prompt,
          panel: null,
        });
        report(result);
        finalizeConsult(result);
      }
      if (form.id === "settings-form") {
        const result = await invoke("mom_llama_settings_update", {
          modelPath: formValue(form, "model_path") || null,
          mmprojPath: formValue(form, "mmproj_path") || null,
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
        report(result);
        applyTheme(formValue(form, "theme"));
        await Promise.all([refreshChat(), refreshSettings("general")]);
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
    } catch (error) { reportError(error); setChatBusy(false); }
  });

  document.addEventListener("input", (event) => {
    if (event.target.matches("#chat-form textarea[name='message']")) scheduleDraft(event.target.value);
    if (event.target.matches("#conversation-search-form input[name='query']")) search().catch(reportError);
  });

  document.addEventListener("keydown", (event) => {
    if (!event.target.matches("#chat-form textarea[name='message']")) return;
    const sendOnEnter = document.querySelector('[data-setting-key="sendOnEnter"]')?.checked !== false;
    const submit = sendOnEnter
      ? event.key === "Enter" && !event.shiftKey && !event.metaKey && !event.ctrlKey
      : event.key === "Enter" && (event.metaKey || event.ctrlKey);
    if (submit) {
      event.preventDefault();
      event.target.form?.requestSubmit();
    }
  });

  document.addEventListener("change", async (event) => {
    if (!event.target.matches("select[name='model_picker']") || !event.target.value) return;
    try {
      report(await invoke("mom_llama_model_select", { modelPath: event.target.value }));
      await refreshChat();
    } catch (error) { reportError(error); }
  });

  const listen = async () => {
    const events = tauri() && tauri().event;
    if (!events || typeof events.listen !== "function") return;
    await events.listen("mom_llama_chat_stream", onChatEvent);
    await events.listen("mom_llama_consult_stream", onConsultEvent);
  };

  const boot = async () => {
    try {
      const root = document.getElementById("app");
      root.innerHTML = await invoke("mom_llama_render_app");
      const alwaysShowSidebar = shell()?.dataset.alwaysShowSidebar === "true";
      if (window.innerWidth >= 1180 && alwaysShowSidebar) shell()?.classList.add("sidebar-open");
      await listen();
    } catch (error) { reportError(error); }
  };

  boot();
})();

(function composerReducerModule(root, factory) {
  const reducer = factory();
  if (typeof module === "object" && module.exports) module.exports = reducer;
  if (root) root.MomLlamaComposerKeyPolicy = reducer;
}(typeof globalThis === "undefined" ? null : globalThis, () => {
  "use strict";

  const StateKind = Object.freeze({
    Idle: "idle",
    Composing: "composing",
    Mention: "mention",
    AiPending: "ai_pending",
    AiPresented: "ai_presented",
  });

  const idleState = () => Object.freeze({ kind: StateKind.Idle });
  const initialState = idleState;
  const result = (state, effects = []) => Object.freeze({
    state: Object.freeze(state),
    effects: Object.freeze(effects),
  });
  const effect = (kind, fields = {}) => Object.freeze({ kind, ...fields });
  const isAiState = (state) => (
    state.kind === StateKind.AiPending || state.kind === StateKind.AiPresented
  );

  const submitEffect = (event) => {
    const submit = event.sendOnEnter
      ? event.key === "Enter" && !event.shiftKey && !event.metaKey && !event.ctrlKey
      : event.key === "Enter" && (event.metaKey || event.ctrlKey);
    return submit ? effect("submit") : null;
  };

  const reduceKey = (state, event) => {
    if (state.kind === StateKind.Composing || event.isComposing || event.keyCode === 229) {
      return result(state);
    }

    if (state.kind === StateKind.Mention) {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        const delta = event.key === "ArrowDown" ? 1 : -1;
        const activeIndex = (
          state.activeIndex + delta + state.optionCount
        ) % state.optionCount;
        return result(
          { ...state, activeIndex },
          [effect("mention_active_changed", { activeIndex })],
        );
      }
      if (event.key === "Enter" && !event.shiftKey) {
        return result(state, [effect("mention_accept", { activeIndex: state.activeIndex })]);
      }
      if (event.key === "Escape") {
        return result(idleState(), [effect("mention_dismiss")]);
      }
    }

    if (state.kind === StateKind.AiPresented) {
      if (event.key === "ArrowRight") {
        return result(idleState(), [effect("ai_accept", {
          requestId: state.requestId,
          anchor: state.anchor,
          suffix: state.suffix,
        })]);
      }
      if (event.key === "Escape") {
        return result(idleState(), [effect("ai_dismiss", { requestId: state.requestId })]);
      }
    }

    const submit = submitEffect(event);
    if (!submit) return result(state);
    if (isAiState(state)) {
      return result(idleState(), [effect("ai_cancel", { requestId: state.requestId }), submit]);
    }
    return result(state, [submit]);
  };

  const reduce = (state, event) => {
    const current = state || idleState();
    switch (event.type) {
      case "composition_start": {
        const effects = [];
        if (current.kind === StateKind.Mention) effects.push(effect("mention_dismiss"));
        if (isAiState(current)) effects.push(effect("ai_cancel", { requestId: current.requestId }));
        return result({ kind: StateKind.Composing }, effects);
      }
      case "composition_end":
        return current.kind === StateKind.Composing ? result(idleState()) : result(current);
      case "mention_open": {
        if (
          current.kind === StateKind.Composing
          || !Number.isSafeInteger(event.optionCount)
          || event.optionCount < 1
        ) return result(current);
        const effects = isAiState(current)
          ? [effect("ai_cancel", { requestId: current.requestId })]
          : [];
        return result({
          kind: StateKind.Mention,
          optionCount: event.optionCount,
          activeIndex: 0,
        }, effects);
      }
      case "mention_close":
        return current.kind === StateKind.Mention ? result(idleState()) : result(current);
      case "ai_request":
        if (current.kind !== StateKind.Idle) return result(current);
        return result({
          kind: StateKind.AiPending,
          requestId: event.requestId,
          anchor: event.anchor,
        });
      case "ai_present":
        if (
          current.kind !== StateKind.AiPending
          || current.requestId !== event.requestId
          || current.anchor !== event.anchor
        ) return result(current);
        return result({
          kind: StateKind.AiPresented,
          requestId: event.requestId,
          anchor: event.anchor,
          suffix: event.suffix,
        });
      case "ai_dismiss":
        return isAiState(current) ? result(idleState()) : result(current);
      case "key_down":
        return reduceKey(current, {
          key: event.key,
          keyCode: event.keyCode,
          isComposing: event.isComposing,
          shiftKey: Boolean(event.shiftKey),
          metaKey: Boolean(event.metaKey),
          ctrlKey: Boolean(event.ctrlKey),
          sendOnEnter: event.sendOnEnter !== false,
        });
      default:
        return result(current);
    }
  };

  return Object.freeze({ StateKind, initialState, reduce });
}));

(function composerKeyPolicyModule(root, factory) {
  const policy = factory();
  if (typeof module === "object" && module.exports) module.exports = policy;
  if (root) root.MomLlamaComposerKeyPolicy = policy;
}(typeof globalThis === "undefined" ? null : globalThis, () => {
  "use strict";

  const decideKey = ({
    key,
    shiftKey = false,
    metaKey = false,
    ctrlKey = false,
    mentionOpen = false,
    mentionCount = 0,
    sendOnEnter = true,
  }) => {
    if (mentionOpen && mentionCount > 0) {
      if (key === "ArrowDown") return Object.freeze({ kind: "mention_next" });
      if (key === "ArrowUp") return Object.freeze({ kind: "mention_previous" });
      if (key === "Enter" && !shiftKey) {
        return Object.freeze({ kind: "mention_accept" });
      }
      if (key === "Escape") return Object.freeze({ kind: "mention_dismiss" });
    }

    const submit = sendOnEnter
      ? key === "Enter" && !shiftKey && !metaKey && !ctrlKey
      : key === "Enter" && (metaKey || ctrlKey);
    return Object.freeze({ kind: submit ? "submit" : "unhandled" });
  };

  return Object.freeze({ decideKey });
}));

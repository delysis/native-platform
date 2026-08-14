(function cacheInspectorModule(root, factory) {
  const inspector = factory();
  if (typeof module === "object" && module.exports) module.exports = inspector;
  if (root) root.MomLlamaCacheInspector = inspector;
}(typeof globalThis === "undefined" ? null : globalThis, () => {
  "use strict";

  const readyEntries = (response) => (
    Array.isArray(response?.result?.entries)
      ? response.result.entries.filter((entry) => entry?.state === "ready")
      : []
  );

  const plural = (count, singular, pluralForm = `${singular}s`) => (
    `${count} ${count === 1 ? singular : pluralForm}`
  );

  const actionState = (response) => (
    response?.status === "blocked" || response?.blocker ? "blocked" : "passed"
  );

  const actionMessage = (action, response) => {
    const blocker = response?.blocker?.message;
    if (blocker) return blocker;
    if (response?.status === "blocked") return "The cache action could not be completed.";
    if (action === "clear") return "Cleared persisted and in-memory prompt caches.";
    if (action === "refresh") {
      const stored = readyEntries(response).length;
      const warm = Number.isSafeInteger(response?.result?.memory_entries)
        ? response.result.memory_entries
        : 0;
      return `Cache status refreshed: ${plural(stored, "stored checkpoint")}, ${plural(warm, "warm entry", "warm entries")}.`;
    }
    return "Prompt cache updated.";
  };

  return Object.freeze({ actionMessage, actionState, readyEntries });
}));

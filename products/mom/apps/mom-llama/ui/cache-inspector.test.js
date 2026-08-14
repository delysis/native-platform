"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const inspector = require("./cache-inspector.js");

test("refresh feedback reports only ready persisted checkpoints and warm entries", () => {
  const response = {
    status: "contracted",
    result: {
      memory_entries: 1,
      entries: [
        { id: "conversation", state: "ready" },
        { id: "persona", state: "ready" },
        { id: "stale", state: "invalidated" },
      ],
    },
  };

  assert.equal(inspector.readyEntries(response).length, 2);
  assert.equal(inspector.actionState(response), "passed");
  assert.equal(
    inspector.actionMessage("refresh", response),
    "Cache status refreshed: 2 stored checkpoints, 1 warm entry.",
  );
});

test("cache feedback preserves typed blockers and honest clear confirmation", () => {
  const blocked = {
    status: "blocked",
    blocker: { message: "The encrypted store is locked." },
  };
  assert.equal(inspector.actionState(blocked), "blocked");
  assert.equal(
    inspector.actionMessage("refresh", blocked),
    "The encrypted store is locked.",
  );
  assert.equal(
    inspector.actionMessage("clear", { status: "contracted", result: {} }),
    "Cleared persisted and in-memory prompt caches.",
  );
});

test("the app loads visible cache feedback before handlers and exposes no generic save or restore action", () => {
  const index = fs.readFileSync(path.join(__dirname, "index.html"), "utf8");
  const handlers = fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");
  assert.ok(index.indexOf("cache-inspector.js") < index.indexOf("coop-hx.js"));
  assert.match(handlers, /"kv-status": async \(\) => refreshCacheInspector/);
  assert.match(handlers, /refreshCacheInspector\("clear"/);
  assert.doesNotMatch(handlers, /"kv-save":/);
  assert.doesNotMatch(handlers, /"kv-restore":/);
});

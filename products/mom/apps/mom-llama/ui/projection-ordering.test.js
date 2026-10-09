"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const source = fs.readFileSync(`${__dirname}/coop-hx.js`, "utf8");
test("late native projections cannot replace the latest pane", async () => {
  const pending = [];
  const replacements = [];
  const node = { replaceWith: (next) => replacements.push(next), contains: () => false };
  const context = {
    document: { querySelector: () => node },
    invokeMarkup: () => new Promise((resolve) => pending.push(resolve)),
    parseFragment: (html) => html,
    activeSpeechPlayback: null,
    releaseAttachmentObjectUrls() {}, sizeComposer() {},
  };
  vm.createContext(context);
  const start = source.indexOf("  const projectionRevisions =");
  const end = source.indexOf("  const rawAttachmentBytes =", start);
  vm.runInContext(`${source.slice(start, end)}\nthis.swap = swap;`, context);
  const first = context.swap("#chat", "render");
  const second = context.swap("#chat", "render");
  pending[1]("latest selection");
  assert.equal(await second, "latest selection");
  pending[0]("old selection");
  assert.equal(await first, null);
  assert.deepEqual(replacements, ["latest selection"]);
});

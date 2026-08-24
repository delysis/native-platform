"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const policy = require("./composer-key-policy.js");

test("current composer precedence is mention navigation then send", () => {
  assert.equal(
    policy.decideKey({ key: "Enter", mentionOpen: true, mentionCount: 2 }).kind,
    "mention_accept",
  );
  assert.equal(policy.decideKey({ key: "Enter" }).kind, "submit");
  assert.equal(policy.decideKey({ key: "Enter", shiftKey: true }).kind, "unhandled");
  assert.equal(
    policy.decideKey({ key: "Enter", sendOnEnter: false, metaKey: true }).kind,
    "submit",
  );
  assert.equal(policy.decideKey({ key: "Tab" }).kind, "unhandled");
});

test("audit characterization records that composition does not yet gate Enter", () => {
  assert.equal(
    policy.decideKey({ key: "Enter", isComposing: true }).kind,
    "submit",
    "the current key policy ignores the composition flag",
  );
  assert.equal(
    policy.decideKey({ key: "Enter", keyCode: 229 }).kind,
    "submit",
    "the current key policy ignores WebKit's composition fallback key code",
  );
  assert.equal(
    policy.decideKey({
      key: "Enter",
      isComposing: true,
      mentionOpen: true,
      mentionCount: 1,
    }).kind,
    "mention_accept",
    "composition does not yet suppress mention acceptance",
  );
});

"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");

const source = fs.readFileSync(`${__dirname}/coop-hx.js`, "utf8");
const start = source.indexOf("  const indexVoiceTurn = (");
const end = source.indexOf("\n  const startPendingCallTranscriptSlot =", start);
assert.ok(start >= 0 && end > start, "production indexVoiceTurn must remain extractable");
const definition = source.slice(start, end);

function makeHarness({ transcription, commit, attachment = "recording-a", message = "message-a" } = {}) {
  const calls = [];
  const stages = [];
  const errors = [];
  const retries = [];
  let now = 0;
  const context = {
    voiceCallActive: false,
    voiceCallPreparing: false,
    voiceLatencyTrace: "trace-a",
    pendingCallTranscriptJobs: [],
    activeCallTranscriptJob: null,
    activeCallTranscriptTask: null,
    activeCallTranscriptOperation: null,
    performance: { now: () => ++now },
    speechOperationToken: () => "operation-a",
    markVoiceLatencyIn: (_trace, stage, detail) => stages.push({ stage, detail }),
    errorMessage: (error) => error instanceof Error ? error.message : String(error),
    reportError: (error) => errors.push(error),
    retryCallPrefillAfterTranscript: (conversation) => retries.push(conversation),
    attachmentPreviewResult: (value) => value?.result ?? value,
    attachmentPreviewAnchor: () => ({}),
    invoke: async (command, args) => {
      calls.push({ command, args });
      if (command === "mom_llama_attachment_preview") {
        return { result: { primary: { kind: "audio", id: "audio-a" }, root_sha256: "root-a" } };
      }
      if (command === "mom_llama_speech_transcribe_attachment") return transcription;
      if (command === "mom_llama_message_record_voice_transcript") return commit;
      throw new Error(`unexpected command ${command}`);
    },
  };
  vm.createContext(context);
  vm.runInContext(`${definition}\nthis.indexVoiceTurn = indexVoiceTurn;`, context);
  return {
    calls,
    stages,
    errors,
    retries,
    run: (options = {}) => context.indexVoiceTurn("conversation-a", {
      voice_attachment_id: attachment,
      user_message_id: message,
    }, null, null, "trace-a", options),
  };
}

const contractedTranscript = (transcript) => ({ status: "contracted", result: { transcript } });
const contractedCommit = { status: "contracted", result: { recorded: true } };

test("empty successful ASR is retained as a skip and never commits or retries", async () => {
  const harness = makeHarness({ transcription: contractedTranscript(" \n "), commit: contractedCommit });
  assert.equal(await harness.run(), false);
  assert.equal(harness.calls.filter(({ command }) => command === "mom_llama_message_record_voice_transcript").length, 0);
  assert.deepEqual(harness.errors, []);
  assert.deepEqual(harness.retries, []);
  assert.equal(harness.stages.some(({ stage }) => stage === "voice_transcript_finished"), false);
  assert.ok(harness.stages.some(({ stage, detail }) => stage === "voice_transcript_skipped"
    && detail?.reason === "empty_transcript" && detail?.recording_retained === true));
});

test("only a successful durable transcript commit returns true and finishes the turn", async () => {
  const harness = makeHarness({ transcription: contractedTranscript("Please call me tomorrow."), commit: contractedCommit });
  assert.equal(await harness.run(), true);
  assert.equal(harness.calls.filter(({ command }) => command === "mom_llama_message_record_voice_transcript").length, 1);
  assert.deepEqual(harness.errors, []);
  assert.deepEqual(harness.retries, ["conversation-a"]);
  assert.equal(harness.stages.filter(({ stage }) => stage === "voice_transcript_finished").length, 1);
  assert.equal(harness.stages.filter(({ stage }) => stage === "voice_transcript_failed").length, 0);
});

test("blocked ASR stays an error and cannot commit or retry", async () => {
  const harness = makeHarness({
    transcription: { status: "blocked", blocker: { code: "asr_unavailable", message: "ASR unavailable" } },
    commit: contractedCommit,
  });
  await assert.rejects(harness.run(), /ASR unavailable/);
  assert.equal(harness.calls.filter(({ command }) => command === "mom_llama_message_record_voice_transcript").length, 0);
  assert.equal(harness.errors.length, 1);
  assert.deepEqual(harness.retries, []);
  assert.equal(harness.stages.filter(({ stage }) => stage === "voice_transcript_finished").length, 0);
});

test("missing recording identity remains an error before ASR admission", async () => {
  const harness = makeHarness({ transcription: contractedTranscript("unreachable"), commit: contractedCommit, attachment: null });
  await assert.rejects(harness.run(), /recording identity/);
  assert.equal(harness.calls.length, 0);
  assert.equal(harness.errors.length, 1);
  assert.deepEqual(harness.retries, []);
});

test("blocked durable commit remains an error and cannot finish or retry", async () => {
  const harness = makeHarness({
    transcription: contractedTranscript("Keep this recording."),
    commit: { status: "blocked", blocker: { code: "commit_denied", message: "Transcript storage denied" } },
  });
  await assert.rejects(harness.run(), /Transcript storage denied/);
  assert.equal(harness.calls.filter(({ command }) => command === "mom_llama_message_record_voice_transcript").length, 1);
  assert.equal(harness.errors.length, 1);
  assert.deepEqual(harness.retries, []);
  assert.equal(harness.stages.filter(({ stage }) => stage === "voice_transcript_finished").length, 0);
  assert.equal(harness.stages.filter(({ stage }) => stage === "voice_transcript_failed").length, 1);
});

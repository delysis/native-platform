import test from "node:test";
import assert from "node:assert/strict";
import { archivePaths } from "./diarize-archive-source.mjs";

test("one archive source has stable diarization, ledger, and review paths", () => {
  assert.deepEqual(archivePaths("/archive", "interview-1"), {
    diarization: "/archive/interview-1.acoustic-diarization.json",
    ledger: "/archive/interview-1.speaker-ledger.json",
    attributedTranscript: "/archive/interview-1.speaker-attributed-transcript.json",
    review: "/archive/interview-1.speaker-review.json",
  });
});

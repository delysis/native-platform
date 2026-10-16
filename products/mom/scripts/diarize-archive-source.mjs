#!/usr/bin/env node

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  attributeTranscriptSegments,
  buildDiarizationLedger,
  createReviewTemplate,
  sha256,
} from "./speaker-diarization-ledger.mjs";

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 2) {
    if (!argv[index]?.startsWith("--") || argv[index + 1] == null) {
      throw new Error(`invalid argument: ${argv[index] ?? ""}`);
    }
    args[argv[index].slice(2).replaceAll("-", "_")] = argv[index + 1];
  }
  return args;
}

function readJSONOrJSONL(path) {
  const text = readFileSync(path, "utf8");
  return path.endsWith(".jsonl")
    ? text.split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))
    : JSON.parse(text);
}

function fileSha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function required(args, name) {
  const value = args[name];
  if (!value) throw new Error(`--${name.replaceAll("_", "-")} is required`);
  return value;
}

export function archivePaths(outputDirectory, sourceID) {
  return {
    diarization: join(outputDirectory, `${sourceID}.acoustic-diarization.json`),
    ledger: join(outputDirectory, `${sourceID}.speaker-ledger.json`),
    attributedTranscript: join(outputDirectory, `${sourceID}.speaker-attributed-transcript.json`),
    review: join(outputDirectory, `${sourceID}.speaker-review.json`),
  };
}

async function run(args) {
  const media = resolve(required(args, "media"));
  const transcript = resolve(required(args, "transcript"));
  const helper = resolve(required(args, "helper"));
  const model = resolve(required(args, "model"));
  const sourceID = required(args, "source_id");
  const outputDirectory = resolve(required(args, "output_dir"));
  const transcriptToolID = required(args, "transcript_tool_id");
  const transcriptToolRevision = required(args, "transcript_tool_revision");
  mkdirSync(outputDirectory, { recursive: true });
  const paths = archivePaths(outputDirectory, sourceID);
  const diarizer = resolve(dirname(fileURLToPath(import.meta.url)), "acoustic-speaker-diarizer.mjs");
  const acousticArguments = [
    diarizer,
    "--media", media,
    "--transcript", transcript,
    "--helper", helper,
    "--model", model,
    "--output", paths.diarization,
  ];
  for (const [argument, field] of [
    ["--max-speakers", "max_speakers"],
    ["--threshold", "threshold"],
    ["--window-ms", "window_ms"],
    ["--min-window-ms", "min_window_ms"],
    ["--join-gap-ms", "join_gap_ms"],
  ]) {
    if (args[field] != null) acousticArguments.push(argument, args[field]);
  }
  execFileSync(process.execPath, acousticArguments, { stdio: "inherit" });

  const transcriptSegments = readJSONOrJSONL(transcript);
  const diarizationSegments = readJSONOrJSONL(paths.diarization);
  const diarizerConfiguration = {
    schema: "mom-acoustic-speaker-diarizer-config-v1",
    model: basename(model),
    threshold: Number(args.threshold ?? 0.78),
    max_speakers: args.max_speakers == null ? null : Number(args.max_speakers),
    window_ms: Number(args.window_ms ?? 3_000),
    min_window_ms: Number(args.min_window_ms ?? 1_500),
    join_gap_ms: Number(args.join_gap_ms ?? 250),
  };
  const ledger = buildDiarizationLedger({
    schema: "mom-speaker-diarization-input-v1",
    source: {
      source_id: sourceID,
      source_url: args.source_url ?? null,
      media_filename: basename(media),
      media_sha256: fileSha256(media),
    },
    transcript: {
      artifact_sha256: fileSha256(transcript),
      kind: args.transcript_kind ?? "asr_or_caption_segments",
      tool: { id: transcriptToolID, revision: transcriptToolRevision },
      segments: transcriptSegments,
    },
    diarization: {
      artifact_sha256: fileSha256(paths.diarization),
      config_sha256: sha256(diarizerConfiguration),
      kind: "acoustic_diarization",
      tool: { id: "qwen3-speaker-encoder", revision: "mom-acoustic-diarizer-v1" },
      minimum_confidence: Number(args.minimum_confidence ?? 0.75),
      segments: diarizationSegments,
    },
  });
  writeFileSync(paths.ledger, `${JSON.stringify(ledger, null, 2)}\n`);
  const attributedTranscript = attributeTranscriptSegments(ledger);
  writeFileSync(paths.attributedTranscript, `${JSON.stringify(attributedTranscript, null, 2)}\n`);
  writeFileSync(paths.review, `${JSON.stringify(createReviewTemplate(ledger), null, 2)}\n`);
  const receipt = {
    status: "passed",
    source_id: sourceID,
    media_sha256: ledger.source.media_sha256,
    transcript_sha256: ledger.transcript_provenance.artifact_sha256,
    diarization_sha256: ledger.diarization_provenance.artifact_sha256,
    ledger_sha256: ledger.ledger_sha256,
    attributed_transcript_sha256: attributedTranscript.attributed_transcript_sha256,
    speakers: ledger.speakers.length,
    segments: ledger.segments.length,
    uncertain_segments: ledger.segments.filter(({ uncertain }) => uncertain).length,
    review_path: paths.review,
    attributed_transcript_path: paths.attributedTranscript,
  };
  console.log(JSON.stringify(receipt));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  run(parseArgs(process.argv.slice(2))).catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}

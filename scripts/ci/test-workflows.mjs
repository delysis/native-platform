#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const root = path.resolve(import.meta.dirname, "../..");
const prPath = path.join(root, ".github/workflows/ci-pr.yml");
const fullPath = path.join(root, ".github/workflows/ci-full.yml");
const releasePath = path.join(root, ".github/workflows/release-macos.yml");
const releaseScriptPath = path.join(root, "scripts/release-macos.sh");
const smokeScriptPath = path.join(root, "scripts/smoke-macos-app.sh");
const embeddedModelScriptPath = path.join(root, "scripts/find-embedded-model.mjs");
const workflowSnapshotPath = path.join(root, "ci/ci-workflow-snapshot.json");
const momPackagePath = path.join(
  root,
  "products/mom/apps/mom-llama/package.json",
);
const momWindowsIconPath = path.join(
  root,
  "products/mom/apps/mom-llama/src-tauri/icons/icon.ico",
);

function read(file) {
  return fs.readFileSync(file, "utf8");
}

function workflowJobIds(source) {
  const jobs = source.slice(source.indexOf("\njobs:\n") + "\njobs:\n".length);
  return [...jobs.matchAll(/^  ([a-z][a-z0-9-]+):$/gm)].map((match) => match[1]);
}

function sha256(file) {
  return createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function writeExecutable(file, source) {
  fs.writeFileSync(file, source, { mode: 0o755 });
}

function macSmokeFixture(t, weightPath = null, weightContents = "fixture model bytes\n") {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "delysis-smoke-identity-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));

  const candidate = path.join(directory, "candidate");
  const fakeTools = path.join(directory, "bin");
  const sourceBundle = path.join(directory, "source", "Mom Llama.app");
  const contents = path.join(sourceBundle, "Contents");
  const executable = path.join(contents, "MacOS", "mom-llama-app");
  const archive = path.join(candidate, "Mom Llama.app.zip");
  const receipt = path.join(candidate, "release-receipt.json");
  fs.mkdirSync(path.dirname(executable), { recursive: true });
  fs.mkdirSync(candidate, { recursive: true });
  fs.mkdirSync(fakeTools, { recursive: true });
  writeExecutable(executable, "#!/bin/sh\nexit 0\n");
  fs.writeFileSync(path.join(contents, "Info.plist"), "fixture plist\n");
  fs.writeFileSync(archive, "fixture archive bytes\n");

  if (weightPath) {
    const target = path.join(contents, "Resources", weightPath);
    if (weightPath.endsWith(".mlpackage")) {
      fs.mkdirSync(target, { recursive: true });
    } else {
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, weightContents);
    }
  }

  writeExecutable(path.join(fakeTools, "uname"), "#!/bin/sh\nprintf 'Darwin\\n'\n");
  writeExecutable(
    path.join(fakeTools, "ditto"),
    "#!/bin/sh\ncp -R \"$FAKE_APP_SOURCE\" \"$4/\"\n",
  );
  writeExecutable(
    path.join(fakeTools, "shasum"),
    `#!/bin/sh
node -e 'const fs=require("fs"),crypto=require("crypto"),p=process.argv[1]; console.log(crypto.createHash("sha256").update(fs.readFileSync(p)).digest("hex")+"  "+p)' "$3"
`,
  );
  writeExecutable(
    path.join(fakeTools, "stat"),
    `#!/bin/sh
if [ "$#" -ne 3 ] || [ "$1" != "-Lf" ] || [ "$2" != "%d:%i" ]; then
  printf 'unsupported fake stat invocation\n' >&2
  exit 2
fi
node -e 'const fs=require("fs"),s=fs.statSync(process.argv[1],{bigint:true}); process.stdout.write(String(s.dev)+":"+String(s.ino)+"\\n")' "$3"
`,
  );

  const validReceipt = {
    schema: "delysis.macos-release-receipt.v1",
    component: "mom",
    macos: {
      bundle_id: "com.delysis.llama-native-kit.mom-llama",
      archive_sha256: sha256(archive),
      executable_sha256: sha256(executable),
    },
  };
  const run = () =>
    spawnSync("sh", [smokeScriptPath, "mom", archive], {
      encoding: "utf8",
      env: {
        ...process.env,
        FAKE_APP_SOURCE: sourceBundle,
        PATH: `${fakeTools}${path.delimiter}${process.env.PATH}`,
        TMPDIR: directory,
      },
    });

  return { archive, receipt, run, validReceipt };
}

test("extracted macOS ZIP rejects an extensionless GGUF payload", (t) => {
  const fixture = macSmokeFixture(
    t,
    "weights/0123456789abcdef",
    Buffer.from("GGUFextensionless model bytes"),
  );
  fs.writeFileSync(fixture.receipt, `${JSON.stringify(fixture.validReceipt)}\n`);
  const result = fixture.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /model weights must remain runtime-discovered/);
  assert.match(result.stderr, /0123456789abcdef/);
});

function externalActionUses(source) {
  return [...source.matchAll(/^\s*-\s+uses:\s+([^\s#]+).*$/gm)]
    .map((match) => match[1])
    .filter((value) => !value.startsWith("./") && !value.startsWith("docker://"));
}

test("only the targeted PR, full, and asynchronous release workflows remain active", () => {
  assert.equal(fs.existsSync(path.join(root, ".github/workflows/ci.yml")), false);
  assert.equal(fs.existsSync(prPath), true);
  assert.equal(fs.existsSync(fullPath), true);
  assert.equal(fs.existsSync(releasePath), true);
});

test("Mom retains the Windows resource icon required by Tauri builds", () => {
  const icon = fs.readFileSync(momWindowsIconPath);
  assert.deepEqual([...icon.subarray(0, 4)], [0, 0, 1, 0]);
});

test("local macOS smoke can verify the exact emitted archive", () => {
  const release = read(releaseScriptPath);
  const smoke = read(smokeScriptPath);
  assert.match(release, /exact-archive smoke:/);
  assert.match(smoke, /ditto -x -k "\$INPUT_ARCHIVE" "\$INSTALL_ROOT"/);
  assert.match(smoke, /BUNDLE=\$\(CDPATH= cd -- "\$BUNDLE" && pwd -P\)/);
  assert.ok(
    smoke.indexOf('BUNDLE=$(CDPATH= cd -- "$BUNDLE" && pwd -P)') <
      smoke.indexOf('EXECUTABLE="$BUNDLE/Contents/MacOS/$BINARY_NAME"'),
    "the bundle path must be physical before exact executable PID binding",
  );
  assert.match(smoke, /input_archive_sha256:/);
  assert.match(smoke, /input_release_receipt_sha256:/);
});

test("Loom UI smoke cannot attach to an active editor or invent a model identity", () => {
  const smoke = read(smokeScriptPath);
  assert.match(smoke, /running_exact_pids=\$\(exact_bundle_pid\)/);
  assert.match(smoke, /refusing to run macOS UI smoke while the exact application bundle is already running/);
  assert.match(smoke, /gemma-4-12B-it-qat-q4_0\.gguf/);
  assert.ok(
    smoke.indexOf('LOOM_SMOKE_MODEL_LINK="$model_library/gemma-4-12B-it-qat-q4_0.gguf"') <
      smoke.indexOf("run_once 1"),
    "the exact Gemma link must exist before Loom startup discovery",
  );
  assert.match(smoke, /stat -Lf '%d:%i' "\$LOOM_SMOKE_GGUF_MODEL_PATH"/);
  assert.doesNotMatch(smoke, /acceptance-writer/);
  assert.match(smoke, /exercise_loom_completion_word_reversal/);
  assert.match(smoke, /kAXValueAttribute as CFString/);
  assert.match(smoke, /kAXSelectedTextRangeAttribute as CFString/);
  assert.match(smoke, /virtualKey: 49/);
  assert.match(smoke, /"terminal_space_key_event": terminalSpace/);
  assert.match(smoke, /event\.postToPid\(pid\)/);
  assert.match(smoke, /native Accessibility input did not stabilize at the exact value and collapsed end caret/);
  assert.match(smoke, /"stable_seconds": 0\.4/);
  assert.match(smoke, /observed_editor_value/);
  assert.match(smoke, /observed_caret_utf16/);
  assert.match(smoke, /Option-Right did not persist one cached completion word/);
  assert.match(smoke, /Option-Left did not restore the exact pre-acceptance manuscript bytes/);
  assert.match(smoke, /generation-run count across all cached completion interactions/);
  assert.match(smoke, /generation-run count before Option reversal/);
  assert.match(smoke, /wait_for_loom_generation_family/);
  assert.match(smoke, /one four-choice batch/);
  assert.match(smoke, /four admitted runs did not form one exact source\/anchor\/model family/);
  assert.match(smoke, /source_revision_id: rows\[0\]\.source_revision_id/);
  assert.match(smoke, /model_environment_artifact_id: rows\[0\]\.model_environment_artifact_id/);
  assert.match(smoke, /family_terminal_status: 'completed'/);
  assert.match(smoke, /generation_terminal_evidence/);
  assert.match(smoke, /row\.terminal_candidate_id === row\.candidate_id/);
  assert.match(smoke, /row\.candidate_output_blob_id === row\.evidence_output_blob_id/);
  assert.match(smoke, /row\.generated_span_artifact_id === row\.output_artifact_id/);
  assert.match(smoke, /completion control state:/);
  assert.match(smoke, /var pressed = false/);
  assert.match(smoke, /if description\.contains\(alreadyName\) \{/);
  assert.match(smoke, /guard pressed \|\| !requirePress/);
  assert.match(smoke, /suggestionLabelPattern/);
  assert.match(smoke, /strings\(element\)\.contains\("Completion suggestions"\)/);
  assert.match(smoke, /kAXListRole/);
  assert.match(smoke, /kAXSelectedAttribute/);
  assert.match(smoke, /let candidate = candidates\[index - 1\]/);
  assert.match(smoke, /waitForAccessibleFan/);
  assert.match(smoke, /fan Return did not persist the selected cached remainder/);
  assert.match(smoke, /fan Tab did not persist the selected cached remainder/);
  assert.match(smoke, /shared engine on-to-off did not clear the cached completion session/);
  assert.match(smoke, /normalizedRunIds/);
  assert.match(smoke, /cached_completion_interactions: cachedCompletionInteractions/);
  assert.match(smoke, /generation_runs_before:/);
  assert.match(smoke, /generation_runs_after:/);
  assert.match(smoke, /generation_family: generationFamily/);
  assert.match(smoke, /editor_input: editorInput/);
  for (const stage of [
    "title",
    "body",
    "heading",
    "heading_body",
    "subheading",
    "subheading_body",
    "bold",
    "bold_reverse",
    "italic",
    "italic_reverse",
    "block_quote",
    "block_quote_reverse",
    "bullet_list",
    "bullet_list_reverse",
    "numbered_list",
    "numbered_list_reverse",
    "link",
    "remove_link",
  ]) {
    assert.match(smoke, new RegExp(`stage\\('${stage}'`));
  }

  const autocompleteOff = smoke.indexOf(
    "RUN_1_AUTOCOMPLETE_OFF_EVIDENCE=$(set_loom_completion_toggle",
  );
  const terminalSpaceInput = smoke.indexOf(
    'type_into_loom_editor "$ACTIVE_PID" "$RUN_1_EDITOR_INPUT_SENTINEL"',
  );
  const autocompleteEnable = smoke.indexOf(
    "RUN_1_AUTOCOMPLETE_ENABLE_EVIDENCE=$(set_loom_completion_toggle",
  );
  assert.ok(autocompleteOff >= 0, "real completion smoke must establish autocomplete off");
  assert.ok(
    autocompleteOff < terminalSpaceInput && terminalSpaceInput < autocompleteEnable,
    "real completion smoke must type with autocomplete off and enable it only afterward",
  );
  assert.match(smoke, /RUN_1_EDITOR_CORE_SENTINEL='Loom native smoke: editor persistence\.'/);
  assert.match(smoke, /RUN_1_EDITOR_INPUT_SENTINEL="\$RUN_1_EDITOR_CORE_SENTINEL "/);
  assert.match(smoke, /pressed_exactly_once/);
  assert.match(smoke, /"require-press"/);
  assert.match(smoke, /observed === expected/);
  assert.match(smoke, /live_wysiwyg_after_persistence: wysiwyg/);

  assert.match(smoke, /start_loom_generation_guard/);
  assert.match(smoke, /fifth_run_observed/);
  assert.match(smoke, /generation_family_guard: generationGuard/);
  assert.match(smoke, /start_loom_live_streaming_monitor/);
  assert.match(smoke, /delysis\.loom-live-stream-witness\.v1/);
  assert.match(smoke, /family_terminal_before_live_witness/);
  assert.match(smoke, /event_kind = 'text_delta'/);
  assert.match(smoke, /durableCumulativeText\.hasPrefix\(visibleSuffix\)/);
  assert.match(smoke, /selectedPresentationKey == renderedPresentationKey/);
  assert.match(smoke, /selectedPresentationKey == inlineVisibleKey/);
  assert.match(smoke, /selectedRunIsTerminal\(selectedRunId\)/);
  assert.match(smoke, /selected_run_terminal_after_accessibility": false/);
  assert.match(smoke, /visible_suffix_is_durable_leading_projection": true/);
  assert.match(smoke, /live_streaming_preterminal: liveStreaming/);
  const liveObserverStart = smoke.indexOf("if ! start_loom_live_streaming_monitor");
  const liveObserverWait = smoke.indexOf("if ! wait_for_loom_live_streaming_monitor");
  const terminalFamilyWait = smoke.indexOf(
    "RUN_1_REAL_GENERATION_EVIDENCE=$(wait_for_loom_generation_family",
  );
  assert.ok(
    liveObserverStart >= 0 &&
      liveObserverStart < autocompleteEnable &&
      autocompleteEnable < liveObserverWait &&
      liveObserverWait < terminalFamilyWait,
    "the initialized AX/store observer must witness live text before terminal-family hydration",
  );
  assert.match(
    smoke,
    /RUN_1_LIVE_STREAMING_EVIDENCE=\$\(cat "\$LOOM_LIVE_STREAM_MONITOR_OUTPUT"\)/,
  );

  assert.match(smoke, /exercise_loom_idle_resume_ghost/);
  assert.match(smoke, /delysis\.loom-idle-resume-ghost-witness\.v1/);
  assert.match(smoke, /runningApplication\.hide\(\)/);
  assert.match(smoke, /kAXHiddenAttribute as CFString/);
  assert.match(smoke, /"PID-addressed AXHidden"/);
  assert.match(smoke, /exact-PID System Events visible=false/);
  assert.match(smoke, /exact-PID System Events visible=true/);
  assert.match(smoke, /kCFBooleanFalse/);
  assert.match(smoke, /"resume_dispatch": resumeDispatch/);
  assert.match(smoke, /runningApplication\.isHidden/);
  assert.match(smoke, /withBundleIdentifier: "com\.apple\.finder"/);
  assert.match(smoke, /NSAppleScript\(/);
  assert.match(smoke, /Finder Apple event/);
  assert.match(smoke, /let minimumIdleSeconds: TimeInterval = 75/);
  assert.match(smoke, /ProcessInfo\.processInfo\.systemUptime - idleStartedAtUptime/);
  assert.match(smoke, /let deadlineUptime = ProcessInfo\.processInfo\.systemUptime \+ 120/);
  assert.match(smoke, /Cross a full minute hidden/);
  assert.match(
    smoke,
    /NSWorkspace\.shared\.frontmostApplication\?\.processIdentifier ==\s+backgroundApplication\.processIdentifier/,
  );
  assert.match(smoke, /generationCount\(\) == expectedGenerationCount/);
  assert.match(smoke, /struct DurableCandidateIdentity: Equatable/);
  assert.match(
    smoke,
    /SELECT f\.run_id, t\.candidate_id, c\.output_blob_id FROM family f/,
  );
  assert.match(smoke, /candidate\.candidateId == "run:\\[(]candidate\.runId[)]"/);
  assert.match(smoke, /!candidate\.presentationKey\.hasPrefix\("stream:"\)/);
  assert.match(smoke, /presentationMatchesDurableCandidate\(candidate, durable\)/);
  assert.match(
    smoke,
    /waitForGhostIdentity\([\s\S]*?durableCandidates: durableFamilyCandidates,[\s\S]*?expected: before/,
  );
  assert.match(smoke, /"terminal_candidate_authority"/);
  assert.match(smoke, /"before_identity"/);
  assert.match(smoke, /"last_observed_identity"/);
  assert.match(smoke, /"before_raw_observation"/);
  assert.match(smoke, /"last_raw_observation"/);
  assert.match(smoke, /"frontmost_matches_expected"/);
  assert.match(smoke, /"application_hidden"/);
  assert.match(smoke, /"application_active"/);
  assert.match(smoke, /"writing_surface_present"/);
  assert.match(smoke, /"selected_text_range"/);
  assert.match(smoke, /"ax_value_utf8_bytes"/);
  assert.match(smoke, /"ax_value_sha256"/);
  assert.match(smoke, /"ax_value_suffix_sha256"/);
  assert.match(smoke, /"completion_witness_text_found"/);
  assert.match(smoke, /"completion_witness_parsed"/);
  assert.match(smoke, /func completionWitnessCore/);
  assert.match(smoke, /"rendered_presentation_key"/);
  assert.match(smoke, /"inline_visible_key"/);
  assert.doesNotMatch(smoke, /"ax_value":/);
  assert.match(smoke, /launch-1-idle-resume-identity-diagnostics\.json/);
  assert.match(smoke, /data\.write\(to: URL\(fileURLWithPath: identityFailurePath\), options: \.atomic\)/);
  assert.match(smoke, /authority_frozen_before": before\.authorityFrozen/);
  assert.match(smoke, /exact_ghost_identity_resynchronized": true/);
  assert.match(smoke, /new_generation_started": false/);
  assert.match(smoke, /ghost_stole_editor_focus": false/);
  assert.match(smoke, /idle_resume_ghost: idleResumeGhost/);
  const terminalGhostWait = smoke.indexOf(
    "RUN_1_REAL_GHOST_EVIDENCE=$(wait_for_loom_accessibility_text",
  );
  const idleResumeWitness = smoke.indexOf(
    "RUN_1_IDLE_RESUME_GHOST_EVIDENCE=$(exercise_loom_idle_resume_ghost",
  );
  const cachedInteractionWitness = smoke.indexOf(
    "RUN_1_REAL_WORD_REVERSAL_EVIDENCE=$(exercise_loom_completion_word_reversal",
  );
  assert.ok(
    terminalFamilyWait < terminalGhostWait &&
      terminalGhostWait < idleResumeWitness &&
      idleResumeWitness < cachedInteractionWitness,
    "native hide/idle/resume must preserve the terminal ghost before cached interactions mutate it",
  );
  assert.match(smoke, /launch-1-ghost-timeout-diagnostics\.json/);
  assert.match(smoke, /latest_generation_runs/);
  assert.match(smoke, /completion diagnostics:/);
  assert.match(smoke, /restoreExactEditorFocus/);
  assert.match(smoke, /NSWorkspace\.shared\.frontmostApplication\?\.processIdentifier == pid/);
  assert.match(smoke, /observed\.hasPrefix\(expectedManuscript\)/);
  assert.match(smoke, /selection\?\.location == expectedManuscript\.utf16\.count/);
  assert.match(smoke, /did not restore exact foreground editor focus before visible completion proof/);
  assert.match(
    smoke,
    /wait_for_loom_accessibility_text \\\n+\s+"\$ACTIVE_PID" "Suggestion available\." "\$RUN_1_EDITOR_SENTINEL"/,
  );

  assert.match(smoke, /start_loom_project_busy_monitor/);
  assert.match(smoke, /another bounded project operation is still running/);
  assert.match(smoke, /project_busy_regression: projectBusyRegression/);
  assert.match(smoke, /stdout_and_stderr_scanned_after_exit/);
  assert.match(smoke, /durable project_busy command-receipt count/);

  for (const action of ["Title", "Body", "Bold", "Bulleted list", "Link", "Remove"]) {
    assert.match(
      smoke,
      new RegExp(`exercise_loom_formatting_palette[^\\n]*\\n?[^\\n]*"${action}"`),
    );
  }
  assert.match(smoke, /PID-targeted Command-A/);
  assert.match(smoke, /NSRunningApplication\.runningApplications/);
  assert.match(smoke, /foreground_loom_process "\$ACTIVE_PID"/);
  assert.match(smoke, /runningApplication\.unhide\(\)/);
  assert.match(smoke, /Activation requested only once at that boundary is lost/);
  assert.match(smoke, /System Events.*frontmost of first application process/s);
  assert.match(smoke, /same-identifier macOS activation is not PID-addressable/);
  assert.match(smoke, /DELYSIS_ACCEPTANCE_SOURCE_SHA/);
  assert.match(smoke, /acceptance source SHA requires a clean repository worktree/);
  assert.match(smoke, /DELYSIS_SMOKE_SOURCE_SHA="\$ACCEPTANCE_SOURCE_SHA"/);
  assert.match(smoke, /attribute\(format, kAXExpandedAttribute as CFString\)/);
  assert.match(smoke, /button\(named: "Title"\) != nil/);
  assert.match(smoke, /sameSemanticSelection\(beforeSelectionWitness, current\)/);
  assert.match(smoke, /sameAXSelectionSemantics\(beforeSelection, currentAX\)/);
  assert.match(smoke, /caret_byte_offset/);
  assert.match(smoke, /all_visible_text/);
  assert.match(smoke, /integer\(witness, "epoch"\)/);
  assert.match(smoke, /\.max \{ left, right in/);
  assert.match(smoke, /selection\.canonical\.location == expected\.utf16\.count/);
  assert.match(smoke, /terminal_line_break_utf16/);
  assert.match(smoke, /delysis\.loom-completion-witness\.v1/);
  assert.match(smoke, /editor_selection/);
  assert.match(smoke, /internal_selection/);
  assert.match(smoke, /bool\(witness, "caret_at_end"\)/);
  assert.match(smoke, /bool\(observedSelectionWitness, "all_visible_text"\)/);
  assert.match(smoke, /Date\(\)\.timeIntervalSince\(exactSelectionSince\) >= 0\.25/);
  assert.match(smoke, /did not stably restore the exact manuscript selection/);
  assert.match(smoke, /editor_refocused/);
  assert.match(smoke, /observed_persisted_markdown/);
});

test("Loom's required macOS lane runs the headless WebKit editor interactions", () => {
  const workflow = read(".github/workflows/ci-pr.yml");
  const macos = workflow.match(/^  platform-macos:[\s\S]*?(?=^  dependency-graph:)/m)?.[0];
  assert.ok(macos, "platform-macos job block is missing");
  assert.match(macos, /component: \$\{\{ fromJSON\(needs\.plan\.outputs\.macos_matrix\) \}\}/);
  assert.match(macos, /if: \$\{\{ matrix\.component == 'loom' \}\}[\s\S]*pnpm --filter @delysis\/loom exec playwright install webkit/);
  assert.match(macos, /name: Loom macOS[\s\S]*if: \$\{\{ matrix\.component == 'loom' \}\}[\s\S]*pnpm --filter @delysis\/loom run test:browser/);
  const required = workflow.match(/^  ci-required:[\s\S]*$/m)?.[0];
  assert.match(required, /^\s{6}- platform-macos$/m);
});

test("stable macOS packaging adds only the real distribution gates", () => {
  const release = read(releaseScriptPath);
  assert.match(release, /candidate\|stable/);
  assert.match(release, /stable releases require the exact annotated tag/);
  assert.match(release, /git cat-file -t "refs\/tags\/\$RELEASE_TAG"/);
  assert.match(release, /Developer ID Application:/);
  assert.match(release, /xcrun notarytool submit/);
  assert.match(release, /xcrun stapler staple/);
  assert.match(release, /xcrun stapler validate/);
  assert.match(release, /spctl --assess --type execute/);
  assert.match(release, /scripts\/smoke-macos-app\.sh" "\$COMPONENT" "\$ARCHIVE"/);
  assert.match(release, /delysis\.macos-release-receipt\.v2/);
  assert.doesNotMatch(release, /windows|linux/i);
});

test("supplied macOS ZIP fails closed without its exact adjacent release receipt", (t) => {
  const fixture = macSmokeFixture(t);

  let result = fixture.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /adjacent release receipt is missing/);

  for (const [field, value, expected] of [
    ["component", "loom", /release receipt component mismatch/],
    ["bundle_id", "example.invalid", /release receipt bundle ID mismatch/],
    ["archive_sha256", "0".repeat(64), /release receipt archive SHA-256 mismatch/],
    ["executable_sha256", "0".repeat(64), /release receipt executable SHA-256 mismatch/],
  ]) {
    const receipt = structuredClone(fixture.validReceipt);
    if (field === "component") receipt.component = value;
    else receipt.macos[field] = value;
    fs.writeFileSync(fixture.receipt, `${JSON.stringify(receipt)}\n`);
    result = fixture.run();
    assert.notEqual(result.status, 0, `${field} mismatch must fail`);
    assert.match(result.stderr, expected);
  }
});

for (const extension of [
  "gguf",
  "safetensors",
  "onnx",
  "pt",
  "pth",
  "ckpt",
  "mlmodel",
  "mlpackage",
]) {
  test(`extracted macOS ZIP rejects .${extension} model weights`, (t) => {
    const fixture = macSmokeFixture(t, `weights/model.${extension}`);
    fs.writeFileSync(fixture.receipt, `${JSON.stringify(fixture.validReceipt)}\n`);
    const result = fixture.run();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /model weights must remain runtime-discovered/);
  });
}

test("the packaged executable is not mistaken for model weights", (t) => {
  const fixture = macSmokeFixture(t);
  fs.writeFileSync(fixture.receipt, `${JSON.stringify(fixture.validReceipt)}\n`);
  const result = fixture.run();
  assert.notEqual(result.status, 0, "the fake unsigned app cannot complete the full smoke");
  assert.doesNotMatch(result.stderr, /model weights must remain runtime-discovered/);
});

test("the macOS builder rejects the same common model-weight formats as the archive smoke", () => {
  const releaseSource = read(releaseScriptPath);
  const smokeSource = read(smokeScriptPath);
  const scannerSource = read(embeddedModelScriptPath);
  assert.match(releaseSource, /scripts\/find-embedded-model\.mjs/);
  assert.match(smokeSource, /scripts\/find-embedded-model\.mjs/);
  for (const extension of [
    "gguf",
    "safetensors",
    "onnx",
    "pt",
    "pth",
    "ckpt",
    "mlmodel",
    "mlpackage",
  ]) {
    assert.ok(scannerSource.includes(`".${extension}"`));
  }
  assert.match(scannerSource, /Buffer\.from\("GGUF"\)/);
});

test("macOS remote candidates are tag or manual artifacts and never PR requirements", () => {
  const source = read(releasePath);
  assert.match(source, /^\s+tags:\s*$/m);
  assert.match(source, /^\s+workflow_dispatch:\s*$/m);
  assert.doesNotMatch(source, /^\s+pull_request:/m);
  assert.match(source, /^\s+package:\s*$/m);
  assert.match(source, /^\s+runs-on: macos-latest$/m);
  assert.match(source, /\.\/scripts\/release-macos\.sh/);
  assert.match(source, /release-macos\.sh "\$\{\{ steps\.component\.outputs\.component \}\}" candidate/);
  assert.match(source, /actions\/upload-artifact@[0-9a-f]{40}/);
  assert.match(source, /release tag\/version mismatch/);
  assert.match(source, /expected_tag="\$tag_prefix-v\$version"/);
  assert.match(source, /remote-candidate-/);
});

test("PR workflow is always triggered and has one truthful aggregate", () => {
  const source = read(prPath);
  assert.match(source, /^on:\n\s+pull_request:\s*$/m);
  assert.doesNotMatch(source, /^\s+paths(?:-ignore)?:/m);
  assert.match(source, /^\s{2}ci-required:\n\s{4}name: ci-required$/m);
  assert.match(source, /^\s{4}if: always\(\)$/m);
  assert.match(source, /CI_NEEDS_JSON:\s*\$\{\{ toJSON\(needs\) \}\}/);
  assert.match(source, /node scripts\/ci\/ci-required\.mjs/);
  assert.match(
    source,
    /node --test scripts\/ci\/test-ci-metadata-shadow\.mjs scripts\/ci\/test-ci-plan\.mjs scripts\/ci\/test-ci-required\.mjs scripts\/ci\/test-ignored-tests\.mjs scripts\/ci\/test-product-state-backup\.mjs scripts\/ci\/test-workflows\.mjs/,
  );
});

test("required job names and workflow matrices match the checked-in R3 snapshot", () => {
  const snapshot = JSON.parse(read(workflowSnapshotPath));
  const pr = read(prPath);
  const full = read(fullPath);
  assert.equal(snapshot.schema, "native-platform.ci-workflow-snapshot.v1");
  assert.match(pr, new RegExp(`^name: ${snapshot.pr.workflow_name}$`, "m"));
  assert.match(full, new RegExp(`^name: ${snapshot.full.workflow_name}$`, "m"));
  assert.deepEqual(workflowJobIds(pr), snapshot.pr.job_ids);
  assert.deepEqual(workflowJobIds(full), snapshot.full.job_ids);
  assert.match(
    pr,
    new RegExp(
      `^  ${snapshot.pr.required_check}:\\n    name: ${snapshot.pr.required_check}$`,
      "m",
    ),
  );

  const commandMatrices = [...pr.matchAll(/^\s+command: \[([^\]]+)\]$/gm)].map(
    (match) => match[1].split(",").map((value) => value.trim()),
  );
  assert.ok(commandMatrices.length > 0);
  for (const matrix of commandMatrices) {
    assert.deepEqual(matrix, snapshot.pr.command_matrix);
  }

  const ignoredBlock = pr.match(/^  ignored-tests:[\s\S]*?(?=^  fuzz-build:)/m)?.[0];
  const ignoredMatrix = ignoredBlock
    ?.match(/^\s+os: \[([^\]]+)\]$/m)?.[1]
    .split(",")
    .map((value) => value.trim());
  assert.deepEqual(ignoredMatrix, snapshot.pr.ignored_test_os_matrix);

  const macosComponents = [
    ...new Set(
      [...pr.matchAll(/matrix\.component (?:==|!=) '([^']+)'/g)].map(
        (match) => match[1],
      ),
    ),
  ].sort();
  assert.deepEqual(macosComponents, [...snapshot.pr.macos_component_superset].sort());

  const fullMatrices = [...full.matchAll(/^\s+os: \[([^\]]+)\]$/gm)].map(
    (match) => match[1].split(",").map((value) => value.trim()),
  );
  assert.ok(fullMatrices.length > 0);
  for (const matrix of fullMatrices) assert.deepEqual(matrix, snapshot.full.os_matrix);
});

test("full CI reconciles each current-platform ignored-test subset through guarded listing", () => {
  const source = read(fullPath);
  assert.match(
    source,
    /name: Reconcile ignored-test evidence registry with guarded list arguments/,
  );
  assert.match(source, /os: \[ubuntu-latest, macos-latest, windows-latest\]/);
  const reconciliation = source.match(
    /- name: Reconcile ignored-test evidence registry[\s\S]*?--cargo-list/,
  )?.[0];
  assert.ok(reconciliation, "full ignored-test reconciliation step is missing");
  assert.doesNotMatch(reconciliation, /if: runner\.os/);
  assert.match(source, /node scripts\/ci\/validate-ignored-tests\.mjs --cargo-list/);
  assert.doesNotMatch(source, /without executing test bodies/);
});

test("relevant PRs require exact guarded-list ignored-test reconciliation", () => {
  const source = read(prPath);
  assert.match(
    source,
    /^\s{6}ignored_tests: \$\{\{ steps\.plan\.outputs\.ignored_tests \}\}$/m,
  );
  const block = source.match(/^  ignored-tests:[\s\S]*?(?=^  fuzz-build:)/m)?.[0];
  assert.ok(block, "ignored-tests PR job block is missing");
  assert.match(block, /fail-fast: false/);
  assert.match(block, /os: \[ubuntu-latest, macos-latest, windows-latest\]/);
  assert.match(block, /runs-on: \$\{\{ matrix\.os \}\}/);
  assert.match(block, /if: runner\.os == 'Windows'/);
  assert.match(block, /if: runner\.os == 'Linux'/);
  assert.match(block, /needs\.plan\.outputs\.ignored_tests == 'true'/);
  assert.match(
    block,
    /name: Reconcile exact ignored-test inventory with guarded list arguments/,
  );
  assert.match(block, /node scripts\/ci\/validate-ignored-tests\.mjs --cargo-list/);
  assert.doesNotMatch(block, /without executing test bodies/);
  assert.doesNotMatch(block, /cargo test[^\n]*--ignored(?! --list)/);
  const required = source.match(/^  ci-required:[\s\S]*$/m)?.[0];
  assert.match(required, /^\s{6}- ignored-tests$/m);
});

test("PR workflow exposes every targeted partition and future product guards", () => {
  const source = read(prPath);
  for (const job of [
    "plan",
    "policy",
    "root-linux",
    "native-linux",
    "gateway-linux",
    "attachment-linux",
    "information-linux",
    "information-windows",
    "speech-linux",
    "mom-linux",
    "mom-windows",
    "loom-linux",
    "loom-windows",
    "frontend",
    "platform-macos",
    "ignored-tests",
    "dependency-graph",
    "fuzz-build",
  ]) {
    assert.match(source, new RegExp(`^  ${job}:`, "m"), `missing ${job}`);
  }
  assert.match(source, /mom_present == 'true'/);
  assert.match(source, /loom_present == 'true'/);
  assert.match(source, /group: ci-pr-/);
  assert.match(source, /cancel-in-progress: true/);
  assert.doesNotMatch(source, /^  platform-windows:/m);
  assert.doesNotMatch(source, /platform_windows/);
});

test("Information changes run their portable tests on Windows before merge", () => {
  const source = read(prPath);
  const windows = source.match(
    /^  information-windows:[\s\S]*?(?=^  speech-linux:)/m,
  )?.[0];
  assert.ok(windows, "information-windows job block is missing");
  assert.match(
    windows,
    /if: \$\{\{ needs\.plan\.outputs\.information == 'true' \|\| needs\.plan\.outputs\.full == 'true' \}\}/,
  );
  assert.match(windows, /runs-on: windows-latest/);
  assert.match(windows, /git config --global core\.longpaths true/);
  assert.match(
    windows,
    /node scripts\/ci\/cargo-group\.mjs test service-information/,
  );
  const required = source.match(/^  ci-required:[\s\S]*$/m)?.[0];
  assert.match(required, /^\s{6}- information-windows$/m);
});

test("Mom changes run their product tests on Windows before merge", () => {
  const source = read(prPath);
  const windows = source.match(/^  mom-windows:[\s\S]*?(?=^  loom-linux:)/m)?.[0];
  assert.ok(windows, "mom-windows job block is missing");
  assert.match(
    windows,
    /if: \$\{\{ needs\.plan\.outputs\.mom_present == 'true' && \(needs\.plan\.outputs\.mom == 'true' \|\| needs\.plan\.outputs\.full == 'true'\) \}\}/,
  );
  assert.match(windows, /runs-on: windows-latest/);
  assert.match(windows, /git config --global core\.longpaths true/);
  assert.match(windows, /node scripts\/ci\/cargo-group\.mjs test product-mom/);
  const required = source.match(/^  ci-required:[\s\S]*$/m)?.[0];
  assert.match(required, /^\s{6}- mom-windows$/m);
});

test("Loom changes run their product tests on Windows before merge", () => {
  const source = read(prPath);
  const windows = source.match(/^  loom-windows:[\s\S]*?(?=^  frontend:)/m)?.[0];
  assert.ok(windows, "loom-windows job block is missing");
  assert.match(
    windows,
    /if: \$\{\{ needs\.plan\.outputs\.loom_present == 'true' && \(needs\.plan\.outputs\.loom == 'true' \|\| needs\.plan\.outputs\.full == 'true'\) \}\}/,
  );
  assert.match(windows, /runs-on: windows-latest/);
  assert.match(windows, /git config --global core\.longpaths true/);
  assert.match(windows, /node scripts\/ci\/cargo-group\.mjs test product-loom/);
  const required = source.match(/^  ci-required:[\s\S]*$/m)?.[0];
  assert.match(required, /^\s{6}- loom-windows$/m);
});

test("root workspace tests can inspect the retained migration evidence", () => {
  const rootLinux = read(prPath).match(/^  root-linux:[\s\S]*?(?=^  native-linux:)/m)?.[0];
  assert.ok(rootLinux, "root-linux job block is missing");
  assert.match(rootLinux, /actions\/checkout@[0-9a-f]{40}\n\s+with:\n\s+fetch-depth: 0/);
});

test("long Linux lanes parallelize test and Clippy without dropping either", () => {
  const source = read(prPath);
  const blocks = [
    source.match(/^  root-linux:[\s\S]*?(?=^  native-linux:)/m)?.[0],
    source.match(/^  mom-linux:[\s\S]*?(?=^  mom-windows:)/m)?.[0],
    source.match(/^  loom-linux:[\s\S]*?(?=^  loom-windows:)/m)?.[0],
  ];
  for (const block of blocks) {
    assert.ok(block, "parallel Linux job block is missing");
    assert.match(block, /fail-fast: false/);
    assert.match(block, /command: \[test, clippy\]/);
    assert.match(block, /save-if: \$\{\{ matrix\.command == 'test' \}\}/);
    assert.match(block, /if: \$\{\{ matrix\.command == 'test' \}\}/);
    assert.match(block, /if: \$\{\{ matrix\.command == 'clippy' \}\}/);
  }
  assert.match(blocks[0], /cargo test --locked --workspace --all-targets/);
  assert.match(blocks[0], /cargo clippy --locked --workspace --all-targets -- -D warnings/);
  assert.match(
    blocks[0],
    /components: \$\{\{ matrix\.command == 'test' && 'rustfmt' \|\| 'clippy' \}\}/,
  );
  assert.match(blocks[1], /cargo-group\.mjs test product-mom/);
  assert.match(blocks[1], /cargo-group\.mjs clippy product-mom/);
  assert.match(blocks[2], /cargo-group\.mjs test product-loom/);
  assert.match(blocks[2], /cargo-group\.mjs clippy product-loom/);
});

test("PR frontend runs only selected product frontend commands", () => {
  const prFrontend = read(prPath).match(/^  frontend:[\s\S]*?(?=^  platform-macos:)/m)?.[0];
  assert.ok(prFrontend, "PR frontend job block is missing");
  assert.match(prFrontend, /pnpm install --frozen-lockfile/);
  assert.doesNotMatch(prFrontend, /pnpm -r/);
  assert.doesNotMatch(prFrontend, /apt-get|libwebkit2gtk|dtolnay\/rust-toolchain/);
  assert.match(prFrontend, /name: FTE frontend/);
  assert.match(prFrontend, /pnpm --filter free-token-energy run check:frontend/);
  assert.match(prFrontend, /pnpm --filter free-token-energy run test:frontend/);
  assert.doesNotMatch(
    prFrontend,
    /free-token-energy run (?:build|check|check:rust|test|test:rust)\s*$/m,
  );
  assert.match(prFrontend, /name: Mom frontend/);
  assert.match(prFrontend, /pnpm --filter @delysis\/mom-llama run check:frontend/);
  assert.match(prFrontend, /pnpm --filter @delysis\/mom-llama run test:frontend/);
  assert.match(prFrontend, /name: Loom frontend/);
  assert.match(prFrontend, /pnpm --filter @delysis\/loom run test/);
  assert.match(prFrontend, /pnpm --filter @delysis\/loom run check/);
  assert.match(prFrontend, /pnpm --filter @delysis\/loom run build/);
  for (const flag of ["frontend_fte", "frontend_mom", "frontend_loom"]) {
    assert.match(prFrontend, new RegExp(`needs\\.plan\\.outputs\\.${flag}`));
  }
});

test("Mom exposes the frontend syntax check used by PR CI", () => {
  const scripts = JSON.parse(read(momPackagePath)).scripts;
  assert.equal(
    scripts["check:frontend"],
    "node --check ui/composer-key-policy.js && node --check ui/coop-hx.js && node --check ui/product-surface-contract.test.js",
  );
  assert.match(scripts["test:frontend"], /(?:^|\s)ui\/persona-sidebar-wiring\.test\.js(?:\s|$)/);
});

test("PR and full CI enforce current service documentation paths", () => {
  const pr = read(prPath);
  const full = read(fullPath);
  const prPolicy = pr.match(/^  policy:[\s\S]*?(?=^  root-linux:)/m)?.[0];
  const fullRoot = full.match(/^  root:[\s\S]*?(?=^  frontend:)/m)?.[0];
  for (const block of [prPolicy, fullRoot]) {
    assert.ok(block, "documentation policy job block is missing");
    assert.match(block, /node --test scripts\/ci\/test-current-docs\.mjs/);
    assert.match(block, /node scripts\/ci\/validate-current-docs\.mjs/);
  }
  assert.match(fullRoot, /if: runner\.os == 'Linux'/);
});

test("full CI executes browser coverage and has no empty Information platform lane", () => {
  const full = read(fullPath);
  const loom = full.match(/^  root:[\s\S]*?(?=^  frontend:)/m)?.[0];
  assert.ok(loom, "full Loom job is missing");
  assert.match(loom, /playwright install webkit/);
  assert.match(loom, /pnpm --filter @delysis\/loom run test:browser/);
  assert.doesNotMatch(full, /information-platform-linux/);
});

test("full frontend coverage remains unchanged", () => {
  const fullFrontend = read(fullPath).match(/^  frontend:[\s\S]*?(?=^  policy-and-graphs:)/m)?.[0];
  assert.ok(fullFrontend, "full frontend job block is missing");
  assert.doesNotMatch(fullFrontend, /dtolnay\/rust-toolchain|apt-get/);
  assert.match(fullFrontend, /pnpm install --frozen-lockfile/);
  for (const command of [
    "pnpm --filter free-token-energy run check:frontend",
    "pnpm --filter free-token-energy run test:frontend",
    "pnpm --filter @delysis/mom-llama run check:frontend",
    "pnpm --filter @delysis/mom-llama run test:frontend",
    "pnpm --filter @delysis/loom run test",
    "pnpm --filter @delysis/loom run check",
    "pnpm --filter @delysis/loom run build",
  ]) assert.ok(fullFrontend.includes(command), command);
  assert.doesNotMatch(fullFrontend, /loom:install|--dir products\/loom/);
});

test("the required macOS matrix preserves every gate without serializing them", () => {
  const macos = read(prPath).match(/^  platform-macos:[\s\S]*?(?=^  dependency-graph:)/m)?.[0];
  const rootGraph = macos?.match(
    /- name: Root platform graph[\s\S]*?(?=\n      - name: Mom macOS parity)/,
  )?.[0];
  assert.ok(macos, "platform-macos job block is missing");
  assert.ok(rootGraph, "Root platform graph step is missing");
  assert.match(macos, /component: \$\{\{ fromJSON\(needs\.plan\.outputs\.macos_matrix\) \}\}/);
  assert.match(macos, /fail-fast: false/);
  assert.match(macos, /name: Release tooling shell syntax\n\s+if: \$\{\{ matrix\.component == 'release' \}\}\n\s+run: sh -n scripts\/release-macos\.sh scripts\/smoke-macos-app\.sh/);
  assert.match(macos, /dtolnay\/rust-toolchain@[0-9a-f]{40}\n\s+if: \$\{\{ matrix\.component != 'release' \}\}/);
  assert.match(macos, /Swatinem\/rust-cache@[0-9a-f]{40}\n\s+if: \$\{\{ matrix\.component != 'release' \}\}/);
  assert.match(macos, /shared-key: platform-macos-\$\{\{ matrix\.component \}\}/);
  assert.doesNotMatch(macos, /save-if:/);
  assert.doesNotMatch(rootGraph, /needs\.plan\.outputs\.mom/);
  assert.match(macos, /name: Mom macOS parity/);
  assert.match(macos, /matrix\.component == 'mom'/);
  assert.match(macos, /cargo-group\.mjs test product-mom/);
  for (const component of ["root", "mom", "attachment", "information", "speech", "loom"]) {
    assert.match(macos, new RegExp(`matrix\\.component == '${component}'`));
  }
  assert.doesNotMatch(macos, /unstable-w1/);
});

test("Speech Linux coverage provisions its GLib build dependencies", () => {
  const prSpeech = read(prPath).match(/^  speech-linux:[\s\S]*?(?=^  mom-linux:)/m)?.[0];
  const fullSpeech = read(fullPath).match(/^  root:[\s\S]*?(?=^  frontend:)/m)?.[0];
  for (const block of [prSpeech, fullSpeech]) {
    assert.ok(block, "Speech job block is missing");
    assert.match(block, /libglib2\.0-dev/);
    assert.match(block, /libwebkit2gtk-4\.1-dev/);
  }
  assert.match(fullSpeech, /if: runner\.os == 'Linux'/);
});

test("Mom and Loom Linux coverage provisions desktop build dependencies", () => {
  const pr = read(prPath);
  const full = read(fullPath);
  const blocks = [
    pr.match(/^  mom-linux:[\s\S]*?(?=^  mom-windows:)/m)?.[0],
    pr.match(/^  loom-linux:[\s\S]*?(?=^  loom-windows:)/m)?.[0],
    full.match(/^  root:[\s\S]*?(?=^  frontend:)/m)?.[0],
    full.match(/^  root:[\s\S]*?(?=^  frontend:)/m)?.[0],
  ];
  for (const block of blocks) {
    assert.ok(block, "product job block is missing");
    assert.match(block, /libglib2\.0-dev/);
    assert.match(block, /libgtk-3-dev/);
  }
  assert.match(blocks[2], /if: runner\.os == 'Linux'/);
  assert.match(blocks[3], /if: runner\.os == 'Linux'/);
});

test("fuzz workflows select the owned nested fuzz workspace explicitly", () => {
  for (const source of [read(prPath), read(fullPath)]) {
    assert.match(source, /^\s{2}fuzz-build:/m);
    assert.match(source, /cargo fuzz (?:build|run) --fuzz-dir crates\/services\/attachment\/fuzz inspect/);
    assert.match(source, /cargo fuzz (?:build|run) --fuzz-dir crates\/services\/attachment\/fuzz pipeline/);
  }
});

test("full workflow covers main, nightly, dispatch, products, policy, and fuzz", () => {
  const source = read(fullPath);
  assert.match(source, /^\s+push:\n\s+branches: \[main\]/m);
  assert.match(source, /^\s+schedule:/m);
  assert.match(source, /^\s+workflow_dispatch:/m);
  assert.match(source, /cargo test --locked --workspace --all-targets/);
  assert.match(source, /cargo test --locked --workspace --doc/);
  assert.match(source, /^\s{2}frontend:/m);
  assert.match(source, /^\s{2}fuzz-build:/m);
  assert.match(source, /cargo clippy --locked --workspace --all-targets -- -D warnings/);
  assert.doesNotMatch(source, /self-hosted|real[-_ ]hardware/i);
  assert.match(source, /^\s{4}if: always\(\)$/m);
});

test("Windows PR coverage is limited to selected portability and inventory gates", () => {
  const pr = read(prPath);
  const full = read(fullPath);
  const ignored = pr.match(/^  ignored-tests:[\s\S]*?(?=^  fuzz-build:)/m)?.[0];
  const information = pr.match(
    /^  information-windows:[\s\S]*?(?=^  speech-linux:)/m,
  )?.[0];
  const mom = pr.match(/^  mom-windows:[\s\S]*?(?=^  loom-linux:)/m)?.[0];
  const loom = pr.match(/^  loom-windows:[\s\S]*?(?=^  frontend:)/m)?.[0];
  assert.match(ignored, /windows-latest/);
  assert.match(information, /windows-latest/);
  assert.match(mom, /windows-latest/);
  assert.match(loom, /windows-latest/);
  assert.doesNotMatch(
    pr
      .replace(ignored, "")
      .replace(information, "")
      .replace(mom, "")
      .replace(loom, ""),
    /windows-latest/,
  );
  assert.match(full, /windows-latest/);
  assert.match(full, /ci-full-/);
});

test("all third-party actions are pinned to immutable commits", () => {
  for (const file of [prPath, fullPath, releasePath]) {
    for (const action of externalActionUses(read(file))) {
      assert.match(action, /^[^/@]+\/[^/@]+@[0-9a-f]{40}$/, `${file}: ${action}`);
    }
  }
});

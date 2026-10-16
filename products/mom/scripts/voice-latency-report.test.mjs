import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  latencyAggregate, latencyDiagnosis, latencyRow, latestSnapshots, percentile,
} from "./voice-latency-report.mjs";

test("selects only the newest durable checkpoint for each trace", () => {
  const directory = mkdtempSync(join(tmpdir(), "mom-voice-latency-"));
  const write = (name, value) => writeFileSync(join(directory, name), JSON.stringify(value));
  write("trace-a-0001-03.json", { id: "trace-a", checkpoint: 1, stages: [{ stage: "silence_onset" }] });
  write("trace-a-0002-06.json", { id: "trace-a", checkpoint: 2, stages: [{ stage: "silence_onset" }, { stage: "generation_finished" }] });
  write("trace-a-0024-117.json", { id: "trace-a", checkpoint: 24,
    stages: Array.from({ length: 117 }, (_, index) => ({
      stage: index === 116 ? "playback_started" : "playback_packet_admitted",
    })) });
  write("trace-b-0001-04.json", { id: "trace-b", checkpoint: 1, stages: [{ stage: "failed" }] });
  const snapshots = latestSnapshots(directory);
  assert.equal(snapshots.length, 2);
  assert.equal(snapshots.find(({ trace }) => trace.id === "trace-a").checkpoint, 24);
  assert.equal(snapshots.find(({ trace }) => trace.id === "trace-a").stages, 117);
});

test("reports the complete conversational latency path without inventing missing values", () => {
  const row = latencyRow({ checkpoint: 4, path: "/trace.json", trace: {
    id: "trace-a", mode: "call", began_at: 1_700_000_000_000,
    stages: [{ stage: "silence_onset" }, { stage: "playback_started" }],
    summary: { silence_to_first_content_ms: 310, first_content_to_tts_pcm_ms: 120,
      tts_pcm_to_playback_ms: 40, silence_to_playback_ms: 470,
      call_request_to_ringing_ms: 2.5, call_request_to_runtime_ready_ms: 830,
      call_request_to_microphone_ready_ms: 41, microphone_ready_to_ringing_ms: 3,
      microphone_native_first_sample_ms: 5,
      call_request_to_conversation_ready_ms: 850, call_request_to_dispatch_ms: 852,
      call_request_to_playback_ms: 1_320,
      runtime_warm_audio_projector_ms: 44,
      runtime_warm_audio_projector_ready: true,
      capture_processing_cpu_ms: 18, capture_queue_delay_max_ms: 0.7,
      audio_ring_write_ms: 1.2, audio_ring_read_ms: 0.8,
      capture_to_model_target_ms: 84, audio_backlog_ms: 40,
      projector_audio_ms: 2_000, projector_realtime_factor: 0.44,
      projector_tail_to_decode_ms: 11, sampler_build_ms: 3.1,
      audio_finish_to_decode_ms: 110, decode_to_first_token_ms: 130,
      silence_to_first_token_native_ms: 240,
      generation_finished_to_tts_pcm_ms: 170,
      barge_in_playback_stop_ms: 18,
      voice_session_prepared_before_capture: true,
      voice_session_settings_cache_hit: true, tts_segment_count: 2,
      tts_aggregate_production_rtf: 0.63, tts_worst_segment_rtf: 0.68,
      tts_pcm_packets: 9, tts_pcm_audio_ms: 1_800,
      tts_first_packet_audio_ms: 138.67, tts_max_packet_gap_ms: 117,
      tts_minimum_producer_lead_ms: 138.67,
      tts_scheduler_target_runway_ms: 240, tts_scheduler_waits: 2,
      tts_scheduler_startup_runway_ms: 180,
      tts_scheduler_observed_packet_gap_ms: 92,
      tts_scheduler_observed_boundary_gap_ms: 410,
      tts_scheduler_wait_ms: 810, tts_scheduler_maximum_runway_ms: 1_150,
      tts_scheduler_observed_rtf: 0.64, tts_scheduler_underrun_penalty_ms: 0,
      semantic_backpressure_switches: 4, semantic_backpressure_pause_ms: 840,
      semantic_backpressure_command_ms: 3.2,
      semantic_backpressure_generation_yields: 2,
      semantic_backpressure_generation_yield_ms: 832,
      semantic_backpressure_maximum_words: 24,
      tts_compute_lease_acquisitions: 3, tts_compute_lease_releases: 3,
      tts_generation_yields: 2,
      tts_generation_yield_ms: 184.25, tts_generation_fair_passes: 7,
      tts_compute_lease_held_ms: 712.5,
      tts_compute_lease_released_ms: 311.75, tts_compute_minimum_lead_ms: 126,
      tts_compute_maximum_lead_ms: 402, tts_compute_low_water_ms: 190,
      tts_compute_high_water_ms: 380,
      tts_all_segments_buffered_runway_ms: 940,
      tts_to_next_prefill_start_ms: 1, next_prefill_prepare_ms: 119,
      next_prefill_buffered_runway_ms: 821,
      next_prefill_overlapped_active_tts: false },
  } });
  assert.equal(row.terminal_stage, "playback_started");
  assert.equal(row.prefill_failure_stage, null);
  assert.equal(row.generation_finished_without_pcm, false);
  assert.equal(row.silence_to_playback_ms, 470);
  assert.equal(row.call_request_to_ringing_ms, 2.5);
  assert.equal(row.call_request_to_microphone_ready_ms, 41);
  assert.equal(row.microphone_ready_to_ringing_ms, 3);
  assert.equal(row.microphone_native_first_sample_ms, 5);
  assert.equal(row.call_request_to_runtime_ready_ms, 830);
  assert.equal(row.call_request_to_playback_ms, 1_320);
  assert.equal(row.runtime_warm_audio_projector_ms, 44);
  assert.equal(row.runtime_warm_audio_projector_ready, true);
  assert.equal(row.capture_processing_cpu_ms, 18);
  assert.equal(row.capture_queue_delay_max_ms, 0.7);
  assert.equal(row.audio_ring_write_ms, 1.2);
  assert.equal(row.audio_ring_read_ms, 0.8);
  assert.equal(row.capture_to_model_target_ms, 84);
  assert.equal(row.audio_backlog_ms, 40);
  assert.equal(row.projector_audio_ms, 2_000);
  assert.equal(row.projector_realtime_factor, 0.44);
  assert.equal(row.projector_tail_to_decode_ms, 11);
  assert.equal(row.sampler_build_ms, 3.1);
  assert.equal(row.barge_in_playback_stop_ms, 18);
  assert.equal(row.voice_session_prepared_before_capture, true);
  assert.equal(row.audio_finish_to_decode_ms, 110);
  assert.equal(row.decode_to_first_token_ms, 130);
  assert.equal(row.silence_to_first_token_native_ms, 240);
  assert.equal(row.generation_finished_to_tts_pcm_ms, 170);
  assert.equal(row.voice_session_settings_cache_hit, true);
  assert.equal(row.tts_segments, 2);
  assert.equal(row.tts_aggregate_rtf, 0.63);
  assert.equal(row.tts_worst_segment_rtf, 0.68);
  assert.equal(row.tts_pcm_packets, 9);
  assert.equal(row.tts_first_packet_audio_ms, 138.67);
  assert.equal(row.tts_max_packet_gap_ms, 117);
  assert.equal(row.tts_minimum_producer_lead_ms, 138.67);
  assert.equal(row.tts_scheduler_target_ms, 240);
  assert.equal(row.tts_scheduler_startup_ms, 180);
  assert.equal(row.tts_scheduler_packet_gap_ms, 92);
  assert.equal(row.tts_scheduler_boundary_gap_ms, 410);
  assert.equal(row.tts_scheduler_waits, 2);
  assert.equal(row.tts_scheduler_wait_ms, 810);
  assert.equal(row.tts_scheduler_max_runway_ms, 1_150);
  assert.equal(row.tts_scheduler_observed_rtf, 0.64);
  assert.equal(row.tts_scheduler_underrun_penalty_ms, 0);
  assert.equal(row.semantic_backpressure_switches, 4);
  assert.equal(row.semantic_backpressure_pause_ms, 840);
  assert.equal(row.semantic_backpressure_command_ms, 3.2);
  assert.equal(row.semantic_backpressure_generation_yields, 2);
  assert.equal(row.semantic_backpressure_generation_yield_ms, 832);
  assert.equal(row.semantic_backpressure_maximum_words, 24);
  assert.equal(row.tts_compute_lease_acquisitions, 3);
  assert.equal(row.tts_compute_lease_releases, 3);
  assert.equal(row.tts_generation_yields, 2);
  assert.equal(row.tts_generation_yield_ms, 184.25);
  assert.equal(row.tts_generation_fair_passes, 7);
  assert.equal(row.tts_compute_lease_held_ms, 712.5);
  assert.equal(row.tts_compute_lease_released_ms, 311.75);
  assert.equal(row.tts_compute_minimum_lead_ms, 126);
  assert.equal(row.tts_compute_maximum_lead_ms, 402);
  assert.equal(row.tts_compute_low_water_ms, 190);
  assert.equal(row.tts_compute_high_water_ms, 380);
  assert.equal(row.tts_final_buffered_ms, 940);
  assert.equal(row.next_prefill_prepare_ms, 119);
  assert.equal(row.next_prefill_overlapped_tts, false);
  assert.equal(row.projector_cpu_ms, null);
});

test("repairs legacy silence latency from the detector decision window", () => {
  const row = latencyRow({ checkpoint: 1, path: "/legacy.json", trace: {
    id: "legacy", mode: "call", stages: [
      { stage: "silence_onset", silence_ms: 250.6875 },
      { stage: "playback_started" },
    ],
    summary: {
      silence_to_first_content_ms: 188.06,
      silence_to_first_token_native_ms: 228.5,
      silence_to_tts_pcm_ms: 514,
      silence_to_playback_ms: 550,
    },
  } });

  assert.equal(row.silence_decision_delay_ms, 250.6875);
  assert.equal(row.silence_release_to_first_content_ms, 188.06);
  assert.equal(row.silence_to_first_content_ms, 438.75);
  assert.equal(row.silence_release_to_first_token_native_ms, 228.5);
  assert.equal(row.silence_to_first_token_native_ms, 479.19);
  assert.equal(row.silence_release_to_tts_pcm_ms, 514);
  assert.equal(row.silence_release_to_playback_ms, 550);
  assert.equal(row.silence_to_playback_ms, 800.69);
});

test("preserves absent metrics instead of reporting false zeroes", () => {
  const row = latencyRow({ checkpoint: 1, path: "/trace.json", trace: {
    id: "trace-a", stages: [{ stage: "failed" }], summary: {
      silence_to_playback_ms: null,
    },
  } });
  assert.equal(row.silence_to_playback_ms, null);
  assert.equal(row.tts_helper_first_pcm_ms, null);
  assert.equal(row.tts_aggregate_rtf, null);
  assert.equal(row.next_prefill_overlapped_tts, null);
  assert.equal(row.transcript_ready_before_next_prefill, null);
  assert.equal(row.capture_stop_to_transcript_committed_ms, null);
});

test("shows whether complete transcript replacement preceded the next prefill", () => {
  const row = (committedAt) => latencyRow({ checkpoint: 1, path: "/turn.json", trace: {
    id: "turn", mode: "call", stages: [
      { stage: "capture_stopped", elapsed_ms: 100 },
      { stage: "voice_transcript_live_verified", elapsed_ms: 145 },
      { stage: "next_turn_prefill_started", elapsed_ms: 200 },
      { stage: "voice_transcript_finished", elapsed_ms: committedAt },
    ], summary: {},
  } });
  assert.equal(row(180).capture_stop_to_transcript_verified_ms, 45);
  assert.equal(row(180).capture_stop_to_transcript_committed_ms, 80);
  assert.equal(row(180).transcript_ready_before_next_prefill, true);
  assert.equal(row(180).transcript_to_next_prefill_ms, 20);
  assert.equal(row(240).transcript_ready_before_next_prefill, false);
  assert.equal(row(240).transcript_to_next_prefill_ms, -40);
});

test("diagnoses a fallen-back voice turn that finishes without PCM", () => {
  const row = latencyRow({ checkpoint: 4, path: "/trace.json", trace: {
    id: "trace-a", mode: "voice_note", stages: [
      { stage: "audio_prefill_begin_failed", message: "voice_audio_begin_panicked:prepare" },
      { stage: "generation_finished" },
    ], summary: {},
  } });
  assert.equal(row.prefill_failure_stage, "audio_prefill_begin_failed");
  assert.equal(row.prefill_failure_message, "voice_audio_begin_panicked:prepare");
  assert.equal(row.generation_finished_without_pcm, true);
  assert.deepEqual(latencyDiagnosis(row).findings, [
    "audio_prefill_failed", "response_without_pcm",
  ]);
});

test("identifies a call that abandoned a pending prefill instead of silently waiting", () => {
  const row = latencyRow({ checkpoint: 1, path: "/call.json", trace: {
    id: "call", mode: "call", stages: [
      { stage: "capture_started" },
      { stage: "voice_activity_first_detected" },
      { stage: "audio_prefill_wait_expired", silence_ms: 900, retry: false },
      { stage: "fallback_capture_encoded" },
    ], summary: {},
  } });
  assert.equal(row.prefill_failure_stage, "audio_prefill_wait_expired");
  assert.equal(row.prefill_wait_expired_ms, 900);
  assert.equal(row.prefill_wait_expired_on_retry, false);
  assert.ok(latencyDiagnosis(row).findings.includes("audio_prefill_wait_expired"));
});

test("locates a stalled audio push drain before the recorded-audio fallback", () => {
  const row = latencyRow({ checkpoint: 1, path: "/call.json", trace: {
    id: "call", mode: "call", stages: [
      { stage: "prefill_flush_started", elapsed_ms: 180 },
      { stage: "audio_prefill_drain_failed", elapsed_ms: 900,
        message: "voice_audio_prefill_drain_timeout" },
      { stage: "fallback_capture_encoded", elapsed_ms: 910 },
    ], summary: {},
  } });
  assert.equal(row.prefill_drain_to_failure_ms, 720);
  assert.equal(row.prefill_failure_stage, "audio_prefill_drain_failed");
  assert.ok(latencyDiagnosis(row).findings.includes("audio_prefill_drain_stalled"));
});

test("reports a bounded browser prefill queue overflow without claiming audio loss", () => {
  const row = latencyRow({ checkpoint: 1, path: "/call.json", trace: {
    id: "call", mode: "call", stages: [
      { stage: "audio_prefill_producer_overflow", queued_samples: 480_000,
        incoming_samples: 640, limit_samples: 480_000 },
      { stage: "fallback_capture_encoded" },
    ], summary: {},
  } });
  assert.equal(row.prefill_producer_overflow_samples, 480_000);
  assert.equal(row.prefill_producer_limit_samples, 480_000);
  assert.ok(latencyDiagnosis(row).findings.includes("audio_prefill_producer_overflow"));
});

test("a pickup completion inside the next capture is not a missing call reply", () => {
  const row = latencyRow({ checkpoint: 4, path: "/trace.json", trace: {
    id: "next-capture", mode: "call", stages: [
      { stage: "capture_started" },
      { stage: "generation_finished", purpose: "call_greeting", completion_tokens: 11 },
      { stage: "audio_prefill_begin_requested" },
    ], summary: { generation_completion_tokens: 11, generation_cache_reused: false },
  } });
  assert.equal(row.terminal_stage, "audio_prefill_begin_requested");
  assert.equal(row.generation_completion_tokens, null);
  assert.equal(row.generation_cache_reused, null);
  assert.equal(row.generation_finished_without_pcm, false);
  assert.ok(!latencyDiagnosis(row).findings.includes("response_without_pcm"));
});

test("classifies a bounded generation stall as a terminal failure", () => {
  const row = latencyRow({ checkpoint: 2, path: "/trace.json", trace: {
    id: "trace-timeout", mode: "call", stages: [
      { stage: "silence_onset" },
      { stage: "generation_timeout", phase: "call_turn", timeout_ms: 10_000 },
    ], summary: {},
  } });
  assert.equal(row.terminal_stage, "generation_timeout");
  assert.deepEqual(latencyDiagnosis(row).findings, ["generation_timeout"]);
});

test("locates a generation timeout inside sampling, emission, or continuation decode", () => {
  const base = {
    terminal_stage: "generation_timeout",
    voice_session_model_resident_hit: null,
    runtime_warm_audio_projector_ready: null,
    prefill_failure_stage: null,
    generation_finished_without_pcm: false,
    speculation_handoff_checkpoint_available: null,
    speculation_handoff_round_trip_ms: null,
    voice_cancel_to_observed_ms: null,
    capture_prefill_pushes: null,
    projector_blocks: null,
    playback_underruns: 0,
    tts_aggregate_rtf: null,
  };
  assert.ok(latencyDiagnosis({
    ...base,
    generation_sampled_tokens: 12,
    generation_emitted_token_events: 8,
    generation_continuation_decode_inflight: true,
  }).findings.includes("generation_decode_stalled"));
  assert.ok(latencyDiagnosis({
    ...base,
    generation_sampled_tokens: 12,
    generation_emitted_token_events: 0,
    generation_continuation_decode_inflight: false,
  }).findings.includes("generation_tokens_not_emitted"));
  assert.ok(latencyDiagnosis({
    ...base,
    generation_sampled_tokens: 12,
    generation_emitted_token_events: 8,
    generation_continuation_decode_inflight: false,
  }).findings.includes("generation_throughput_stall"));
});

test("diagnoses a slow resumed-speech rebuild that missed its resident checkpoint", () => {
  const row = latencyRow({ checkpoint: 2, path: "/trace.json", trace: {
    id: "trace-resume", mode: "call", stages: [
      { stage: "speculation_handoff_ready" },
    ], summary: {
      speculation_handoff_round_trip_ms: 2_170,
      speculation_handoff_resume_wait_ms: 2_168.22,
      speculation_handoff_restore_ms: 2_158.35,
      speculation_handoff_checkpoint_available: false,
      speculation_handoff_checkpoint_reused: false,
      speculation_handoff_replay_samples: 54_896,
      speculation_handoff_replay_ms: 0.13,
      voice_cancel_to_observed_ms: 658.11,
    },
  } });
  assert.equal(row.speculation_handoff_round_trip_ms, 2_170);
  assert.equal(row.speculation_handoff_restore_ms, 2_158.35);
  assert.deepEqual(latencyDiagnosis(row).findings, [
    "voice_resume_checkpoint_missed", "voice_resume_slow", "voice_cancel_slow",
  ]);
});

test("derives aggregate TTS production rate from older per-segment traces", () => {
  const row = latencyRow({ checkpoint: 3, path: "/trace.json", trace: {
    id: "trace-a", stages: [
      { stage: "tts_segment_finished", total_ms: 1_355, output_audio_ms: 1_952 },
      { stage: "tts_segment_finished", total_ms: 4_276, output_audio_ms: 2_949 },
      { stage: "playback_underrun", occurrences: 2 },
    ], summary: {},
  } });
  assert.equal(row.tts_segments, 2);
  assert.equal(row.tts_total_synthesis_ms, 5_631);
  assert.equal(row.tts_total_output_audio_ms, 4_901);
  assert.equal(row.tts_aggregate_rtf, 1.1489);
  assert.equal(row.tts_worst_segment_rtf, 1.45);
});

test("recovers native critical-path timing from an older generation receipt", () => {
  const row = latencyRow({ checkpoint: 5, path: "/trace.json", trace: {
    id: "trace-native", mode: "call", stages: [
      { stage: "generation_finished", elapsed_ms: 10_000, first_token_ms: 5_280,
        voice_audio: { finish_us: 5_000_000, decode_start_us: 5_110_000,
          begin: { total_us: 80 } } },
      { stage: "tts_first_pcm", elapsed_ms: 10_140 },
    ], summary: {},
  } });
  assert.equal(row.audio_finish_to_decode_ms, 110);
  assert.equal(row.decode_to_first_token_ms, 170);
  assert.equal(row.silence_to_first_token_native_ms, 280);
  assert.equal(row.voice_session_prepare_native_ms, 0.08);
  assert.equal(row.generation_finished_to_tts_pcm_ms, 140);
});

test("diagnoses the measured critical path without inventing absent stages", () => {
  const complete = latencyDiagnosis({
    mode: "call",
    silence_to_first_content_ms: 620,
    first_content_to_tts_pcm_ms: 170,
    tts_pcm_to_playback_ms: 230,
    silence_to_playback_ms: 1_020,
    voice_session_model_resident_hit: true,
    runtime_warm_audio_projector_ready: true,
    call_request_to_playback_ms: 4_800,
    capture_prefill_pushes: 146,
    capture_speculation_resume_cancels: 4,
    projector_blocks: 2,
    playback_underruns: 1,
    tts_aggregate_rtf: 0.62,
  });
  assert.equal(complete.dominant_stage, "silence_to_first_content");
  assert.equal(complete.dominant_stage_ms, 620);
  assert.equal(complete.critical_path_accounted_ms, 1_020);
  assert.equal(complete.critical_path_unaccounted_ms, 0);
  assert.equal(complete.subsecond, false);
  assert.deepEqual(complete.findings, [
    "speculation_churn", "playback_underrun", "conversation_target_missed",
  ]);

  const partial = latencyDiagnosis({
    silence_to_first_content_ms: null,
    first_content_to_tts_pcm_ms: null,
    tts_pcm_to_playback_ms: null,
    silence_to_playback_ms: null,
    voice_session_model_resident_hit: false,
    runtime_warm_audio_projector_ready: false,
    call_request_to_playback_ms: 7_200,
    capture_prefill_pushes: 0,
    projector_blocks: 0,
    playback_underruns: null,
    tts_aggregate_rtf: null,
  });
  assert.equal(partial.dominant_stage, null);
  assert.equal(partial.critical_path_accounted_ms, null);
  assert.equal(partial.subsecond, null);
  assert.deepEqual(partial.findings, [
    "model_not_resident", "audio_projector_not_warmed", "continuous_prefill_absent",
    "audio_projector_absent", "call_pickup_target_missed",
  ]);
});

test("identifies cold greeting prefill without calling it model loading", () => {
  const diagnosis = latencyDiagnosis({
    mode: "call_greeting",
    call_prefill_to_first_content_ms: 3_560,
    generation_cache_reused: false,
    call_request_to_playback_ms: 6_470,
    silence_to_first_content_ms: null,
    first_content_to_tts_pcm_ms: 360,
    tts_pcm_to_playback_ms: 20,
    silence_to_playback_ms: null,
    voice_session_model_resident_hit: true,
    runtime_warm_audio_projector_ready: true,
    capture_prefill_pushes: null,
    projector_blocks: null,
    playback_underruns: 0,
    tts_aggregate_rtf: .58,
  });
  assert.deepEqual(diagnosis.findings, [
    "call_pickup_target_missed", "call_greeting_cold_prefix",
  ]);
});

test("aggregates the latency scoreboard without treating missing metrics as zero", () => {
  assert.equal(percentile([100, 200, 300, 400], 0.5), 250);
  const aggregate = latencyAggregate([
    { mode: "call", terminal_stage: "playback_started", subsecond: true,
      call_request_to_playback_ms: 1_240, runtime_warm_audio_projector_ms: 40,
      silence_to_first_token_native_ms: 180, silence_to_playback_ms: 620,
      playback_underruns: 0, findings: [] },
    { mode: "call", terminal_stage: "failed", subsecond: null,
      call_request_to_playback_ms: null, runtime_warm_audio_projector_ms: 55,
      silence_to_first_token_native_ms: 320, silence_to_playback_ms: null,
      playback_underruns: 2, findings: ["playback_underrun"] },
    { mode: "voice_note", terminal_stage: "playback_started", subsecond: false,
      silence_to_first_token_native_ms: null, silence_to_playback_ms: 1_240,
      playback_underruns: null, findings: ["conversation_target_missed"] },
  ]);
  assert.equal(aggregate.traces, 3);
  assert.deepEqual(aggregate.modes.call, { traces: 2, playback_started: 1, subsecond: 1 });
  assert.equal(aggregate.metrics.silence_to_first_token_native_ms.samples, 2);
  assert.equal(aggregate.metrics.call_request_to_playback_ms.samples, 1);
  assert.equal(aggregate.metrics.runtime_warm_audio_projector_ms.samples, 2);
  assert.equal(aggregate.metrics.silence_to_first_token_native_ms.p50, 250);
  assert.equal(aggregate.metrics.silence_to_playback_ms.samples, 2);
  assert.equal(aggregate.metrics.playback_underruns.samples, 2);
  assert.deepEqual(aggregate.findings, {
    playback_underrun: 1,
    conversation_target_missed: 1,
  });
});

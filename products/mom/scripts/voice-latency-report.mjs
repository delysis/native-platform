#!/usr/bin/env node

import { readdirSync, readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

// Checkpoint and stage counters are minimum-width padded, not bounded-width.
// Long calls routinely exceed 99 recorded stages; rejecting the third digit
// silently selected an older snapshot and erased TTS/playback from reports.
const historyName = /^(.+)-(\d{4,})-(\d{2,})\.json$/;

function number(value) {
  if (value === null || value === undefined || value === "") return null;
  return Number.isFinite(Number(value)) ? Number(value) : null;
}

export function latestSnapshots(directory) {
  const latest = new Map();
  for (const name of readdirSync(directory)) {
    const match = historyName.exec(name);
    if (!match) continue;
    const path = join(directory, name);
    const trace = JSON.parse(readFileSync(path, "utf8"));
    const candidate = {
      path,
      trace,
      checkpoint: number(trace.checkpoint) ?? Number(match[2]),
      stages: Array.isArray(trace.stages) ? trace.stages.length : Number(match[3]),
      modified_ms: statSync(path).mtimeMs,
    };
    const prior = latest.get(trace.id);
    if (!prior || candidate.checkpoint > prior.checkpoint
        || (candidate.checkpoint === prior.checkpoint && candidate.stages > prior.stages)
        || (candidate.checkpoint === prior.checkpoint && candidate.stages === prior.stages
          && candidate.modified_ms > prior.modified_ms)) latest.set(trace.id, candidate);
  }
  return [...latest.values()].sort((left, right) => right.modified_ms - left.modified_ms);
}

export function latencyRow(snapshot) {
  const { trace } = snapshot;
  const summary = trace.summary ?? {};
  // The renderer's summary is the durable measurement contract. Preserve
  // every scalar it emits so adding a ring, VAD, projector, sampler, or
  // playback metric cannot silently make that measurement disappear here.
  // Curated fields below still reconstruct values for older trace formats.
  const scalarSummary = Object.fromEntries(Object.entries(summary).filter(([key, value]) => (
    /^[a-z0-9_]+$/.test(key)
      && (value === null || ["number", "string", "boolean"].includes(typeof value))
  )));
  const stages = Array.isArray(trace.stages) ? trace.stages : [];
  const lastStage = (name) => [...stages].reverse().find(({ stage }) => stage === name) ?? null;
  const belongsToThisTurn = ({ stage, purpose }) =>
    stage !== "generation_finished"
      || purpose !== "call_greeting"
      || trace.mode === "call_greeting";
  const generationStage = [...stages].reverse().find((entry) =>
    entry.stage === "generation_finished" && belongsToThisTurn(entry)) ?? null;
  const foreignGreetingOnly = !generationStage && stages.some((entry) =>
    entry.stage === "generation_finished" && !belongsToThisTurn(entry));
  const nativeAudio = generationStage?.voice_audio ?? null;
  const preciseMs = (name) => {
    const precise = number(nativeAudio?.[`${name}_us`]);
    if (precise !== null) return Math.round(precise / 10) / 100;
    return number(nativeAudio?.[`${name}_ms`]);
  };
  const nativeDelta = (from, to) => from !== null && to !== null && to >= from
    ? Math.round((to - from) * 100) / 100 : null;
  const nativeFinish = preciseMs("finish");
  const nativeDecode = preciseMs("decode_start");
  const generationFirstToken = number(generationStage?.first_token_ms);
  const generationIndex = generationStage ? stages.indexOf(generationStage) : -1;
  const firstPcmAfterGeneration = generationIndex >= 0
    ? stages.slice(generationIndex + 1).find(({ stage }) => stage === "tts_first_pcm") ?? null
    : null;
  const generationToPcm = generationStage && firstPcmAfterGeneration
    ? nativeDelta(number(generationStage.elapsed_ms), number(firstPcmAfterGeneration.elapsed_ms))
    : null;
  const terminal = [...stages].reverse().find((entry) => belongsToThisTurn(entry) && [
    "playback_started", "generation_finished", "failed", "durable_voice_audio_failed",
    "native_capture_failed", "generation_timeout",
  ].includes(entry.stage))?.stage ?? stages.at(-1)?.stage ?? null;
  const pick = (name) => number(summary[name]);
  const silenceStage = lastStage("silence_onset");
  const persistedSilenceDecisionMs = pick("silence_decision_delay_ms");
  const silenceDecisionMs = persistedSilenceDecisionMs
    ?? Math.max(0, number(silenceStage?.silence_ms) ?? 0);
  // Traces written before the physical-silence contract used `silence_to_*`
  // for the later endpoint-release edge. Preserve that internal timing under
  // `silence_release_to_*`, and repair the user-visible elapsed time when the
  // detector recorded its decision window.
  const silenceMetric = (physicalName) => {
    const physical = pick(physicalName);
    if (persistedSilenceDecisionMs !== null || physical === null) return physical;
    return Math.round((physical + silenceDecisionMs) * 100) / 100;
  };
  const silenceReleaseMetric = (releaseName, legacyName) => (
    pick(releaseName) ?? (persistedSilenceDecisionMs === null ? pick(legacyName) : null)
  );
  const ttsStages = stages.filter(({ stage }) => stage === "tts_segment_finished");
  const handoffStage = lastStage("speculation_handoff_ready");
  const prefillFailure = [...stages].reverse().find(({ stage }) => (
    stage === "audio_prefill_begin_failed" || stage === "audio_prefill_push_failed"
      || stage === "audio_prefill_wait_expired" || stage === "audio_prefill_drain_failed"
  ));
  const prefillWaitExpired = lastStage("audio_prefill_wait_expired");
  const prefillDrainFailed = lastStage("audio_prefill_drain_failed");
  const generationFinished = Boolean(generationStage);
  const firstPcmObserved = stages.some(({ stage }) => stage === "tts_first_pcm");
  const sumStageMetric = (name) => {
    const values = ttsStages.map((stage) => number(stage[name])).filter((value) => value !== null);
    return values.length ? values.reduce((total, value) => total + value, 0) : null;
  };
  const derivedSynthesisMs = sumStageMetric("total_ms");
  const derivedOutputAudioMs = sumStageMetric("output_audio_ms");
  const acceptedAudioSamples = pick("audio_accepted_samples");
  const consumedAudioSamples = pick("audio_consumed_samples");
  const projectedAudioSamples = pick("projector_samples");
  const projectorCpuMs = pick("projector_cpu_ms");
  const derivedAudioBacklogMs = acceptedAudioSamples !== null && consumedAudioSamples !== null
    ? Math.round((Math.max(0, acceptedAudioSamples - consumedAudioSamples) / 16) * 100) / 100
    : null;
  const derivedProjectorAudioMs = projectedAudioSamples === null
    ? null : Math.round((projectedAudioSamples / 16) * 100) / 100;
  const derivedProjectorRtf = projectorCpuMs !== null && derivedProjectorAudioMs > 0
    ? Math.round((projectorCpuMs / derivedProjectorAudioMs) * 10_000) / 10_000
    : null;
  const stageRtfs = ttsStages.map((stage) => number(stage.production_rtf)
    ?? (number(stage.total_ms) !== null && number(stage.output_audio_ms) > 0
      ? number(stage.total_ms) / number(stage.output_audio_ms) : null))
    .filter((value) => value !== null);
  const stageDelta = (from, to) => nativeDelta(
    number(lastStage(from)?.elapsed_ms), number(lastStage(to)?.elapsed_ms),
  );
  const transcriptFinished = number(lastStage("voice_transcript_finished")?.elapsed_ms);
  const nextPrefillStarted = number(lastStage("next_turn_prefill_started")?.elapsed_ms);
  return {
    ...scalarSummary,
    ...(foreignGreetingOnly ? {
      generation_prompt_tokens: null,
      generation_completion_tokens: null,
      generation_cache_reused: null,
      generation_cache_resident_prefix_tokens: null,
      generation_cache_restored_prefix_tokens: null,
      generation_cache_replayed_prefix_tokens: null,
    } : {}),
    id: trace.id,
    mode: trace.mode ?? null,
    began_at: trace.began_at ? new Date(trace.began_at).toISOString() : null,
    checkpoint: snapshot.checkpoint,
    stage_count: stages.length,
    terminal_stage: terminal,
    prefill_failure_stage: prefillFailure?.stage ?? null,
    prefill_failure_message: prefillFailure?.message ?? null,
    prefill_wait_expired_ms: number(prefillWaitExpired?.silence_ms),
    prefill_wait_expired_on_retry: prefillWaitExpired?.retry ?? null,
    prefill_drain_to_failure_ms: prefillDrainFailed
      ? stageDelta("prefill_flush_started", "audio_prefill_drain_failed") : null,
    generation_finished_without_pcm: generationFinished && !firstPcmObserved,
    runtime_warm_total_ms: pick("runtime_warm_total_ms"),
    runtime_warm_settings_ms: pick("runtime_warm_settings_ms"),
    runtime_warm_model_acquire_ms: pick("runtime_warm_model_acquire_ms"),
    call_request_to_microphone_ready_ms: pick("call_request_to_microphone_ready_ms"),
    microphone_ready_to_ringing_ms: pick("microphone_ready_to_ringing_ms"),
    microphone_native_first_sample_ms: pick("microphone_native_first_sample_ms"),
    model_load_total_ms: pick("model_load_total_ms"),
    model_load_backend_ms: pick("model_load_backend_ms"),
    model_load_artifact_open_ms: pick("model_load_artifact_open_ms"),
    model_load_weights_ms: pick("model_load_weights_ms"),
    model_load_projector_ms: pick("model_load_projector_ms"),
    model_load_artifact_verify_ms: pick("model_load_artifact_verify_ms"),
    model_load_context_create_ms: pick("model_load_context_create_ms"),
    model_load_fingerprint_ms: pick("model_load_fingerprint_ms"),
    silence_decision_delay_ms: silenceDecisionMs,
    silence_release_to_first_content_ms:
      silenceReleaseMetric("silence_release_to_first_content_ms", "silence_to_first_content_ms"),
    silence_to_first_content_ms:
      silenceMetric("silence_to_first_content_ms"),
    first_content_to_tts_pcm_ms: pick("first_content_to_tts_pcm_ms"),
    tts_pcm_to_playback_ms: pick("tts_pcm_to_playback_ms"),
    playback_startup_target_ms: pick("playback_startup_target_ms"),
    playback_startup_buffered_ms: pick("playback_startup_buffered_ms"),
    silence_release_to_tts_pcm_ms:
      silenceReleaseMetric("silence_release_to_tts_pcm_ms", "silence_to_tts_pcm_ms"),
    silence_release_to_playback_ms:
      silenceReleaseMetric("silence_release_to_playback_ms", "silence_to_playback_ms"),
    silence_to_playback_ms:
      silenceMetric("silence_to_playback_ms"),
    barge_in_playback_stop_ms: pick("barge_in_playback_stop_ms"),
    dispatch_to_native_worker_ms: pick("dispatch_to_native_worker_ms"),
    native_worker_to_first_content_ms: pick("native_worker_to_first_content_ms"),
    silence_to_native_release_ms: pick("silence_to_native_release_ms"),
    native_release_to_worker_ms: pick("native_release_to_worker_ms"),
    audio_first_accept_ms: pick("audio_first_accept_ms"),
    audio_first_read_ms: pick("audio_first_read_ms"),
    prompt_prefix_start_ms: pick("prompt_prefix_start_ms"),
    prompt_prefix_finish_ms: pick("prompt_prefix_finish_ms"),
    prompt_prefix_cpu_ms: pick("prompt_prefix_cpu_ms"),
    projector_first_start_ms: pick("projector_first_start_ms"),
    projector_first_finish_ms: pick("projector_first_finish_ms"),
    projector_last_finish_ms: pick("projector_last_finish_ms"),
    projector_cpu_ms: projectorCpuMs,
    projector_audio_ms: pick("projector_audio_ms") ?? derivedProjectorAudioMs,
    projector_realtime_factor: pick("projector_realtime_factor") ?? derivedProjectorRtf,
    audio_backlog_ms: pick("audio_backlog_ms") ?? derivedAudioBacklogMs,
    capture_to_model_target_ms: pick("capture_to_model_target_ms"),
    projector_blocks: pick("projector_blocks"),
    projector_samples: pick("projector_samples"),
    projector_overlap_before_finish_ms: pick("projector_overlap_before_finish_ms"),
    model_decode_start_ms: pick("model_decode_start_ms"),
    audio_finish_to_decode_ms: pick("audio_finish_to_decode_ms")
      ?? nativeDelta(nativeFinish, nativeDecode),
    decode_to_first_token_ms: pick("decode_to_first_token_ms")
      ?? nativeDelta(nativeDecode, generationFirstToken),
    generation_sampled_tokens: pick("generation_sampled_tokens"),
    generation_nonempty_token_pieces: pick("generation_nonempty_token_pieces"),
    generation_emitted_token_events: pick("generation_emitted_token_events"),
    generation_last_token_sample_ms: pick("generation_last_token_sample_ms"),
    generation_continuation_decode_steps: pick("generation_continuation_decode_steps"),
    generation_continuation_decode_cpu_ms: pick("generation_continuation_decode_cpu_ms"),
    generation_continuation_decode_max_ms: pick("generation_continuation_decode_max_ms"),
    generation_continuation_decode_inflight:
      summary.generation_continuation_decode_inflight ?? null,
    silence_release_to_first_token_native_ms:
      silenceReleaseMetric(
        "silence_release_to_first_token_native_ms",
        "silence_to_first_token_native_ms",
      ) ?? nativeDelta(nativeFinish, generationFirstToken),
    silence_to_first_token_native_ms:
      silenceMetric("silence_to_first_token_native_ms")
      ?? nativeDelta(nativeFinish, generationFirstToken),
    voice_session_prepare_ms: pick("voice_session_prepare_ms"),
    voice_session_prepared_before_capture: summary.voice_session_prepared_before_capture ?? null,
    voice_session_prepare_native_ms: number(nativeAudio?.begin?.total_us) !== null
      ? Math.round(number(nativeAudio.begin.total_us) / 10) / 100
      : pick("voice_session_prepare_native_ms"),
    voice_session_settings_ms: pick("voice_session_settings_ms"),
    voice_session_model_acquire_ms: pick("voice_session_model_acquire_ms"),
    voice_session_settings_cache_hit: summary.voice_session_settings_cache_hit ?? null,
    voice_session_model_resident_hit: summary.voice_session_model_resident_hit ?? null,
    capture_vad_deferred_silence_onsets:
      pick("capture_vad_deferred_silence_onsets"),
    capture_vad_settle_ms: pick("capture_vad_settle_ms"),
    capture_vad_voice_onsets: pick("capture_vad_voice_onsets"),
    capture_vad_silence_onsets: pick("capture_vad_silence_onsets"),
    capture_vad_latched_ms: pick("capture_vad_latched_ms"),
    capture_vad_voiced_evidence_ms: pick("capture_vad_voiced_evidence_ms"),
    capture_vad_stationary_noise_releases:
      pick("capture_vad_stationary_noise_releases"),
    capture_speculation_short_utterance_releases:
      pick("capture_speculation_short_utterance_releases"),
    capture_speculation_resume_cancels:
      pick("capture_speculation_resume_cancels"),
    capture_speculation_backoffs: pick("capture_speculation_backoffs"),
    capture_speculation_release_delay_max_ms:
      pick("capture_speculation_release_delay_max_ms"),
    capture_retained_only_samples: pick("capture_retained_only_samples"),
    capture_prefill_pushes: pick("capture_prefill_pushes"),
    capture_prefill_samples: pick("capture_prefill_samples"),
    speculation_handoff_round_trip_ms: pick("speculation_handoff_round_trip_ms")
      ?? number(handoffStage?.round_trip_ms),
    speculation_handoff_resume_wait_ms: pick("speculation_handoff_resume_wait_ms")
      ?? (number(handoffStage?.resume_wait_us) === null
        ? null : Math.round(number(handoffStage.resume_wait_us) / 10) / 100),
    speculation_handoff_restore_ms: pick("speculation_handoff_restore_ms")
      ?? (number(handoffStage?.resume_restore_us) === null
        ? null : Math.round(number(handoffStage.resume_restore_us) / 10) / 100),
    speculation_handoff_checkpoint_reused:
      summary.speculation_handoff_checkpoint_reused
        ?? (typeof handoffStage?.resume_reused === "boolean" ? handoffStage.resume_reused : null),
    speculation_handoff_checkpoint_available:
      summary.speculation_handoff_checkpoint_available
        ?? (typeof handoffStage?.resume_checkpoint_available === "boolean"
          ? handoffStage.resume_checkpoint_available : null),
    speculation_handoff_replay_samples: pick("speculation_handoff_replay_samples")
      ?? number(handoffStage?.replay?.replay_samples),
    speculation_handoff_replay_ms: pick("speculation_handoff_replay_ms")
      ?? (number(handoffStage?.replay?.total_us) === null
        ? null : Math.round(number(handoffStage.replay.total_us) / 10) / 100),
    tts_helper_first_pcm_ms: pick("tts_helper_first_pcm_ms"),
    generation_finished_to_tts_pcm_ms: pick("generation_finished_to_tts_pcm_ms")
      ?? generationToPcm,
    tts_segments: pick("tts_segment_count") ?? (ttsStages.length || null),
    tts_total_synthesis_ms: pick("tts_total_synthesis_ms") ?? derivedSynthesisMs,
    tts_total_output_audio_ms: pick("tts_total_output_audio_ms") ?? derivedOutputAudioMs,
    tts_aggregate_rtf: pick("tts_aggregate_production_rtf")
      ?? (derivedSynthesisMs !== null && derivedOutputAudioMs > 0
        ? Math.round((derivedSynthesisMs / derivedOutputAudioMs) * 10_000) / 10_000 : null),
    tts_pcm_packets: pick("tts_pcm_packets"),
    tts_pcm_audio_ms: pick("tts_pcm_audio_ms"),
    tts_first_packet_audio_ms: pick("tts_first_packet_audio_ms"),
    tts_max_packet_gap_ms: pick("tts_max_packet_gap_ms"),
    tts_minimum_producer_lead_ms: pick("tts_minimum_producer_lead_ms"),
    tts_scheduler_target_ms: pick("tts_scheduler_target_runway_ms"),
    tts_scheduler_startup_ms: pick("tts_scheduler_startup_runway_ms"),
    tts_scheduler_packet_gap_ms: pick("tts_scheduler_observed_packet_gap_ms"),
    tts_scheduler_boundary_gap_ms: pick("tts_scheduler_observed_boundary_gap_ms"),
    tts_scheduler_waits: pick("tts_scheduler_waits"),
    tts_scheduler_wait_ms: pick("tts_scheduler_wait_ms"),
    tts_scheduler_max_runway_ms: pick("tts_scheduler_maximum_runway_ms"),
    tts_scheduler_observed_rtf: pick("tts_scheduler_observed_rtf"),
    tts_scheduler_underrun_penalty_ms: pick("tts_scheduler_underrun_penalty_ms"),
    semantic_backpressure_switches: pick("semantic_backpressure_switches"),
    semantic_backpressure_pause_ms: pick("semantic_backpressure_pause_ms"),
    semantic_backpressure_command_ms: pick("semantic_backpressure_command_ms"),
    semantic_backpressure_generation_yields: pick("semantic_backpressure_generation_yields"),
    semantic_backpressure_generation_yield_ms: pick("semantic_backpressure_generation_yield_ms"),
    semantic_backpressure_maximum_words: pick("semantic_backpressure_maximum_words"),
    tts_compute_lease_acquisitions: pick("tts_compute_lease_acquisitions"),
    tts_compute_lease_releases: pick("tts_compute_lease_releases"),
    tts_generation_yields: pick("tts_generation_yields"),
    tts_generation_yield_ms: pick("tts_generation_yield_ms"),
    tts_generation_fair_passes: pick("tts_generation_fair_passes"),
    tts_compute_lease_held_ms: pick("tts_compute_lease_held_ms"),
    tts_compute_lease_released_ms: pick("tts_compute_lease_released_ms"),
    tts_compute_minimum_lead_ms: pick("tts_compute_minimum_lead_ms"),
    tts_compute_maximum_lead_ms: pick("tts_compute_maximum_lead_ms"),
    tts_compute_low_water_ms: pick("tts_compute_low_water_ms"),
    tts_compute_high_water_ms: pick("tts_compute_high_water_ms"),
    tts_worst_segment_rtf: pick("tts_worst_segment_rtf")
      ?? (stageRtfs.length ? Math.round(Math.max(...stageRtfs) * 10_000) / 10_000 : null),
    tts_final_buffered_ms: pick("tts_all_segments_buffered_runway_ms"),
    tts_to_next_prefill_ms: pick("tts_to_next_prefill_start_ms"),
    next_prefill_prepare_ms: pick("next_prefill_prepare_ms"),
    next_prefill_buffered_ms: pick("next_prefill_buffered_runway_ms"),
    next_prefill_overlapped_tts: summary.next_prefill_overlapped_active_tts ?? null,
    capture_stop_to_transcript_verified_ms:
      stageDelta("capture_stopped", "voice_transcript_live_verified"),
    capture_stop_to_transcript_committed_ms:
      stageDelta("capture_stopped", "voice_transcript_finished"),
    transcript_ready_before_next_prefill: transcriptFinished !== null && nextPrefillStarted !== null
      ? transcriptFinished <= nextPrefillStarted : null,
    transcript_to_next_prefill_ms: transcriptFinished !== null && nextPrefillStarted !== null
      ? Math.round((nextPrefillStarted - transcriptFinished) * 100) / 100 : null,
    playback_underruns: pick("playback_underrun_occurrences"),
    path: snapshot.path,
  };
}

export function latencyDiagnosis(row) {
  const criticalPath = [
    ["silence_to_first_content", row.silence_to_first_content_ms],
    ["first_content_to_tts_pcm", row.first_content_to_tts_pcm_ms],
    ["tts_pcm_to_playback", row.tts_pcm_to_playback_ms],
  ].filter(([, value]) => value !== null);
  const accountedMs = criticalPath.length === 3
    ? criticalPath.reduce((total, [, value]) => total + value, 0)
    : null;
  const dominant = criticalPath.length
    ? criticalPath.reduce((largest, candidate) => candidate[1] > largest[1] ? candidate : largest)
    : [null, null];
  const unaccountedMs = row.silence_to_playback_ms !== null && accountedMs !== null
    ? Math.round((row.silence_to_playback_ms - accountedMs) * 100) / 100
    : null;
  const findings = [];
  if (row.voice_session_model_resident_hit === false) findings.push("model_not_resident");
  if (row.runtime_warm_audio_projector_ready === false) {
    findings.push("audio_projector_not_warmed");
  }
  if (row.prefill_failure_stage) findings.push("audio_prefill_failed");
  if (row.prefill_wait_expired_ms != null) findings.push("audio_prefill_wait_expired");
  if (row.prefill_drain_to_failure_ms != null) findings.push("audio_prefill_drain_stalled");
  if (row.generation_finished_without_pcm) findings.push("response_without_pcm");
  if (row.terminal_stage === "generation_timeout") {
    findings.push("generation_timeout");
    if (row.generation_continuation_decode_inflight === true) {
      findings.push("generation_decode_stalled");
    } else if (row.generation_sampled_tokens === 0) {
      findings.push("generation_sampling_not_started");
    } else if (row.generation_emitted_token_events === 0) {
      findings.push("generation_tokens_not_emitted");
    } else if (row.generation_sampled_tokens !== null) {
      findings.push("generation_throughput_stall");
    }
  }
  if (row.speculation_handoff_checkpoint_available === false) {
    findings.push("voice_resume_checkpoint_missed");
  }
  if (row.speculation_handoff_round_trip_ms !== null
      && row.speculation_handoff_round_trip_ms > 500) {
    findings.push("voice_resume_slow");
  }
  if (row.voice_cancel_to_observed_ms !== null
      && row.voice_cancel_to_observed_ms > 100) {
    findings.push("voice_cancel_slow");
  }
  if (row.capture_prefill_pushes === 0) findings.push("continuous_prefill_absent");
  if ((row.capture_speculation_resume_cancels ?? 0) > 2) {
    findings.push("speculation_churn");
  }
  if (row.projector_blocks === 0) findings.push("audio_projector_absent");
  if (row.projector_realtime_factor !== null && row.projector_realtime_factor >= 1) {
    findings.push("audio_projector_slower_than_realtime");
  }
  if (row.audio_backlog_ms !== null && row.audio_backlog_ms > 200) {
    findings.push("continuous_prefill_backlog");
  }
  if ((row.playback_underruns ?? 0) > 0) findings.push("playback_underrun");
  if (row.tts_aggregate_rtf !== null && row.tts_aggregate_rtf >= 1) {
    findings.push("tts_slower_than_realtime");
  }
  if (row.mode === "call" && row.silence_to_playback_ms !== null
      && row.silence_to_playback_ms > 1_000) {
    findings.push("conversation_target_missed");
  }
  if (row.call_request_to_playback_ms !== null && row.call_request_to_playback_ms > 5_000) {
    findings.push("call_pickup_target_missed");
  }
  if (row.mode === "call_greeting"
      && row.call_prefill_to_first_content_ms !== null
      && row.call_prefill_to_first_content_ms > 500
      && row.generation_cache_reused === false) {
    findings.push("call_greeting_cold_prefix");
  }
  return {
    dominant_stage: dominant[0],
    dominant_stage_ms: dominant[1],
    critical_path_accounted_ms: accountedMs,
    critical_path_unaccounted_ms: unaccountedMs,
    subsecond: row.mode !== "call" || row.silence_to_playback_ms === null
      ? null : row.silence_to_playback_ms <= 1_000,
    findings,
  };
}

const aggregateMetricNames = [
  "call_request_to_ringing_ms",
  "call_request_to_microphone_ready_ms",
  "microphone_ready_to_ringing_ms",
  "microphone_native_first_sample_ms",
  "call_request_to_runtime_ready_ms",
  "call_request_to_conversation_ready_ms",
  "call_request_to_dispatch_ms",
  "call_request_to_playback_ms",
  "runtime_warm_total_ms",
  "runtime_warm_audio_projector_ms",
  "capture_to_model_target_ms",
  "audio_backlog_ms",
  "projector_realtime_factor",
  "voice_session_prepare_native_ms",
  "speculation_handoff_round_trip_ms",
  "speculation_handoff_resume_wait_ms",
  "speculation_handoff_restore_ms",
  "speculation_handoff_replay_ms",
  "voice_cancel_to_observed_ms",
  "silence_event_delivery_ms",
  "capture_processing_max_ms",
  "capture_queue_delay_max_ms",
  "capture_prefill_push_max_ms",
  "capture_vad_latched_ms",
  "capture_vad_voiced_evidence_ms",
  "capture_speculation_release_delay_max_ms",
  "silence_decision_delay_ms",
  "audio_finish_to_decode_ms",
  "decode_to_first_token_ms",
  "generation_last_token_sample_ms",
  "generation_continuation_decode_max_ms",
  "silence_to_first_token_native_ms",
  "silence_release_to_first_token_native_ms",
  "tts_native_dispatch_to_pcm_ms",
  "tts_helper_first_pcm_ms",
  "generation_finished_to_tts_pcm_ms",
  "tts_pcm_to_playback_ms",
  "silence_release_to_first_content_ms",
  "silence_release_to_tts_pcm_ms",
  "silence_release_to_playback_ms",
  "silence_to_playback_ms",
  "tts_aggregate_rtf",
  "playback_underruns",
];

export function percentile(values, fraction) {
  const sorted = values.filter(Number.isFinite).toSorted((left, right) => left - right);
  if (!sorted.length) return null;
  const position = Math.max(0, Math.min(sorted.length - 1, (sorted.length - 1) * fraction));
  const lower = Math.floor(position);
  const upper = Math.ceil(position);
  const value = lower === upper
    ? sorted[lower]
    : sorted[lower] + (sorted[upper] - sorted[lower]) * (position - lower);
  return Math.round(value * 100) / 100;
}

export function latencyAggregate(rows) {
  const metrics = Object.fromEntries(aggregateMetricNames.map((name) => {
    const values = rows.map((row) => number(row[name])).filter((value) => value !== null);
    return [name, {
      samples: values.length,
      p50: percentile(values, 0.5),
      p95: percentile(values, 0.95),
      max: values.length ? Math.max(...values) : null,
    }];
  }));
  const modes = {};
  const findings = {};
  for (const row of rows) {
    const mode = row.mode ?? "unknown";
    const entry = modes[mode] ??= { traces: 0, playback_started: 0, subsecond: 0 };
    entry.traces += 1;
    entry.playback_started += Number(row.terminal_stage === "playback_started");
    entry.subsecond += Number(row.subsecond === true);
    for (const finding of row.findings ?? []) findings[finding] = (findings[finding] ?? 0) + 1;
  }
  return { traces: rows.length, modes, metrics, findings };
}

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const key = argv[index];
    if (key === "--json") args.json = true;
    else if (key === "--summary") args.summary = true;
    else if (key === "--dir" || key === "--limit") args[key.slice(2)] = argv[++index];
    else throw new Error(`unknown argument: ${key}`);
  }
  return args;
}

function run(args) {
  const directory = resolve(args.dir ?? join(homedir(), "Library/Logs/com.delysis.llama-native-kit.mom-llama/voice-latency-history"));
  const limit = Math.max(1, Number(args.limit ?? 20));
  const rows = latestSnapshots(directory).slice(0, limit).map((snapshot) => {
    const row = latencyRow(snapshot);
    return { ...row, ...latencyDiagnosis(row) };
  });
  if (args.summary && args.json) console.log(JSON.stringify(latencyAggregate(rows), null, 2));
  else if (args.summary) {
    const aggregate = latencyAggregate(rows);
    console.table(Object.entries(aggregate.metrics).map(([metric, value]) => ({ metric, ...value })));
    console.table(Object.entries(aggregate.modes).map(([mode, value]) => ({ mode, ...value })));
    console.table(Object.entries(aggregate.findings).map(([finding, traces]) => ({ finding, traces })));
  }
  else if (args.json) console.log(JSON.stringify(rows, null, 2));
  else console.table(rows.map(({ path: _path, id, ...row }) => ({ id: id.slice(0, 8), ...row })));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { run(parseArgs(process.argv.slice(2))); }
  catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

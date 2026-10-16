"use strict";

((root) => {
  const SPEECH_RELEASE_MS = 20;
  const SPEECH_OPEN_MS = 48;
  const SPEECH_RESUME_MS = 40;
  // Match Gemma's 40 ms audio-projector frame at 16 kHz. A delayed WebView
  // callback may accumulate much more; catch-up still publishes every frame.
  const PREFILL_MAX_BLOCK_SAMPLES = 640;
  const clamp = (value, minimum, maximum) => Math.min(maximum, Math.max(minimum, value));

  const splitPrefillPcm = (samples) => {
    const blocks = [];
    for (let offset = 0; offset < samples.length; offset += PREFILL_MAX_BLOCK_SAMPLES) {
      blocks.push(samples.subarray(offset, offset + PREFILL_MAX_BLOCK_SAMPLES));
    }
    return blocks;
  };

  const speechThreshold = (noiseFloor) => clamp(
    ((Number(noiseFloor) || 0) * 2.4) + .0005,
    .003,
    .02,
  );

  const initialQuantile = (quantile = .2) => ({
    quantile,
    initial: [],
    heights: null,
    positions: null,
    desired: null,
  });

  // Jain and Chlamtac's P² estimator tracks a streaming percentile with five
  // markers: constant memory, no window sorting, and O(1) work per subframe.
  const updateQuantile = (state, value) => {
    const sample = Math.max(0, Number(value) || 0);
    if (!state.heights) {
      state.initial.push(sample);
      state.initial.sort((left, right) => left - right);
      if (state.initial.length < 5) {
        return state.initial[Math.floor((state.initial.length - 1) * state.quantile)];
      }
      state.heights = state.initial.slice(0, 5);
      state.positions = [1, 2, 3, 4, 5];
      const p = state.quantile;
      state.desired = [1, 1 + (2 * p), 1 + (4 * p), 3 + (2 * p), 5];
      state.initial = [];
      return state.heights[2];
    }
    const { heights, positions, desired } = state;
    let bucket;
    if (sample < heights[0]) {
      heights[0] = sample;
      bucket = 0;
    } else if (sample >= heights[4]) {
      heights[4] = sample;
      bucket = 3;
    } else {
      bucket = heights.findIndex((height, index) => index < 4
        && sample >= height && sample < heights[index + 1]);
    }
    for (let index = bucket + 1; index < 5; index += 1) positions[index] += 1;
    const p = state.quantile;
    const increments = [0, p / 2, p, (1 + p) / 2, 1];
    for (let index = 0; index < 5; index += 1) desired[index] += increments[index];
    for (let index = 1; index <= 3; index += 1) {
      const distance = desired[index] - positions[index];
      const direction = Math.sign(distance);
      if ((distance >= 1 && positions[index + 1] - positions[index] > 1)
          || (distance <= -1 && positions[index - 1] - positions[index] < -1)) {
        const leftSpan = positions[index] - positions[index - 1];
        const rightSpan = positions[index + 1] - positions[index];
        const proposed = heights[index] + (direction / (leftSpan + rightSpan)) * (
          ((leftSpan + direction) * (heights[index + 1] - heights[index]) / rightSpan)
          + ((rightSpan - direction) * (heights[index] - heights[index - 1]) / leftSpan)
        );
        heights[index] = proposed > heights[index - 1] && proposed < heights[index + 1]
          ? proposed
          : heights[index] + direction * (
            heights[index + direction] - heights[index]
          ) / (positions[index + direction] - positions[index]);
        positions[index] += direction;
      }
    }
    return heights[2];
  };

  const initialVoiceActivity = () => ({
    heardVoice: false,
    silentMs: 0,
    noiseFloor: .00125,
    noiseQuantile: initialQuantile(.2),
    voiced: false,
    releaseMs: 0,
    candidateMs: 0,
  });

  const initialPlaybackEchoGuard = () => ({
    playbackMs: 0,
    residualRms: .003,
    aboveMs: 0,
  });

  const updatePlaybackEchoGuard = (guard, rms, durationMs) => {
    const current = guard || initialPlaybackEchoGuard();
    const level = Math.max(0, Number(rms) || 0);
    const elapsed = Math.max(0, Number(durationMs) || 0);
    current.playbackMs += elapsed;
    const threshold = Math.max(.009, (current.residualRms * 2.8) + .001);
    const warming = current.playbackMs <= 250;
    if (level < threshold || warming) {
      // Learn only plausible residual echo. A caller who speaks during the
      // short convergence interval must not teach the guard to reject them.
      if (level < .012) current.residualRms = (current.residualRms * .92) + (level * .08);
      current.aboveMs = 0;
      return { guard: current, suppressed: true, threshold, warming };
    }
    current.aboveMs += elapsed;
    return {
      guard: current,
      suppressed: current.aboveMs < 48,
      threshold,
      warming: false,
    };
  };

  const createStreamingMonoResampler = (sourceRate, targetRate = 16000) => {
    const ratio = Number(sourceRate) / Number(targetRate);
    if (!Number.isFinite(ratio) || ratio < 1) {
      throw new Error("Voice capture requires a finite source rate at least as high as its target rate.");
    }
    let remaining = ratio;
    let weighted = 0;
    return {
      process(samples) {
        if (!(samples instanceof Float32Array) || samples.length === 0) return new Float32Array();
        if (ratio === 1) return samples.slice();
        const output = new Float32Array(Math.ceil(samples.length / ratio) + 1);
        let outputLength = 0;
        for (const sample of samples) {
          let available = 1;
          while (available > 1e-7) {
            const consumed = Math.min(available, remaining);
            weighted += sample * consumed;
            available -= consumed;
            remaining -= consumed;
            if (remaining <= 1e-7) {
              output[outputLength] = weighted / ratio;
              outputLength += 1;
              remaining = ratio;
              weighted = 0;
            }
          }
        }
        return output.slice(0, outputLength);
      },
    };
  };

  const nextVoiceActivity = (activity, rms, durationMs) => {
    const current = activity || initialVoiceActivity();
    const level = Math.max(0, Number(rms) || 0);
    const threshold = speechThreshold(current.noiseFloor);
    const aboveThreshold = level >= threshold;
    let noiseFloor = current.noiseFloor;

    // Learn only from frames that are safely below the current speech gate.
    // Updating the floor from a quiet speaker would teach the gate to reject
    // that speaker on the next frame.
    if (!aboveThreshold && level < threshold * .85) {
      const percentile = updateQuantile(current.noiseQuantile, level);
      // The percentile rejects intermittent sounds; the light smoothing keeps
      // the gate from moving abruptly around a quiet speaker.
      noiseFloor = (noiseFloor * .8) + (percentile * .2);
    }

    // A single loud subframe is not an utterance. Keep the prospective onset
    // in pre-roll, then admit speech only after sustained evidence.
    const candidateMs = current.voiced ? 0 : aboveThreshold
      ? (Number(current.candidateMs) || 0) + Math.max(0, durationMs)
      : Math.max(0, (Number(current.candidateMs) || 0) - Math.max(0, durationMs));
    const heardVoice = current.heardVoice || candidateMs >= SPEECH_OPEN_MS;
    const releaseMs = aboveThreshold ? 0
      : (heardVoice ? (Number(current.releaseMs) || 0) + Math.max(0, durationMs) : 0);
    const freshOnset = candidateMs >= (current.heardVoice ? SPEECH_RESUME_MS : SPEECH_OPEN_MS);
    const voiced = heardVoice && (current.voiced
      ? releaseMs < SPEECH_RELEASE_MS : freshOnset);
    return {
      heardVoice,
      silentMs: heardVoice ? releaseMs : 0,
      noiseFloor,
      threshold,
      voiced,
      releaseMs,
      candidateMs: voiced ? 0 : candidateMs,
      noiseQuantile: current.noiseQuantile,
    };
  };

  const analyzeVoiceSamples = (activity, samples, sampleRate, subframeSamples = 64) => {
    let current = activity || initialVoiceActivity();
    let voiceOnsetSample = null;
    let silenceOnsetSample = null;
    let speechResumedSample = null;
    let hadVoiced = false;
    let lastRms = 0;
    const rate = Math.max(1, Number(sampleRate) || 16000);
    const width = Math.max(16, Math.trunc(subframeSamples));
    for (let start = 0; start < samples.length; start += width) {
      const end = Math.min(samples.length, start + width);
      let power = 0;
      for (let index = start; index < end; index += 1) power += samples[index] * samples[index];
      lastRms = Math.sqrt(power / Math.max(1, end - start));
      const prior = current;
      current = nextVoiceActivity(current, lastRms, ((end - start) / rate) * 1000);
      hadVoiced ||= current.voiced;
      if (!prior.heardVoice && current.voiced && voiceOnsetSample === null) voiceOnsetSample = start;
      if (prior.voiced && !current.voiced && silenceOnsetSample === null) silenceOnsetSample = start;
      if (prior.heardVoice && !prior.voiced && current.voiced && speechResumedSample === null) {
        speechResumedSample = start;
      }
    }
    return {
      activity: current,
      hadVoiced,
      voiceOnsetSample,
      silenceOnsetSample,
      speechResumedSample,
      rms: lastRms,
      threshold: current.threshold,
      subframes: Math.ceil(samples.length / width),
    };
  };

  // Staged bridge endpoint: send/finish/cancel must be supplied by the admitted
  // native turn owner. This object does not invoke commands or edit history.
  // A fixed source survives Gemma speculation; never append Gemma replay here.
  const createLiveTranscriptionTap = ({ turn, capture, send, finish, cancel, onTrace = () => {},
    deadlineMs = 5000, maxQueuedSamples = 40960 } = {}) => {
    if (![turn, capture].every((id) => typeof id === "string" && id.length > 0 && id.length <= 128)
        || typeof send !== "function" || typeof finish !== "function" || typeof cancel !== "function"
        || !Number.isInteger(maxQueuedSamples) || maxQueuedSamples < 640 || maxQueuedSamples > 40960
        || !Number.isFinite(deadlineMs) || deadlineMs <= 0 || deadlineMs > 5000) {
      throw new Error("live_transcription_configuration_invalid");
    }
    let phase = "open";
    let accepted = 0;
    let delivered = 0;
    let highWater = 0;
    let running = false;
    let scheduled = false;
    let staged = new Float32Array(640);
    let stagedCount = 0;
    const queue = [];
    const began = performance.now();
    let firstCaptureMs = null;
    let lastCaptureMs = null;
    let firstIngressMs = null;
    let lastIngressMs = null;
    let failure = null;
    let resolveFinal;
    let rejectFinal;
    const finished = new Promise((resolve, reject) => { resolveFinal = resolve; rejectFinal = reject; });
    // The owning command may attach its final handler after physical capture
    // stops. Observe rejection now, without converting it into success.
    void finished.catch(() => {});
    const metrics = () => ({ turn, capture, phase, accepted_samples: accepted,
      bridge_accepted_samples: delivered, backlog_samples: accepted - delivered,
      backlog_high_water_samples: highWater, first_sample_offset: accepted ? 0 : null,
      last_sample_exclusive: accepted, first_capture_ms: firstCaptureMs, last_capture_ms: lastCaptureMs,
      first_bridge_ack_ms: firstIngressMs, last_bridge_ack_ms: lastIngressMs, failure });
    const trace = (stage) => {
      const event = { schema: "mom_llama.live_transcription_browser.v1", stage, ...metrics() };
      // Keep an arbitrary logging callback off the microphone's synchronous work.
      queueMicrotask(() => { try { onTrace(event); } catch { /* telemetry is not capture authority */ } });
    };
    const fail = (code) => {
      if (phase === "failed" || phase === "complete") return false;
      phase = "failed";
      failure = String(code);
      queue.length = 0;
      stagedCount = 0;
      trace(code === "live_transcription_overflow" ? "overflow" : "failed_audio_retained");
      // At most one send is in flight. Native cancellation must poison that
      // source even when its eventual IPC completion races with this failure.
      void Promise.resolve().then(() => cancel({ turn, capture, reason: failure })).catch(() => {});
      rejectFinal(new Error(failure));
      return false;
    };
    const bounded = async (operation) => {
      let timer;
      try {
        return await Promise.race([
          Promise.resolve().then(operation),
          new Promise((_, reject) => {
            timer = setTimeout(() => reject(new Error("live_transcription_backlog_timeout")), deadlineMs);
          }),
        ]);
      } finally { clearTimeout(timer); }
    };
    const schedule = () => {
      if (scheduled || running) return;
      scheduled = true;
      queueMicrotask(() => { scheduled = false; void drain(); });
    };
    const drain = async () => {
      if (running || phase === "failed" || phase === "complete") return;
      running = true;
      try {
        while (queue.length && phase !== "failed") {
          const packet = queue.shift();
          await bounded(() => send({ turn, capture, sample_offset: packet.offset, samples: packet.samples }));
          if (phase === "failed") return;
          delivered += packet.samples.length;
          lastIngressMs = performance.now() - began;
          if (firstIngressMs === null) { firstIngressMs = lastIngressMs; trace("first_bridge_ack"); }
        }
        if (phase === "sealed" && delivered === accepted) {
          trace("last_bridge_ack");
          const result = await bounded(() => finish({ turn, capture, samples: accepted }));
          if (phase === "failed") return;
          phase = "complete";
          trace("final_uncommitted");
          resolveFinal(result);
        }
      } catch (error) {
        fail(error?.message || "live_transcription_failed");
      } finally {
        running = false;
        if (queue.length && phase !== "failed" && phase !== "complete") schedule();
      }
    };
    const flush = () => {
      if (!stagedCount) return;
      queue.push({ offset: accepted - stagedCount, samples: staged.slice(0, stagedCount) });
      staged = new Float32Array(640);
      stagedCount = 0;
    };
    return {
      finished,
      metrics,
      append(samples) {
        if (phase !== "open") return phase === "failed" ? false : fail("live_transcription_source_after_seal");
        if (!(samples instanceof Float32Array)) return fail("live_transcription_invalid_pcm");
        if (!samples.length) return true;
        // Keep the browser source bound aligned with the native streaming
        // reader; a long physical turn must not drop its tail at 29 seconds.
        if (accepted + samples.length > 16000 * 2 * 60 * 60) return fail("live_transcription_too_long");
        // Includes the in-flight packet and staging, not just queued packets.
        if (accepted - delivered + samples.length > maxQueuedSamples) return fail("live_transcription_overflow");
        if (samples.some((sample) => !Number.isFinite(sample) || Math.abs(sample) > 1)) {
          return fail("live_transcription_invalid_pcm");
        }
        lastCaptureMs = performance.now() - began;
        if (firstCaptureMs === null) firstCaptureMs = lastCaptureMs;
        for (const sample of samples) {
          staged[stagedCount++] = sample;
          accepted += 1;
          if (stagedCount === 640) flush();
        }
        highWater = Math.max(highWater, accepted - delivered);
        if (accepted === samples.length) trace("first_capture_ingress");
        schedule();
        return true;
      },
      seal() {
        if (phase !== "open") return finished;
        if (!accepted) { fail("live_transcription_empty"); return finished; }
        phase = "sealed";
        flush();
        trace("source_sealed");
        schedule();
        return finished;
      },
      cancel() { fail("live_transcription_cancelled"); },
    };
  };

  const api = {
    createLiveTranscriptionTap,
    analyzeVoiceSamples,
    createStreamingMonoResampler,
    initialPlaybackEchoGuard,
    initialVoiceActivity,
    nextVoiceActivity,
    speechThreshold,
    splitPrefillPcm,
    updatePlaybackEchoGuard,
  };
  root.MomVoiceCapturePolicy = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof globalThis === "undefined" ? window : globalThis);

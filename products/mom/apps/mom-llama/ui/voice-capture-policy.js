"use strict";

((root) => {
  const SPEECH_RELEASE_MS = 20;
  const clamp = (value, minimum, maximum) => Math.min(maximum, Math.max(minimum, value));

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

    const heardVoice = current.heardVoice || aboveThreshold;
    const releaseMs = aboveThreshold ? 0
      : (heardVoice ? (Number(current.releaseMs) || 0) + Math.max(0, durationMs) : 0);
    const voiced = aboveThreshold || (current.voiced && releaseMs < SPEECH_RELEASE_MS);
    return {
      heardVoice,
      silentMs: heardVoice ? releaseMs : 0,
      noiseFloor,
      threshold,
      voiced,
      releaseMs,
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

  const api = {
    analyzeVoiceSamples,
    createStreamingMonoResampler,
    initialPlaybackEchoGuard,
    initialVoiceActivity,
    nextVoiceActivity,
    speechThreshold,
    updatePlaybackEchoGuard,
  };
  root.MomVoiceCapturePolicy = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof globalThis === "undefined" ? window : globalThis);

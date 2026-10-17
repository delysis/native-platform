(function installVoicePipelineScheduler(root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  root.MomVoicePipelineScheduler = api;
}(typeof globalThis === "object" ? globalThis : this, () => {
  const clamp = (value, minimum, maximum) => Math.min(maximum, Math.max(minimum, value));

  class AdaptiveVoiceLeadController {
    constructor({
      startupMs = 120,
      initialFirstAudioMs = 140,
      initialPacketAudioMs = 139,
      initialMaximumPacketGapMs = initialPacketAudioMs,
      initialMaximumBoundaryGapMs = initialFirstAudioMs,
      // The same-process resident trace needs 15.4 ms beyond first PCM for
      // its complete packet schedule to remain underrun-free. Keep a small
      // first-call margin until adaptive packet history takes over.
      initialRequiredStartupDelayMs = 16,
      initialProductionRtf = 0.7,
      initialSpeechMsPerWord = 400,
      initialUnderrunPenaltyMs = 0,
      initialSlowSegmentCount = 0,
      initialHealthySegmentCount = 0,
      initialBufferComplete = false,
      minimumTargetMs = 180,
      maximumTargetMs = 1_200,
    } = {}) {
      this.startupMs = startupMs;
      this.firstAudioMs = initialFirstAudioMs;
      this.firstPacketAudioMs = initialPacketAudioMs;
      this.maximumPacketGapMs = initialMaximumPacketGapMs;
      this.maximumBoundaryGapMs = initialMaximumBoundaryGapMs;
      this.requiredStartupDelayMs = initialRequiredStartupDelayMs;
      this.productionRtf = initialProductionRtf;
      this.speechMsPerWord = initialSpeechMsPerWord;
      this.minimumTargetMs = minimumTargetMs;
      this.maximumTargetMs = maximumTargetMs;
      this.underrunPenaltyMs = initialUnderrunPenaltyMs;
      this.slowSegmentCount = initialSlowSegmentCount;
      this.healthySegmentCount = initialHealthySegmentCount;
      this.bufferComplete = initialBufferComplete;
      this.replySegmentCount = 0;
      this.waits = 0;
      this.waitMs = 0;
      this.maximumRunwayMs = 0;
    }

    notePacket({ audioMs, gapMs = null, boundary = false, startupDeficitMs = null } = {}) {
      if (Number.isFinite(audioMs) && audioMs > 0) {
        this.firstPacketAudioMs = this.firstPacketAudioMs * 0.75 + audioMs * 0.25;
      }
      if (Number.isFinite(gapMs) && gapMs >= 0) {
        // Keep a decaying high-water mark: react immediately to a stall, then
        // recover gradually when the current machine proves a steadier cadence.
        if (boundary) {
          this.maximumBoundaryGapMs = Math.max(gapMs, this.maximumBoundaryGapMs * 0.92);
        } else {
          this.maximumPacketGapMs = Math.max(gapMs, this.maximumPacketGapMs * 0.92);
        }
      }
      if (Number.isFinite(startupDeficitMs) && startupDeficitMs >= 0) {
        // This is the exact wall-clock delay a playback start at the first PCM
        // callback would have needed to survive all packets observed so far.
        // React immediately to a larger deficit and decay only across healthy
        // packet observations. It is distinct from buffered audio duration.
        this.requiredStartupDelayMs = Math.max(
          startupDeficitMs,
          this.requiredStartupDelayMs * 0.92,
        );
      }
    }

    noteSegment({ synthesisMs, audioMs, firstAudioMs = null, words = null } = {}) {
      if (Number.isFinite(firstAudioMs) && firstAudioMs > 0) {
        this.firstAudioMs = this.firstAudioMs * 0.7 + firstAudioMs * 0.3;
      }
      if (Number.isFinite(synthesisMs) && Number.isFinite(audioMs) && audioMs > 0) {
        const observed = clamp(synthesisMs / audioMs, 0.05, 4);
        this.productionRtf = this.productionRtf * 0.7 + observed * 0.3;
        if (this.bufferComplete) {
          if (observed <= 0.85) {
            this.healthySegmentCount += 1;
            if (this.healthySegmentCount >= 2) {
              this.bufferComplete = false;
              this.slowSegmentCount = 0;
              this.healthySegmentCount = 0;
            }
          } else {
            this.healthySegmentCount = 0;
          }
        } else if (observed >= 1.05) {
          this.slowSegmentCount += 1;
          if ((this.replySegmentCount === 0 && observed >= 1.4) || this.slowSegmentCount >= 2) {
            this.bufferComplete = true;
            this.healthySegmentCount = 0;
          }
        } else if (observed <= 0.85) {
          this.slowSegmentCount = 0;
        }
        this.replySegmentCount += 1;
      }
      if (Number.isFinite(words) && words > 0 && Number.isFinite(audioMs) && audioMs > 0) {
        const observed = clamp(audioMs / words, 180, 900);
        this.speechMsPerWord = this.speechMsPerWord * 0.7 + observed * 0.3;
      }
      this.underrunPenaltyMs *= 0.8;
    }

    noteUnderrun(gapMs = 0) {
      const gap = Number.isFinite(gapMs) ? Math.max(0, gapMs) : 0;
      this.underrunPenaltyMs = clamp(
        Math.max(this.underrunPenaltyMs * 1.5, gap * 2 + 120),
        120,
        this.maximumTargetMs * 0.75,
      );
    }

    targetRunwayMs() {
      const contentionPenalty = Math.max(0, this.productionRtf - 0.8) * 800;
      const uncoveredPacketGapMs = Math.max(
        0,
        this.maximumPacketGapMs - this.firstPacketAudioMs,
      );
      // Dispatch the next segment early enough to cover its measured startup
      // and only the portion of an internal packet gap that preceding PCM
      // cannot play through. The aggregate segment-boundary gap includes this
      // controller's deliberate wait; feeding that whole interval back into
      // the target creates a self-inflating buffer and starves Gemma of useful
      // decode windows between Qwen requests.
      const refillRunwayMs = this.firstAudioMs + uncoveredPacketGapMs + 16;
      return Math.round(clamp(
        Math.max(
          this.startupMs,
          refillRunwayMs,
          this.firstPacketAudioMs * 1.25,
        ) + contentionPenalty + this.underrunPenaltyMs,
        this.minimumTargetMs,
        this.maximumTargetMs,
      ));
    }

    startupRunwayMs() {
      // A packet-arrival gap is not itself required runway: the preceding
      // packet is playing during most of that interval. Protect only the
      // uncovered part of the worst observed gap, plus a small callback
      // margin. The shared-Metal benchmark measured 80-110 ms of required
      // startup lead despite 248-250 ms callback gaps. Keep the healthy floor
      // below the 139 ms first packet so playback does not wait an entire
      // additional callback; an actual underrun raises it immediately.
      const uncoveredPacketGapMs = Math.max(
        0,
        this.maximumPacketGapMs - this.firstPacketAudioMs,
      );
      return Math.round(clamp(
        Math.max(
          this.startupMs,
          uncoveredPacketGapMs + 8,
        ) + Math.min(this.underrunPenaltyMs, 160),
        120,
        500,
      ));
    }

    startupDelayMs() {
      return Math.round(clamp(
        this.requiredStartupDelayMs + 2 + Math.min(this.underrunPenaltyMs, 80),
        0,
        250,
      ));
    }

    shouldDispatch({ first = false, playing = false, bufferedMs = 0 } = {}) {
      const runway = Number.isFinite(bufferedMs) ? Math.max(0, bufferedMs) : 0;
      this.maximumRunwayMs = Math.max(this.maximumRunwayMs, runway);
      return first || !playing || runway <= this.targetRunwayMs();
    }

    shouldPrepareNextCall({ generationComplete = false, playing = false, bufferedMs = 0 } = {}) {
      if (!generationComplete || !playing || !Number.isFinite(bufferedMs)) return false;
      // Preserve a full adaptive TTS refill runway before yielding Metal to
      // the next microphone prefix. Actual caller speech bypasses this gate.
      return bufferedMs >= Math.max(240, this.targetRunwayMs());
    }

    shouldBufferCompleteReply() {
      return this.bufferComplete;
    }

    progressTimeoutMs() {
      // Allow a cold/slower machine proportionally more time when its observed
      // first audio or packet cadence is high, while retaining a hard bound.
      return Math.round(clamp(
        Math.max(20_000, this.firstAudioMs * 8, this.maximumPacketGapMs * 8),
        20_000,
        60_000,
      ));
    }

    chunkWordLimit({ first = false, bufferedMs = 0 } = {}) {
      if (first) return 4;
      const runway = Number.isFinite(bufferedMs) ? Math.max(0, bufferedMs) : 0;
      const missingLeadMs = Math.max(0, this.targetRunwayMs() - runway);
      // Each TTS invocation pays a first-audio/switching cost. Ask for enough
      // speech to amortize roughly six such costs, but cap the net lead a fast
      // producer can add before the scheduler gets another decision point.
      const switchingQuantumMs = Math.max(1_200, this.firstAudioMs * 6);
      const desiredLeadMs = Math.max(300, missingLeadMs + this.underrunPenaltyMs);
      const leadGainPerWordMs = this.speechMsPerWord
        * Math.max(0.05, 1 - Math.min(this.productionRtf, 0.95));
      const switchingWords = Math.ceil(switchingQuantumMs / this.speechMsPerWord);
      const leadWords = Math.ceil(desiredLeadMs / leadGainPerWordMs);
      return Math.round(clamp(Math.max(switchingWords, leadWords), 4, 16));
    }

    noteWait(durationMs) {
      if (!Number.isFinite(durationMs) || durationMs < 0) return;
      this.waits += 1;
      this.waitMs += durationMs;
    }

    snapshot() {
      return {
        target_runway_ms: this.targetRunwayMs(),
        startup_runway_ms: this.startupRunwayMs(),
        startup_delay_ms: this.startupDelayMs(),
        observed_required_startup_delay_ms:
          Math.round(this.requiredStartupDelayMs * 100) / 100,
        observed_first_audio_ms: Math.round(this.firstAudioMs * 100) / 100,
        observed_packet_audio_ms: Math.round(this.firstPacketAudioMs * 100) / 100,
        observed_max_packet_gap_ms: Math.round(this.maximumPacketGapMs * 100) / 100,
        observed_max_boundary_gap_ms: Math.round(this.maximumBoundaryGapMs * 100) / 100,
        observed_production_rtf: Math.round(this.productionRtf * 10_000) / 10_000,
        buffer_complete_reply: this.bufferComplete,
        slow_segment_count: this.slowSegmentCount,
        healthy_segment_count: this.healthySegmentCount,
        observed_speech_ms_per_word: Math.round(this.speechMsPerWord * 100) / 100,
        underrun_penalty_ms: Math.round(this.underrunPenaltyMs * 100) / 100,
        waits: this.waits,
        wait_ms: Math.round(this.waitMs * 100) / 100,
        maximum_runway_ms: Math.round(this.maximumRunwayMs * 100) / 100,
      };
    }

    calibration() {
      return {
        initialFirstAudioMs: this.firstAudioMs,
        initialPacketAudioMs: this.firstPacketAudioMs,
        initialMaximumPacketGapMs: this.maximumPacketGapMs,
        initialMaximumBoundaryGapMs: this.maximumBoundaryGapMs,
        initialRequiredStartupDelayMs: this.requiredStartupDelayMs,
        initialProductionRtf: this.productionRtf,
        initialSpeechMsPerWord: this.speechMsPerWord,
        initialUnderrunPenaltyMs: this.underrunPenaltyMs,
        initialSlowSegmentCount: this.slowSegmentCount,
        initialHealthySegmentCount: this.healthySegmentCount,
        initialBufferComplete: this.bufferComplete,
        minimumTargetMs: this.minimumTargetMs,
        maximumTargetMs: this.maximumTargetMs,
      };
    }
  }

  class SemanticLeadController {
    constructor({ lowWaterWords = 8, highWaterWords = 16 } = {}) {
      if (!(lowWaterWords >= 0 && highWaterWords > lowWaterWords)) {
        throw new RangeError("semantic lead watermarks must form a positive hysteresis band");
      }
      this.lowWaterWords = lowWaterWords;
      this.highWaterWords = highWaterWords;
      this.paused = false;
      this.maximumQueuedWords = 0;
    }

    shouldPause({ queuedWords = 0, readyChunk = false, terminal = false } = {}) {
      const words = Number.isFinite(queuedWords) ? Math.max(0, Math.floor(queuedWords)) : 0;
      this.maximumQueuedWords = Math.max(this.maximumQueuedWords, words);
      if (terminal || !readyChunk) this.paused = false;
      else if (this.paused) this.paused = words > this.lowWaterWords;
      else this.paused = words >= this.highWaterWords;
      return this.paused;
    }

    snapshot() {
      return {
        desired_pause: this.paused,
        maximum_queued_words: this.maximumQueuedWords,
        low_water_words: this.lowWaterWords,
        high_water_words: this.highWaterWords,
      };
    }
  }

  return { AdaptiveVoiceLeadController, SemanticLeadController };
}));

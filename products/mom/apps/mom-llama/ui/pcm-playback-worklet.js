class MomPcmRingProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const requestedChannels = Number(options?.processorOptions?.channels || 1);
    const requestedFrames = Number(options?.processorOptions?.capacityFrames || sampleRate * 30);
    this.channels = Math.max(1, Math.min(2, Math.trunc(requestedChannels)));
    this.capacity = Math.max(128, Math.trunc(requestedFrames));
    this.recoveryFrames = Math.min(
      this.capacity,
      Math.max(128, Math.trunc(Number(options?.processorOptions?.recoveryFrames || sampleRate / 10))),
    );
    this.rings = Array.from({ length: this.channels }, () => new Float32Array(this.capacity));
    this.read = 0;
    this.write = 0;
    this.available = 0;
    this.receivedFrames = 0;
    this.renderedFrames = 0;
    this.renderQuanta = 0;
    this.consumedSinceCredit = 0;
    this.maxBufferedFrames = 0;
    this.minBufferedFrames = null;
    this.underruns = 0;
    this.recoveries = 0;
    this.rebufferedFrames = 0;
    this.rebuffering = false;
    this.rebufferSilentFrames = 0;
    this.receivedAudio = false;
    this.underrunReported = false;
    this.finished = false;
    this.drained = false;
    // Signal evidence is numeric only. Count samples copied from the PCM ring;
    // underrun fills and the deliberate drain grace are never counted.
    this.signal = {
      frames: 0,
      samples: 0,
      sumSquares: 0,
      peak: 0,
      quanta: 0,
      minQuantumRms: null,
      maxQuantumRms: 0,
      lowEnergyFrames: 0,
      lowEnergyQuanta: 0,
      currentLowEnergyFrames: 0,
      maxLowEnergyFrames: 0,
    };
    this.lowEnergyThresholdRms = Math.pow(10, -50 / 20);
    // A render quantum is submitted ahead of the device. Keep the node
    // connected briefly after its last PCM frame so the output route can
    // actually play that frame before the main thread tears the node down.
    this.drainGraceFrames = Math.ceil(sampleRate / 10);
    this.silentDrainFrames = 0;
    this.stopped = false;
    this.paused = false;
    this.firstReceiveContextFrame = null;
    this.firstRenderContextFrame = null;
    this.port.onmessage = ({ data }) => this.receive(data);
  }

  receive(message) {
    if (message?.type === "reset") {
      this.stopped = true;
      this.available = 0;
      return;
    }
    if (this.stopped) return;
    if (message?.type === "pause" || message?.type === "resume") {
      this.paused = message.type === "pause";
      return;
    }
    if (message?.type === "finish") {
      if (this.rebuffering) this.reportRecoveredUnderrun();
      this.finished = true;
      return;
    }
    if (message?.type !== "pcm" || this.stopped) return;
    const channels = Number(message.channels || 0);
    const byteOffset = Number(message.byteOffset || 0);
    const byteLength = Number(message.byteLength || 0);
    if (channels !== this.channels || byteLength % (channels * 2) !== 0) {
      this.port.postMessage({ type: "error", message: "PCM worklet received an invalid frame." });
      return;
    }
    const frames = byteLength / (channels * 2);
    if (frames > this.capacity - this.available) {
      this.port.postMessage({
        type: "error",
        message: "PCM worklet ring overflowed.",
        frames,
        availableFrames: this.available,
      });
      return;
    }
    const pcm = new DataView(message.buffer, byteOffset, byteLength);
    for (let frame = 0; frame < frames; frame += 1) {
      const target = (this.write + frame) % this.capacity;
      for (let channel = 0; channel < channels; channel += 1) {
        this.rings[channel][target] = pcm.getInt16((frame * channels + channel) * 2, true) / 32768;
      }
    }
    this.write = (this.write + frames) % this.capacity;
    this.available += frames;
    this.silentDrainFrames = 0;
    this.receivedFrames += frames;
    this.maxBufferedFrames = Math.max(this.maxBufferedFrames, this.available);
    if (this.firstReceiveContextFrame === null) this.firstReceiveContextFrame = currentFrame;
    this.receivedAudio = true;
    this.underrunReported = false;
    this.port.postMessage({
      type: "packet_admitted",
      packetIndex: Number(message.packetIndex || 0),
      sequence: Number(message.sequence || 0),
      sampleOffset: Number(message.sampleOffset || 0),
      frames,
      bufferedFrames: this.available,
      audioContextFrame: currentFrame,
      enqueuedContextTimeMs: Number(message.enqueuedContextTimeMs || 0),
    });
  }

  reportRecoveredUnderrun() {
    this.port.postMessage({
      type: "underrun",
      missingFrames: this.rebufferSilentFrames,
      bufferedFrames: this.available,
      recovered: true,
      audioContextFrame: currentFrame,
    });
    this.recoveries += 1;
    this.rebufferedFrames += this.rebufferSilentFrames;
    this.rebufferSilentFrames = 0;
    this.rebuffering = false;
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    if (!output?.length) return !this.stopped;
    if (this.stopped) {
      for (const channel of output) channel.fill(0);
      return false;
    }
    if (this.paused) {
      for (const channel of output) channel.fill(0);
      return true;
    }
    const frames = output[0].length;
    this.renderQuanta += 1;
    if (this.receivedAudio && !this.finished && (this.rebuffering || this.available < frames)) {
      if (!this.rebuffering) {
        this.underruns += 1;
        this.underrunReported = true;
        this.rebuffering = true;
        this.port.postMessage({
          type: "underrun_started",
          bufferedFrames: this.available,
          audioContextFrame: currentFrame,
        });
      }
      if (this.available < this.recoveryFrames) {
        // Starvation zeros are not PCM signal and break a contiguous PCM run.
        this.signal.currentLowEnergyFrames = 0;
        for (const channel of output) channel.fill(0);
        this.rebufferSilentFrames += frames;
        return true;
      }
      this.reportRecoveredUnderrun();
    }
    let quantumPcmFrames = 0;
    let quantumSampleCount = 0;
    let quantumSumSquares = 0;
    let quantumPeak = 0;
    for (let frame = 0; frame < frames; frame += 1) {
      const hasSample = this.available > 0;
      for (let channel = 0; channel < output.length; channel += 1) {
        const sample = hasSample ? this.rings[channel][this.read] : 0;
        output[channel][frame] = sample;
        if (hasSample) {
          const magnitude = Math.abs(sample);
          quantumPeak = Math.max(quantumPeak, magnitude);
          quantumSumSquares += sample * sample;
          quantumSampleCount += 1;
        }
      }
      if (hasSample) {
        quantumPcmFrames += 1;
        if (this.firstRenderContextFrame === null) {
          this.firstRenderContextFrame = currentFrame + frame;
          this.port.postMessage({
            type: "render_started",
            audioContextFrame: this.firstRenderContextFrame,
            bufferedFrames: this.available,
          });
        }
        this.read = (this.read + 1) % this.capacity;
        this.available -= 1;
        this.renderedFrames += 1;
        this.consumedSinceCredit += 1;
      }
    }
    if (quantumPcmFrames > 0) {
      const signal = this.signal;
      const rms = Math.sqrt(quantumSumSquares / quantumSampleCount);
      signal.frames += quantumPcmFrames;
      signal.samples += quantumSampleCount;
      signal.sumSquares += quantumSumSquares;
      signal.peak = Math.max(signal.peak, quantumPeak);
      signal.quanta += 1;
      signal.minQuantumRms = signal.minQuantumRms === null
        ? rms : Math.min(signal.minQuantumRms, rms);
      signal.maxQuantumRms = Math.max(signal.maxQuantumRms, rms);
      if (rms <= this.lowEnergyThresholdRms) {
        signal.lowEnergyFrames += quantumPcmFrames;
        signal.lowEnergyQuanta += 1;
        signal.currentLowEnergyFrames += quantumPcmFrames;
        signal.maxLowEnergyFrames = Math.max(
          signal.maxLowEnergyFrames, signal.currentLowEnergyFrames,
        );
      } else {
        signal.currentLowEnergyFrames = 0;
      }
      // A partial quantum ends at a non-PCM output frame. Count only its PCM
      // prefix, then close the run so later audio cannot bridge that gap.
      if (quantumPcmFrames < frames) signal.currentLowEnergyFrames = 0;
    } else {
      this.signal.currentLowEnergyFrames = 0;
    }
    if (this.receivedAudio && !this.finished && !this.rebuffering) {
      this.minBufferedFrames = this.minBufferedFrames === null
        ? this.available
        : Math.min(this.minBufferedFrames, this.available);
    }
    if (this.consumedSinceCredit >= 2_048) {
      this.port.postMessage({
        type: "consumed",
        frames: this.consumedSinceCredit,
        bufferedFrames: this.available,
        renderedFrames: this.renderedFrames,
        audioContextFrame: currentFrame + frames,
      });
      this.consumedSinceCredit = 0;
    }
    if (this.finished && this.available === 0 && !this.drained) {
      this.silentDrainFrames += frames;
      if (this.silentDrainFrames < this.drainGraceFrames) return true;
      if (this.consumedSinceCredit > 0) {
        this.port.postMessage({
          type: "consumed",
          frames: this.consumedSinceCredit,
          bufferedFrames: this.available,
          renderedFrames: this.renderedFrames,
          audioContextFrame: currentFrame + frames,
        });
        this.consumedSinceCredit = 0;
      }
      this.drained = true;
      this.port.postMessage({
        type: "drained",
        receivedFrames: this.receivedFrames,
        renderedFrames: this.renderedFrames,
        renderQuanta: this.renderQuanta,
        maxBufferedFrames: this.maxBufferedFrames,
        underruns: this.underruns,
        recoveries: this.recoveries,
        rebufferedFrames: this.rebufferedFrames,
        minBufferedFrames: this.minBufferedFrames,
        firstReceiveContextFrame: this.firstReceiveContextFrame,
        firstRenderContextFrame: this.firstRenderContextFrame,
        pcmSignalFrames: this.signal.frames,
        pcmSignalRms: this.signal.samples > 0
          ? Math.sqrt(this.signal.sumSquares / this.signal.samples) : null,
        pcmSignalPeak: this.signal.samples > 0 ? this.signal.peak : null,
        pcmSignalMinQuantumRms: this.signal.minQuantumRms,
        pcmSignalMaxQuantumRms: this.signal.quanta > 0 ? this.signal.maxQuantumRms : null,
        pcmLowEnergyThresholdRms: this.lowEnergyThresholdRms,
        pcmLowEnergyFrames: this.signal.lowEnergyFrames,
        pcmLowEnergyQuanta: this.signal.lowEnergyQuanta,
        pcmMaxLowEnergyFrames: this.signal.maxLowEnergyFrames,
      });
    }
    return true;
  }
}

registerProcessor("mom-pcm-ring", MomPcmRingProcessor);

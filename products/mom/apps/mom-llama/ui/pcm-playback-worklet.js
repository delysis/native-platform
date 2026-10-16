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
    // A render quantum is submitted ahead of the device. Keep the node
    // connected briefly after its last PCM frame so the output route can
    // actually play that frame before the main thread tears the node down.
    this.drainGraceFrames = Math.ceil(sampleRate / 10);
    this.silentDrainFrames = 0;
    this.stopped = false;
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
    if (!output?.length || this.stopped) return !this.stopped;
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
        for (const channel of output) channel.fill(0);
        this.rebufferSilentFrames += frames;
        return true;
      }
      this.reportRecoveredUnderrun();
    }
    for (let frame = 0; frame < frames; frame += 1) {
      const hasSample = this.available > 0;
      for (let channel = 0; channel < output.length; channel += 1) {
        output[channel][frame] = hasSample ? this.rings[channel][this.read] : 0;
      }
      if (hasSample) {
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
      });
    }
    return true;
  }
}

registerProcessor("mom-pcm-ring", MomPcmRingProcessor);

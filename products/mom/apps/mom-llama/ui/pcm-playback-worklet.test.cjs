const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "pcm-playback-worklet.js"), "utf8");

const harness = (options = { channels: 1, capacityFrames: 128 }) => {
  const messages = [];
  let Processor = null;
  class AudioWorkletProcessor {
    constructor() {
      this.port = {
        onmessage: null,
        postMessage(message) { messages.push(message); },
      };
    }
  }
  const context = {
    AudioWorkletProcessor,
    sampleRate: 24_000,
    currentFrame: 0,
    registerProcessor(name, constructor) {
      assert.equal(name, "mom-pcm-ring");
      Processor = constructor;
    },
  };
  vm.runInNewContext(source, context);
  return { processor: new Processor({ processorOptions: options }), messages };
};

const pcm16 = (...samples) => {
  const buffer = new ArrayBuffer(samples.length * 2);
  const view = new DataView(buffer);
  samples.forEach((sample, index) => view.setInt16(index * 2, sample, true));
  return buffer;
};

test("the audio-thread ring converts and drains exact PCM order", () => {
  const { processor, messages } = harness();
  const buffer = pcm16(-32768, -16384, 0, 32767);
  processor.receive({ type: "pcm", buffer, byteOffset: 0, byteLength: 8, channels: 1 });
  processor.receive({ type: "finish" });
  const output = new Float32Array(4);
  assert.equal(processor.process([], [[output]]), true);
  assert.deepEqual(Array.from(output), [-1, -0.5, 0, 32767 / 32768]);
  assert.equal(messages.some((message) => message.type === "drained"), false);
  for (let index = 0; index < 19; index += 1) {
    processor.process([], [[new Float32Array(128)]]);
  }
  assert.deepEqual({ ...messages.at(-1) }, {
    type: "drained",
    receivedFrames: 4,
    renderedFrames: 4,
    renderQuanta: 20,
    maxBufferedFrames: 4,
    underruns: 0,
    recoveries: 0,
    rebufferedFrames: 0,
    minBufferedFrames: null,
    firstReceiveContextFrame: 0,
    firstRenderContextFrame: 0,
    pcmSignalFrames: 4,
    pcmSignalRms: Math.sqrt((1 + 0.25 + (32767 / 32768) ** 2) / 4),
    pcmSignalPeak: 1,
    pcmSignalMinQuantumRms: Math.sqrt((1 + 0.25 + (32767 / 32768) ** 2) / 4),
    pcmSignalMaxQuantumRms: Math.sqrt((1 + 0.25 + (32767 / 32768) ** 2) / 4),
    pcmLowEnergyThresholdRms: Math.pow(10, -50 / 20),
    pcmLowEnergyFrames: 0,
    pcmLowEnergyQuanta: 0,
    pcmMaxLowEnergyFrames: 0,
  });
});

test("explicit pause freezes the ring without consuming frames, and resume continues at the same sample", () => {
  const { processor, messages } = harness({ channels: 1, capacityFrames: 128 });
  const buffer = pcm16(8192, 4096, -4096, -8192, ...Array(124).fill(0));
  processor.receive({ type: "pcm", buffer, byteOffset: 0, byteLength: buffer.byteLength, channels: 1 });
  processor.receive({ type: "pause" });
  const pausedOutput = new Float32Array(4);
  assert.equal(processor.process([], [[pausedOutput]]), true);
  assert.deepEqual(Array.from(pausedOutput), [0, 0, 0, 0]);
  assert.equal(processor.available, 128, "pause leaves every buffered source frame queued");
  assert.equal(processor.read, 0);
  assert.equal(processor.renderedFrames, 0);
  assert.equal(messages.some((message) => message.type === "underrun_started"), false);

  processor.receive({ type: "resume" });
  const resumedOutput = new Float32Array(4);
  assert.equal(processor.process([], [[resumedOutput]]), true);
  assert.deepEqual(Array.from(resumedOutput), [0.25, 0.125, -0.125, -0.25]);
  assert.equal(processor.available, 124);
  assert.equal(processor.read, 4);
  assert.equal(processor.renderedFrames, 4);
});

test("reset remains terminal if a paused renderer later receives resume", () => {
  const { processor, messages } = harness({ channels: 1, capacityFrames: 128 });
  const buffer = pcm16(8192, 4096, -4096, -8192);
  processor.receive({ type: "pcm", buffer, byteOffset: 0, byteLength: buffer.byteLength, channels: 1 });
  processor.receive({ type: "pause" });
  processor.receive({ type: "reset" });
  processor.receive({ type: "resume" });
  const output = new Float32Array([1, -1, 0.5, -0.5]);
  assert.equal(processor.process([], [[output]]), false, "reset terminates the old processor");
  assert.deepEqual(Array.from(output), [0, 0, 0, 0]);
  assert.equal(processor.available, 0);
  assert.equal(processor.renderedFrames, 0);
  assert.equal(messages.some((message) => message.type === "render_started"), false);
});

test("PCM signal aggregates cross packet boundaries and count only the partial PCM tail", () => {
  const { processor, messages } = harness({ channels: 1, capacityFrames: 32 });
  const send = (samples, packetIndex) => {
    const buffer = pcm16(...samples);
    processor.receive({
      type: "pcm", buffer, byteOffset: 0, byteLength: buffer.byteLength,
      channels: 1, packetIndex,
    });
  };
  // Four loud frames, then ten quiet frames split across packets and 4-frame
  // render quanta. The last quantum has two PCM frames and two drain zeros.
  send([16384, 16384, 16384], 0);
  send([16384, 0], 1);
  send(Array(5).fill(0), 2);
  send(Array(4).fill(0), 3);
  processor.receive({ type: "finish" });
  for (let index = 0; index < 4; index += 1) {
    processor.process([], [[new Float32Array(4)]]);
  }
  for (let index = 0; index < 600; index += 1) {
    processor.process([], [[new Float32Array(4)]]);
  }
  const drained = messages.find((message) => message.type === "drained");
  assert.ok(drained);
  assert.equal(drained.pcmSignalFrames, 14);
  assert.equal(drained.pcmSignalPeak, 0.5);
  assert.ok(Math.abs(drained.pcmSignalRms - Math.sqrt(1 / 14)) < 1e-6);
  assert.equal(drained.pcmLowEnergyFrames, 10);
  assert.equal(drained.pcmMaxLowEnergyFrames, 10);
  assert.equal(drained.pcmLowEnergyQuanta, 3);
});

test("PCM signal aggregates exclude the worklet's starvation-filled output quantum", () => {
  const { processor, messages } = harness({
    channels: 1, capacityFrames: 128, recoveryFrames: 4,
  });
  const short = pcm16(0, 0);
  processor.receive({ type: "pcm", buffer: short, byteOffset: 0, byteLength: short.byteLength, channels: 1 });
  processor.process([], [[new Float32Array(4)]]);
  assert.equal(messages.find((message) => message.type === "underrun_started")?.bufferedFrames, 2);

  const rest = pcm16(...Array(126).fill(16384));
  processor.receive({ type: "pcm", buffer: rest, byteOffset: 0, byteLength: rest.byteLength, channels: 1 });
  processor.process([], [[new Float32Array(128)]]);
  processor.receive({ type: "finish" });
  for (let index = 0; index < 19; index += 1) {
    processor.process([], [[new Float32Array(128)]]);
  }
  const drained = messages.find((message) => message.type === "drained");
  assert.equal(drained.underruns, 1);
  assert.equal(drained.pcmSignalFrames, 128);
  assert.equal(drained.pcmLowEnergyFrames, 0);
  assert.equal(drained.pcmMaxLowEnergyFrames, 0);
});

test("the audio-thread ring reports bounded overflow without overwriting unread PCM", () => {
  const { processor, messages } = harness();
  const buffer = new ArrayBuffer(130 * 2);
  processor.receive({ type: "pcm", buffer, byteOffset: 0, byteLength: buffer.byteLength, channels: 1 });
  assert.equal(processor.available, 0);
  assert.equal(messages.at(-1).type, "error");
  assert.match(messages.at(-1).message, /overflowed/);
});

test("the audio-thread ring coherently re-buffers after a producer underrun", () => {
  const { processor, messages } = harness({ channels: 1, capacityFrames: 128, recoveryFrames: 4 });
  const buffer = pcm16(8192, 4096);
  processor.receive({ type: "pcm", buffer, byteOffset: 0, byteLength: 4, channels: 1 });
  const output = new Float32Array(4);
  processor.process([], [[output]]);
  assert.deepEqual(Array.from(output), [0, 0, 0, 0]);
  assert.equal(messages.find((message) => message.type === "underrun_started")?.bufferedFrames, 2);
  const recovery = pcm16(-8192, -4096, ...Array(124).fill(0));
  processor.receive({
    type: "pcm",
    buffer: recovery,
    byteOffset: 0,
    byteLength: recovery.byteLength,
    channels: 1,
  });
  processor.process([], [[output]]);
  assert.deepEqual(Array.from(output), [0.25, 0.125, -0.25, -0.125]);
  const underrun = messages.find((message) => message.type === "underrun");
  assert.equal(underrun.missingFrames, 4);
  assert.equal(underrun.recovered, true);
  assert.equal(messages.find((message) => message.type === "render_started")?.audioContextFrame, 0);
});

test("the audio thread reports packet admission and consumption on its render clock", () => {
  const { processor, messages } = harness({ channels: 1, capacityFrames: 4096 });
  const buffer = new ArrayBuffer(2_048 * 2);
  processor.receive({
    type: "pcm",
    buffer,
    byteOffset: 0,
    byteLength: buffer.byteLength,
    channels: 1,
    packetIndex: 7,
    sequence: 3,
    sampleOffset: 1024,
    enqueuedContextTimeMs: 12.5,
  });
  const admitted = messages.find((message) => message.type === "packet_admitted");
  assert.deepEqual({ ...admitted }, {
    type: "packet_admitted",
    packetIndex: 7,
    sequence: 3,
    sampleOffset: 1024,
    frames: 2048,
    bufferedFrames: 2048,
    audioContextFrame: 0,
    enqueuedContextTimeMs: 12.5,
  });
  for (let index = 0; index < 16; index += 1) {
    processor.process([], [[new Float32Array(128)]]);
  }
  const consumed = messages.find((message) => message.type === "consumed");
  assert.equal(consumed.frames, 2048);
  assert.equal(consumed.bufferedFrames, 0);
  assert.equal(consumed.renderedFrames, 2048);
});

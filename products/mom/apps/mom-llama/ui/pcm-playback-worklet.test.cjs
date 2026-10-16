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
  assert.deepEqual({ ...messages.at(-1) }, {
    type: "drained",
    receivedFrames: 4,
    renderedFrames: 4,
    renderQuanta: 1,
    maxBufferedFrames: 4,
    underruns: 0,
    recoveries: 0,
    rebufferedFrames: 0,
    minBufferedFrames: null,
    firstReceiveContextFrame: 0,
    firstRenderContextFrame: 0,
  });
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

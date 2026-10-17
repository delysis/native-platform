#!/usr/bin/env node

// Render an A/B from the same resident streaming route used by the app.
// Usage: node voice-resident-audition.mjs PLAN.json OUTPUT_DIR
// PLAN: { model, helper, persona_id, text, references: [{ label, audio }] }

import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { performance } from 'node:perf_hooks';

const [planName, outputName] = process.argv.slice(2);
if (!planName || !outputName) {
  console.error('Usage: node voice-resident-audition.mjs PLAN.json OUTPUT_DIR');
  process.exit(2);
}

const plan = JSON.parse(await readFile(planName, 'utf8'));
if (!plan.model || !plan.helper || !plan.persona_id || !plan.text ||
    !Array.isArray(plan.references) || plan.references.length < 1) {
  throw new Error('Plan needs model, helper, persona_id, text, and references');
}
const outputDir = resolve(outputName);
await mkdir(outputDir, { recursive: true });
const references = await Promise.all(plan.references.map(async (entry) => {
  if (!/^[a-z0-9_-]+$/.test(entry.label) || !entry.audio) {
    throw new Error('Each reference needs a filename-safe label and audio path');
  }
  const audio = resolve(entry.audio);
  return { ...entry, audio, sha256: createHash('sha256').update(await readFile(audio)).digest('hex') };
}));
if (new Set(references.map((entry) => entry.label)).size !== references.length) {
  throw new Error('Reference labels must be unique');
}

const helper = spawn(resolve(plan.helper), ['--resident', resolve(plan.model)], {
  stdio: ['pipe', 'pipe', 'pipe'],
});
let stderr = '';
helper.stderr.setEncoding('utf8');
helper.stderr.on('data', (chunk) => { stderr = (stderr + chunk).slice(-8192); });

function wav(pcm, sampleRate) {
  const header = Buffer.alloc(44);
  header.write('RIFF', 0);
  header.writeUInt32LE(36 + pcm.length, 4);
  header.write('WAVEfmt ', 8);
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20);
  header.writeUInt16LE(1, 22);
  header.writeUInt32LE(sampleRate, 24);
  header.writeUInt32LE(sampleRate * 2, 28);
  header.writeUInt16LE(2, 32);
  header.writeUInt16LE(16, 34);
  header.write('data', 36);
  header.writeUInt32LE(pcm.length, 40);
  return Buffer.concat([header, pcm]);
}

const receipt = {
  schema: 'mom-voice-resident-audition-v1',
  route: 'Qwen3TTSHelper --resident; same bounded streaming generation as the app',
  reference_transcript_use: 'none; resident synthesis uses speaker embedding and decoder context',
  model: resolve(plan.model),
  helper: resolve(plan.helper),
  persona_id: plan.persona_id,
  text: plan.text,
  results: [],
};
let active = null;
let index = 0;
let readyAt = 0;
const startedAt = performance.now();
const deadline = setTimeout(() => {
  console.error('Resident audition exceeded 180 seconds:', stderr);
  helper.kill('SIGTERM');
}, 180_000);

function sendNext() {
  if (index === references.length) {
    helper.stdin.end();
    return;
  }
  const reference = references[index++];
  active = { reference, startedAt: performance.now(), firstPcmMs: null, sampleRate: null,
    packets: [], samples: 0, cache: null, prefixHit: null };
  helper.stdin.write(JSON.stringify({
    id: reference.label,
    text: plan.text,
    persona_id: plan.persona_id,
    ref_audio: reference.audio,
    ref_sha256: reference.sha256,
    ref_text: reference.transcript ?? '',
    chunk_size: 2,
  }) + '\n');
}

async function finish(event) {
  const item = active;
  if (!item) throw new Error(`Unexpected ${event.event} event`);
  if (event.event !== 'done') throw new Error(`${item.reference.label}: ${event.event}: ${event.detail ?? ''}`);
  if (!item.samples || !item.sampleRate) throw new Error(`${item.reference.label}: no playable PCM`);
  const output = join(outputDir, `${item.reference.label}.wav`);
  const buffer = wav(Buffer.concat(item.packets), item.sampleRate);
  await writeFile(output, buffer);
  receipt.results.push({
    label: item.reference.label,
    reference_audio: item.reference.audio,
    reference_sha256: item.reference.sha256,
    transcript_status: item.reference.transcript_status ?? 'not_used_by_resident_route',
    output,
    output_sha256: createHash('sha256').update(buffer).digest('hex'),
    sample_rate: item.sampleRate,
    samples: item.samples,
    audio_seconds: item.samples / item.sampleRate,
    packet_count: item.packets.length,
    first_pcm_ms: item.firstPcmMs,
    total_ms: Math.round(performance.now() - item.startedAt),
    reference_cache: item.cache,
    prepared_prefix_cache_hit: item.prefixHit,
  });
  console.log(`${item.reference.label}: ${receipt.results.at(-1).audio_seconds.toFixed(2)}s audio, ${receipt.results.at(-1).total_ms}ms render`);
  active = null;
  sendNext();
}

let pending = '';
let handling = Promise.resolve();
helper.stdout.setEncoding('utf8');
helper.stdout.on('data', (chunk) => {
  pending += chunk;
  while (pending.includes('\n')) {
    const end = pending.indexOf('\n');
    const line = pending.slice(0, end);
    pending = pending.slice(end + 1);
    if (!line.trim()) continue;
    handling = handling.then(async () => {
      const event = JSON.parse(line);
      if (event.event === 'ready') {
        readyAt = performance.now();
        sendNext();
      } else if (event.event === 'audio') {
        if (!active || event.id !== active.reference.label) throw new Error('Audio request ID mismatch');
        const pcm = Buffer.from(event.data, 'base64');
        if (pcm.length !== event.samples * 2 ||
            (active.sampleRate !== null && active.sampleRate !== event.sample_rate)) {
          throw new Error('Invalid or inconsistent PCM packet');
        }
        if (active.firstPcmMs === null) active.firstPcmMs = Math.round(performance.now() - active.startedAt);
        active.sampleRate = event.sample_rate;
        active.samples += event.samples;
        active.packets.push(pcm);
        active.cache ??= event.reference_cache ?? null;
        active.prefixHit ??= event.prepared_prefix_cache_hit ?? null;
      } else if (['done', 'error', 'cancelled', 'fatal'].includes(event.event)) {
        await finish(event);
      }
    }).catch((error) => {
      console.error(error);
      helper.kill('SIGTERM');
    });
  }
});

const exitCode = await new Promise((done, fail) => {
  helper.on('error', fail);
  helper.on('close', (code) => done(code));
});
clearTimeout(deadline);
await handling;
if (exitCode !== 0 || receipt.results.length !== references.length) {
  throw new Error(`Helper exited ${exitCode}; ${receipt.results.length}/${references.length} results. ${stderr}`);
}
receipt.model_load_ms = Math.round(readyAt - startedAt);
await writeFile(join(outputDir, 'receipt.json'), JSON.stringify(receipt, null, 2) + '\n');
console.log(`Receipt: ${join(outputDir, 'receipt.json')}`);

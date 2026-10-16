#!/usr/bin/env python3
"""Measure resident CPU Chatterbox Nano latency on live voice-sized phrases."""

from __future__ import annotations

import argparse
import json
import platform
import resource
import statistics
import time
from pathlib import Path

import soundfile
import torch
from chatterbox.tts_turbo import ChatterboxTurboTTS


PHRASES = {
    "four_words": "I hear you clearly.",
    "eight_words": "I hear you, and we can slow down.",
    "twenty_four_words": (
        "I hear what you are saying, and we can slow down together while we notice "
        "what feels most important in this moment right now."
    ),
}


def milliseconds(started: float) -> float:
    return round((time.perf_counter() - started) * 1_000, 3)


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * fraction)))
    return ordered[index]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--threads", type=int, default=8)
    parser.add_argument("--temperature", type=float, default=0.8)
    parser.add_argument("--sample-dir", type=Path)
    args = parser.parse_args()

    if args.runs < 1:
        raise SystemExit("--runs must be positive")
    if not args.reference.is_file():
        raise SystemExit(f"missing reference audio: {args.reference}")

    torch.set_num_threads(args.threads)
    torch.set_num_interop_threads(1)
    load_started = time.perf_counter()
    model = ChatterboxTurboTTS.from_pretrained(device="cpu", nano=True)
    model_load_ms = milliseconds(load_started)

    condition_started = time.perf_counter()
    model.prepare_conditionals(str(args.reference), exaggeration=0.0)
    condition_ms = milliseconds(condition_started)

    # Separate one-time graph/library initialization from resident samples.
    warm_started = time.perf_counter()
    warm = model.generate(PHRASES["four_words"], temperature=args.temperature)
    warmup_ms = milliseconds(warm_started)
    warmup_audio_ms = round(warm.shape[-1] * 1_000 / model.sr, 3)

    rows: list[dict[str, object]] = []
    for run in range(args.runs):
        for label, text in PHRASES.items():
            torch.manual_seed(10_000 + run)
            started = time.perf_counter()
            wav = model.generate(text, temperature=args.temperature)
            generate_ms = milliseconds(started)
            audio_ms = round(wav.shape[-1] * 1_000 / model.sr, 3)
            rows.append(
                {
                    "run": run + 1,
                    "label": label,
                    "words": len(text.split()),
                    "characters": len(text),
                    # The official Nano implementation returns a complete
                    # waveform, so first PCM cannot precede this value.
                    "first_pcm_ms": generate_ms,
                    "generation_ms": generate_ms,
                    "output_audio_ms": audio_ms,
                    "production_rtf": round(generate_ms / audio_ms, 4),
                }
            )
            if args.sample_dir is not None and run == 0:
                args.sample_dir.mkdir(parents=True, exist_ok=True)
                soundfile.write(
                    args.sample_dir / f"chatterbox_nano_{label}.wav",
                    wav.squeeze(0).numpy(),
                    model.sr,
                )

    summaries = {}
    for label in PHRASES:
        selected = [row for row in rows if row["label"] == label]
        first_pcm = [float(row["first_pcm_ms"]) for row in selected]
        rtfs = [float(row["production_rtf"]) for row in selected]
        summaries[label] = {
            "words": selected[0]["words"],
            "first_pcm_median_ms": round(statistics.median(first_pcm), 3),
            "first_pcm_p95_ms": round(percentile(first_pcm, 0.95), 3),
            "production_rtf_median": round(statistics.median(rtfs), 4),
            "production_rtf_worst": round(max(rtfs), 4),
        }

    result = {
        "schema": "mom.chatterbox-nano-cpu-benchmark.v1",
        "device": "cpu",
        "machine": platform.machine(),
        "platform": platform.platform(),
        "torch_version": torch.__version__,
        "torch_threads": torch.get_num_threads(),
        "torch_interop_threads": torch.get_num_interop_threads(),
        "sample_rate_hz": model.sr,
        "temperature": args.temperature,
        "reference": str(args.reference.resolve()),
        "model_load_ms": model_load_ms,
        "reference_condition_ms": condition_ms,
        "warmup_ms": warmup_ms,
        "warmup_audio_ms": warmup_audio_ms,
        "maximum_resident_set_bytes": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
        "implementation_streams_pcm": False,
        "summaries": summaries,
        "runs": rows,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.output.with_suffix(args.output.suffix + ".tmp")
    temporary.write_text(json.dumps(result, indent=2) + "\n")
    temporary.replace(args.output)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()

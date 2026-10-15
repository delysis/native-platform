"""Resident Qwen Base ICL worker. Stdout is a line-delimited JSON protocol."""

import base64
import json
import sys

protocol = sys.stdout
sys.stdout = sys.stderr  # Keep MLX and package logging off the protocol stream.

def send(value):
    protocol.write(json.dumps(value, separators=(",", ":")) + "\n")
    protocol.flush()

try:
    import numpy as np
    from mlx_audio.tts import load_model

    model = load_model(sys.argv[1])
    send({"event": "ready", "sample_rate": int(model.sample_rate)})
    for line in sys.stdin:
        try:
            job = json.loads(line)
            job_id = job["id"]
            first_chunk = True
            for result in model.generate(
                text=job["text"],
                ref_audio=job["ref_audio"],
                ref_text=job["ref_text"],
                language="English",
                stream=True,
                streaming_interval=0.4,
            ):
                samples = np.asarray(result.audio, dtype=np.float32).reshape(-1).copy()
                # MLX's stateful streaming decoder emits contiguous, non-overlap
                # chunks, so Hann fades at utterance edges are safe; the CUDA
                # fork's overlap crossfade would duplicate audio here.
                fade = min(512, samples.size)
                if first_chunk and fade:
                    phase = np.linspace(0.0, np.pi / 2.0, fade, endpoint=True, dtype=np.float32)
                    samples[:fade] *= np.sin(phase)
                if result.is_final_chunk and fade:
                    phase = np.linspace(0.0, np.pi / 2.0, fade, endpoint=True, dtype=np.float32)
                    samples[-fade:] *= np.cos(phase)
                # Clamp before conversion to avoid wraparound on out-of-range samples.
                pcm = (np.clip(samples, -1.0, 1.0) * 32767.0).astype("<i2").tobytes()
                if pcm:
                    send({
                        "event": "audio",
                        "id": job_id,
                        "sample_rate": int(result.sample_rate),
                        "samples": int(samples.size),
                        "data": base64.b64encode(pcm).decode("ascii"),
                    })
                    first_chunk = False
            send({"event": "done", "id": job_id})
        except Exception as error:
            send({"event": "error", "id": job.get("id") if isinstance(job, dict) else None,
                  "detail": str(error)[:512]})
except Exception as error:
    send({"event": "fatal", "detail": str(error)[:512]})

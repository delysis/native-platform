# Qwen3-TTS ANE evaluation

Isolated evaluation of TinyTrashLabs' Qwen3-TTS 0.6B Core ML runtime for Mom/Blessenger.
It is not selected by the product runtime. The evaluator preserves the required reference-voice
contract and refuses to synthesize without a matching WAV plus transcript.

The `QwenANE` sources are vendored from `TinyTrashLabs/gloam-voice-studio` commit
`60e0ddaf8797e4c33043427a6c7712833f4d82d3` under its MIT license. The model set is
`tinytrashlabs/Qwen3-TTS-0.6B-Base-ANE` commit
`551e82a5618b4804a51559bc508fece40d63b6a7` under Apache-2.0.

The evaluator records model load, voice preparation, warm-up, first playable PCM, total render,
RTF, cancellation acknowledgement, chunk sizes, and a Core ML compute-plan audit. It exits
nonzero if any required synthesis graph lacks Neural Engine-preferred operations. CPU-only voice
preparation and the CPU vocoder head are explicit upstream design choices and are reported rather
than disguised as ANE work.

Run `scripts/evaluate.sh --persona-id <id> --text <reply>` from this directory. The script reads the
existing local consultant reference manifest, downloads no voice data, writes the WAV and JSON
receipt under `artifacts/`, and requires the separately downloaded model directory.

# Collect voice diagnostics

The collector reads the installed app identity, fixed local model/resource locations, bundled voice manifest file names, and allowlisted numeric startup/navigation/voice-latency fields. It does not read encrypted settings, conversation storage, keys, transcripts, or arbitrary error text. It writes the report with owner-only file permissions and never changes the app or user data.

Run it **before updating or replacing the installed app**, while the current app is still in the state that showed the problem:

```sh
node products/mom/scripts/collect-voice-diagnostics.mjs \
  --app "/Applications/Blessenger.app" \
  --data-dir "$HOME/Library/Application Support/com.delysis.llama-native-kit.mom-llama" \
  --log-dir "$HOME/Library/Logs/com.delysis.llama-native-kit.mom-llama" \
  --out "$HOME/Desktop/blessenger-voice-diagnostics-$(date +%Y%m%d-%H%M%S).json"
```

Source may be pulled before this capture, but leave the installed app untouched until its report is saved. Use a new timestamped output name for every report so earlier evidence is preserved. If you have a built candidate bundle, pass its exact path as `--candidate-app` to record its bundle identity and executable digest beside the installed identity. If the candidate app uses a separate data or log directory, pass those exact directories instead. The report includes whether each location was explicit or discovered, but omits the path itself. Missing files are reported as missing evidence; a partial inventory is never described as ready. Model integrity is not checked. The bundle inventory identifies files on disk; it does not establish which executable an already-running process loaded.

For a daily local Codex task, use this instruction:

> Before updating Blessenger, collect the voice diagnostics report from the currently installed app and record its bundle version, build, source revision, and executable SHA-256. After a candidate is built, collect the candidate bundle identity separately with `--candidate-app`; never infer candidate identity from the build folder name. Keep the build receipt that maps the exact Git SHA/tree to the final executable digest alongside that report, because the installed bundle may not contain a source revision. Check that Blessenger is idle before any update. Pull and build only with the repository's existing documented commands and tests. Preserve the previous working bundle and do not replace it if a test or build fails. Never restart or replace the app while it is in use. Do not invent tests or expose conversation text, transcripts, keys, full user paths, or raw error strings in the report. Summarize only numeric timings, allowlisted stage names, safe error codes, resource-presence counts, and missing evidence.

This is reusable task text only; it does not create a scheduled task or authorize a daily update automatically.

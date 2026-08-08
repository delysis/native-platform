const COMMANDS: &[&str] = &[
    "gateway_status",
    "gateway_models",
    "gateway_generate",
    "gateway_stream",
    "gateway_cancel",
    "speech_status",
    "speech_plan_transcription",
    "speech_plan_synthesis",
    "speech_synthesize",
    "speech_synthesize_stream",
    "speech_transcribe",
    "speech_transcribe_stream",
    "speech_transcription_audio_push",
    "speech_transcription_audio_finish",
    "speech_cancel",
    "loopback_status",
    "loopback_start",
    "loopback_stop",
    "loopback_rotate_token",
];

fn main() {
    if let Err(error) = tauri_plugin::Builder::new(COMMANDS).try_build() {
        panic!("failed to build Free Token Energy plugin metadata: {error}");
    }
}

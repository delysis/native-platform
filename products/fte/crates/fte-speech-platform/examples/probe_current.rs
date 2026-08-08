use fte_speech_platform::PlatformCapabilityProbe;

#[cfg(target_os = "macos")]
use fte_speech_platform::apple::AppleCapabilitySource;

#[tokio::main]
async fn main() {
    let mut probe = PlatformCapabilityProbe::current();
    #[cfg(target_os = "macos")]
    probe
        .register(std::sync::Arc::new(AppleCapabilitySource))
        .expect("register Apple runtime capability source");

    let snapshot = probe.probe().await;
    match serde_json::to_string_pretty(&snapshot) {
        Ok(json) => println!("{json}"),
        Err(error) => eprintln!("failed to serialize speech capability snapshot: {error}"),
    }
}

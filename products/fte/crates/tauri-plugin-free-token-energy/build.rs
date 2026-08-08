const COMMANDS: &[&str] = &[
    "gateway_status",
    "gateway_models",
    "gateway_generate",
    "gateway_stream",
    "gateway_cancel",
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

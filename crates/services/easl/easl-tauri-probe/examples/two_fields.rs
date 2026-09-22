#![forbid(unsafe_code)]
//! Explicit opt-in only: run --build-info without constructing any application.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--build-info" {
        println!("{}", easl_tauri_probe::native::build_info());
        return Ok(());
    }
    if args.len() != 1 {
        return Err("Usage: easl-tauri-two-fields --build-info | --check-native-lifecycle | --open-probe (ephemeral, no IME/OS accessibility qualification)".into());
    }
    let context = tauri::generate_context!("tauri.conf.json");
    if args[0] == "--check-native-lifecycle" {
        let evidence = easl_tauri_probe::native::check_lifecycle(context)?;
        println!("{evidence}");
        return Ok(());
    }
    if args[0] != "--open-probe" {
        return Err("Usage: easl-tauri-two-fields --build-info | --check-native-lifecycle | --open-probe (ephemeral, no IME/OS accessibility qualification)".into());
    }
    easl_tauri_probe::native::run(context)
}

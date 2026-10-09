#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    use desktop_launch::{AppMode, LaunchAction};

    match desktop_launch::resolve(std::env::args_os().skip(1)) {
        Ok(LaunchAction::Help) => print!("{}", desktop_launch::HELP),
        Ok(LaunchAction::Run(AppMode::Document)) => loom_app_lib::run(),
        Ok(LaunchAction::Run(AppMode::Chat)) => {
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            mom_llama_app::run();
            #[cfg(any(target_os = "android", target_os = "ios"))]
            {
                eprintln!("The retained chat shell is currently desktop-only");
                return std::process::ExitCode::from(2);
            }
        }
        Err(error) => {
            eprintln!("{error}\n\n{}", desktop_launch::HELP);
            return std::process::ExitCode::from(2);
        }
    }
    std::process::ExitCode::SUCCESS
}

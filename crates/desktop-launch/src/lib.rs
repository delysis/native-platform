#![forbid(unsafe_code)]
//! Pure presentation selection before constructing either native application.
//!
//! Loading this configuration cannot grant network, tool, file-export, model,
//! or storage authority. It selects one existing lifecycle owner, not two
//! independently shutting-down runtimes in the same Tauri event loop.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

pub const HELP: &str = "Usage: loom-app [--mode document|chat] [--config ABSOLUTE_PATH]\n\n\
Defaults to the normal Tauri document editor. Chat selects the retained Mom\n\
Tauri shell, including its encrypted store and joined shutdown coordinator.\n\
The strict TOML configuration accepts only: mode = \"document\" or \"chat\".\n\
An explicit --mode overrides the file. No working-directory file is loaded.\n";
const MAX_CONFIG_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AppMode {
    #[default]
    Document,
    Chat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchAction {
    Help,
    Run(AppMode),
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LaunchConfig {
    mode: AppMode,
}

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("unknown or non-UTF-8 launch option")]
    UnknownOption,
    #[error("{0} requires a value")]
    MissingValue(&'static str),
    #[error("{0} may only be specified once")]
    DuplicateOption(&'static str),
    #[error("mode must be document or chat; simultaneous shells are not supported")]
    InvalidMode,
    #[error("configuration must be an absolute local regular-file path")]
    InvalidConfigPath,
    #[error("configuration exceeds 16 KiB")]
    ConfigTooLarge,
    #[error("configuration is not valid UTF-8")]
    ConfigEncoding,
    #[error("configuration is not valid launch TOML")]
    ConfigSyntax,
    #[error("configuration could not be read: {0}")]
    ConfigIo(#[from] std::io::Error),
}

/// Caller supplies arguments without the executable name. No runtime/store is
/// created and no path is read unless --config was supplied explicitly.
pub fn resolve(args: impl IntoIterator<Item = OsString>) -> Result<LaunchAction, LaunchError> {
    resolve_with(args, read_config)
}

fn resolve_with(
    args: impl IntoIterator<Item = OsString>,
    read: impl FnOnce(&Path) -> Result<Vec<u8>, LaunchError>,
) -> Result<LaunchAction, LaunchError> {
    let mut args = args.into_iter();
    let mut mode = None;
    let mut config_path = None;
    let mut help = false;
    while let Some(argument) = args.next() {
        let option = argument.to_str().ok_or(LaunchError::UnknownOption)?;
        match option {
            "--help" | "-h" => help = true,
            "--mode" => {
                if mode.is_some() {
                    return Err(LaunchError::DuplicateOption("--mode"));
                }
                let value = args.next().ok_or(LaunchError::MissingValue("--mode"))?;
                mode = Some(parse_mode(value.to_str())?);
            }
            "--config" => {
                if config_path.is_some() {
                    return Err(LaunchError::DuplicateOption("--config"));
                }
                config_path = Some(PathBuf::from(
                    args.next().ok_or(LaunchError::MissingValue("--config"))?,
                ));
            }
            value if value.starts_with("--mode=") => {
                if mode.is_some() {
                    return Err(LaunchError::DuplicateOption("--mode"));
                }
                mode = Some(parse_mode(value.strip_prefix("--mode="))?);
            }
            value if value.starts_with("--config=") => {
                if config_path.is_some() {
                    return Err(LaunchError::DuplicateOption("--config"));
                }
                config_path = Some(PathBuf::from(
                    value
                        .strip_prefix("--config=")
                        .ok_or(LaunchError::UnknownOption)?,
                ));
            }
            // Legacy Launch Services process-serial-number argument has no
            // application authority. Do not ignore arbitrary unknown flags.
            value if cfg!(target_os = "macos") && valid_process_serial_number(value) => {}
            _ => return Err(LaunchError::UnknownOption),
        }
    }
    if help {
        return Ok(LaunchAction::Help);
    }
    let configured = if let Some(path) = config_path {
        validate_config_path(&path)?;
        let bytes = read(&path)?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(LaunchError::ConfigTooLarge);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| LaunchError::ConfigEncoding)?;
        toml::from_str::<LaunchConfig>(text)
            .map_err(|_| LaunchError::ConfigSyntax)?
            .mode
    } else {
        AppMode::default()
    };
    Ok(LaunchAction::Run(mode.unwrap_or(configured)))
}

fn valid_process_serial_number(value: &str) -> bool {
    let Some(value) = value.strip_prefix("-psn_") else {
        return false;
    };
    let Some((high, low)) = value.split_once('_') else {
        return false;
    };
    !high.is_empty()
        && !low.is_empty()
        && high.bytes().all(|byte| byte.is_ascii_digit())
        && low.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_mode(value: Option<&str>) -> Result<AppMode, LaunchError> {
    match value {
        Some("document") => Ok(AppMode::Document),
        Some("chat") => Ok(AppMode::Chat),
        _ => Err(LaunchError::InvalidMode),
    }
}

fn validate_config_path(path: &Path) -> Result<(), LaunchError> {
    if !path.is_absolute() {
        return Err(LaunchError::InvalidConfigPath);
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if !matches!(path.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        {
            // Reject UNC/network/device/named-pipe namespaces before open.
            return Err(LaunchError::InvalidConfigPath);
        }
    }
    Ok(())
}

fn open_regular(path: &Path) -> Result<File, LaunchError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use nix::fcntl::OFlag;
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC).bits());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(LaunchError::InvalidConfigPath);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(LaunchError::InvalidConfigPath);
        }
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(LaunchError::ConfigTooLarge);
    }
    Ok(file)
}

fn read_config(path: &Path) -> Result<Vec<u8>, LaunchError> {
    validate_config_path(path)?;
    // Check the opened descriptor, not a pre-open metadata snapshot. The
    // nonblocking Unix open prevents replacement with a FIFO from hanging.
    // Parent directories remain explicit caller-selected path authority.
    let mut file = open_regular(path)?.take(MAX_CONFIG_BYTES + 1);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(LaunchError::ConfigTooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }
    fn config_path() -> String {
        std::env::temp_dir()
            .join("launch.toml")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn normal_launch_is_document_without_any_configuration_io() {
        let action =
            resolve_with(Vec::new(), |_| panic!("no implicit config access")).expect("parse");
        assert_eq!(action, LaunchAction::Run(AppMode::Document));
    }
    #[test]
    fn each_mode_selects_exactly_one_shell() {
        assert_eq!(
            resolve(args(&["--mode", "chat"])).expect("parse"),
            LaunchAction::Run(AppMode::Chat)
        );
        assert_eq!(
            resolve(args(&["--mode=document"])).expect("parse"),
            LaunchAction::Run(AppMode::Document)
        );
        assert!(matches!(
            resolve(args(&["--mode", "both"])),
            Err(LaunchError::InvalidMode)
        ));
    }
    #[test]
    fn cli_overrides_explicit_file_but_does_not_ignore_invalid_file() {
        let path = config_path();
        assert_eq!(
            resolve_with(args(&["--config", &path, "--mode", "document"]), |_| Ok(
                b"mode='chat'".to_vec()
            ))
            .expect("parse"),
            LaunchAction::Run(AppMode::Document)
        );
        assert!(matches!(
            resolve_with(args(&["--config", &path, "--mode", "document"]), |_| Ok(
                b"network=true".to_vec()
            )),
            Err(LaunchError::ConfigSyntax)
        ));
    }
    #[test]
    fn file_can_select_chat_and_unknown_keys_cannot_grant_authority() {
        let path = config_path();
        assert_eq!(
            resolve_with(args(&["--config", &path]), |_| Ok(b"mode='chat'".to_vec()))
                .expect("parse"),
            LaunchAction::Run(AppMode::Chat)
        );
        for text in [
            "mode='both'",
            "mode='chat'\n[features]\nnetwork=true",
            "mode='chat'\nmode='document'",
        ] {
            assert!(matches!(
                resolve_with(args(&["--config", &path]), |_| Ok(text.as_bytes().to_vec())),
                Err(LaunchError::ConfigSyntax)
            ));
        }
    }
    #[test]
    fn invalid_arguments_fail_instead_of_falling_back_to_another_shell() {
        for input in [
            vec!["--mode"],
            vec!["--mode", "chat", "--mode=document"],
            vec!["--unknown"],
            vec!["--config"],
            vec!["--config", "relative.toml"],
            vec!["--config="],
        ] {
            assert!(resolve(args(&input)).is_err());
        }
    }
    #[test]
    fn help_never_opens_config_or_initializes_a_store() {
        assert_eq!(
            resolve_with(args(&["--help", "--config", &config_path()]), |_| panic!(
                "help must be inert"
            ))
            .expect("parse"),
            LaunchAction::Help
        );
    }
    #[test]
    fn malformed_and_oversize_bytes_fail_closed() {
        let path = config_path();
        assert!(matches!(
            resolve_with(args(&["--config", &path]), |_| Ok(vec![0xff])),
            Err(LaunchError::ConfigEncoding)
        ));
        assert!(matches!(
            resolve_with(args(&["--config", &path]), |_| Ok(vec![
                b' ';
                MAX_CONFIG_BYTES
                    as usize
                    + 1
            ])),
            Err(LaunchError::ConfigTooLarge)
        ));
    }
    #[test]
    fn real_regular_file_is_bounded_and_directory_is_rejected() {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("launch.toml");
        std::fs::write(&path, "mode='chat'").expect("write");
        assert_eq!(read_config(&path).expect("read"), b"mode='chat'");
        assert!(read_config(dir.path()).is_err());
    }
    #[test]
    #[cfg(unix)]
    fn final_symlinks_and_fifos_are_not_config_files() {
        use nix::sys::stat::Mode;
        let dir = tempfile::tempdir().expect("directory");
        let target = dir.path().join("target.toml");
        std::fs::write(&target, "mode='chat'").expect("write");
        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(read_config(&link).is_err());
        let fifo = dir.path().join("fifo.toml");
        nix::unistd::mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).expect("fifo");
        assert!(matches!(
            read_config(&fifo),
            Err(LaunchError::InvalidConfigPath)
        ));
    }
    #[test]
    fn legacy_process_serial_numbers_are_narrowly_recognized() {
        assert!(valid_process_serial_number("-psn_0_12345"));
        for value in ["-psn_", "-psn_0_", "-psn_0_/tmp", "-psn_0_1_2", "--psn_0_1"] {
            assert!(!valid_process_serial_number(value));
        }
    }
}

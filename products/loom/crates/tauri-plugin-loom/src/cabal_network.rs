//! Connection preferences are local to the device and apply at the next
//! profile start. Saving never interrupts a workspace or starts networking.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use fs2::FileExt;
use loom_cabal::NetworkMode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;

use super::{IpcFailure, PluginState, directory, failure};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    schema: u32,
    connection: NetworkMode,
}

#[derive(Debug, Serialize)]
pub(crate) struct NetworkSettings {
    configured: NetworkMode,
    active: Option<NetworkMode>,
    revision: String,
}

pub(super) fn read(directory: &Path) -> Result<NetworkMode, IpcFailure> {
    if fs::symlink_metadata(directory)
        .is_ok_and(|entry| !entry.is_dir() || entry.file_type().is_symlink())
    {
        return Err(failure("Cabal profile is not an ordinary directory"));
    }
    let path = directory.join("network.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NetworkMode::default());
        }
        Err(error) => return Err(failure(error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return Err(failure(
            "Cabal connection settings are invalid; the file was preserved",
        ));
    }
    let mut bytes = Vec::new();
    File::open(&path)
        .map_err(failure)?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.len() > 4096 {
        return Err(failure("Cabal connection settings exceed their limit"));
    }
    let settings: Settings = serde_json::from_slice(&bytes).map_err(failure)?;
    if settings.schema != 1 {
        return Err(failure(
            "Unsupported cabal connection settings; the file was preserved",
        ));
    }
    settings.connection.validate().map_err(failure)?;
    Ok(settings.connection)
}

fn revision(mode: &NetworkMode) -> Result<String, IpcFailure> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(mode).map_err(failure)?)
    ))
}

pub(super) fn lease(directory: &Path) -> Result<File, IpcFailure> {
    if fs::symlink_metadata(directory)
        .is_ok_and(|entry| !entry.is_dir() || entry.file_type().is_symlink())
    {
        return Err(failure("Cabal profile is not an ordinary directory"));
    }
    fs::create_dir_all(directory).map_err(failure)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(failure)?;
    }
    let path = directory.join("profile.lock");
    if fs::symlink_metadata(&path)
        .is_ok_and(|entry| !entry.is_file() || entry.file_type().is_symlink())
    {
        return Err(failure("Cabal profile lease is not an ordinary file"));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(failure)?;
    file.try_lock_exclusive()
        .map_err(|_| failure("Another Loom process owns this cabal profile"))?;
    Ok(file)
}

fn write(directory: &Path, expected: &str, mode: &NetworkMode) -> Result<(), IpcFailure> {
    mode.validate().map_err(failure)?;
    let current = read(directory)?;
    if revision(&current)? != expected {
        return Err(failure(
            "Connection settings changed. Reopen them before saving.",
        ));
    }
    if &current == mode {
        return Ok(());
    }
    let mut file = atomic_write_file::AtomicWriteFile::open(directory.join("network.json"))
        .map_err(failure)?;
    file.write_all(
        &serde_json::to_vec(&Settings {
            schema: 1,
            connection: mode.clone(),
        })
        .map_err(failure)?,
    )
    .map_err(failure)?;
    file.commit().map_err(failure)?;
    #[cfg(unix)]
    File::open(directory)
        .map_err(failure)?
        .sync_all()
        .map_err(failure)?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn cabal_network_get(
    state: State<'_, PluginState>,
) -> Result<NetworkSettings, IpcFailure> {
    let slot = state.cabals.profile.lock().await;
    let configured = read(&directory(&state)?)?;
    Ok(NetworkSettings {
        revision: revision(&configured)?,
        configured,
        active: slot.as_ref().map(|profile| profile.network.mode().clone()),
    })
}

#[tauri::command]
pub(crate) async fn cabal_network_set(
    expected: String,
    connection: NetworkMode,
    state: State<'_, PluginState>,
) -> Result<NetworkSettings, IpcFailure> {
    crate::ensure_application_running(&state, "connection settings")?;
    connection.validate().map_err(failure)?;
    let slot = state.cabals.profile.lock().await;
    if state
        .cabals
        .closed
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(failure("Cabals are closing"));
    }
    let directory = directory(&state)?;
    let _lease = if slot.is_none() {
        Some(lease(&directory)?)
    } else {
        None
    };
    write(&directory, &expected, &connection)?;
    let configured = read(&directory)?;
    Ok(NetworkSettings {
        revision: revision(&configured)?,
        configured,
        active: slot.as_ref().map(|profile| profile.network.mode().clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_defaults_does_not_create_identity_or_open_networking_and_settings_survive_reopen() {
        let root = tempfile::tempdir().expect("temp");
        let path = root.path().join("cabals");
        assert_eq!(read(&path).expect("default"), NetworkMode::Internet {});
        assert!(!path.exists());
        let lease = lease(&path).expect("lease");
        let initial = revision(&NetworkMode::Internet {}).expect("revision");
        write(&path, &initial, &NetworkMode::Direct {}).expect("save");
        assert!(!path.join("identity.key").exists());
        assert!(write(&path, &initial, &NetworkMode::Internet {}).is_err());
        drop(lease);
        let _lease = self::lease(&path).expect("reopen lease");
        assert_eq!(read(&path).expect("reopen"), NetworkMode::Direct {});
    }

    #[test]
    fn invalid_settings_are_preserved_without_default_fallback() {
        let root = tempfile::tempdir().expect("temp");
        let path = root.path().join("network.json");
        for bytes in [
            br#"{"schema":2,"connection":{"mode":"internet"}}"#.as_slice(),
            b"corrupt",
            br#"{"schema":1,"connection":{"mode":"relays","urls":[]}}"#,
        ] {
            fs::write(&path, bytes).expect("fixture");
            assert!(read(root.path()).is_err());
            assert_eq!(fs::read(&path).expect("preserved"), bytes);
        }
    }

    #[tokio::test]
    async fn profile_start_honors_saved_policy_and_rejects_corruption_before_identity_creation() {
        let root = tempfile::tempdir().expect("temp");
        let directory = root.path().join("cabals");
        let lease = lease(&directory).expect("lease");
        let path = directory.join("network.json");
        fs::write(&path, b"corrupt").expect("fixture");
        drop(lease);
        let service = super::super::CabalService::default();
        assert!(service.start(&directory).await.is_err());
        assert!(service.profile.lock().await.is_none());
        assert!(!directory.join("identity.key").exists());
        assert_eq!(fs::read(&path).expect("preserved"), b"corrupt");
        fs::remove_file(&path).expect("remove fixture");
        let lease = self::lease(&directory).expect("lease released after failure");
        write(
            &directory,
            &revision(&NetworkMode::Internet {}).expect("revision"),
            &NetworkMode::Direct {},
        )
        .expect("save");
        drop(lease);
        service
            .start(&directory)
            .await
            .expect("start direct profile");
        {
            let slot = service.profile.lock().await;
            let profile = slot.as_ref().expect("started profile");
            assert_eq!(profile.network.mode(), &NetworkMode::Direct {});
            assert_eq!(profile.network.address().relay_urls().count(), 0);
            write(
                &directory,
                &revision(&NetworkMode::Direct {}).expect("revision"),
                &NetworkMode::Internet {},
            )
            .expect("save next-start policy");
            assert_eq!(profile.network.mode(), &NetworkMode::Direct {});
        }
        service.shutdown().await;
    }

    #[test]
    #[cfg(unix)]
    fn a_second_profile_owner_and_symbolic_settings_are_rejected() {
        let root = tempfile::tempdir().expect("temp");
        let profile = root.path().join("cabals");
        let _lease = lease(&profile).expect("first lease");
        assert!(lease(&profile).is_err());
        let private = root.path().join("private");
        fs::write(&private, b"private").expect("fixture");
        std::os::unix::fs::symlink(&private, profile.join("network.json")).expect("symlink");
        assert!(read(&profile).is_err());
        assert_eq!(fs::read(private).expect("preserved"), b"private");
    }
}

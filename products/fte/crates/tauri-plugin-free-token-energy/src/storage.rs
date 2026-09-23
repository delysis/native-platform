//! Storage preparation used by the plugin's real setup callback. Disk-free
//! injection stays portable; default private storage is checked before I/O.
use fte_loopback::LoopbackConfig;
use fte_store::{ResponseStore, SqliteStore};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

type SetupResult<T> = Result<T, Box<dyn std::error::Error>>;

pub(super) struct PreparedStorage {
    pub(super) store: Arc<dyn ResponseStore>,
    pub(super) loopback: Option<LoopbackConfig>,
}

pub(super) fn prepare(
    store: Option<Arc<dyn ResponseStore>>,
    loopback: Option<LoopbackConfig>,
    default_loopback: bool,
    resolve_root: impl FnOnce() -> SetupResult<PathBuf>,
) -> SetupResult<PreparedStorage> {
    prepare_with(
        store,
        loopback,
        default_loopback,
        cfg!(unix),
        resolve_root,
        |path| Ok(Arc::new(SqliteStore::open(path)?) as Arc<dyn ResponseStore>),
    )
}

fn prepare_with(
    store: Option<Arc<dyn ResponseStore>>,
    loopback: Option<LoopbackConfig>,
    default_loopback: bool,
    private_storage_supported: bool,
    resolve_root: impl FnOnce() -> SetupResult<PathBuf>,
    open_store: impl FnOnce(&Path) -> SetupResult<Arc<dyn ResponseStore>>,
) -> SetupResult<PreparedStorage> {
    let needs_default_loopback = default_loopback && loopback.is_none();
    // An explicitly supplied loopback owns its own path and validates it when
    // explicitly started. It does not authorize touching the default root.
    if let Some(store) = store.as_ref().filter(|_| !needs_default_loopback) {
        return Ok(PreparedStorage {
            store: Arc::clone(store),
            loopback,
        });
    }
    if !private_storage_supported {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private FTE plugin storage is unsupported on this platform",
        )
        .into());
    }
    let root = resolve_root()?;
    secure_private_root(&root)?;
    let store = match store {
        Some(store) => store,
        None => open_store(&root.join("gateway-v2.db"))?,
    };
    let loopback = loopback.or_else(|| {
        needs_default_loopback.then(|| LoopbackConfig::app_private(root.join("loopback-token")))
    });
    Ok(PreparedStorage { store, loopback })
}

#[cfg(unix)]
fn secure_private_root(root: &Path) -> io::Result<()> {
    use std::fs::{self, DirBuilder, File, Permissions};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    match fs::symlink_metadata(root) {
        Ok(metadata) if !metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "FTE private storage root must be a directory, not a file or symlink",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Restrictive mode applies at creation, rather than after SQLite
            // has already had an opportunity to create files there.
            DirBuilder::new().recursive(true).mode(0o700).create(root)?;
        }
        Err(error) => return Err(error),
    }
    // Check again after create_dir_all's concurrent-existing-directory path.
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "FTE private storage root is no longer a directory",
        ));
    }
    let directory = File::open(root)?;
    directory.set_permissions(Permissions::from_mode(0o700))?;
    directory.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_private_root(_root: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private FTE plugin storage is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::cell::Cell;
    use std::fs;

    struct TestRoot(PathBuf);
    impl TestRoot {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("fte-storage-{}", fte_types::RequestId::new())))
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn injected_store_without_default_loopback_never_resolves_disk() -> SetupResult<()> {
        for supported in [false, true] {
            let store: Arc<dyn ResponseStore> = Arc::new(SqliteStore::in_memory()?);
            let prepared = prepare_with(
                Some(Arc::clone(&store)),
                None,
                false,
                supported,
                || panic!("disk-free setup must not resolve a root"),
                |_| panic!("injected store must not open a database"),
            )?;
            assert!(Arc::ptr_eq(&store, &prepared.store));
            assert!(prepared.loopback.is_none());
        }
        Ok(())
    }

    #[test]
    fn explicit_loopback_with_injected_store_does_not_resolve_default_root() -> SetupResult<()> {
        let explicit = LoopbackConfig::app_private(PathBuf::from("explicit-token"));
        let prepared = prepare_with(
            Some(Arc::new(SqliteStore::in_memory()?)),
            Some(explicit),
            true,
            false,
            || panic!("explicit loopback owns its own token path"),
            |_| panic!("injected store must not open a database"),
        )?;
        assert_eq!(
            prepared
                .loopback
                .expect("explicit configuration")
                .token_path,
            PathBuf::from("explicit-token")
        );
        Ok(())
    }

    #[test]
    fn unsupported_default_storage_rejects_before_any_root_resolution() -> SetupResult<()> {
        for injected in [false, true] {
            let store = if injected {
                Some(Arc::new(SqliteStore::in_memory()?) as Arc<dyn ResponseStore>)
            } else {
                None
            };
            let error = prepare_with(
                store,
                None,
                true,
                false,
                || panic!("unsupported storage must not resolve or touch a root"),
                |_| panic!("unsupported storage must not open a database"),
            )
            .err()
            .expect("unsupported private storage");
            assert_eq!(
                error
                    .downcast_ref::<io::Error>()
                    .expect("platform error")
                    .kind(),
                io::ErrorKind::Unsupported
            );
        }
        Ok(())
    }

    #[cfg(not(unix))]
    #[test]
    fn native_unsupported_setup_preserves_existing_bytes() -> SetupResult<()> {
        let root = TestRoot::new();
        fs::create_dir_all(&root.0)?;
        let database = root.0.join("gateway-v2.db");
        fs::write(&database, b"existing private state")?;
        let result = prepare(None, None, false, || Ok(root.0.clone()));
        let error = result.err().expect("unsupported");
        assert_eq!(
            error
                .downcast_ref::<io::Error>()
                .expect("platform error")
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(fs::read(database)?, b"existing private state");
        assert_eq!(fs::read_dir(&root.0)?.count(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn root_is_private_before_first_database_open_and_reopens() -> SetupResult<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = TestRoot::new();
        fs::create_dir_all(&root.0)?;
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o755))?;
        let calls = Cell::new(0);
        let prepared = prepare_with(
            None,
            None,
            true,
            true,
            || Ok(root.0.clone()),
            |path| {
                calls.set(calls.get() + 1);
                let permissions =
                    fs::metadata(path.parent().expect("database parent"))?.permissions();
                assert_eq!(permissions.mode() & 0o777, 0o700);
                Ok(Arc::new(SqliteStore::open(path)?) as Arc<dyn ResponseStore>)
            },
        )?;
        assert_eq!(calls.get(), 1);
        assert_eq!(
            prepared.loopback.expect("default config").token_path,
            root.0.join("loopback-token")
        );
        assert!(
            !root.0.join("loopback-token").exists(),
            "setup must not start loopback"
        );
        drop(prepared.store);
        let reopened = prepare(None, None, false, || Ok(root.0.clone()))?;
        drop(reopened);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fresh_root_and_injected_default_loopback_need_no_database() -> SetupResult<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = TestRoot::new();
        let prepared = prepare(
            Some(Arc::new(SqliteStore::in_memory()?)),
            None,
            true,
            || Ok(root.0.clone()),
        )?;
        assert_eq!(fs::metadata(&root.0)?.permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::read_dir(&root.0)?.count(), 0);
        assert_eq!(
            prepared.loopback.expect("default config").token_path,
            root.0.join("loopback-token")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn invalid_root_or_symlink_never_reaches_sqlite() -> SetupResult<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = TestRoot::new();
        fs::create_dir_all(&root.0)?;
        let file = root.0.join("file");
        fs::write(&file, b"not a directory")?;
        let outside = root.0.join("outside");
        fs::create_dir(&outside)?;
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755))?;
        let link = root.0.join("link");
        symlink(&outside, &link)?;
        for invalid in [file.clone(), link] {
            let result = prepare_with(
                None,
                None,
                false,
                true,
                || Ok(invalid),
                |_| panic!("root validation must precede database access"),
            );
            let error = result.err().expect("invalid root");
            assert_eq!(
                error
                    .downcast_ref::<io::Error>()
                    .expect("path error")
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(fs::read(file)?, b"not a directory");
        assert_eq!(fs::metadata(&outside)?.permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::read_dir(outside)?.count(), 0);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn resolution_and_database_errors_are_not_reported_as_success() -> SetupResult<()> {
        let result = prepare_with(
            None,
            None,
            false,
            true,
            || Err(io::Error::new(io::ErrorKind::NotFound, "root unavailable").into()),
            |_| panic!("unresolved root must not open"),
        );
        assert_eq!(
            result.err().expect("resolution failure").to_string(),
            "root unavailable"
        );
        let root = TestRoot::new();
        let result = prepare_with(
            None,
            None,
            false,
            true,
            || Ok(root.0.clone()),
            |_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "open denied").into()),
        );
        assert_eq!(
            result.err().expect("open failure").to_string(),
            "open denied"
        );
        assert_eq!(fs::read_dir(&root.0)?.count(), 0);
        Ok(())
    }
}

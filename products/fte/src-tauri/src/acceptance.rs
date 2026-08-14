use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const ACCEPTANCE_DIR_ENV: &str = "DELYSIS_FTE_ACCEPTANCE_DIR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptanceIsolation {
    root: PathBuf,
}

impl AcceptanceIsolation {
    pub fn from_environment() -> Result<Option<Self>> {
        Self::from_value(std::env::var_os(ACCEPTANCE_DIR_ENV))
    }

    fn from_value(value: Option<OsString>) -> Result<Option<Self>> {
        let Some(value) = value else {
            return Ok(None);
        };
        let configured = PathBuf::from(value);
        if !configured.is_absolute() {
            bail!("{ACCEPTANCE_DIR_ENV} must name an absolute directory");
        }
        let metadata = std::fs::symlink_metadata(&configured).with_context(|| {
            format!(
                "{ACCEPTANCE_DIR_ENV} does not name an existing directory: {}",
                configured.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            bail!("{ACCEPTANCE_DIR_ENV} must not name a symbolic link");
        }
        if !metadata.is_dir() {
            bail!("{ACCEPTANCE_DIR_ENV} must name a directory");
        }
        let root = configured.canonicalize().with_context(|| {
            format!(
                "{ACCEPTANCE_DIR_ENV} could not be canonicalized: {}",
                configured.display()
            )
        })?;
        if root.parent().is_none() {
            bail!("{ACCEPTANCE_DIR_ENV} must not name the filesystem root");
        }
        if std::env::var_os("HOME")
            .and_then(|home| PathBuf::from(home).canonicalize().ok())
            .as_deref()
            == Some(root.as_path())
        {
            bail!("{ACCEPTANCE_DIR_ENV} must not name the user's home directory");
        }
        Ok(Some(Self { root }))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn desktop_database(&self) -> PathBuf {
        self.root.join("gateway.db")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn test_directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fte-acceptance-{name}-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create acceptance test directory");
        path
    }

    #[test]
    fn absent_acceptance_environment_preserves_normal_mode() {
        assert_eq!(AcceptanceIsolation::from_value(None).unwrap(), None);
    }

    #[test]
    fn existing_absolute_directory_owns_the_acceptance_database() {
        let root = test_directory("valid");
        let isolation = AcceptanceIsolation::from_value(Some(root.clone().into_os_string()))
            .expect("valid acceptance directory")
            .expect("acceptance mode");
        assert_eq!(isolation.root(), root.canonicalize().unwrap());
        assert_eq!(
            isolation.desktop_database(),
            root.canonicalize().unwrap().join("gateway.db")
        );
        std::fs::remove_dir(root).expect("remove acceptance test directory");
    }

    #[test]
    fn relative_and_missing_acceptance_directories_fail_closed() {
        let relative = AcceptanceIsolation::from_value(Some(OsString::from("relative")))
            .unwrap_err()
            .to_string();
        assert!(relative.contains("absolute"));

        let missing = std::env::temp_dir().join(format!(
            "fte-acceptance-missing-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let error = AcceptanceIsolation::from_value(Some(missing.into_os_string()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("existing directory"));
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_acceptance_directory_fails_closed() {
        use std::os::unix::fs::symlink;

        let root = test_directory("symlink-target");
        let link = root.with_extension("link");
        symlink(&root, &link).expect("create acceptance symlink");
        let error = AcceptanceIsolation::from_value(Some(link.clone().into_os_string()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("symbolic link"));
        std::fs::remove_file(link).expect("remove acceptance symlink");
        std::fs::remove_dir(root).expect("remove acceptance target");
    }
}

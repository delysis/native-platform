//! Explicit plaintext export to a user-selected new file. No staging plaintext,
//! overwrites, launch, or access to a project's private payload directory.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Component, Path};

pub(super) fn suggested_file_name(name: &str) -> String {
    let mut suggestion = String::new();
    for character in name.chars() {
        let character = if character.is_control() || matches!(character, '/' | '\\' | ':') {
            '_'
        } else {
            character
        };
        if suggestion.len() + character.len_utf8() > 240 {
            break;
        }
        suggestion.push(character);
    }
    match suggestion.trim() {
        "" | "." | ".." => "attachment".into(),
        name => name.to_owned(),
    }
}

fn open_directory(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let directory = {
        use rustix::fs::{Mode, OFlags, open};
        File::from(open(
            path,
            OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::empty(),
        )?)
    };
    #[cfg(not(unix))]
    let directory = {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            };
            options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
        }
        options.open(path)?
    };
    if !directory.metadata()?.is_dir() {
        return Err(invalid("The export destination is not a folder."));
    }
    Ok(directory)
}

pub(super) fn write_new(destination: &Path, bytes: &[u8]) -> io::Result<()> {
    if !destination.is_absolute()
        || destination
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(invalid(
            "Choose an absolute destination without parent traversal.",
        ));
    }
    let name = destination
        .file_name()
        .ok_or_else(|| invalid("Choose a file name."))?;
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("Choose a destination folder."))?
        .canonicalize()?;
    if parent.components().any(|part| {
        matches!(part, Component::Normal(name) if name.to_string_lossy().eq_ignore_ascii_case(".loom"))
    }) {
        return Err(invalid("Originals cannot be exported inside a private .loom folder."));
    }
    let directory = open_directory(&parent)?;
    let identity = same_file::Handle::from_file(directory.try_clone()?)?;
    #[cfg(unix)]
    let mut file = {
        use rustix::fs::{Mode, OFlags, openat};
        File::from(openat(
            &directory,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?)
    };
    #[cfg(not(unix))]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(parent.join(name))?;
    #[cfg(all(test, unix))]
    if let Some(interrupt) = tests::AFTER_CREATE.with(|hook| hook.borrow_mut().take()) {
        interrupt();
    }
    // create_new also refuses an existing final symlink. A user selection never
    // grants permission to replace a file that appeared while the dialog ran.
    if same_file::Handle::from_file(open_directory(&parent)?)? != identity
        || fs::symlink_metadata(&parent)?.file_type().is_symlink()
    {
        return Err(invalid("The export folder changed before writing."));
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    directory.sync_all()?;
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    thread_local! {
        pub(super) static AFTER_CREATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    #[test]
    fn original_export_preserves_exact_bytes_and_never_clobbers_a_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("original.bin");
        let bytes = b"\0original bytes\r\n\xff";
        write_new(&destination, bytes).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert_eq!(
            write_new(&destination, b"replacement").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn original_export_rejects_private_folders_and_traversal() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join(".loom/attachments");
        fs::create_dir_all(&private).unwrap();
        assert!(write_new(&private.join("original.bin"), b"private").is_err());
        assert!(write_new(&private.join("../../escape.bin"), b"private").is_err());
        assert!(write_new(Path::new("relative.bin"), b"private").is_err());
        assert_eq!(fs::read_dir(private).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn original_export_rejects_final_symlinks_and_private_folder_aliases() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.bin");
        fs::write(&source, b"keep").unwrap();
        let destination = root.path().join("export.bin");
        std::os::unix::fs::symlink(&source, &destination).unwrap();
        assert!(write_new(&destination, b"overwrite").is_err());
        assert_eq!(fs::read(source).unwrap(), b"keep");
        let private = root.path().join(".loom");
        fs::create_dir(&private).unwrap();
        let alias = root.path().join("public-looking");
        std::os::unix::fs::symlink(&private, &alias).unwrap();
        assert!(write_new(&alias.join("plaintext.bin"), b"private").is_err());
        assert_eq!(fs::read_dir(private).unwrap().count(), 0);
    }

    #[test]
    fn original_export_filename_is_only_a_suggestion() {
        assert_eq!(suggested_file_name("notes α.txt"), "notes α.txt");
        assert_eq!(
            suggested_file_name("../private\\name:\n.txt"),
            ".._private_name__.txt"
        );
        assert_eq!(suggested_file_name(".."), "attachment");
        assert_eq!(suggested_file_name(&"尾".repeat(1_000)), "尾".repeat(80));
    }

    #[cfg(unix)]
    #[test]
    fn export_folder_replacement_refuses_without_a_fifo_peer_and_retains_partial_effect() {
        use rustix::fs::{Mode, OFlags, open};
        use std::sync::mpsc;
        use std::time::Duration;

        let project = tempfile::tempdir().expect("export fixture");
        let parent = project.path().join("chosen");
        let retained = project.path().join("retained");
        fs::create_dir(&parent).expect("chosen directory");
        write_new(&parent.join("ordinary.txt"), b"exact original\r\n").expect("ordinary export");
        let (swapped, ready) = mpsc::channel();
        let (finished, result) = mpsc::channel();
        let worker_parent = parent.clone();
        let worker_retained = retained.clone();
        let worker = std::thread::spawn(move || {
            let replaced = worker_parent.clone();
            AFTER_CREATE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    fs::rename(&replaced, worker_retained).expect("retain chosen directory");
                    nix::unistd::mkfifo(
                        &replaced,
                        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
                    )
                    .expect("owned FIFO");
                    swapped.send(()).expect("replacement observer");
                }));
            });
            finished
                .send(write_new(
                    &worker_parent.join("new.txt"),
                    b"must not be published",
                ))
                .expect("export observer");
        });
        ready
            .recv_timeout(Duration::from_secs(5))
            .expect("after-create boundary");
        let early = result.recv_timeout(Duration::from_secs(5));
        let needed_peer = matches!(early, Err(mpsc::RecvTimeoutError::Timeout));
        let rescue = needed_peer.then(|| {
            open(
                &parent,
                OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .expect("release the controlled old reopen")
        });
        let outcome = early.unwrap_or_else(|_| {
            result
                .recv_timeout(Duration::from_secs(5))
                .expect("rescued export result")
        });
        worker.join().expect("physical export worker join");
        drop(rescue);
        assert!(outcome.is_err());
        assert_eq!(
            fs::read(retained.join("ordinary.txt")).unwrap(),
            b"exact original\r\n"
        );
        assert_eq!(
            fs::read(retained.join("new.txt")).unwrap(),
            b"",
            "the owned create-new effect is retained, not silently rolled back or written"
        );
        assert!(
            !needed_peer,
            "folder identity refusal waited for a FIFO peer"
        );
    }
}

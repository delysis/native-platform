use archive_friends::FriendsConfig;
#[cfg(unix)]
#[test]
fn a_nonregular_dotfile_cannot_block_native_preparation() {
    use std::{fs::OpenOptions, io::Write, process::Command, sync::mpsc, thread, time::Duration};
    let temp = tempfile::tempdir().expect("controlled test fixture");
    let path = temp.path().join(".community-archive.toml");
    assert!(
        Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("controlled test fixture")
            .success()
    );
    let (ready, started) = mpsc::channel();
    let (send, receive) = mpsc::channel();
    let reader = path.clone();
    let worker = thread::spawn(move || {
        ready.send(()).expect("controlled test fixture");
        send.send(FriendsConfig::load(&reader))
            .expect("controlled test fixture");
    });
    started
        .recv_timeout(Duration::from_secs(2))
        .expect("controlled test fixture");
    let observation = receive.recv_timeout(Duration::from_millis(500));
    // Unblock the old reader before failing the negative control; leave no
    // blocked thread or fixture behind merely to prove the defect.
    if observation.is_err() {
        let mut writer = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("controlled test fixture");
        writer
            .write_all(b"version = 1\n")
            .expect("controlled test fixture");
    }
    worker.join().expect("controlled test fixture");
    assert!(
        observation.is_ok(),
        "dotfile open blocked beyond the observation bound"
    );
    assert!(
        observation.expect("controlled test fixture").is_err(),
        "a FIFO is not a regular authored dotfile"
    );
}

#[test]
fn regular_configuration_retains_its_relative_archive_authority() {
    let temp = tempfile::tempdir().expect("controlled test fixture");
    let path = temp.path().join(".community-archive.toml");
    std::fs::write(
        &path,
        "archive = 'snapshot.sqlite'\n[friends.a]\nhandle = 'a'\n",
    )
    .expect("controlled test fixture");
    let config = FriendsConfig::load(&path).expect("controlled test fixture");
    assert_eq!(
        config.archive,
        temp.path()
            .canonicalize()
            .expect("controlled test fixture")
            .join("snapshot.sqlite")
    );
    assert_eq!(config.friends["a"].handle, "a");
}
#[cfg(unix)]
#[test]
fn aliases_cannot_redirect_the_authored_dotfile_snapshot() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().expect("controlled test fixture");
    let original = temp.path().join("original.toml");
    std::fs::write(
        &original,
        "archive = 'snapshot.sqlite'\n[friends.a]\nhandle = 'a'\n",
    )
    .expect("controlled test fixture");
    let symbolic = temp.path().join("symbolic.toml");
    symlink(&original, &symbolic).expect("controlled test fixture");
    assert!(FriendsConfig::load(&symbolic).is_err());
    let hard = temp.path().join("hard.toml");
    std::fs::hard_link(&original, &hard).expect("controlled test fixture");
    assert!(FriendsConfig::load(&hard).is_err());
    assert!(FriendsConfig::load(&original).is_err());
}
#[test]
fn oversized_dotfiles_fail_before_parsing_or_archive_access() {
    let temp = tempfile::tempdir().expect("controlled test fixture");
    let path = temp.path().join(".community-archive.toml");
    std::fs::write(&path, vec![b'x'; 65_537]).expect("controlled test fixture");
    assert!(FriendsConfig::load(&path).is_err());
}

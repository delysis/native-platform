use super::*;
use information_native_types::ArtifactId;
use std::error::Error;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use tempfile::tempdir;

#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type TestServer = JoinHandle<io::Result<Vec<String>>>;

fn unpublished_siblings(directory: &Path) -> io::Result<Vec<PathBuf>> {
    fs::read_dir(directory)?
        .filter_map(|entry| match entry {
            Ok(entry)
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".information-native-unpublished-") =>
            {
                Some(Ok(entry.path()))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

#[test]
fn restrictive_policy_denies_non_public_destinations() -> TestResult {
    for address in [
        "http://0.0.0.0/resource",
        "http://127.0.0.1/resource",
        "http://10.1.2.3/resource",
        "http://169.254.169.254/latest/meta-data",
        "http://192.168.1.1/resource",
        "http://[::]/resource",
        "http://[::1]/resource",
        "http://[fc00::1]/resource",
        "http://[fe80::1]/resource",
    ] {
        let url = Url::parse(address)?;
        assert!(matches!(
            resolve_destination_for_test(&url, &AcquisitionPolicy::restricted()),
            Err(AcquireError::NetworkDestinationForbidden { .. })
        ));
    }
    Ok(())
}

#[test]
fn public_address_classifier_is_conservative() -> TestResult {
    for address in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
        assert!(is_publicly_routable(address.parse()?));
    }
    for address in [
        "100.64.0.1",
        "192.0.2.1",
        "198.18.0.1",
        "203.0.113.1",
        "224.0.0.1",
        "2001:db8::1",
        "2002:0a00:0001::1",
        "3fff::1",
    ] {
        assert!(!is_publicly_routable(address.parse()?));
    }
    Ok(())
}

#[test]
fn dns_resolution_obeys_an_already_expired_transfer_deadline() -> TestResult {
    let url = Url::parse("https://example.com/resource")?;
    let timeout = Duration::from_secs(1);
    let deadline = Instant::now() - timeout;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(resolve_destination(
        &url,
        &AcquisitionPolicy::restricted(),
        deadline,
        timeout,
    ));
    assert!(matches!(
        result,
        Err(AcquireError::TotalTransferTimeout { .. })
    ));
    Ok(())
}

#[test]
fn file_uri_requires_and_honors_a_canonical_root_grant() -> TestResult {
    let allowed = tempdir()?;
    let outside = tempdir()?;
    let source = allowed.path().join("source.bin");
    let outside_source = outside.path().join("outside.bin");
    fs::write(&source, b"offline corpus")?;
    fs::write(&outside_source, b"offline corpus")?;
    let source_uri = file_uri(&source)?;
    let outside_uri = file_uri(&outside_source)?;
    let digest = digest(b"offline corpus");
    let client = AcquireClient::with_defaults()?;

    let denied_staging = allowed.path().join("denied.bin");
    assert!(matches!(
        client.fetch_artifact(&source_uri, &denied_staging, 14, &digest, 1024),
        Err(AcquireError::FileUriForbidden)
    ));
    assert!(!denied_staging.exists());

    let policy =
        AcquisitionPolicy::restricted().with_file_root(CanonicalFileRoot::new(allowed.path())?);
    let options = ArtifactFetchOptions {
        acquisition_policy: policy,
        resume: ResumePolicy::Disabled,
    };
    let mut progress = |_progress| ProgressControl::Continue;
    let staging = allowed.path().join("staging.bin");
    let verified = client.fetch_artifact_with_options(
        &source_uri,
        &staging,
        14,
        &digest,
        1024,
        &options,
        &mut progress,
    )?;
    assert_eq!(fs::read(&staging)?, b"offline corpus");
    assert_eq!(
        verified.final_source_uri.as_deref(),
        Some(source_uri.as_str())
    );

    let outside_staging = allowed.path().join("outside-staging.bin");
    assert!(matches!(
        client.fetch_artifact_with_options(
            &outside_uri,
            &outside_staging,
            14,
            &digest,
            1024,
            &options,
            &mut progress,
        ),
        Err(AcquireError::FileOutsideGrantedRoots(_))
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn canonical_file_grant_rejects_a_symlink_escape() -> TestResult {
    let allowed = tempdir()?;
    let outside = tempdir()?;
    let outside_source = outside.path().join("outside.bin");
    fs::write(&outside_source, b"secret")?;
    let link = allowed.path().join("link.bin");
    symlink(&outside_source, &link)?;
    let options = ArtifactFetchOptions {
        acquisition_policy: AcquisitionPolicy::restricted()
            .with_file_root(CanonicalFileRoot::new(allowed.path())?),
        resume: ResumePolicy::Disabled,
    };
    let mut progress = |_progress| ProgressControl::Continue;
    let result = AcquireClient::with_defaults()?.fetch_artifact_with_options(
        &file_uri(&link)?,
        &allowed.path().join("staging.bin"),
        6,
        &digest(b"secret"),
        1024,
        &options,
        &mut progress,
    );
    assert!(matches!(
        result,
        Err(AcquireError::FileOutsideGrantedRoots(_))
    ));
    Ok(())
}

#[test]
fn explicit_file_fetch_verifies_and_never_overwrites() -> TestResult {
    let directory = tempdir()?;
    let source = directory.path().join("source.bin");
    fs::write(&source, b"payload")?;
    let client = AcquireClient::with_defaults()?;

    let staging = directory.path().join("staging.bin");
    let verified = client.fetch_file_artifact(&source, &staging, 7, &digest(b"payload"), 1024)?;
    assert_eq!(verified.bytes, 7);
    assert_eq!(verified.resumed_bytes, 0);
    assert!(verified.finished_at_unix_ms >= verified.started_at_unix_ms);
    assert_eq!(fs::read(&staging)?, b"payload");

    let occupied = directory.path().join("occupied.bin");
    fs::write(&occupied, b"caller-owned")?;
    assert!(matches!(
        client.fetch_file_artifact(&source, &occupied, 7, &digest(b"payload"), 1024),
        Err(AcquireError::StagingPathExists)
    ));
    assert_eq!(fs::read(occupied)?, b"caller-owned");
    Ok(())
}

#[test]
fn digest_mismatch_leaves_destination_absent_and_removes_private_temp() -> TestResult {
    let directory = tempdir()?;
    let source = directory.path().join("source.bin");
    let destination = directory.path().join("destination.bin");
    fs::write(&source, b"payload")?;

    let result = AcquireClient::with_defaults()?.fetch_file_artifact(
        &source,
        &destination,
        7,
        &digest(b"different"),
        1024,
    );

    assert!(matches!(result, Err(AcquireError::DigestMismatch { .. })));
    assert!(!destination.exists());
    assert!(unpublished_siblings(directory.path())?.is_empty());
    Ok(())
}

#[test]
fn length_mismatch_leaves_destination_absent_and_removes_private_temp() -> TestResult {
    let directory = tempdir()?;
    let destination = directory.path().join("destination.bin");
    let mut source = io::Cursor::new(b"short".to_vec());
    let mut progress = |_progress| ProgressControl::Continue;

    let result = stream_verified_file(
        &mut source,
        &destination,
        6,
        &digest(b"short!"),
        1024,
        None,
        0,
        &mut progress,
    );

    assert!(matches!(result, Err(AcquireError::LengthMismatch { .. })));
    assert!(!destination.exists());
    assert!(unpublished_siblings(directory.path())?.is_empty());
    Ok(())
}

#[test]
fn source_read_error_leaves_destination_absent_and_removes_private_temp() -> TestResult {
    struct FailingSource {
        emitted: bool,
    }

    impl Read for FailingSource {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.emitted {
                return Err(io::Error::other("injected source read failure"));
            }
            self.emitted = true;
            buffer[..4].copy_from_slice(b"part");
            Ok(4)
        }
    }

    let directory = tempdir()?;
    let destination = directory.path().join("destination.bin");
    let mut source = FailingSource { emitted: false };
    let mut progress = |_progress| ProgressControl::Continue;
    let result = stream_verified_file(
        &mut source,
        &destination,
        8,
        &digest(b"partrest"),
        1024,
        None,
        0,
        &mut progress,
    );

    assert!(matches!(result, Err(AcquireError::SourceIo(_))));
    assert!(!destination.exists());
    assert!(unpublished_siblings(directory.path())?.is_empty());
    Ok(())
}

#[test]
fn cancellation_at_start_leaves_destination_absent() -> TestResult {
    let directory = tempdir()?;
    let source = directory.path().join("source.bin");
    let destination = directory.path().join("destination.bin");
    fs::write(&source, b"payload")?;
    let mut progress = |event: TransferProgress| {
        if event.phase == TransferPhase::Starting {
            ProgressControl::Cancel
        } else {
            ProgressControl::Continue
        }
    };

    let result = AcquireClient::with_defaults()?.fetch_file_artifact_with_progress(
        &source,
        &destination,
        7,
        &digest(b"payload"),
        1024,
        &mut progress,
    );

    assert!(matches!(result, Err(AcquireError::Cancelled { .. })));
    assert!(!destination.exists());
    assert!(unpublished_siblings(directory.path())?.is_empty());
    Ok(())
}

#[test]
fn cancellation_after_validation_but_before_publish_leaves_destination_absent() -> TestResult {
    let directory = tempdir()?;
    let source = directory.path().join("source.bin");
    let destination = directory.path().join("destination.bin");
    fs::write(&source, b"payload")?;
    let mut progress = |event: TransferProgress| {
        if event.phase == TransferPhase::Publishing {
            ProgressControl::Cancel
        } else {
            ProgressControl::Continue
        }
    };

    let result = AcquireClient::with_defaults()?.fetch_file_artifact_with_progress(
        &source,
        &destination,
        7,
        &digest(b"payload"),
        1024,
        &mut progress,
    );

    assert!(matches!(result, Err(AcquireError::Cancelled { .. })));
    assert!(!destination.exists());
    assert!(unpublished_siblings(directory.path())?.is_empty());
    Ok(())
}

#[test]
fn destination_created_at_publish_is_never_clobbered() -> TestResult {
    let directory = tempdir()?;
    let source = directory.path().join("source.bin");
    let destination = directory.path().join("destination.bin");
    fs::write(&source, b"payload")?;
    let callback_destination = destination.clone();
    let mut progress = move |event: TransferProgress| {
        if event.phase == TransferPhase::Publishing {
            fs::write(&callback_destination, b"caller-owned")
                .expect("test publishes a competing destination");
        }
        ProgressControl::Continue
    };

    let result = AcquireClient::with_defaults()?.fetch_file_artifact_with_progress(
        &source,
        &destination,
        7,
        &digest(b"payload"),
        1024,
        &mut progress,
    );

    assert!(matches!(result, Err(AcquireError::StagingPathExists)));
    assert_eq!(fs::read(&destination)?, b"caller-owned");
    assert!(unpublished_siblings(directory.path())?.is_empty());
    Ok(())
}

#[test]
fn successful_fresh_publish_leaves_exact_bytes_and_no_private_temp() -> TestResult {
    let directory = tempdir()?;
    let source = directory.path().join("source.bin");
    let destination = directory.path().join("destination.bin");
    fs::write(&source, b"payload")?;

    AcquireClient::with_defaults()?.fetch_file_artifact(
        &source,
        &destination,
        7,
        &digest(b"payload"),
        1024,
    )?;

    assert_eq!(fs::read(&destination)?, b"payload");
    assert!(unpublished_siblings(directory.path())?.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn unpublished_temp_is_private_and_replacement_safe() -> TestResult {
    let directory = tempdir()?;
    let destination = directory.path().join("destination.bin");
    let unpublished = UnpublishedFile::create(&destination)?;
    let temporary_path = unpublished
        .temporary
        .as_ref()
        .ok_or_else(|| io::Error::other("missing unpublished temp"))?
        .path()
        .to_path_buf();
    assert_eq!(temporary_path.parent(), Some(directory.path()));
    assert_eq!(
        fs::metadata(&temporary_path)?.permissions().mode() & 0o077,
        0
    );

    let displaced = directory.path().join("displaced-private-temp.bin");
    fs::rename(&temporary_path, &displaced)?;
    fs::write(&temporary_path, b"caller replacement")?;
    drop(unpublished);

    assert_eq!(fs::read(&temporary_path)?, b"caller replacement");
    assert!(displaced.exists());
    assert!(!destination.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn staging_files_are_private_at_creation() -> TestResult {
    let directory = tempdir()?;
    let source = directory.path().join("source.bin");
    let staging = directory.path().join("staging.bin");
    fs::write(&source, b"private")?;
    AcquireClient::with_defaults()?.fetch_file_artifact(
        &source,
        &staging,
        7,
        &digest(b"private"),
        1024,
    )?;
    assert_eq!(fs::metadata(staging)?.permissions().mode() & 0o077, 0);
    Ok(())
}

#[test]
fn failed_cleanup_does_not_delete_a_replacement_path() -> TestResult {
    let directory = tempdir()?;
    let staging_path = directory.path().join("staging.bin");
    let displaced_path = directory.path().join("displaced.bin");
    let mut staging = StagingFile::create(&staging_path)?;
    staging.file.write_all(b"partial")?;
    fs::rename(&staging_path, &displaced_path)?;
    fs::write(&staging_path, b"replacement")?;
    drop(staging);
    assert_eq!(fs::read(&staging_path)?, b"replacement");
    assert_eq!(fs::read(&displaced_path)?, b"partial");
    Ok(())
}

#[cfg(unix)]
#[test]
fn durable_resume_never_follows_or_chmods_existing_aliases() -> TestResult {
    let directory = private_tempdir()?;
    let victim = directory.path().join("victim.bin");
    fs::write(&victim, b"victim")?;
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o644))?;
    let staging = directory.path().join("staging.bin");
    let sidecar = directory.path().join("staging.resume.json");
    symlink(&victim, &staging)?;

    let result = PreparedTransfer::open(
        "https://example.com/archive",
        &staging,
        7,
        &digest(b"partial"),
        &ResumePolicy::durable(&sidecar),
    );
    assert!(matches!(result, Err(AcquireError::UnsafeStagingFile)));
    assert_eq!(fs::read(&victim)?, b"victim");
    assert_eq!(fs::metadata(&victim)?.permissions().mode() & 0o777, 0o644);

    fs::remove_file(&staging)?;
    fs::hard_link(&victim, &staging)?;
    let result = PreparedTransfer::open(
        "https://example.com/archive",
        &staging,
        7,
        &digest(b"partial"),
        &ResumePolicy::durable(&sidecar),
    );
    assert!(matches!(result, Err(AcquireError::UnsafeStagingFile)));
    assert_eq!(fs::read(&victim)?, b"victim");
    assert_eq!(fs::metadata(&victim)?.permissions().mode() & 0o777, 0o644);
    Ok(())
}

#[test]
fn progress_callback_can_cancel_before_validation() -> TestResult {
    let directory = tempdir()?;
    let source = directory.path().join("source.bin");
    let staging = directory.path().join("staging.bin");
    fs::write(&source, b"payload")?;
    let mut callback = |progress: TransferProgress| {
        if progress.phase == TransferPhase::Downloading {
            ProgressControl::Cancel
        } else {
            ProgressControl::Continue
        }
    };
    let result = AcquireClient::with_defaults()?.fetch_file_artifact_with_progress(
        &source,
        &staging,
        7,
        &digest(b"payload"),
        1024,
        &mut callback,
    );
    assert!(matches!(result, Err(AcquireError::Cancelled { .. })));
    assert!(!staging.exists());
    Ok(())
}

#[test]
fn http_fetch_records_full_redirect_attestation() -> TestResult {
    let body = b"network corpus";
    let (final_url, final_server) = serve_sequence(vec![response(
        "200 OK",
        &[("Content-Length", &body.len().to_string())],
        body,
    )])?;
    let redirect_body = format!(
        "HTTP/1.1 302 Found\r\nLocation: {final_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes();
    let (initial_url, initial_server) = serve_sequence(vec![redirect_body])?;
    let directory = tempdir()?;
    let staging = directory.path().join("staging.bin");
    let options = local_options(ResumePolicy::Disabled);
    let mut progress = |_progress| ProgressControl::Continue;
    let artifact = PlannedArtifact {
        artifact_id: ArtifactId::parse("payload")?,
        file_name: "payload.bin".to_string(),
        source_uri: initial_url.clone(),
        expected_bytes: u64::try_from(body.len())?,
        sha256: digest(body),
    };
    let verified = AcquireClient::with_defaults()?.fetch_planned_artifact_with_options(
        &artifact,
        &staging,
        1024,
        &options,
        &mut progress,
    )?;
    join_server(initial_server)?;
    join_server(final_server)?;

    assert!(verified.network_used);
    assert_eq!(verified.redirects, 1);
    assert_eq!(fs::read(staging)?, body);
    let attestation = verified
        .source_attestation
        .ok_or_else(|| io::Error::other("missing source attestation"))?;
    assert_eq!(attestation.requested_uri, initial_url);
    assert_eq!(attestation.final_uri, final_url);
    assert!(!attestation.final_peer_address.is_empty());
    assert_eq!(attestation.redirect_chain.len(), 1);
    assert_eq!(attestation.redirect_chain[0].status, 302);
    assert_eq!(attestation.redirect_chain[0].from_uri, initial_url);
    assert_eq!(attestation.redirect_chain[0].to_uri, final_url);
    Ok(())
}

#[test]
fn fresh_http_digest_mismatch_never_publishes_destination() -> TestResult {
    let body = b"network corpus";
    let (url, server) = serve_sequence(vec![response(
        "200 OK",
        &[("Content-Length", &body.len().to_string())],
        body,
    )])?;
    let directory = tempdir()?;
    let destination = directory.path().join("destination.bin");
    let options = local_options(ResumePolicy::Disabled);
    let mut progress = |_progress| ProgressControl::Continue;

    let result = AcquireClient::with_defaults()?.fetch_artifact_with_options(
        &url,
        &destination,
        u64::try_from(body.len())?,
        &digest(b"different bytes"),
        1024,
        &options,
        &mut progress,
    );
    join_server(server)?;

    assert!(matches!(result, Err(AcquireError::DigestMismatch { .. })));
    assert!(!destination.exists());
    assert!(unpublished_siblings(directory.path())?.is_empty());
    Ok(())
}

#[test]
fn fresh_http_destination_race_is_no_clobber() -> TestResult {
    let body = b"network corpus";
    let (url, server) = serve_sequence(vec![response(
        "200 OK",
        &[("Content-Length", &body.len().to_string())],
        body,
    )])?;
    let directory = tempdir()?;
    let destination = directory.path().join("destination.bin");
    let callback_destination = destination.clone();
    let options = local_options(ResumePolicy::Disabled);
    let mut progress = move |event: TransferProgress| {
        if event.phase == TransferPhase::Publishing {
            fs::write(&callback_destination, b"caller-owned")
                .expect("test publishes a competing destination");
        }
        ProgressControl::Continue
    };

    let result = AcquireClient::with_defaults()?.fetch_artifact_with_options(
        &url,
        &destination,
        u64::try_from(body.len())?,
        &digest(body),
        1024,
        &options,
        &mut progress,
    );
    join_server(server)?;

    assert!(matches!(result, Err(AcquireError::StagingPathExists)));
    assert_eq!(fs::read(&destination)?, b"caller-owned");
    assert!(unpublished_siblings(directory.path())?.is_empty());
    Ok(())
}

#[test]
fn every_redirect_rechecks_scheme_and_limit() -> TestResult {
    let file_redirect = b"HTTP/1.1 302 Found\r\nLocation: file:///etc/passwd\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let (url, server) = serve_sequence(vec![file_redirect])?;
    let policy = AcquisitionPolicy::restricted().with_network_scope(NetworkScope::AnyAddress);
    let result = AcquireClient::with_defaults()?.fetch_catalogue_with_policy(&url, 1024, &policy);
    join_server(server)?;
    assert!(matches!(
        result,
        Err(AcquireError::RedirectSchemeForbidden(scheme)) if scheme == "file"
    ));

    let (url, server) = serve_sequence(vec![
        b"HTTP/1.1 302 Found\r\nLocation: /again\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec(),
    ])?;
    let config = AcquireConfig {
        max_redirects: 0,
        ..AcquireConfig::default()
    };
    let result = AcquireClient::new(config)?.fetch_catalogue_with_policy(&url, 1024, &policy);
    join_server(server)?;
    assert!(matches!(
        result,
        Err(AcquireError::TooManyRedirects { max_redirects: 0 })
    ));
    Ok(())
}

#[test]
fn artifact_redirects_cannot_persist_query_or_fragment_secrets() -> TestResult {
    let redirect = b"HTTP/1.1 302 Found\r\nLocation: /payload?token=secret#private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let (url, server) = serve_sequence(vec![redirect])?;
    let directory = tempdir()?;
    let staging = directory.path().join("staging.bin");
    let options = local_options(ResumePolicy::Disabled);
    let mut progress = |_progress| ProgressControl::Continue;
    let result = AcquireClient::with_defaults()?.fetch_artifact_with_options(
        &url,
        &staging,
        1,
        &digest(b"x"),
        1024,
        &options,
        &mut progress,
    );
    join_server(server)?;
    assert!(matches!(
        result,
        Err(AcquireError::ArtifactQueryOrFragmentForbidden)
    ));
    assert!(!staging.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn durable_resume_uses_validator_and_exact_range() -> TestResult {
    let body = b"network corpus";
    let first = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nAccept-Ranges: bytes\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n7\r\nnetwork\r\n".to_vec();
    let second = b"HTTP/1.1 206 Partial Content\r\nContent-Length: 7\r\nContent-Range: bytes 7-13/14\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n corpus".to_vec();
    let (url, server) = serve_sequence(vec![first, second])?;
    let directory = private_tempdir()?;
    let staging = directory.path().join("staging.bin");
    let sidecar = directory.path().join("staging.resume.json");
    let options = local_options(ResumePolicy::durable(&sidecar));
    let client = AcquireClient::with_defaults()?;
    let mut cancel_after_first_chunk = |progress: TransferProgress| {
        if progress.phase == TransferPhase::Downloading && progress.downloaded_bytes >= 7 {
            ProgressControl::Cancel
        } else {
            ProgressControl::Continue
        }
    };
    let first_result = client.fetch_artifact_with_options(
        &url,
        &staging,
        14,
        &digest(body),
        1024,
        &options,
        &mut cancel_after_first_chunk,
    );
    assert!(
        matches!(
            first_result,
            Err(AcquireError::Cancelled {
                downloaded_bytes: 7
            })
        ),
        "{first_result:?}"
    );
    assert_eq!(fs::read(&staging)?, b"network");
    assert!(sidecar.exists());
    let persisted: ResumeSidecar = serde_json::from_slice(&fs::read(&sidecar)?)?;
    assert_eq!(persisted.source_attestations.len(), 1);
    assert_eq!(persisted.source_attestations[0].byte_start, 0);
    assert_eq!(persisted.source_attestations[0].byte_end, 7);
    assert!(
        persisted.source_attestations[0]
            .finished_at_unix_ms
            .is_some()
    );
    #[cfg(unix)]
    {
        assert_eq!(fs::metadata(&staging)?.permissions().mode() & 0o077, 0);
        assert_eq!(fs::metadata(&sidecar)?.permissions().mode() & 0o077, 0);
    }

    let mut progress = |_progress| ProgressControl::Continue;
    let verified = client.fetch_artifact_with_options(
        &url,
        &staging,
        14,
        &digest(body),
        1024,
        &options,
        &mut progress,
    )?;
    let requests = join_server(server)?;
    assert_eq!(requests.len(), 2);
    let resumed_request = requests[1].to_ascii_lowercase();
    assert!(resumed_request.contains("range: bytes=7-\r\n"));
    assert!(resumed_request.contains("if-range: \"v1\"\r\n"));
    assert_eq!(verified.resumed_bytes, 7);
    assert_eq!(verified.bytes, 14);
    assert_eq!(verified.source_attestations.len(), 2);
    assert_eq!(
        verified
            .source_attestations
            .iter()
            .map(|attempt| (attempt.byte_start, attempt.byte_end))
            .collect::<Vec<_>>(),
        vec![(0, 7), (7, 14)]
    );
    assert!(
        verified
            .source_attestations
            .iter()
            .all(|attempt| attempt.finished_at_unix_ms.is_some())
    );
    assert_eq!(
        verified.source_attestation.as_ref(),
        verified
            .source_attestations
            .last()
            .map(|attempt| &attempt.source)
    );
    assert_eq!(fs::read(staging)?, body);
    assert!(!sidecar.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn crash_unfinished_attempt_history_survives_the_next_resume() -> TestResult {
    let body = b"network corpus";
    let continuation = b"HTTP/1.1 206 Partial Content\r\nContent-Length: 7\r\nContent-Range: bytes 7-13/14\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n corpus".to_vec();
    let (url, server) = serve_sequence(vec![continuation])?;
    let directory = private_tempdir()?;
    let staging_path = directory.path().join("staging.bin");
    let sidecar_path = directory.path().join("staging.resume.json");
    let mut staging = StagingFile::create(&staging_path)?;
    staging.file.write_all(b"network")?;
    staging.file.sync_all()?;
    let staging_identity = staging.identity;
    staging.keep = true;
    drop(staging);
    let state = ResumeSidecar {
        version: RESUME_SIDECAR_VERSION,
        requested_uri: url.clone(),
        expected_bytes: 14,
        expected_sha256: digest(body),
        staging_identity,
        validator: HttpValidator::StrongEtag("\"v1\"".to_string()),
        source_attestations: vec![SourceAttemptAttestation {
            source: SourceAttestation {
                requested_uri: url.clone(),
                redirect_chain: Vec::new(),
                final_uri: url.clone(),
                final_peer_address: "127.0.0.1:1".to_string(),
            },
            byte_start: 0,
            byte_end: 0,
            started_at_unix_ms: 1,
            finished_at_unix_ms: None,
        }],
    };
    let mut sidecar = SidecarFile::create(&sidecar_path, &state)?;
    sidecar.keep = true;
    drop(sidecar);

    let options = local_options(ResumePolicy::durable(&sidecar_path));
    let mut progress = |_progress| ProgressControl::Continue;
    let verified = AcquireClient::with_defaults()?.fetch_artifact_with_options(
        &url,
        &staging_path,
        14,
        &digest(body),
        1024,
        &options,
        &mut progress,
    )?;
    let requests = join_server(server)?;
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("range: bytes=7-\r\n")
    );
    assert_eq!(
        verified
            .source_attestations
            .iter()
            .map(|attempt| (attempt.byte_start, attempt.byte_end))
            .collect::<Vec<_>>(),
        vec![(0, 7), (7, 14)]
    );
    assert!(
        verified.source_attestations[0]
            .finished_at_unix_ms
            .is_none()
    );
    assert!(
        verified.source_attestations[1]
            .finished_at_unix_ms
            .is_some()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn changed_validator_restarts_from_zero_without_mixing_partial_bytes() -> TestResult {
    let body = b"network corpus";
    let first = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nAccept-Ranges: bytes\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n7\r\nnetwork\r\n".to_vec();
    let second = response(
        "200 OK",
        &[
            ("Content-Length", "14"),
            ("Accept-Ranges", "bytes"),
            ("ETag", "\"v2\""),
        ],
        body,
    );
    let (url, server) = serve_sequence(vec![first, second])?;
    let directory = private_tempdir()?;
    let staging = directory.path().join("staging.bin");
    let sidecar = directory.path().join("staging.resume.json");
    let options = local_options(ResumePolicy::durable(&sidecar));
    let client = AcquireClient::with_defaults()?;
    let mut cancel = |progress: TransferProgress| {
        if progress.phase == TransferPhase::Downloading {
            ProgressControl::Cancel
        } else {
            ProgressControl::Continue
        }
    };
    let first_result = client.fetch_artifact_with_options(
        &url,
        &staging,
        14,
        &digest(body),
        1024,
        &options,
        &mut cancel,
    );
    assert!(matches!(first_result, Err(AcquireError::Cancelled { .. })));

    let mut progress = |_progress| ProgressControl::Continue;
    let verified = client.fetch_artifact_with_options(
        &url,
        &staging,
        14,
        &digest(body),
        1024,
        &options,
        &mut progress,
    )?;
    let requests = join_server(server)?;
    assert!(
        requests[1]
            .to_ascii_lowercase()
            .contains("range: bytes=7-\r\n")
    );
    assert_eq!(verified.resumed_bytes, 0);
    assert_eq!(
        verified
            .source_attestations
            .iter()
            .map(|attempt| (attempt.byte_start, attempt.byte_end))
            .collect::<Vec<_>>(),
        vec![(0, 7), (0, 14)]
    );
    assert_eq!(fs::read(staging)?, body);
    assert!(!sidecar.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn partial_bytes_without_validator_are_not_preserved() -> TestResult {
    let first = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n7\r\nnetwork\r\n".to_vec();
    let (url, server) = serve_sequence(vec![first])?;
    let directory = private_tempdir()?;
    let staging = directory.path().join("staging.bin");
    let sidecar = directory.path().join("staging.resume.json");
    let options = local_options(ResumePolicy::durable(&sidecar));
    let mut cancel = |progress: TransferProgress| {
        if progress.phase == TransferPhase::Downloading {
            ProgressControl::Cancel
        } else {
            ProgressControl::Continue
        }
    };
    let result = AcquireClient::with_defaults()?.fetch_artifact_with_options(
        &url,
        &staging,
        14,
        &digest(b"network corpus"),
        1024,
        &options,
        &mut cancel,
    );
    join_server(server)?;
    assert!(matches!(result, Err(AcquireError::Cancelled { .. })));
    assert!(!staging.exists());
    assert!(!sidecar.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn mismatched_resume_state_is_identity_safely_restarted() -> TestResult {
    let replacement_body = b"second payload";
    let first = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nAccept-Ranges: bytes\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n7\r\nnetwork\r\n".to_vec();
    let second = response(
        "200 OK",
        &[
            ("Content-Length", "14"),
            ("Accept-Ranges", "bytes"),
            ("ETag", "\"v2\""),
        ],
        replacement_body,
    );
    let (url, server) = serve_sequence(vec![first, second])?;
    let directory = private_tempdir()?;
    let staging = directory.path().join("staging.bin");
    let sidecar = directory.path().join("staging.resume.json");
    let options = local_options(ResumePolicy::durable(&sidecar));
    let mut cancel = |progress: TransferProgress| {
        if progress.phase == TransferPhase::Downloading {
            ProgressControl::Cancel
        } else {
            ProgressControl::Continue
        }
    };
    let client = AcquireClient::with_defaults()?;
    let _first_result = client.fetch_artifact_with_options(
        &url,
        &staging,
        14,
        &digest(b"network corpus"),
        1024,
        &options,
        &mut cancel,
    );
    let mut progress = |_progress| ProgressControl::Continue;
    let verified = client.fetch_artifact_with_options(
        &url,
        &staging,
        14,
        &digest(replacement_body),
        1024,
        &options,
        &mut progress,
    )?;
    let requests = join_server(server)?;
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].to_ascii_lowercase().contains("range:"));
    assert_eq!(verified.resumed_bytes, 0);
    assert_eq!(fs::read(staging)?, replacement_body);
    assert!(!sidecar.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn malformed_resume_sidecar_is_identity_safely_restarted() -> TestResult {
    let body = b"network corpus";
    let first = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nAccept-Ranges: bytes\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n7\r\nnetwork\r\n".to_vec();
    let second = response(
        "200 OK",
        &[
            ("Content-Length", "14"),
            ("Accept-Ranges", "bytes"),
            ("ETag", "\"v1\""),
        ],
        body,
    );
    let (url, server) = serve_sequence(vec![first, second])?;
    let directory = private_tempdir()?;
    let staging = directory.path().join("staging.bin");
    let sidecar = directory.path().join("staging.resume.json");
    let options = local_options(ResumePolicy::durable(&sidecar));
    let client = AcquireClient::with_defaults()?;
    let mut cancel = |progress: TransferProgress| {
        if progress.phase == TransferPhase::Downloading {
            ProgressControl::Cancel
        } else {
            ProgressControl::Continue
        }
    };
    let first_result = client.fetch_artifact_with_options(
        &url,
        &staging,
        14,
        &digest(body),
        1024,
        &options,
        &mut cancel,
    );
    assert!(matches!(first_result, Err(AcquireError::Cancelled { .. })));
    fs::write(&sidecar, b"{malformed")?;

    let mut progress = |_progress| ProgressControl::Continue;
    let verified = client.fetch_artifact_with_options(
        &url,
        &staging,
        14,
        &digest(body),
        1024,
        &options,
        &mut progress,
    )?;
    let requests = join_server(server)?;
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].to_ascii_lowercase().contains("range:"));
    assert_eq!(verified.resumed_bytes, 0);
    assert_eq!(fs::read(staging)?, body);
    assert!(!sidecar.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn invalid_persisted_source_attestation_is_never_returned_as_provenance() -> TestResult {
    let directory = private_tempdir()?;
    let staging_path = directory.path().join("staging.bin");
    let sidecar_path = directory.path().join("staging.resume.json");
    let mut staging = StagingFile::create(&staging_path)?;
    staging.file.write_all(b"partial")?;
    staging.file.sync_all()?;
    let staging_identity = staging.identity;
    staging.keep = true;
    drop(staging);
    let mut state = test_resume_state("https://example.com/archive", &digest(b"partial"), "\"v1\"");
    state.staging_identity = staging_identity;
    state.source_attestations[0].source.final_peer_address = "not-a-peer".to_string();
    let mut sidecar = SidecarFile::create(&sidecar_path, &state)?;
    sidecar.keep = true;
    drop(sidecar);

    let prepared = PreparedTransfer::open(
        "https://example.com/archive",
        &staging_path,
        7,
        &digest(b"partial"),
        &ResumePolicy::durable(&sidecar_path),
    )?;
    assert_eq!(prepared.offset, 0);
    assert!(prepared.source_attestations.is_empty());
    assert_eq!(fs::metadata(&staging_path)?.len(), 0);
    assert!(!sidecar_path.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn interrupted_attempt_range_is_reconciled_from_identity_bound_staging() -> TestResult {
    let directory = private_tempdir()?;
    let staging_path = directory.path().join("staging.bin");
    let sidecar_path = directory.path().join("staging.resume.json");
    let mut staging = StagingFile::create(&staging_path)?;
    staging.file.write_all(b"partial")?;
    staging.file.sync_all()?;
    let staging_identity = staging.identity;
    staging.keep = true;
    drop(staging);
    let mut state = test_resume_state("https://example.com/archive", &digest(b"partial"), "\"v1\"");
    state.staging_identity = staging_identity;
    state.source_attestations[0].byte_end = 0;
    state.source_attestations[0].finished_at_unix_ms = None;
    let mut sidecar = SidecarFile::create(&sidecar_path, &state)?;
    sidecar.keep = true;
    drop(sidecar);

    let prepared = PreparedTransfer::open(
        "https://example.com/archive",
        &staging_path,
        7,
        &digest(b"partial"),
        &ResumePolicy::durable(&sidecar_path),
    )?;
    assert_eq!(prepared.offset, 7);
    assert_eq!(prepared.source_attestations.len(), 1);
    assert_eq!(prepared.source_attestations[0].byte_end, 7);
    assert!(
        prepared.source_attestations[0]
            .finished_at_unix_ms
            .is_none()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn sidecar_rewrite_uses_a_new_private_inode_and_leaves_no_temp() -> TestResult {
    use std::os::unix::fs::MetadataExt;

    let directory = tempdir()?;
    let path = directory.path().join("staging.resume.json");
    let first = test_resume_state("https://example.com/archive", &digest(b"first"), "\"v1\"");
    let second = test_resume_state("https://example.com/archive", &digest(b"second"), "\"v2\"");
    let mut sidecar = SidecarFile::create(&path, &first)?;
    let first_inode = fs::metadata(&path)?.ino();
    sidecar.replace_state(&second)?;
    let second_inode = fs::metadata(&path)?.ino();

    assert_ne!(first_inode, second_inode);
    assert_eq!(sidecar.read_state()?.validator, second.validator);
    assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o077, 0);
    let entries = fs::read_dir(directory.path())?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path(), path);
    Ok(())
}

#[cfg(unix)]
#[test]
fn sidecar_rewrite_never_overwrites_or_deletes_a_replacement_path() -> TestResult {
    let directory = tempdir()?;
    let path = directory.path().join("staging.resume.json");
    let displaced_path = directory.path().join("displaced.resume.json");
    let first = test_resume_state("https://example.com/archive", &digest(b"first"), "\"v1\"");
    let second = test_resume_state("https://example.com/archive", &digest(b"second"), "\"v2\"");
    let mut sidecar = SidecarFile::create(&path, &first)?;
    fs::rename(&path, &displaced_path)?;
    fs::write(&path, b"replacement")?;

    let result = sidecar.replace_state(&second);
    assert!(matches!(result, Err(AcquireError::CleanupIdentityChanged)));
    assert_eq!(fs::read(&path)?, b"replacement");
    assert!(displaced_path.exists());
    assert!(
        fs::read_dir(directory.path())?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .contains(".information-native-tmp-"))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn published_sidecar_recovers_a_crash_left_temp_hardlink() -> TestResult {
    use std::os::unix::fs::MetadataExt;

    let directory = tempdir()?;
    let path = directory.path().join("staging.resume.json");
    let state = test_resume_state("https://example.com/archive", &digest(b"partial"), "\"v1\"");
    let encoded = encode_resume_sidecar(&state)?;
    let mut temp = PrivateSidecarTemp::create(&path, &encoded)?;
    let temp_path = temp.path.clone();
    fs::hard_link(&temp_path, &path)?;
    sync_parent_directory(&path)?;
    temp.keep = true;
    drop(temp);
    assert_eq!(fs::metadata(&path)?.nlink(), 2);

    let mut sidecar = SidecarFile::open(&path)?;
    assert!(!temp_path.exists());
    assert_eq!(fs::metadata(&path)?.nlink(), 1);
    assert_eq!(sidecar.read_state()?.validator, state.validator);
    Ok(())
}

#[cfg(unix)]
#[test]
fn staging_without_sidecar_is_recovered_as_fresh() -> TestResult {
    use std::os::unix::fs::MetadataExt;

    let directory = private_tempdir()?;
    let staging_path = directory.path().join("staging.bin");
    let sidecar_path = directory.path().join("staging.resume.json");
    let mut abandoned = StagingFile::create(&staging_path)?;
    abandoned.file.write_all(b"durable partial")?;
    abandoned.file.sync_all()?;
    let abandoned_inode = fs::metadata(&staging_path)?.ino();
    abandoned.keep = true;
    drop(abandoned);

    let prepared = PreparedTransfer::open(
        "https://example.com/archive",
        &staging_path,
        7,
        &digest(b"partial"),
        &ResumePolicy::durable(&sidecar_path),
    )?;
    assert_eq!(prepared.offset, 0);
    assert_eq!(fs::metadata(&staging_path)?.len(), 0);
    assert_ne!(fs::metadata(&staging_path)?.ino(), abandoned_inode);
    assert!(!sidecar_path.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn sidecar_without_staging_is_recovered_as_fresh() -> TestResult {
    let directory = private_tempdir()?;
    let staging_path = directory.path().join("staging.bin");
    let sidecar_path = directory.path().join("staging.resume.json");
    let mut orphan = SidecarFile::create(
        &sidecar_path,
        &test_resume_state("https://example.com/archive", &digest(b"partial"), "\"v1\""),
    )?;
    orphan.keep = true;
    drop(orphan);

    let prepared = PreparedTransfer::open(
        "https://example.com/archive",
        &staging_path,
        7,
        &digest(b"partial"),
        &ResumePolicy::durable(&sidecar_path),
    )?;
    assert_eq!(prepared.offset, 0);
    assert!(staging_path.exists());
    assert!(!sidecar_path.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn invalid_resume_recovery_never_deletes_a_replacement_sidecar() -> TestResult {
    let directory = private_tempdir()?;
    let staging_path = directory.path().join("staging.bin");
    let sidecar_path = directory.path().join("staging.resume.json");
    let displaced_path = directory.path().join("displaced.resume.json");
    let mut staging = StagingFile::create(&staging_path)?;
    staging.file.write_all(b"partial")?;
    let sidecar = SidecarFile::create(
        &sidecar_path,
        &test_resume_state("https://example.com/archive", &digest(b"partial"), "\"v1\""),
    )?;
    let lease = DurableResumePaths::acquire(&staging_path, &sidecar_path)?.lease;
    let files = TransferFiles::resuming(staging, sidecar, lease);
    fs::rename(&sidecar_path, &displaced_path)?;
    fs::write(&sidecar_path, b"replacement")?;

    let result = files.recover_fresh(&staging_path);
    assert!(matches!(result, Err(AcquireError::CleanupIdentityChanged)));
    assert_eq!(fs::read(&sidecar_path)?, b"replacement");
    assert!(displaced_path.exists());
    assert_eq!(fs::read(&staging_path)?, b"partial");
    Ok(())
}

#[cfg(unix)]
#[test]
fn durable_resume_requires_one_private_exclusively_leased_directory() -> TestResult {
    let public_directory = tempdir()?;
    fs::set_permissions(public_directory.path(), fs::Permissions::from_mode(0o755))?;
    let public_staging = public_directory.path().join("staging.bin");
    let public_sidecar = public_directory.path().join("staging.resume.json");
    let result = PreparedTransfer::open(
        "https://example.com/archive",
        &public_staging,
        7,
        &digest(b"partial"),
        &ResumePolicy::durable(&public_sidecar),
    );
    assert!(matches!(result, Err(AcquireError::UnsafeResumeDirectory)));
    assert!(!public_staging.exists());
    assert!(!public_sidecar.exists());

    let first = private_tempdir()?;
    let second = private_tempdir()?;
    let split_staging = first.path().join("staging.bin");
    let split_sidecar = second.path().join("staging.resume.json");
    let result = PreparedTransfer::open(
        "https://example.com/archive",
        &split_staging,
        7,
        &digest(b"partial"),
        &ResumePolicy::durable(&split_sidecar),
    );
    assert!(matches!(
        result,
        Err(AcquireError::ResumePathsDifferentDirectories)
    ));

    let staging = first.path().join("leased.bin");
    let sidecar = first.path().join("leased.resume.json");
    let lease = DurableResumePaths::acquire(&staging, &sidecar)?;
    let result = DurableResumePaths::acquire(&staging, &sidecar);
    assert!(matches!(result, Err(AcquireError::ResumeDirectoryBusy)));
    drop(lease);
    assert!(DurableResumePaths::acquire(&staging, &sidecar).is_ok());
    Ok(())
}

#[cfg(not(unix))]
#[test]
fn durable_resume_is_rejected_before_creating_any_paths() -> TestResult {
    let directory = tempdir()?;
    let staging = directory.path().join("staging.bin");
    let sidecar = directory.path().join("staging.resume.json");
    let options = local_options(ResumePolicy::durable(&sidecar));
    let mut progress = |_progress| ProgressControl::Continue;
    let result = AcquireClient::with_defaults()?.fetch_artifact_with_options(
        "http://127.0.0.1:9/resource",
        &staging,
        1,
        &digest(b"x"),
        1,
        &options,
        &mut progress,
    );
    assert!(matches!(
        result,
        Err(AcquireError::DurableResumeUnsupportedOnPlatform)
    ));
    assert!(!staging.exists());
    assert!(!sidecar.exists());
    Ok(())
}

#[test]
fn read_idle_timeout_is_independent_from_total_timeout() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let server = thread::spawn(move || -> io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let _request = read_request(&mut stream)?;
        thread::sleep(Duration::from_millis(150));
        Ok(())
    });
    let config = AcquireConfig {
        connect_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(2),
        max_redirects: 1,
        user_agent: "acquire-timeout-test".to_string(),
    };
    let client = AcquireClient::new_with_timeouts(
        config,
        TransferTimeouts::new(Duration::from_millis(30), Duration::from_secs(2)),
    )?;
    let policy = AcquisitionPolicy::restricted().with_network_scope(NetworkScope::AnyAddress);
    let result =
        client.fetch_catalogue_with_policy(&format!("http://{address}/resource"), 1024, &policy);
    join_unit_server(server)?;
    assert!(matches!(result, Err(AcquireError::Network(_))));
    Ok(())
}

#[cfg(unix)]
fn private_tempdir() -> Result<TempDir, io::Error> {
    let directory = tempdir()?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

fn local_options(resume: ResumePolicy) -> ArtifactFetchOptions {
    ArtifactFetchOptions {
        acquisition_policy: AcquisitionPolicy::restricted()
            .with_network_scope(NetworkScope::AnyAddress),
        resume,
    }
}

fn test_resume_state(uri: &str, sha256: &str, etag: &str) -> ResumeSidecar {
    ResumeSidecar {
        version: RESUME_SIDECAR_VERSION,
        requested_uri: uri.to_string(),
        expected_bytes: 7,
        expected_sha256: sha256.to_string(),
        staging_identity: test_file_identity(),
        validator: HttpValidator::StrongEtag(etag.to_string()),
        source_attestations: vec![SourceAttemptAttestation {
            source: SourceAttestation {
                requested_uri: uri.to_string(),
                redirect_chain: Vec::new(),
                final_uri: uri.to_string(),
                final_peer_address: "203.0.113.1:443".to_string(),
            },
            byte_start: 0,
            byte_end: 7,
            started_at_unix_ms: 1,
            finished_at_unix_ms: Some(1),
        }],
    }
}

#[cfg(unix)]
fn test_file_identity() -> FileIdentity {
    FileIdentity {
        device: 0,
        inode: 0,
    }
}

#[cfg(not(unix))]
fn test_file_identity() -> FileIdentity {
    FileIdentity
}

fn resolve_destination_for_test(
    url: &Url,
    policy: &AcquisitionPolicy,
) -> Result<ResolvedDestination, AcquireError> {
    let timeout = Duration::from_secs(1);
    let deadline = transfer_deadline(timeout)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| AcquireError::Runtime(error.to_string()))?;
    runtime.block_on(resolve_destination(url, policy, deadline, timeout))
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn file_uri(path: &Path) -> Result<String, io::Error> {
    Url::from_file_path(path)
        .map(Into::into)
        .map_err(|()| io::Error::other("test path could not become a file URI"))
}

fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 {status}\r\n").into_bytes();
    for (name, value) in headers {
        response.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    response.extend_from_slice(b"Connection: close\r\n\r\n");
    response.extend_from_slice(body);
    response
}

fn serve_sequence(responses: Vec<Vec<u8>>) -> Result<(String, TestServer), io::Error> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let server = thread::spawn(move || -> io::Result<Vec<String>> {
        let mut requests = Vec::with_capacity(responses.len());
        for response in responses {
            let (mut stream, _) = listener.accept()?;
            requests.push(read_request(&mut stream)?);
            stream.write_all(&response)?;
            stream.flush()?;
            let _shutdown_result = stream.shutdown(Shutdown::Both);
        }
        Ok(requests)
    });
    Ok((format!("http://{address}/resource"), server))
}

fn read_request(stream: &mut TcpStream) -> io::Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.len() > 64 * 1024 {
            return Err(io::Error::other("test request exceeded 64 KiB"));
        }
    }
    String::from_utf8(request).map_err(io::Error::other)
}

fn join_server(server: TestServer) -> Result<Vec<String>, io::Error> {
    server
        .join()
        .map_err(|_| io::Error::other("test server thread panicked"))?
}

fn join_unit_server(server: JoinHandle<io::Result<()>>) -> Result<(), io::Error> {
    server
        .join()
        .map_err(|_| io::Error::other("test server thread panicked"))?
}

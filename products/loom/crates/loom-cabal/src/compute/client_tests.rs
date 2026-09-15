use super::*;

#[cfg(not(unix))]
#[test]
fn unsupported_private_storage_fails_before_creating_a_request_directory() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("requests");
    assert!(ComputeClient::open(&root, Identity::generate()?.public_key()).is_err());
    assert!(!root.exists());
    Ok(())
}

#[cfg(unix)]
mod supported {
    use super::*;

    #[test]
    fn recovery_never_creates_a_replacement_ledger() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let peer = Identity::generate()?.public_key();
        let root = temporary.path().join("requests");
        assert!(ComputeClient::open_existing(&root, peer).is_err());
        assert!(!root.exists());
        std::fs::create_dir(&root)?;
        assert!(ComputeClient::open_existing(&root, peer).is_err());
        assert_eq!(std::fs::read_dir(&root)?.count(), 0);
        drop(ComputeClient::open(&root, peer)?);
        drop(ComputeClient::open_existing(&root, peer)?);
        std::fs::rename(root.join("requests.db"), root.join("preserved.db"))?;
        assert!(ComputeClient::open_existing(&root, peer).is_err());
        assert!(!root.join("requests.db").exists());
        assert!(root.join("preserved.db").exists());
        Ok(())
    }

    struct Fixture {
        directory: tempfile::TempDir,
        peer: PublicKey,
        host: Identity,
        request: ClientRequest,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let peer = Identity::generate()?.public_key();
            let host = Identity::generate()?;
            let request = ClientRequest {
                id: Uuid::new_v4(),
                host: host.public_key(),
                grant: ComputeGrant {
                    id: Uuid::new_v4(),
                    cabal: Uuid::new_v4(),
                    epoch: 1,
                    peer,
                    model: ComputeModel {
                        media: Vec::new(),
                        name: "Remote model claim".into(),
                        fingerprint: "ab".repeat(32),
                    },
                    max_output_tokens: 128,
                    max_seconds: 30,
                    jobs: 2,
                },
                input: ComputeInput {
                    media: Vec::new(),
                    prompt: "Exact 🌱 @document =function() input".into(),
                    max_output_tokens: 16,
                    seed: 42,
                },
            };
            Ok(Self {
                directory: tempfile::tempdir()?,
                peer,
                host,
                request,
            })
        }
        fn open(&self) -> Result<ComputeClient> {
            ComputeClient::open(self.directory.path(), self.peer)
        }
        fn receipt(&self, status: ComputeStatus, revision: u32) -> Result<RemoteJobReceipt> {
            self.host.sign(RemoteJobRecord {
                kind: RemoteRecordKind::Execution,
                job: self.request.id,
                peer: self.peer,
                grant: self.request.grant.id,
                request_fingerprint: self.request.input.fingerprint(self.request.grant.id)?,
                model: self.request.grant.model.clone(),
                revision,
                created_at_ms: 1000,
                recorded_at_ms: 1000 + u64::from(revision),
                status,
            })
        }
    }

    #[test]
    fn media_and_cancel_intent_reopen_exactly_without_following_later_bytes() -> Result<()> {
        let mut fixture = Fixture::new()?;
        fixture.request.grant.model.media = vec![ComputeModality::Image];
        fixture.request.input.media = vec![ComputeMedia::new(
            ComputeMediaFormat::Png,
            &vec![19; 1024 * 1024],
        )?];
        let mut client = fixture.open()?;
        client.prepare(fixture.request.clone())?;
        client.request_cancel(fixture.request.id)?;
        drop(client);
        let mut client = fixture.open()?;
        let saved = client.get(fixture.request.id)?.expect("retained request");
        assert_eq!(saved.request, fixture.request);
        assert!(saved.cancel_requested);
        assert!(saved.receipt.is_none());
        let mut changed = fixture.request.clone();
        changed.input.media[0] =
            ComputeMedia::new(ComputeMediaFormat::Png, b"later file contents")?;
        assert!(client.prepare(changed).is_err());
        Ok(())
    }

    #[test]
    fn exact_input_and_uncertain_cancellation_survive_restart_without_fabricating_a_result()
    -> Result<()> {
        let fixture = Fixture::new()?;
        let mut client = fixture.open()?;
        assert!(client.prepare(fixture.request.clone())?.receipt.is_none());
        let mut changed = fixture.request.clone();
        changed.input.seed += 1;
        assert!(client.prepare(changed).is_err());
        client.request_cancel(fixture.request.id)?;
        client.request_cancel(fixture.request.id)?;
        drop(client);
        let mut client = fixture.open()?;
        let pending = client.prepare(fixture.request.clone())?;
        assert_eq!(pending.request, fixture.request);
        assert!(pending.cancel_requested);
        assert!(
            pending.receipt.is_none(),
            "restart cannot invent a remote terminal receipt"
        );
        assert_eq!(client.jobs(fixture.request.grant.cabal)?.len(), 1);
        assert!(client.jobs(Uuid::new_v4())?.is_empty());
        Ok(())
    }

    #[test]
    fn only_bound_host_assertions_are_retained_and_a_terminal_cannot_be_rewritten() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut client = fixture.open()?;
        client.prepare(fixture.request.clone())?;
        let accepted = fixture.receipt(ComputeStatus::Accepted, 0)?;
        client.record(accepted.clone())?;
        let mut changed = fixture.receipt(ComputeStatus::Running, 1)?.payload;
        changed.model.name = "A different model".into();
        assert!(client.record(fixture.host.sign(changed)?).is_err());
        let mut wrong_input = fixture.receipt(ComputeStatus::Running, 1)?.payload;
        wrong_input.request_fingerprint = "cd".repeat(32);
        assert!(client.record(fixture.host.sign(wrong_input)?).is_err());
        let other = Identity::generate()?;
        assert!(
            client
                .record(other.sign(fixture.receipt(ComputeStatus::Running, 1)?.payload)?)
                .is_err()
        );
        assert!(
            client
                .record(fixture.receipt(ComputeStatus::Running, 8)?)
                .is_err()
        );
        let completed = fixture.receipt(
            ComputeStatus::Completed {
                text: "A remote assertion, not a local witness".into(),
            },
            2,
        )?;
        client.record(completed.clone())?;
        assert_eq!(
            client
                .record(accepted)?
                .receipt
                .expect("latest receipt")
                .hash()?,
            completed.hash()?
        );
        let rewrite = fixture.receipt(
            ComputeStatus::Completed {
                text: "replacement".into(),
            },
            2,
        )?;
        assert!(client.record(rewrite).is_err());
        assert!(
            client
                .record(fixture.receipt(ComputeStatus::Interrupted, 3)?)
                .is_err()
        );
        drop(client);
        let client = fixture.open()?;
        assert_eq!(
            client
                .get(fixture.request.id)?
                .expect("job")
                .receipt
                .expect("terminal")
                .hash()?,
            completed.hash()?
        );
        let encoded = serde_json::to_value(client.get(fixture.request.id)?)?;
        assert_eq!(
            encoded["receipt"]["payload"]["kind"],
            "loom_remote_execution_v1"
        );
        assert!(encoded.get("token_trace").is_none());
        Ok(())
    }

    #[test]
    fn cancellation_cannot_turn_into_success_or_change_its_recorded_reason() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut client = fixture.open()?;
        client.prepare(fixture.request.clone())?;
        client.record(fixture.receipt(
            ComputeStatus::Cancelling {
                reason: ComputeCancellation::Requested,
            },
            1,
        )?)?;
        assert!(
            client
                .record(fixture.receipt(
                    ComputeStatus::Completed {
                        text: "late".into()
                    },
                    2
                )?)
                .is_err()
        );
        assert!(
            client
                .record(fixture.receipt(
                    ComputeStatus::Cancelled {
                        reason: ComputeCancellation::GrantRevoked
                    },
                    2
                )?)
                .is_err()
        );
        let cancelled = fixture.receipt(
            ComputeStatus::Cancelled {
                reason: ComputeCancellation::Requested,
            },
            2,
        )?;
        client.record(cancelled.clone())?;
        assert_eq!(
            client
                .record(cancelled.clone())?
                .receipt
                .expect("terminal")
                .hash()?,
            cancelled.hash()?
        );
        Ok(())
    }

    #[test]
    fn pending_result_space_is_reserved_before_dispatch_and_can_still_accept_the_terminal()
    -> Result<()> {
        let fixture = Fixture::new()?;
        let mut client = fixture.open()?;
        client.prepare(fixture.request.clone())?;
        let mut prepared = 1;
        loop {
            let mut request = fixture.request.clone();
            request.id = Uuid::new_v4();
            if client.prepare(request).is_err() {
                break;
            }
            prepared += 1;
            assert!(prepared < 65, "each unresolved job reserves its result");
        }
        assert!(prepared > 1);
        let completed = fixture.receipt(
            ComputeStatus::Completed {
                text: "\0".repeat(MAX_COMPUTE_TEXT_BYTES),
            },
            2,
        )?;
        client.record(completed)?;
        let mut next = fixture.request.clone();
        next.id = Uuid::new_v4();
        client.prepare(next)?;
        assert_eq!(
            client
                .prepare(fixture.request.clone())?
                .receipt
                .expect("terminal")
                .payload
                .revision,
            2
        );
        assert!(client.request_cancel(Uuid::new_v4()).is_err());
        Ok(())
    }

    #[test]
    fn one_device_owns_private_storage_and_incompatible_or_corrupt_data_is_preserved() -> Result<()>
    {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new()?;
        let mut client = fixture.open()?;
        assert_eq!(
            std::fs::metadata(fixture.directory.path())?
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(fixture.open().is_err());
        client.prepare(fixture.request.clone())?;
        let body: String = client
            .database
            .query_row("SELECT body FROM requests", [], |row| row.get(0))?;
        client.database.pragma_update(None, "user_version", 9)?;
        drop(client);
        assert!(fixture.open().is_err());
        let database = Connection::open(fixture.directory.path().join("requests.db"))?;
        assert_eq!(
            database.query_row("SELECT body FROM requests", [], |row| row
                .get::<_, String>(0))?,
            body
        );
        database.pragma_update(None, "user_version", 2)?;
        drop(database);
        assert!(
            ComputeClient::open(fixture.directory.path(), Identity::generate()?.public_key())
                .is_err()
        );
        let client = fixture.open()?;
        client.database.execute(
            "UPDATE requests SET cabal = ?",
            [Uuid::new_v4().to_string()],
        )?;
        drop(client);
        assert!(
            fixture.open().is_err(),
            "a corrupt workspace index cannot transplant a job"
        );
        Ok(())
    }
}

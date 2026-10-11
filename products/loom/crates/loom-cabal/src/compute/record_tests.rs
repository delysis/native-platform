use super::*;

fn request(host: &Identity) -> Result<ClientRequest> {
    Ok(ClientRequest {
        id: Uuid::new_v4(),
        host: host.public_key(),
        grant: ComputeGrant {
            id: Uuid::new_v4(),
            cabal: Uuid::new_v4(),
            epoch: 1,
            peer: Identity::generate()?.public_key(),
            model: ComputeModel {
                fingerprint: "ab".repeat(32),
                name: "Receipt fixture".into(),
                media: Vec::new(),
            },
            max_output_tokens: 16,
            max_seconds: 1,
            jobs: 1,
        },
        input: ComputeInput {
            prompt: "An exact request".into(),
            format: ComputePromptFormat::Raw,
            max_output_tokens: 16,
            seed: 1,
            media: Vec::new(),
        },
    })
}

fn receipt(
    host: &Identity,
    request: &ClientRequest,
    status: ComputeStatus,
    revision: u32,
) -> Result<RemoteJobReceipt> {
    host.sign(RemoteJobRecord {
        kind: RemoteRecordKind::Execution,
        job: request.id,
        peer: request.grant.peer,
        grant: request.grant.id,
        request_fingerprint: request.input.fingerprint(request.grant.id)?,
        model: request.grant.model.clone(),
        revision,
        created_at_ms: 1,
        recorded_at_ms: 1 + u64::from(revision),
        status,
    })
}

#[test]
fn signatures_do_not_make_malformed_compute_records_valid() -> Result<()> {
    let host = Identity::generate()?;
    let request = request(&host)?;
    let valid = receipt(
        &host,
        &request,
        ComputeStatus::Completed { text: "ok".into() },
        2,
    )?;
    valid.payload.validate()?;
    let record = valid.payload;
    for invalid in [
        RemoteJobRecord {
            job: Uuid::nil(),
            ..record.clone()
        },
        RemoteJobRecord {
            grant: Uuid::nil(),
            ..record.clone()
        },
        RemoteJobRecord {
            revision: 0,
            ..record.clone()
        },
        RemoteJobRecord {
            recorded_at_ms: 0,
            ..record.clone()
        },
        RemoteJobRecord {
            request_fingerprint: "not a digest".into(),
            ..record.clone()
        },
        RemoteJobRecord {
            model: ComputeModel {
                fingerprint: "AB".repeat(32),
                ..record.model.clone()
            },
            ..record.clone()
        },
        RemoteJobRecord {
            model: ComputeModel {
                name: "hidden\ncontrol".into(),
                ..record.model.clone()
            },
            ..record.clone()
        },
        RemoteJobRecord {
            model: ComputeModel {
                media: vec![ComputeModality::Image, ComputeModality::Image],
                ..record.model.clone()
            },
            ..record.clone()
        },
        RemoteJobRecord {
            status: ComputeStatus::Completed {
                text: "x".repeat(MAX_COMPUTE_TEXT_BYTES + 1),
            },
            ..record.clone()
        },
    ] {
        let signed = host.sign(invalid)?;
        signed.verify()?;
        assert!(signed.payload.validate().is_err());
        assert!(client::validate_receipt(&request, &signed).is_err());
    }
    Ok(())
}

#[test]
fn every_supported_receipt_shape_is_accepted() -> Result<()> {
    let host = Identity::generate()?;
    let request = request(&host)?;
    for (status, revision) in [
        (ComputeStatus::Accepted, 0),
        (ComputeStatus::Running, 1),
        (
            ComputeStatus::Cancelling {
                reason: ComputeCancellation::Requested,
            },
            1,
        ),
        (
            ComputeStatus::Cancelling {
                reason: ComputeCancellation::Requested,
            },
            2,
        ),
        (
            ComputeStatus::Cancelled {
                reason: ComputeCancellation::Requested,
            },
            2,
        ),
        (
            ComputeStatus::Cancelled {
                reason: ComputeCancellation::Requested,
            },
            3,
        ),
        (ComputeStatus::Completed { text: "ok".into() }, 2),
        (
            ComputeStatus::Failed {
                failure: ComputeFailure::ExecutionFailed,
            },
            2,
        ),
        (ComputeStatus::Interrupted, 1),
        (ComputeStatus::Interrupted, 2),
        (ComputeStatus::Interrupted, 3),
    ] {
        client::validate_receipt(&request, &receipt(&host, &request, status, revision)?)?;
    }
    Ok(())
}

#[test]
fn skipped_observations_must_still_have_a_reachable_revision_path() -> Result<()> {
    let host = Identity::generate()?;
    let request = request(&host)?;
    let accepted = receipt(&host, &request, ComputeStatus::Accepted, 0)?;
    let running = receipt(&host, &request, ComputeStatus::Running, 1)?;
    let early_cancel = receipt(
        &host,
        &request,
        ComputeStatus::Cancelling {
            reason: ComputeCancellation::Requested,
        },
        1,
    )?;
    let cancelled_before_dispatch = receipt(
        &host,
        &request,
        ComputeStatus::Cancelled {
            reason: ComputeCancellation::Requested,
        },
        2,
    )?;
    let cancelled_after_dispatch = receipt(
        &host,
        &request,
        ComputeStatus::Cancelled {
            reason: ComputeCancellation::Requested,
        },
        3,
    )?;
    let interrupted = receipt(&host, &request, ComputeStatus::Interrupted, 3)?;
    for value in [
        &accepted,
        &running,
        &early_cancel,
        &cancelled_before_dispatch,
        &cancelled_after_dispatch,
        &interrupted,
    ] {
        client::validate_receipt(&request, value)?;
    }
    // Missing intermediate network replies are allowed; impossible histories are not.
    client::validate_advance(&accepted, &cancelled_before_dispatch)?;
    client::validate_advance(&accepted, &cancelled_after_dispatch)?;
    client::validate_advance(&running, &cancelled_after_dispatch)?;
    client::validate_advance(&running, &interrupted)?;
    client::validate_advance(&early_cancel, &cancelled_before_dispatch)?;
    assert!(client::validate_advance(&running, &cancelled_before_dispatch).is_err());
    assert!(client::validate_advance(&early_cancel, &cancelled_after_dispatch).is_err());
    assert!(client::validate_advance(&early_cancel, &interrupted).is_err());
    assert!(client::validate_advance(&cancelled_before_dispatch, &interrupted).is_err());
    Ok(())
}

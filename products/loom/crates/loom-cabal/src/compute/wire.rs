use super::*;
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use tokio::sync::Semaphore;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Offers {
        cabal: Uuid,
    },
    Submit {
        job: Uuid,
        grant: Uuid,
        input: ComputeInput,
    },
    Status {
        job: Uuid,
    },
    Cancel {
        job: Uuid,
        grant: Uuid,
        input: ComputeInput,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Offers { grants: Vec<ComputeGrant> },
    Receipt { receipt: Box<RemoteJobReceipt> },
    Rejected { reason: ComputeRejection },
}

#[derive(Debug, Clone)]
pub(crate) struct Handler {
    host: Arc<Mutex<HostSlot>>,
    slots: Arc<Semaphore>,
}

#[derive(Debug, Default)]
struct HostSlot {
    host: Option<Arc<ComputeHost>>,
    closed: bool,
}

impl Handler {
    pub fn new() -> Self {
        Self {
            host: Arc::new(Mutex::new(HostSlot::default())),
            slots: Arc::new(Semaphore::new(8)),
        }
    }

    pub fn configure(
        &self,
        open: impl FnOnce() -> Result<Arc<ComputeHost>>,
    ) -> Result<Arc<ComputeHost>> {
        let mut host = self
            .host
            .lock()
            .map_err(|_| Error::Invalid("Compute protocol stopped"))?;
        if host.closed {
            return Err(Error::Invalid("Compute protocol is closing"));
        }
        if host.host.is_some() {
            return Err(Error::Invalid("Compute host is already configured"));
        }
        let configured = open()?;
        host.host = Some(configured.clone());
        Ok(configured)
    }

    pub fn stop(&self) {
        if let Ok(mut slot) = self.host.lock() {
            slot.closed = true;
            if let Some(host) = &slot.host {
                host.stop();
            }
        }
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.stop();
        let host = self
            .host
            .lock()
            .map_err(|_| Error::Invalid("Compute protocol stopped"))?
            .host
            .clone();
        if let Some(host) = host {
            host.shutdown().await?;
        }
        Ok(())
    }
}

impl ProtocolHandler for Handler {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        let Ok(_slot) = self.slots.try_acquire() else {
            connection.close(1u32.into(), b"busy");
            return Ok(());
        };
        let handle = async {
            let (mut send, mut receive) = connection.accept_bi().await.map_err(network_error)?;
            let bytes = receive
                .read_to_end(crate::MAX_FRAME_BYTES)
                .await
                .map_err(network_error)?;
            let incoming: Request = serde_json::from_slice(&bytes)?;
            let host = self
                .host
                .lock()
                .map_err(|_| Error::Invalid("Compute protocol stopped"))?
                .host
                .clone();
            let response = host.map_or(
                Response::Rejected {
                    reason: ComputeRejection::Stopped,
                },
                |host| {
                    host.respond(connection.remote_id(), incoming)
                        .unwrap_or_else(|_| {
                            // A failed durable cancellation must still stop the
                            // executor. Do not keep accepting work on a broken ledger.
                            host.stop();
                            Response::Rejected {
                                reason: ComputeRejection::Unavailable,
                            }
                        })
                },
            );
            send.write_all(&encode(&response)?)
                .await
                .map_err(network_error)?;
            send.finish().map_err(network_error)?;
            connection.closed().await;
            Ok::<(), Error>(())
        };
        let _ = tokio::time::timeout(Duration::from_secs(12), handle).await;
        connection.close(0u32.into(), b"complete");
        Ok(())
    }
}

pub(crate) async fn request(
    endpoint: &Endpoint,
    address: EndpointAddr,
    request: Request,
) -> Result<Response> {
    let bytes = encode(&request)?;
    let host = address.id;
    let peer = endpoint.id();
    tokio::time::timeout(Duration::from_secs(10), async {
        let connection = endpoint
            .connect(address, ALPN)
            .await
            .map_err(network_error)?;
        let result = async {
            let (mut send, mut receive) = connection.open_bi().await.map_err(network_error)?;
            send.write_all(&bytes).await.map_err(network_error)?;
            send.finish().map_err(network_error)?;
            let bytes = receive
                .read_to_end(crate::MAX_FRAME_BYTES)
                .await
                .map_err(network_error)?;
            let response = serde_json::from_slice(&bytes)?;
            validate_response(&response, &request, host, peer)?;
            Ok(response)
        }
        .await;
        connection.close(0u32.into(), b"complete");
        result
    })
    .await
    .map_err(|_| {
        Error::Invalid("Compute peer did not respond in time; check the same job before retrying")
    })?
}

fn validate_response(
    response: &Response,
    request: &Request,
    host: PublicKey,
    peer: PublicKey,
) -> Result<()> {
    match (response, request) {
        (Response::Rejected { .. }, _) => Ok(()),
        (Response::Offers { grants }, Request::Offers { cabal }) => {
            if grants.len() > 64 {
                return Err(Error::Invalid("Too many compute offers"));
            }
            for grant in grants {
                grant.validate()?;
                if grant.cabal != *cabal || grant.peer != peer {
                    return Err(Error::Invalid("Compute offer identity mismatch"));
                }
            }
            Ok(())
        }
        (
            Response::Receipt { receipt },
            Request::Submit { job, .. } | Request::Status { job } | Request::Cancel { job, .. },
        ) => {
            receipt.verify()?;
            if receipt.signer != host || receipt.payload.peer != peer || receipt.payload.job != *job
            {
                return Err(Error::Invalid("Remote compute receipt identity mismatch"));
            }
            if let Request::Submit { grant, input, .. } | Request::Cancel { grant, input, .. } =
                request
                && (receipt.payload.grant != *grant
                    || receipt.payload.request_fingerprint != input.fingerprint(*grant)?)
            {
                return Err(Error::Invalid("Remote compute receipt input mismatch"));
            }
            if let ComputeStatus::Completed { text } = &receipt.payload.status
                && text.len() > MAX_COMPUTE_TEXT_BYTES
            {
                return Err(Error::Invalid("Remote compute output exceeds limit"));
            }
            Ok(())
        }
        _ => Err(Error::Invalid("Unexpected compute reply")),
    }
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > crate::MAX_FRAME_BYTES {
        return Err(Error::Invalid("Compute frame exceeds limit"));
    }
    Ok(bytes)
}

fn network_error(error: impl std::fmt::Display) -> Error {
    Error::Network(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_before_configuration_prevents_opening_an_executor() {
        let handler = Handler::new();
        handler.stop();
        assert!(
            handler
                .configure(|| { panic!("shutdown must prevent constructing a new executor") })
                .is_err()
        );
    }

    #[test]
    fn remote_claims_bind_the_endpoint_peer_job_and_exact_input() -> Result<()> {
        let host = Identity::generate()?;
        let peer = Identity::generate()?.public_key();
        let input = ComputeInput {
            prompt: "My words".into(),
            max_output_tokens: 10,
            seed: 42,
        };
        let job = Uuid::new_v4();
        let grant = Uuid::new_v4();
        let request = Request::Submit {
            job,
            grant,
            input: input.clone(),
        };
        let record = RemoteJobRecord {
            kind: RemoteRecordKind::Execution,
            job,
            peer,
            grant,
            request_fingerprint: input.fingerprint(grant)?,
            model: ComputeModel {
                fingerprint: "ab".repeat(32),
                name: "An assertion from the host".into(),
            },
            revision: 2,
            created_at_ms: 1,
            recorded_at_ms: 2,
            status: ComputeStatus::Completed {
                text: "Generated words".into(),
            },
        };
        let response = Response::Receipt {
            receipt: Box::new(host.sign(record.clone())?),
        };
        validate_response(&response, &request, host.public_key(), peer)?;
        let bytes = encode(&response)?;
        assert!(String::from_utf8_lossy(&bytes).contains("loom_remote_execution_v1"));
        validate_response(
            &serde_json::from_slice(&bytes)?,
            &request,
            host.public_key(),
            peer,
        )?;
        assert!(
            validate_response(
                &response,
                &request,
                Identity::generate()?.public_key(),
                peer
            )
            .is_err()
        );
        assert!(
            validate_response(&response, &request, host.public_key(), host.public_key()).is_err()
        );
        for altered in [
            RemoteJobRecord {
                job: Uuid::new_v4(),
                ..record.clone()
            },
            RemoteJobRecord {
                grant: Uuid::new_v4(),
                ..record.clone()
            },
            RemoteJobRecord {
                request_fingerprint: "00".repeat(32),
                ..record.clone()
            },
        ] {
            let response = Response::Receipt {
                receipt: Box::new(host.sign(altered)?),
            };
            assert!(validate_response(&response, &request, host.public_key(), peer).is_err());
        }
        let mut forged = host.sign(record)?;
        forged.payload.status = ComputeStatus::Completed {
            text: "Replaced output".into(),
        };
        assert!(
            validate_response(
                &Response::Receipt {
                    receipt: Box::new(forged)
                },
                &request,
                host.public_key(),
                peer
            )
            .is_err()
        );
        Ok(())
    }
}

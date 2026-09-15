//! Authenticated pull-based anti-entropy. Every admitted device exchanges its
//! missing signed changes; disconnected devices catch up after rejoining.
use base64::{Engine, engine::general_purpose::STANDARD};
use iroh::{
    Endpoint, EndpointAddr, PublicKey,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Semaphore, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::compute::{self, ComputeExecutor, ComputeHost, ComputeInput, ComputeReply};
use crate::{
    ASSET_CHUNK_BYTES, AssetDescriptor, Cabal, ChangeEnvelope, Error, Identity, Invitation,
    MAX_FRAME_BYTES, NetworkMode, Result, Roster,
};

const ALPN: &[u8] = b"app.delysis.loom/cabal/2";
const MAX_CABALS: usize = 16;
type SharedCabal = Arc<Mutex<Cabal>>;
type Cabals = Arc<Mutex<BTreeMap<Uuid, SharedCabal>>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerStatus {
    pub cabal: Uuid,
    pub key: PublicKey,
    pub connected: bool,
    pub changed: bool,
}

#[derive(Debug)]
pub struct Network {
    router: Router,
    mode: NetworkMode,
    identity: Identity,
    compute: compute::Handler,
    compute_outbound: Semaphore,
    cabals: Cabals,
    stop: CancellationToken,
    worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    status: watch::Receiver<Vec<PeerStatus>>,
}

impl Network {
    pub async fn start(identity: &Identity, mode: NetworkMode) -> Result<Self> {
        let builder = mode.builder()?;
        let endpoint = builder
            .secret_key(identity.secret_key())
            .bind()
            .await
            .map_err(network_error)?;
        Self::attach(identity, mode, endpoint)
    }

    pub(crate) fn attach(
        identity: &Identity,
        mode: NetworkMode,
        endpoint: Endpoint,
    ) -> Result<Self> {
        mode.validate()?;
        if endpoint.id() != identity.public_key() {
            return Err(Error::Invalid("Network endpoint belongs to another device"));
        }
        let cabals = Arc::new(Mutex::new(BTreeMap::new()));
        let compute = compute::Handler::new();
        let router = Router::builder(endpoint.clone())
            .accept(compute::ALPN, compute.clone())
            .accept(
                ALPN,
                Handler {
                    cabals: cabals.clone(),
                    slots: Arc::new(Semaphore::new(8)),
                },
            )
            .spawn();
        let stop = CancellationToken::new();
        let (status_tx, status) = watch::channel(Vec::new());
        let worker_cabals = cabals.clone();
        let worker_stop = stop.clone();
        let worker_mode = mode.clone();
        let worker = tokio::spawn(async move {
            supervise(endpoint, worker_mode, worker_cabals, status_tx, worker_stop).await;
        });
        Ok(Self {
            router,
            mode,
            identity: identity.clone(),
            compute,
            compute_outbound: Semaphore::new(4),
            cabals,
            stop,
            worker: tokio::sync::Mutex::new(Some(worker)),
            status,
        })
    }

    pub fn address(&self) -> EndpointAddr {
        self.mode.address(self.router.endpoint().addr())
    }
    pub fn mode(&self) -> &NetworkMode {
        &self.mode
    }
    pub fn status(&self) -> watch::Receiver<Vec<PeerStatus>> {
        self.status.clone()
    }

    pub fn add(&self, cabal: SharedCabal) -> Result<()> {
        let value = cabal
            .lock()
            .map_err(|_| Error::Invalid("Cabal owner stopped"))?;
        if value.identity().public_key() != self.address().id {
            return Err(Error::Invalid("Cabal belongs to another device"));
        }
        let id = value.id();
        drop(value);
        let mut cabals = self
            .cabals
            .lock()
            .map_err(|_| Error::Invalid("Cabal registry stopped"))?;
        if cabals.len() >= MAX_CABALS && !cabals.contains_key(&id) {
            return Err(Error::Invalid("Too many open cabals"));
        }
        cabals.insert(id, cabal);
        Ok(())
    }

    pub fn remove(&self, id: Uuid) -> Result<()> {
        self.cabals
            .lock()
            .map_err(|_| Error::Invalid("Cabal registry stopped"))?
            .remove(&id);
        Ok(())
    }

    pub async fn join(&self, invitation: &Invitation, name: &str) -> Result<Roster> {
        let reply = request(
            self.router.endpoint(),
            self.mode.address(invitation.owner.clone()),
            &Request::Join {
                cabal: invitation.cabal,
                token: invitation.token.clone(),
                name: name.into(),
                address: self.address(),
            },
        )
        .await?;
        match reply {
            Response::Joined { roster } => {
                roster.verify()?;
                if roster.signer != invitation.owner.id
                    || roster.payload.owner != invitation.owner.id
                    || roster.payload.cabal != invitation.cabal
                    || !roster
                        .payload
                        .members
                        .iter()
                        .any(|member| member.key == self.address().id)
                {
                    return Err(Error::Invalid("Invitation did not admit this device"));
                }
                Ok(roster)
            }
            _ => Err(Error::Invalid("The cabal could not accept this invitation")),
        }
    }

    pub async fn sync_now(&self, cabal: SharedCabal, peer: PublicKey) -> Result<bool> {
        synchronize(self.router.endpoint(), &self.mode, cabal, peer).await
    }

    /// Configures a local executor. No peer gains access until the host records
    /// an explicit grant. Authority follows current signed cabal membership.
    pub fn host_compute(
        &self,
        directory: &std::path::Path,
        executor: Arc<dyn ComputeExecutor>,
    ) -> Result<Arc<ComputeHost>> {
        if self.stop.is_cancelled() {
            return Err(Error::Invalid("Cabals are closing"));
        }
        let cabals = self.cabals.clone();
        let me = self.address().id;
        self.compute.configure(|| {
            ComputeHost::open(
                directory,
                self.identity.clone(),
                Arc::new(move |grant| {
                    let cabal = cabals
                        .lock()
                        .ok()
                        .and_then(|cabals| cabals.get(&grant.cabal).cloned());
                    cabal
                        .and_then(|cabal| {
                            cabal.lock().ok().map(|cabal| {
                                cabal.roster().payload.epoch == grant.epoch
                                    && cabal.is_member(me)
                                    && cabal.is_member(grant.peer)
                            })
                        })
                        .unwrap_or(false)
                }),
                executor,
            )
        })
    }

    pub async fn compute_offers(&self, host: EndpointAddr, cabal: Uuid) -> Result<ComputeReply> {
        self.request_compute(host, compute::Request::Offers { cabal })
            .await
    }

    pub async fn compute_submit(
        &self,
        host: EndpointAddr,
        job: Uuid,
        grant: Uuid,
        input: ComputeInput,
    ) -> Result<ComputeReply> {
        self.request_compute(host, compute::Request::Submit { job, grant, input })
            .await
    }

    pub async fn compute_status(&self, host: EndpointAddr, job: Uuid) -> Result<ComputeReply> {
        self.request_compute(host, compute::Request::Status { job })
            .await
    }

    pub async fn compute_cancel(
        &self,
        host: EndpointAddr,
        job: Uuid,
        grant: Uuid,
        input: ComputeInput,
    ) -> Result<ComputeReply> {
        self.request_compute(host, compute::Request::Cancel { job, grant, input })
            .await
    }

    async fn request_compute(
        &self,
        host: EndpointAddr,
        request: compute::Request,
    ) -> Result<ComputeReply> {
        if self.stop.is_cancelled() {
            return Err(Error::Invalid("Cabals are closing"));
        }
        let _slot = self
            .compute_outbound
            .try_acquire()
            .map_err(|_| Error::Invalid("Compute connections are busy"))?;
        compute::request(self.router.endpoint(), self.mode.address(host), request).await
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.stop.cancel();
        self.compute.stop();
        let mut slot = self.worker.lock().await;
        let worker = if let Some(worker) = slot.take() {
            worker.await.map_err(network_error)
        } else {
            Ok(())
        };
        let compute = self.compute.shutdown().await;
        let network = self.router.shutdown().await.map_err(network_error);
        worker.and(compute).and(network)
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        self.stop.cancel();
        self.compute.stop();
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Probe {
        cabal: Uuid,
        fingerprint: String,
        address: EndpointAddr,
    },
    Join {
        cabal: Uuid,
        token: String,
        name: String,
        address: EndpointAddr,
    },
    Asset {
        cabal: Uuid,
        roster: Roster,
        sha256: String,
        offset: u64,
    },
    Sync {
        cabal: Uuid,
        roster: Roster,
        known: BTreeSet<String>,
        address: EndpointAddr,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Response {
    Unchanged,
    Changed,
    Joined {
        roster: Roster,
    },
    Asset {
        sha256: String,
        offset: u64,
        bytes: String,
    },
    Sync {
        roster: Roster,
        changes: Vec<ChangeEnvelope>,
        assets: Vec<AssetDescriptor>,
    },
    Revoked {
        roster: Roster,
    },
    Rejected,
}

#[derive(Debug)]
struct Handler {
    cabals: Cabals,
    slots: Arc<Semaphore>,
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
                .read_to_end(MAX_FRAME_BYTES)
                .await
                .map_err(network_error)?;
            let incoming: Request = serde_json::from_slice(&bytes)?;
            let response = self
                .respond(connection.remote_id(), incoming)
                .unwrap_or(Response::Rejected);
            let bytes = encode(&response)?;
            send.write_all(&bytes).await.map_err(network_error)?;
            send.finish().map_err(network_error)?;
            connection.closed().await;
            Ok::<(), Error>(())
        };
        let _ = tokio::time::timeout(Duration::from_secs(12), handle).await;
        connection.close(0u32.into(), b"complete");
        Ok(())
    }
}

impl Handler {
    fn respond(&self, peer: PublicKey, request: Request) -> Result<Response> {
        let id = match &request {
            Request::Probe { cabal, .. }
            | Request::Asset { cabal, .. }
            | Request::Join { cabal, .. }
            | Request::Sync { cabal, .. } => *cabal,
        };
        let cabal = self
            .cabals
            .lock()
            .map_err(|_| Error::Invalid("Cabal registry stopped"))?
            .get(&id)
            .cloned()
            .ok_or(Error::Invalid("Unknown cabal"))?;
        let mut cabal = cabal
            .lock()
            .map_err(|_| Error::Invalid("Cabal owner stopped"))?;
        match request {
            Request::Asset {
                roster,
                sha256,
                offset,
                ..
            } => {
                cabal.accept_roster(roster)?;
                let bytes = cabal.asset_chunk_for(peer, &sha256, offset)?;
                Ok(Response::Asset {
                    sha256,
                    offset,
                    bytes: STANDARD.encode(bytes),
                })
            }
            Request::Probe {
                fingerprint,
                address,
                ..
            } => {
                if address.id != peer || !cabal.is_member(peer) {
                    return Ok(Response::Changed);
                }
                cabal.remember_peer(&address)?;
                Ok(if cabal.fingerprint()? == fingerprint {
                    Response::Unchanged
                } else {
                    Response::Changed
                })
            }
            Request::Join {
                token,
                name,
                address,
                ..
            } => {
                if address.id != peer {
                    return Err(Error::Invalid("Peer address identity mismatch"));
                }
                let roster = cabal.admit(&token, peer, &name)?;
                cabal.remember_peer(&address)?;
                Ok(Response::Joined { roster })
            }
            Request::Sync {
                roster,
                known,
                address,
                ..
            } => {
                if address.id != peer {
                    return Err(Error::Invalid("Peer address identity mismatch"));
                }
                roster.verify()?;
                let was_member = roster.payload.owner == cabal.roster().payload.owner
                    && roster.payload.cabal == cabal.id()
                    && roster
                        .payload
                        .members
                        .iter()
                        .any(|member| member.key == peer);
                if !was_member && !cabal.is_member(peer) {
                    return Err(Error::Invalid("Unknown cabal peer"));
                }
                cabal.accept_roster(roster)?;
                if !cabal.is_member(peer) {
                    return Ok(Response::Revoked {
                        roster: cabal.roster().clone(),
                    });
                }
                if !cabal.is_member(cabal.identity().public_key()) {
                    return Ok(Response::Rejected);
                }
                cabal.remember_peer(&address)?;
                Ok(Response::Sync {
                    roster: cabal.roster().clone(),
                    changes: cabal.missing(&known)?,
                    assets: cabal.assets()?,
                })
            }
        }
    }
}

async fn request(endpoint: &Endpoint, address: EndpointAddr, value: &Request) -> Result<Response> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let connection = endpoint
            .connect(address, ALPN)
            .await
            .map_err(network_error)?;
        let result = async {
            let (mut send, mut receive) = connection.open_bi().await.map_err(network_error)?;
            send.write_all(&encode(value)?)
                .await
                .map_err(network_error)?;
            send.finish().map_err(network_error)?;
            let bytes = receive
                .read_to_end(MAX_FRAME_BYTES)
                .await
                .map_err(network_error)?;
            decode(&bytes)
        }
        .await;
        connection.close(0u32.into(), b"complete");
        result
    })
    .await
    .map_err(|_| Error::Invalid("Cabal peer did not respond in time"))?
}

async fn synchronize(
    endpoint: &Endpoint,
    mode: &NetworkMode,
    cabal: SharedCabal,
    peer: PublicKey,
) -> Result<bool> {
    let (probe_address, probe) = {
        let cabal = cabal
            .lock()
            .map_err(|_| Error::Invalid("Cabal owner stopped"))?;
        if !cabal.is_member(peer) || !cabal.is_member(endpoint.id()) {
            return Err(Error::Invalid("Device is outside current cabal membership"));
        }
        (
            mode.address(cabal.peer_address(peer)?),
            Request::Probe {
                cabal: cabal.id(),
                fingerprint: cabal.fingerprint()?,
                address: mode.address(endpoint.addr()),
            },
        )
    };
    if matches!(
        request(endpoint, probe_address, &probe).await?,
        Response::Unchanged
    ) {
        return Ok(false);
    }
    let (address, request_value) = {
        let cabal = cabal
            .lock()
            .map_err(|_| Error::Invalid("Cabal owner stopped"))?;
        if !cabal.is_member(peer) || !cabal.is_member(endpoint.id()) {
            return Err(Error::Invalid("Device is outside current cabal membership"));
        }
        (
            mode.address(cabal.peer_address(peer)?),
            Request::Sync {
                cabal: cabal.id(),
                roster: cabal.roster().clone(),
                known: cabal.hashes()?,
                address: mode.address(endpoint.addr()),
            },
        )
    };
    let response = request(endpoint, address, &request_value).await?;
    let (changed, assets) = {
        let mut cabal = cabal
            .lock()
            .map_err(|_| Error::Invalid("Cabal owner stopped"))?;
        match response {
            Response::Sync {
                roster,
                changes,
                assets,
            } => {
                let changed = cabal.accept_roster(roster)?;
                (cabal.apply(changes)? || changed, assets)
            }
            Response::Revoked { roster } => return cabal.accept_roster(roster),
            _ => return Err(Error::Invalid("Cabal peer rejected synchronization")),
        }
    };
    Ok(synchronize_assets(endpoint, mode, &cabal, peer, assets).await? || changed)
}

/// A pass transfers at most four MiB, yielding to document synchronization and
/// other peers. Completed hashes converge with the normal probe fingerprint;
/// partial prefixes remain in SQLite across disconnects and process restart.
async fn synchronize_assets(
    endpoint: &Endpoint,
    mode: &NetworkMode,
    cabal: &SharedCabal,
    peer: PublicKey,
    assets: Vec<AssetDescriptor>,
) -> Result<bool> {
    if assets.len() > 256 {
        return Err(Error::Invalid(
            "Shared attachment catalog exceeds its limit",
        ));
    }
    let mut changed = false;
    let mut chunks = 0;
    for descriptor in assets {
        loop {
            let (address, value, offset) = {
                let mut cabal = cabal
                    .lock()
                    .map_err(|_| Error::Invalid("Cabal owner stopped"))?;
                if !cabal.is_member(peer) {
                    return Err(Error::Invalid("Attachment provider left the cabal"));
                }
                let Some(offset) = cabal.begin_asset(&descriptor)? else {
                    break;
                };
                (
                    mode.address(cabal.peer_address(peer)?),
                    Request::Asset {
                        cabal: cabal.id(),
                        roster: cabal.roster().clone(),
                        sha256: descriptor.sha256.clone(),
                        offset,
                    },
                    offset,
                )
            };
            let Response::Asset {
                sha256,
                offset: actual_offset,
                bytes,
            } = request(endpoint, address, &value).await?
            else {
                return Err(Error::Invalid(
                    "Shared attachment is unavailable from this peer",
                ));
            };
            if sha256 != descriptor.sha256
                || actual_offset != offset
                || bytes.len() > ASSET_CHUNK_BYTES * 4 / 3 + 4
            {
                return Err(Error::Invalid("Unrelated shared attachment chunk"));
            }
            let bytes = STANDARD
                .decode(bytes)
                .map_err(|_| Error::Invalid("Invalid attachment encoding"))?;
            let mut cabal = cabal
                .lock()
                .map_err(|_| Error::Invalid("Cabal owner stopped"))?;
            if !cabal.is_member(peer) {
                return Err(Error::Invalid("Attachment provider left the cabal"));
            }
            changed |= cabal.accept_asset_chunk(&descriptor, offset, &bytes)?;
            chunks += 1;
            if chunks >= 4 {
                return Ok(changed);
            }
        }
    }
    Ok(changed)
}

async fn supervise(
    endpoint: Endpoint,
    mode: NetworkMode,
    cabals: Cabals,
    status: watch::Sender<Vec<PeerStatus>>,
    stop: CancellationToken,
) {
    let mut retry: BTreeMap<(Uuid, PublicKey), (u32, tokio::time::Instant)> = BTreeMap::new();
    loop {
        let snapshot = match cabals.lock() {
            Ok(values) => values.values().cloned().collect::<Vec<_>>(),
            Err(_) => break,
        };
        let mut jobs = VecDeque::new();
        let mut active = BTreeSet::new();
        for cabal in snapshot {
            let Ok(value) = cabal.lock() else {
                continue;
            };
            if !value.is_member(endpoint.id()) {
                continue;
            }
            for member in &value.roster().payload.members {
                if member.key == endpoint.id() {
                    continue;
                }
                let pair = (value.id(), member.key);
                active.insert(pair);
                if retry
                    .get(&pair)
                    .is_none_or(|(_, deadline)| *deadline <= tokio::time::Instant::now())
                {
                    jobs.push_back((value.id(), member.key, cabal.clone()));
                }
            }
        }
        retry.retain(|pair, _| active.contains(pair));
        let mut outcomes: Vec<PeerStatus> = status
            .borrow()
            .iter()
            .filter(|item| active.contains(&(item.cabal, item.key)))
            .cloned()
            .collect();
        let mut running = JoinSet::new();
        while !jobs.is_empty() || !running.is_empty() {
            while running.len() < 4 {
                let Some((id, peer, cabal)) = jobs.pop_front() else {
                    break;
                };
                let endpoint = endpoint.clone();
                let mode = mode.clone();
                running.spawn(async move {
                    (id, peer, synchronize(&endpoint, &mode, cabal, peer).await)
                });
            }
            let completed = tokio::select! {
                () = stop.cancelled() => return,
                value = running.join_next() => value,
            };
            if let Some(Ok((id, key, result))) = completed {
                let attempts = if result.is_ok() {
                    0
                } else {
                    retry
                        .get(&(id, key))
                        .map_or(1, |(attempt, _)| attempt.saturating_add(1).min(5))
                };
                retry.insert(
                    (id, key),
                    (
                        attempts,
                        tokio::time::Instant::now() + Duration::from_secs(1 << attempts),
                    ),
                );
                outcomes.retain(|item| item.cabal != id || item.key != key);
                outcomes.push(PeerStatus {
                    cabal: id,
                    key,
                    connected: result.is_ok(),
                    changed: result.unwrap_or(false),
                });
                status.send_replace(outcomes.clone());
            }
        }
        tokio::select! { () = stop.cancelled() => break, () = tokio::time::sleep(Duration::from_millis(250)) => () }
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::Invalid("Cabal frame exceeds limit"));
    }
    Ok(bytes)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(bytes)?)
}
fn network_error(error: impl std::fmt::Display) -> Error {
    Error::Network(error.to_string())
}

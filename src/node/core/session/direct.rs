//! Signal-free transport admission. All paths finish through the normal node auth gate.
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn dial(
    network: &DesktopNetwork,
    addresses: Vec<SocketAddr>,
    peer: NodeId,
    generation: u64,
    local: NodeId,
    identity: Identity,
    inputs: mpsc::Sender<SessionInput>,
    auth: AuthContext,
    verifier: Option<RemoteVerifier>,
    capabilities: u64,
    password: Option<SecretPassword>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) -> impl Future<Output = ()> + Send + 'static {
    let choices: Vec<_> = addresses
        .into_iter()
        .filter_map(|address| {
            network
                .path(AddressFamily::of(address))
                .map(|path| (path.endpoint.clone(), address))
        })
        .collect();
    async move {
        let _permit = _permit;
        let result = time::timeout(Duration::from_secs(20), async {
            let _ = inputs
                .send(SessionInput::PeerProgress {
                    peer,
                    generation,
                    state: PeerLifecycle::Authenticating,
                })
                .await;
            let mut candidates = JoinSet::new();
            for (index, (endpoint, address)) in choices.into_iter().enumerate() {
                let identity = identity.clone();
                candidates.spawn(async move {
                    time::sleep(crate::net::race::CANDIDATE_STAGGER * index as u32).await;
                    time::timeout(
                        crate::net::race::PATH_TIMEOUT,
                        PreparedTransport::dial(&endpoint, address, &identity, peer),
                    )
                    .await
                    .map_err(|_| Error::Transport("IP直连身份握手超时".into()))?
                });
            }
            let guard = crate::net::race::finish_prepared_guard(candidates)
                .await
                .map_err(|e| e.to_string())?;
            finish_peer_transport(
                peer,
                generation,
                local,
                guard,
                None,
                inputs.clone(),
                auth,
                verifier,
                AttemptOptions { capabilities },
                password,
            )
            .await
        })
        .await
        .map_err(|_| "IP直连认证超时".to_owned())
        .and_then(|r| r);
        report(result, inputs, peer, generation).await;
    }
}

pub(super) async fn identify(
    incoming: quinn::Incoming,
    identity: Identity,
    inputs: mpsc::Sender<SessionInput>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    // Remote address / discovery TXT records do not establish an identity. Only the
    // existing signed TLS-exporter handshake is allowed to produce a peer ID.
    if let Ok(Ok((peer, prepared))) = time::timeout(
        crate::net::race::PATH_TIMEOUT,
        PreparedTransport::accept_identified(incoming, &identity),
    )
    .await
    {
        let _ = inputs
            .send(SessionInput::DirectIdentified {
                peer,
                prepared,
                permit,
            })
            .await;
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn accept(
    prepared: PreparedTransport,
    peer: NodeId,
    generation: u64,
    local: NodeId,
    inputs: mpsc::Sender<SessionInput>,
    auth: AuthContext,
    verifier: Option<RemoteVerifier>,
    capabilities: u64,
    password: Option<SecretPassword>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let result = time::timeout(Duration::from_secs(20), async {
        let guard = prepared
            .wait_for_selection()
            .await
            .map_err(|e| e.to_string())?;
        finish_peer_transport(
            peer,
            generation,
            local,
            guard,
            None,
            inputs.clone(),
            auth,
            verifier,
            AttemptOptions { capabilities },
            password,
        )
        .await
    })
    .await
    .map_err(|_| "入站直连授权超时".to_owned())
    .and_then(|r| r);
    report(result, inputs, peer, generation).await;
}
async fn report(
    result: std::result::Result<(), String>,
    inputs: mpsc::Sender<SessionInput>,
    peer: NodeId,
    generation: u64,
) {
    if let Err(detail) = result {
        let _ = inputs
            .send(SessionInput::PeerFailed {
                peer,
                generation,
                detail,
            })
            .await;
    }
}

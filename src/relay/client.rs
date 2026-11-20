//! A dedicated socket per peer attempt: admission first, then the same socket to Quinn.
use crate::identity::{Identity, NodeId};
use crate::nat::punch::PunchToken;
use crate::net::{AddressFamily, NetworkFamilies};
use crate::{Error, Result};
use std::net::SocketAddr;
use std::time::Duration;
use veytrane_transport::connection::{ConnectionGuard, PreparedTransport};
use veytrane_transport::quic::relay::{Progress, prepare_with_progress};

pub use veytrane_transport::connection::EndpointPool;
pub use veytrane_transport::quic::relay::{bind, prepare};

#[derive(Clone)]
pub struct Fallback {
    pub server: String,
    pub families: NetworkFamilies,
    pub endpoints: EndpointPool,
    pub(crate) progress: Option<Progress>,
}
impl Fallback {
    pub fn new(server: String, families: NetworkFamilies) -> Self {
        Self {
            server,
            families,
            endpoints: EndpointPool::default(),
            progress: None,
        }
    }
    pub async fn prepare(
        &self,
        identity: Identity,
        peer: NodeId,
        token: PunchToken,
    ) -> Result<PreparedTransport> {
        tokio::time::sleep(super::RELAY_FALLBACK_DELAY).await;
        tracing::info!(relay = %self.server, "直连尚无 authenticated winner，正在尝试 Relay");
        if let Some(progress) = &self.progress {
            progress("IPv6 / IPv4 直连未完成，正在尝试 Relay");
        }
        prepare_any_with_progress(
            identity,
            peer,
            token,
            self.server.clone(),
            self.families,
            self.progress.clone(),
        )
        .await
    }
    pub async fn wait(
        &self,
        identity: Identity,
        peer: NodeId,
        token: PunchToken,
    ) -> Result<ConnectionGuard> {
        tokio::time::sleep(super::RELAY_FALLBACK_DELAY).await;
        tracing::info!(relay = %self.server, "直连尚无 authenticated winner，正在尝试 Relay");
        if let Some(progress) = &self.progress {
            progress("IPv6 / IPv4 直连未完成，正在尝试 Relay");
        }
        await_peer_selection_with_progress(
            identity,
            peer,
            token,
            self.server.clone(),
            self.families,
            self.progress.clone(),
        )
        .await
    }
}
/// Resolving is deliberately called only inside the delayed fallback task.
pub async fn resolve(spec: &str, families: NetworkFamilies) -> Result<Vec<SocketAddr>> {
    let addresses = tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host(spec))
        .await
        .map_err(|_| Error::Discovery("Relay DNS timeout".into()))??;
    let addresses: Vec<_> = addresses.take(32).collect();
    let mut result = Vec::new();
    for family in [AddressFamily::Ipv6, AddressFamily::Ipv4] {
        for address in addresses
            .iter()
            .copied()
            .filter(|address| family.accepts(*address) && families.enabled(family))
            .take(2)
        {
            if !result.contains(&address) {
                result.push(address);
            }
        }
    }
    if result.is_empty() {
        return Err(Error::Discovery(
            "Relay has no usable native address family".into(),
        ));
    }
    Ok(result)
}

/// Each family/target is bounded and independently owns a UDP socket.
pub async fn prepare_any(
    identity: Identity,
    peer: NodeId,
    token: PunchToken,
    spec: String,
    families: NetworkFamilies,
) -> Result<PreparedTransport> {
    prepare_any_with_progress(identity, peer, token, spec, families, None).await
}
async fn prepare_any_with_progress(
    identity: Identity,
    peer: NodeId,
    token: PunchToken,
    spec: String,
    families: NetworkFamilies,
    progress: Option<Progress>,
) -> Result<PreparedTransport> {
    let addresses = resolve(&spec, families).await?;
    let mut tasks = tokio::task::JoinSet::new();
    let has_v6 = addresses.iter().any(SocketAddr::is_ipv6);
    for (index, server) in addresses.into_iter().enumerate() {
        let identity = identity.clone();
        let progress = progress.clone();
        tasks.spawn(async move {
            let delay = if has_v6 && server.is_ipv4() {
                crate::net::race::FAMILY_STAGGER
            } else {
                Duration::ZERO
            } + crate::net::race::CANDIDATE_STAGGER * index as u32;
            tokio::time::sleep(delay).await;
            prepare_with_progress(&identity, peer, token, server, progress).await
        });
    }
    crate::net::race::select_prepared(tasks).await
}

pub async fn await_peer_selection(
    identity: Identity,
    peer: NodeId,
    token: PunchToken,
    spec: String,
    families: NetworkFamilies,
) -> Result<ConnectionGuard> {
    await_peer_selection_with_progress(identity, peer, token, spec, families, None).await
}
async fn await_peer_selection_with_progress(
    identity: Identity,
    peer: NodeId,
    token: PunchToken,
    spec: String,
    families: NetworkFamilies,
    progress: Option<Progress>,
) -> Result<ConnectionGuard> {
    // Keep all prepared candidates participating until the peer's FIN selects
    // one; choosing the first RelayReady or local handshake would split winners.
    let addresses = resolve(&spec, families).await?;
    let mut tasks = tokio::task::JoinSet::new();
    let has_v6 = addresses.iter().any(SocketAddr::is_ipv6);
    for (index, server) in addresses.into_iter().enumerate() {
        let identity = identity.clone();
        let progress = progress.clone();
        tasks.spawn(async move {
            let delay = if has_v6 && server.is_ipv4() {
                crate::net::race::FAMILY_STAGGER
            } else {
                Duration::ZERO
            } + crate::net::race::CANDIDATE_STAGGER * index as u32;
            tokio::time::sleep(delay).await;
            prepare_with_progress(&identity, peer, token, server, progress)
                .await?
                .wait_for_selection()
                .await
        });
    }
    let mut failures = Vec::new();
    let result = loop {
        match tasks.join_next().await {
            Some(Ok(Ok(winner))) => break Ok(winner),
            Some(Ok(Err(error))) => failures.push(error.to_string()),
            Some(Err(error)) => failures.push(error.to_string()),
            None => {
                break Err(Error::Transport(format!(
                    "no selected Relay transport: {}",
                    failures.join("; ")
                )));
            }
        }
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    result
}

/// Validate local configuration without DNS or network traffic.
pub fn validate_server_spec(spec: &str) -> Result<()> {
    if spec.len() > 300 || spec.trim() != spec || spec.chars().any(char::is_whitespace) {
        return Err(Error::Discovery(
            "Relay must be HOST:PORT or [IPv6]:PORT".into(),
        ));
    }
    if let Ok(address) = spec.parse::<SocketAddr>() {
        if crate::net::family::usable_address(address) {
            return Ok(());
        }
        return Err(Error::Discovery(
            "Relay address must be native, usable and have a nonzero port".into(),
        ));
    }
    let (host, port) = spec
        .rsplit_once(':')
        .ok_or_else(|| Error::Discovery("Relay requires HOST:PORT".into()))?;
    if port.parse::<u16>().ok().is_none_or(|port| port == 0)
        || host.is_empty()
        || host.len() > 253
        || host.contains(':')
    {
        return Err(Error::Discovery("invalid Relay host or port".into()));
    }
    for label in host.strip_suffix('.').unwrap_or(host).split('.') {
        if label.is_empty()
            || label.len() > 63
            || !label.as_bytes()[0].is_ascii_alphanumeric()
            || !label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            || !label
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(Error::Discovery("invalid Relay hostname".into()));
        }
    }
    if host.bytes().all(|c| c.is_ascii_digit() || c == b'.') {
        return Err(Error::Discovery("invalid Relay IPv4 address".into()));
    }
    Ok(())
}

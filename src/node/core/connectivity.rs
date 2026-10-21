//! Optional signal-free addressing. Discovery is an untrusted hint, never an ACL.
use crate::{
    error::{Error, Result},
    identity::NodeId,
    net::DesktopNetwork,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    time::{Duration, Instant},
};

pub const SERVICE_TYPE: &str = "_veytrane._udp.local.";
pub const MAX_NEARBY: usize = 64;
pub const MAX_ADDRESSES: usize = 8;
const MAX_AGE: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DirectSettings {
    pub enabled: bool,
    pub lan_discovery: bool,
    /// Zero selects OS-assigned ports. A fixed port is useful for public ingress.
    pub listen_port: u16,
    /// Explicitly saved addresses, keyed by the expected full NodeId.
    pub peers: BTreeMap<String, Vec<SocketAddr>>,
}
impl Default for DirectSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            lan_discovery: true,
            listen_port: 0,
            peers: BTreeMap::new(),
        }
    }
}
impl DirectSettings {
    pub fn validate(&self) -> Result<()> {
        if self.peers.len() > 128 {
            return Err(invalid("直连设备数量超限"));
        }
        for (peer, addresses) in &self.peers {
            if NodeId::from_hex(peer)?.to_hex() != *peer {
                return Err(invalid("直连设备必须使用规范完整NodeId"));
            }
            validate_addresses(addresses)?;
        }
        Ok(())
    }
}
pub fn validate_addresses(addresses: &[SocketAddr]) -> Result<()> {
    if addresses.is_empty() || addresses.len() > MAX_ADDRESSES {
        return Err(invalid("直连地址须为1..8个IP:端口"));
    }
    for address in addresses {
        if !crate::net::family::usable_address(*address) {
            return Err(invalid(
                "直连地址不能是未指定、组播、IPv6链路本地或IPv4映射IPv6地址，端口不能为0",
            ));
        }
    }
    Ok(())
}
fn invalid(s: &str) -> Error {
    Error::Protocol(s.into())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NearbyPeer {
    pub node_id: String,
    pub addresses: Vec<SocketAddr>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectivitySnapshot {
    pub enabled: bool,
    pub discovery: String,
    pub listen: Vec<SocketAddr>,
    pub nearby: Vec<NearbyPeer>,
}

pub(crate) struct NearbyCache {
    local: NodeId,
    records: BTreeMap<String, (NearbyPeer, Instant)>,
}
impl NearbyCache {
    pub fn new(local: NodeId) -> Self {
        Self {
            local,
            records: BTreeMap::new(),
        }
    }
    pub fn observe(&mut self, event: mdns_sd::ServiceEvent, now: Instant) {
        match event {
            mdns_sd::ServiceEvent::ServiceResolved(service) => {
                if let Some(peer) = parse(&service, self.local) {
                    if self.records.contains_key(&service.fullname)
                        || self.records.len() < MAX_NEARBY
                    {
                        self.records.insert(service.fullname.clone(), (peer, now));
                    }
                } else {
                    self.records.remove(&service.fullname);
                }
            }
            mdns_sd::ServiceEvent::ServiceRemoved(_, name) => {
                self.records.remove(&name);
            }
            _ => {}
        }
        self.expire(now);
    }
    pub fn expire(&mut self, now: Instant) {
        self.records
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < MAX_AGE);
    }
    pub fn peers(&self) -> Vec<NearbyPeer> {
        let mut peers: BTreeMap<String, Vec<SocketAddr>> = BTreeMap::new();
        for (peer, _) in self.records.values() {
            let addresses = peers.entry(peer.node_id.clone()).or_default();
            for address in &peer.addresses {
                if addresses.len() < MAX_ADDRESSES && !addresses.contains(address) {
                    addresses.push(*address);
                }
            }
        }
        peers
            .into_iter()
            .map(|(node_id, addresses)| NearbyPeer { node_id, addresses })
            .collect()
    }
    pub fn addresses(&self, peer: NodeId) -> Vec<SocketAddr> {
        self.peers()
            .into_iter()
            .find(|p| p.node_id == peer.to_hex())
            .map_or_else(Vec::new, |p| p.addresses)
    }
}
fn parse(service: &mdns_sd::ResolvedService, local: NodeId) -> Option<NearbyPeer> {
    if !service.fullname.ends_with(SERVICE_TYPE)
        || service.txt_properties.get_property_val_str("wire") != Some("2")
    {
        return None;
    }
    let id = service.txt_properties.get_property_val_str("id")?;
    let peer = NodeId::from_hex(id).ok()?;
    if peer == local {
        return None;
    }
    let mut addresses = Vec::new();
    for ip in &service.addresses {
        let ip = ip.to_ip_addr();
        let key = if ip.is_ipv4() { "v4" } else { "v6" };
        let Some(port) = service
            .txt_properties
            .get_property_val_str(key)
            .and_then(|p| p.parse::<u16>().ok())
        else {
            continue;
        };
        let address = SocketAddr::new(ip, port);
        if validate_addresses(&[address]).is_ok() && !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    addresses.sort_by_key(|a| (!a.is_ipv6(), *a));
    addresses.truncate(MAX_ADDRESSES);
    (!addresses.is_empty()).then(|| NearbyPeer {
        node_id: peer.to_hex(),
        addresses,
    })
}

pub(crate) struct Discovery {
    daemon: mdns_sd::ServiceDaemon,
    pub receiver: mdns_sd::Receiver<mdns_sd::ServiceEvent>,
}
impl Discovery {
    pub fn start(local: NodeId, network: &DesktopNetwork) -> Result<Self> {
        let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| Error::Discovery(e.to_string()))?;
        // Clean up even if registration or browsing fails partway through startup.
        let result = (|| {
            let mut properties = std::collections::HashMap::new();
            properties.insert("id".to_owned(), local.to_hex());
            properties.insert("wire".to_owned(), "2".to_owned());
            for path in &network.paths {
                properties.insert(
                    if path.local_addr.is_ipv4() {
                        "v4"
                    } else {
                        "v6"
                    }
                    .to_owned(),
                    path.local_addr.port().to_string(),
                );
            }
            let port = network
                .paths
                .first()
                .ok_or_else(|| invalid("没有可公告的直连端点"))?
                .local_addr
                .port();
            let name = format!("veytrane-{}", local.to_hex());
            let addresses: Vec<_> = network
                .local_candidates
                .iter()
                .filter(|c| c.kind == crate::discovery::signal::CandidateKind::Host)
                .map(|c| c.addr.ip())
                .collect();
            let info = mdns_sd::ServiceInfo::new(
                SERVICE_TYPE,
                &name,
                &format!("{name}.local."),
                addresses.as_slice(),
                port,
                properties,
            )
            .map_err(|e| Error::Discovery(e.to_string()))?
            .enable_addr_auto();
            daemon
                .register(info)
                .map_err(|e| Error::Discovery(e.to_string()))?;
            let receiver = daemon
                .browse(SERVICE_TYPE)
                .map_err(|e| Error::Discovery(e.to_string()))?;
            Ok(receiver)
        })();
        match result {
            Ok(receiver) => Ok(Self { daemon, receiver }),
            Err(e) => {
                let _ = daemon.shutdown();
                Err(e)
            }
        }
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

pub(crate) fn snapshot(
    network: &DesktopNetwork,
    enabled: bool,
    discovery: String,
    cache: &NearbyCache,
) -> ConnectivitySnapshot {
    let mut listen: Vec<_> = network.local_candidates.iter().map(|c| c.addr).collect();
    for path in &network.paths {
        listen.push(SocketAddr::new(
            path.family.loopback(0).ip(),
            path.local_addr.port(),
        ));
    }
    listen.sort();
    listen.dedup();
    ConnectivitySnapshot {
        enabled,
        discovery,
        listen,
        nearby: cache.peers(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn announcement(id: NodeId, wire: &str, name: &str) -> mdns_sd::ResolvedService {
        let props = std::collections::HashMap::from([
            ("id".to_owned(), id.to_hex()),
            ("wire".into(), wire.into()),
            ("v4".into(), "9001".into()),
            ("v6".into(), "9002".into()),
        ]);
        mdns_sd::ServiceInfo::new(
            SERVICE_TYPE,
            name,
            &format!("{name}.local."),
            "127.0.0.1,2001:db8::1,fe80::1",
            9001,
            props,
        )
        .unwrap()
        .as_resolved_service()
    }
    #[test]
    fn only_node_v2_announcements_produce_family_correct_untrusted_hints() {
        let local = crate::identity::Identity::generate().node_id();
        let peer = crate::identity::Identity::generate().node_id();
        let valid = announcement(peer, "2", "valid");
        let parsed = parse(&valid, local).unwrap();
        assert_eq!(
            parsed.addresses,
            vec![
                "[2001:db8::1]:9002".parse().unwrap(),
                "127.0.0.1:9001".parse().unwrap()
            ]
        );
        assert!(parse(&announcement(peer, "1", "old"), local).is_none());
        assert!(parse(&announcement(local, "2", "self"), local).is_none());
        let mut wrong_namespace = valid;
        wrong_namespace.fullname = "old._p2pfile._udp.local.".into();
        assert!(parse(&wrong_namespace, local).is_none());
    }
    #[test]
    fn discovery_expiry_and_goodbye_remove_stale_addresses() {
        let local = crate::identity::Identity::generate().node_id();
        let peer = crate::identity::Identity::generate().node_id();
        let mut cache = NearbyCache::new(local);
        let now = Instant::now();
        let service = announcement(peer, "2", "fixture");
        let name = service.fullname.clone();
        cache.observe(
            mdns_sd::ServiceEvent::ServiceResolved(Box::new(service)),
            now,
        );
        assert_eq!(cache.peers().len(), 1);
        cache.observe(
            mdns_sd::ServiceEvent::ServiceRemoved(SERVICE_TYPE.into(), name),
            now,
        );
        assert!(cache.peers().is_empty());
        cache.observe(
            mdns_sd::ServiceEvent::ServiceResolved(Box::new(announcement(peer, "2", "fixture"))),
            now,
        );
        cache.expire(now + MAX_AGE);
        assert!(cache.peers().is_empty());
    }
    #[test]
    fn advertisements_are_bounded_and_cannot_smuggle_unsafe_direct_addresses() {
        let local = crate::identity::Identity::generate().node_id();
        let mut cache = NearbyCache::new(local);
        for n in 0..(MAX_NEARBY + 4) {
            let peer = crate::identity::Identity::generate().node_id();
            cache.observe(
                mdns_sd::ServiceEvent::ServiceResolved(Box::new(announcement(
                    peer,
                    "2",
                    &format!("p{n}"),
                ))),
                Instant::now(),
            );
        }
        assert_eq!(cache.peers().len(), MAX_NEARBY);
        for a in [
            "0.0.0.0:9000",
            "224.0.0.1:9000",
            "[ff02::1]:9000",
            "[fe80::1]:9000",
            "[::ffff:127.0.0.1]:9000",
            "127.0.0.1:0",
        ] {
            assert!(validate_addresses(&[a.parse().unwrap()]).is_err(), "{a}");
        }
        assert!(
            validate_addresses(&vec!["127.0.0.1:9".parse().unwrap(); MAX_ADDRESSES + 1]).is_err()
        );
    }
}

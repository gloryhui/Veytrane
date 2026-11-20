//! Native address-family UDP binding.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum AddressFamily {
    Ipv6,
    Ipv4,
}

impl AddressFamily {
    pub fn of(addr: SocketAddr) -> Self {
        if addr.is_ipv4() {
            Self::Ipv4
        } else {
            Self::Ipv6
        }
    }
    pub fn wildcard(self, port: u16) -> SocketAddr {
        SocketAddr::new(
            match self {
                Self::Ipv4 => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                Self::Ipv6 => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            },
            port,
        )
    }
    pub fn loopback(self, port: u16) -> SocketAddr {
        SocketAddr::new(
            match self {
                Self::Ipv4 => IpAddr::V4(Ipv4Addr::LOCALHOST),
                Self::Ipv6 => IpAddr::V6(Ipv6Addr::LOCALHOST),
            },
            port,
        )
    }
    pub fn accepts(self, addr: SocketAddr) -> bool {
        Self::of(addr) == self && usable_address(addr)
    }
}

impl std::fmt::Display for AddressFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ipv4 => "IPv4",
            Self::Ipv6 => "IPv6",
        })
    }
}

/// No scoped link-local, mapped, unspecified, multicast or zero-port candidates.
/// ULA and global IPv6 remain ordinary Host candidates, independently of STUN.
pub fn usable_address(addr: SocketAddr) -> bool {
    if addr.port() == 0 || addr.ip().is_unspecified() || addr.ip().is_multicast() {
        return false;
    }
    match addr {
        SocketAddr::V4(a) => !a.ip().is_broadcast(),
        SocketAddr::V6(a) => {
            a.scope_id() == 0
                && a.flowinfo() == 0
                && !a.ip().is_unicast_link_local()
                && a.ip().to_ipv4_mapped().is_none()
        }
    }
}

/// Bind a UDP socket without implicit IPv4 dual-stack behavior on IPv6.
pub fn bind_udp(bind: SocketAddr) -> std::io::Result<UdpSocket> {
    let domain = if bind.is_ipv4() {
        socket2::Domain::IPV4
    } else {
        socket2::Domain::IPV6
    };
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    if bind.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&bind.into())?;
    Ok(socket.into())
}

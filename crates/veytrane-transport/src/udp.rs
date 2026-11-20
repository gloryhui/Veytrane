//! Native address-family UDP binding.

use std::net::{SocketAddr, UdpSocket};

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

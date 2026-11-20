//! Native address families: an IPv6 socket never carries IPv4-mapped packets.
#[cfg(test)]
use std::net::Ipv6Addr;
use std::net::SocketAddr;

pub use veytrane_transport::udp::{AddressFamily, usable_address};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum NetworkFamilies {
    #[default]
    DualStack,
    Ipv4Only,
    Ipv6Only,
}
impl NetworkFamilies {
    pub fn enabled(self, family: AddressFamily) -> bool {
        self == Self::DualStack
            || matches!(
                (self, family),
                (Self::Ipv4Only, AddressFamily::Ipv4) | (Self::Ipv6Only, AddressFamily::Ipv6)
            )
    }
}

/// Set IPV6_V6ONLY before bind on every platform; do not release/rebind the port.
pub fn bind_udp(bind: SocketAddr) -> std::io::Result<std::net::UdpSocket> {
    veytrane_transport::udp::bind_udp(bind)
}

#[cfg(test)]
pub(crate) fn ipv6_test_available() -> bool {
    match bind_udp(AddressFamily::Ipv6.loopback(0)) {
        Ok(_) => true,
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(97 | 99 | 49 | 47 | 10047 | 10049)
            ) || error.kind() == std::io::ErrorKind::Unsupported =>
        {
            eprintln!("SKIP IPv6 loopback: runner has no IPv6 capability: {error}");
            false
        }
        Err(error) => panic!("IPv6 capability probe failed unexpectedly: {error}"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_ula_and_loopback_are_native_but_scoped_mapped_and_multicast_are_not() {
        for address in [
            "[2001:db8::1]:9000",
            "[fd00::7]:9000",
            "[::1]:9000",
            "192.168.1.2:9000",
        ] {
            assert!(usable_address(address.parse().unwrap()), "{address}");
        }
        for address in [
            "[fe80::1]:9000",
            "[ff02::1]:9000",
            "[::ffff:192.168.1.2]:9000",
            "[::]:9000",
            "0.0.0.0:9000",
            "[::1]:0",
        ] {
            assert!(!usable_address(address.parse().unwrap()), "{address}");
        }
        let scoped = SocketAddr::V6(std::net::SocketAddrV6::new(Ipv6Addr::LOCALHOST, 9000, 0, 2));
        assert!(!usable_address(scoped));
        assert!(!AddressFamily::Ipv4.accepts("[::1]:9000".parse().unwrap()));
        assert!(!AddressFamily::Ipv6.accepts("127.0.0.1:9000".parse().unwrap()));
    }
    #[test]
    fn native_v6only_and_v4_can_hold_the_same_fixed_port() {
        if !ipv6_test_available() {
            return;
        }
        let v6 = bind_udp(AddressFamily::Ipv6.wildcard(0)).unwrap();
        let port = v6.local_addr().unwrap().port();
        let reference = socket2::SockRef::from(&v6);
        assert!(reference.only_v6().unwrap());
        let v4 = bind_udp(AddressFamily::Ipv4.wildcard(port)).unwrap();
        assert_eq!(v4.local_addr().unwrap().port(), port);
        assert!(bind_udp(AddressFamily::Ipv6.wildcard(port)).is_err());
        assert!(bind_udp(AddressFamily::Ipv4.wildcard(port)).is_err());
    }
}

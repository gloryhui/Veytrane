//! QUIC endpoint construction, TLS/ALPN policy and base connection setup.
//!
//! Certificates are deliberately not verified: node identity is established by
//! the Ed25519 application handshake, bound to the current TLS session. Every
//! connection must still complete that handshake before its peer is trusted.

pub mod relay;

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use quinn::rustls;
use quinn::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use quinn::{
    AsyncUdpSocket, ClientConfig, Endpoint, IdleTimeout, Runtime, ServerConfig, TransportConfig,
    VarIntBoundsExceeded,
};

use crate::error::{Error, Result};
use crate::udp::bind_udp;

/// ALPN identifier shared by every Veytrane QUIC endpoint.
pub const ALPN: &[u8] = b"p2pfile/1";

/// Keepalive interval chosen to survive typical 30–120 second NAT timeouts.
pub const KEEP_ALIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Connection idle timeout.
pub const MAX_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

static CRYPTO_PROVIDER: OnceLock<()> = OnceLock::new();

/// Install rustls' default crypto provider once per process.
pub fn install_crypto_provider() {
    CRYPTO_PROVIDER.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn transport_config() -> Result<TransportConfig> {
    let mut transport = TransportConfig::default();
    transport.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    transport.keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));
    let idle: IdleTimeout = MAX_IDLE_TIMEOUT
        .try_into()
        .map_err(|err: VarIntBoundsExceeded| Error::Transport(format!("空闲超时非法: {err}")))?;
    transport.max_idle_timeout(Some(idle));
    Ok(transport)
}

fn server_config() -> Result<ServerConfig> {
    let certified = rcgen::generate_simple_self_signed(vec!["p2pfile".to_string()])
        .map_err(|err| Error::Transport(format!("生成自签证书失败: {err}")))?;
    let cert_der: CertificateDer<'static> = certified.cert.der().clone();
    let key_der = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());

    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der.into())
        .map_err(|err| Error::Transport(format!("TLS 服务端配置失败: {err}")))?;
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .map_err(|err| Error::Transport(format!("QUIC 服务端配置失败: {err}")))?;

    let mut config = ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(Arc::new(transport_config()?));
    Ok(config)
}

fn client_config() -> Result<ClientConfig> {
    let mut tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|err| Error::Transport(format!("QUIC 客户端配置失败: {err}")))?;

    let mut config = ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(transport_config()?));
    Ok(config)
}

/// Build a bidirectional endpoint over an already-bound UDP socket.
pub fn endpoint_from_socket(socket: std::net::UdpSocket) -> Result<Endpoint> {
    endpoint_from_socket_with_config(socket, quinn::EndpointConfig::default())
}

/// Build over an existing socket while preserving caller-specific endpoint flags.
pub fn endpoint_from_socket_with_config(
    socket: std::net::UdpSocket,
    endpoint_config: quinn::EndpointConfig,
) -> Result<Endpoint> {
    install_crypto_provider();
    socket
        .set_nonblocking(true)
        .map_err(|err| Error::Transport(format!("设置非阻塞失败: {err}")))?;

    let runtime = quinn::default_runtime()
        .ok_or_else(|| Error::Transport("当前没有可用的异步运行时".into()))?;
    let mut endpoint = Endpoint::new(endpoint_config, Some(server_config()?), socket, runtime)
        .map_err(|err| Error::Transport(format!("用已有 socket 创建 QUIC 端点失败: {err}")))?;
    endpoint.set_default_client_config(client_config()?);
    Ok(endpoint)
}

/// Build a bidirectional endpoint over an adapter whose receive side is already owned.
///
/// The packet demultiplexer lives in the caller; this constructor only applies the
/// shared TLS, ALPN and transport policy.
pub fn endpoint_from_abstract_socket(
    socket: Arc<dyn AsyncUdpSocket>,
    runtime: Arc<dyn Runtime>,
) -> Result<Endpoint> {
    install_crypto_provider();
    let mut endpoint = Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        Some(server_config()?),
        socket,
        runtime,
    )
    .map_err(|err| Error::Transport(format!("用已有 socket 创建 QUIC 端点失败: {err}")))?;
    endpoint.set_default_client_config(client_config()?);
    Ok(endpoint)
}

/// Create a server endpoint bound to `bind`.
pub fn server_endpoint(bind: SocketAddr) -> Result<Endpoint> {
    install_crypto_provider();
    Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_config()?),
        bind_udp(bind)?,
        Arc::new(quinn::TokioRuntime),
    )
    .map_err(|err| Error::Transport(format!("绑定 {bind} 失败: {err}")))
}

/// Create a client endpoint bound to `bind` (usually an ephemeral wildcard port).
pub fn client_endpoint(bind: SocketAddr) -> Result<Endpoint> {
    install_crypto_provider();
    let mut endpoint = Endpoint::new(
        quinn::EndpointConfig::default(),
        None,
        bind_udp(bind)?,
        Arc::new(quinn::TokioRuntime),
    )
    .map_err(|err| Error::Transport(format!("绑定 {bind} 失败: {err}")))?;
    endpoint.set_default_client_config(client_config()?);
    Ok(endpoint)
}

/// Connect to `remote`; `server_name` is TLS SNI only because certificates are not verified.
pub async fn connect(
    endpoint: &Endpoint,
    remote: SocketAddr,
    server_name: &str,
) -> Result<quinn::Connection> {
    let connecting = endpoint
        .connect(remote, server_name)
        .map_err(|err| Error::Transport(format!("发起连接到 {remote} 失败: {err}")))?;
    connecting
        .await
        .map_err(|err| Error::Transport(format!("与 {remote} 建立连接失败: {err}")))
}

#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_binding::ChannelBinding;
    use crate::handshake::{handshake_initiator, handshake_responder};
    use crate::identity::Identity;

    #[test]
    fn transport_policy_is_valid() {
        assert!(transport_config().is_ok());
    }

    #[tokio::test]
    async fn endpoint_reuses_the_bound_port() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let expected = socket.local_addr().unwrap();
        let endpoint = endpoint_from_socket(socket).unwrap();
        assert_eq!(endpoint.local_addr().unwrap(), expected);
        endpoint.close(0u32.into(), b"test complete");
    }

    #[tokio::test]
    async fn fixed_port_can_be_rebound_after_release() {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let bind = probe.local_addr().unwrap();
        drop(probe);

        let server = server_endpoint(bind).unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let server_for_accept = server.clone();
        let server_accept = tokio::spawn(async move {
            server_for_accept
                .accept()
                .await
                .expect("server should accept")
                .await
                .expect("QUIC handshake should succeed")
        });
        let connection = connect(&client, server_addr, "127.0.0.1").await.unwrap();
        let server_connection = server_accept.await.unwrap();

        connection.close(0u32.into(), b"release");
        server_connection.close(0u32.into(), b"release");
        connection.closed().await;
        server_connection.closed().await;
        drop(server_connection);
        drop(connection);
        server.close(0u32.into(), b"release endpoint");
        client.wait_idle().await;
        server.wait_idle().await;
        drop(client);
        drop(server);
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }

        let rebound = server_endpoint(bind).expect("fixed port should be reusable");
        rebound.close(0u32.into(), b"done");
        rebound.wait_idle().await;
    }

    #[tokio::test]
    async fn same_connection_exports_matching_bindings_and_authenticates() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let bob_id = bob.node_id();
        let server = server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let server_addr = server.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let connection = server.accept().await.unwrap().await.unwrap();
            let binding = ChannelBinding::from_connection(&connection).unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            let outcome = handshake_responder(&mut send, &mut recv, &bob, &binding)
                .await
                .unwrap();
            let _ = send.finish();
            connection.closed().await;
            server.wait_idle().await;
            (binding, outcome)
        });

        let client = client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let connection = connect(&client, server_addr, "127.0.0.1").await.unwrap();
        let client_binding = ChannelBinding::from_connection(&connection).unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let outcome = handshake_initiator(&mut send, &mut recv, &alice, &client_binding)
            .await
            .unwrap();
        let _ = send.finish();

        connection.close(0u32.into(), b"bye");
        client.wait_idle().await;

        let (server_binding, server_outcome) = server_task.await.unwrap();
        assert_eq!(client_binding.as_bytes(), server_binding.as_bytes());
        assert_eq!(outcome.peer_node_id, bob_id);
        assert_eq!(server_outcome.peer_node_id, alice.node_id());
    }

    #[tokio::test]
    async fn separate_connections_export_different_bindings() {
        let server = server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let server_addr = server.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let first = server.accept().await.unwrap().await.unwrap();
            let first_binding = ChannelBinding::from_connection(&first).unwrap();
            let second = server.accept().await.unwrap().await.unwrap();
            let second_binding = ChannelBinding::from_connection(&second).unwrap();
            first.closed().await;
            second.closed().await;
            server.wait_idle().await;
            (first_binding, second_binding)
        });

        let client = client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let first = connect(&client, server_addr, "127.0.0.1").await.unwrap();
        let first_binding = ChannelBinding::from_connection(&first).unwrap();
        let second = connect(&client, server_addr, "127.0.0.1").await.unwrap();
        let second_binding = ChannelBinding::from_connection(&second).unwrap();
        assert_ne!(first_binding.as_bytes(), second_binding.as_bytes());

        first.close(0u32.into(), b"bye");
        second.close(0u32.into(), b"bye");
        client.wait_idle().await;

        let (server_first, server_second) = server_task.await.unwrap();
        assert_eq!(first_binding.as_bytes(), server_first.as_bytes());
        assert_eq!(second_binding.as_bytes(), server_second.as_bytes());
        assert_ne!(server_first.as_bytes(), server_second.as_bytes());
    }
}

use super::*;
use crate::node::core::dispatch;
use crate::node::core::wire::{self, NodeFrame, NodeMessage, StreamKind};
use crate::transport::quic::{client_endpoint, connect, server_endpoint};
use tokio::sync::watch;
fn serve_connection(
    connection: Connection,
    peer: NodeId,
    grants: watch::Receiver<Vec<AllowedForwardTarget>>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let (_dispatcher, incoming) =
            dispatch::Dispatcher::start(connection.clone(), wire::capabilities(false, false, true));
        let (_resources, policy) = watch::channel(Default::default());
        serve(
            peer,
            crate::node::core::remote_auth::RemoteAuthorization::BOTH.into(),
            grants,
            policy,
            incoming.services,
            Arc::new(crate::tunnel::ForwardStreamOwner::new(16)),
        )
        .await
    })
}
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn exercise_forwarding(allowed: bool, peer_allowed: bool) {
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = echo.local_addr().unwrap();
    let echo_task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = stream.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });

    let server = server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
    let client = client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
    let server_addr = server.local_addr().unwrap();
    let server_for_accept = server.clone();
    let server_connection_task =
        tokio::spawn(async move { server_for_accept.accept().await.unwrap().await.unwrap() });
    let client_connection = connect(&client, server_addr, "p2pfile").await.unwrap();
    let server_connection = server_connection_task.await.unwrap();

    let peer = crate::identity::Identity::generate().node_id();
    let server_allowed = if allowed {
        vec![crate::node::core::config::AllowedForwardTarget::new(
            "echo",
            target,
            vec![if peer_allowed {
                peer.to_hex()
            } else {
                crate::identity::Identity::generate().node_id().to_hex()
            }],
        )]
    } else {
        Vec::new()
    };
    let mut grants = server_allowed;
    for grant in &mut grants {
        grant.id = "echo".into();
    }
    let (_, grants) = watch::channel(grants);
    let server_task = serve_connection(server_connection.clone(), peer, grants);

    let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = local_listener.local_addr().unwrap();
    let (peer_updates, peer_connection) = watch::channel(Some(client_connection.clone()));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (errors_tx, mut errors_rx) = tokio::sync::mpsc::unbounded_channel();
    let forwarder = tokio::spawn(crate::tunnel::forward_on_authenticated_session(
        local_listener,
        target,
        peer_connection,
        shutdown_rx,
        errors_tx,
        |tcp, connection, _target| async move {
            let stream = open_tcp(&connection, "echo").await?;
            crate::tunnel::bridge_tcp(tcp, stream.send, stream.recv).await
        },
    ));

    let mut local = tokio::net::TcpStream::connect(local_addr).await.unwrap();
    local.write_all(b"desktop tcp tunnel").await.unwrap();
    if allowed && peer_allowed {
        let mut echoed = [0; 18];
        local.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"desktop tcp tunnel");
        local.shutdown().await.unwrap();
        let mut tail = Vec::new();
        local.read_to_end(&mut tail).await.unwrap();
    } else {
        let mut rejected = Vec::new();
        let _ = local.read_to_end(&mut rejected).await;
        assert!(rejected.is_empty(), "denied targets must not return bytes");
        let detail = tokio::time::timeout(Duration::from_secs(2), errors_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(detail.contains("服务不存在或不可访问"), "{detail}");
    }

    echo_task.abort();
    server_task.abort();
    shutdown_tx.send_replace(true);
    tokio::time::timeout(Duration::from_secs(2), forwarder)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    client_connection.close(0u32.into(), b"test complete");
    client.close(0u32.into(), b"test complete");
    server.close(0u32.into(), b"test complete");
    let _ = peer_updates;
}

#[tokio::test]
async fn service_stream_reuses_core_for_allowed_target_and_reports_whitelist_denial() {
    exercise_forwarding(true, true).await;
    exercise_forwarding(false, true).await;
    exercise_forwarding(true, false).await;
}
#[tokio::test]
async fn service_checks_latest_grant_after_delayed_open() {
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = echo.local_addr().unwrap();
    let peer = crate::identity::Identity::generate().node_id();
    let server = server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
    let client = client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
    let accept = server.clone();
    let accepted = tokio::spawn(async move { accept.accept().await.unwrap().await.unwrap() });
    let client_conn = connect(&client, server.local_addr().unwrap(), "p2pfile")
        .await
        .unwrap();
    let server_conn = accepted.await.unwrap();
    let mut grant =
        crate::node::core::config::AllowedForwardTarget::new("echo", target, vec![peer.to_hex()]);
    grant.id = "echo".into();
    let (updates, grants) = watch::channel(vec![grant]);
    let service = serve_connection(server_conn.clone(), peer, grants);
    let stream = dispatch::open_bi(&client_conn, StreamKind::Service)
        .await
        .unwrap();
    let (mut send, mut recv) = (stream.send, stream.recv);
    let frame = NodeFrame::new(
        1,
        NodeMessage::OpenTcp {
            service_id: "echo".into(),
        },
    )
    .encode()
    .unwrap();
    let length = (frame.len() as u32).to_le_bytes();
    // Deliver only the header. accept_bi can complete, but authorization must wait for OpenTcp.
    send.write_all(&length).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while updates.receiver_count() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("service handler must have accepted the pending stream");
    updates.send_replace(Vec::new());
    send.write_all(&frame).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2), wire::read(&mut recv))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.request_id, 1);
    assert_eq!(
        reply.message,
        NodeMessage::Error {
            code: wire::NodeErrorCode::UnknownService
        }
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), echo.accept())
            .await
            .is_err()
    );
    client_conn.close(0u32.into(), b"done");
    server_conn.close(0u32.into(), b"done");
    service.await.unwrap().unwrap();
}

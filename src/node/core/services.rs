//! Authorized service discovery and ID-based TCP streams for node-v1.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::Connection;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinSet;

use crate::error::{Error, Result};
use crate::identity::NodeId;
use crate::node::core::authorization::ResourcePolicy;
use crate::node::core::config::AllowedForwardTarget;
use crate::node::core::dispatch::{self, BiStream};
use crate::node::core::remote_auth::LiveAuthorization;
use crate::node::core::wire::{
    self, NodeErrorCode, NodeFrame, NodeMessage, ServiceDescriptor, StreamKind,
};

const MAX_PENDING_CONTROLS: usize = 8;
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

fn visible(entry: &AllowedForwardTarget, peer: NodeId) -> bool {
    entry.enabled
        && entry
            .allowed_peers
            .iter()
            .any(|id| NodeId::from_hex(id).ok() == Some(peer))
}
fn target_for(targets: &[AllowedForwardTarget], peer: NodeId, id: &str) -> Option<SocketAddr> {
    targets
        .iter()
        .find(|entry| entry.id == id && visible(entry, peer))
        .map(|entry| entry.target)
}

pub async fn serve(
    peer: NodeId,
    authorization: LiveAuthorization,
    targets: watch::Receiver<Vec<AllowedForwardTarget>>,
    resources: watch::Receiver<ResourcePolicy>,
    mut incoming: mpsc::Receiver<BiStream>,
    tcp_streams: Arc<crate::tunnel::ForwardStreamOwner>,
) -> Result<()> {
    let mut streams = JoinSet::new();
    let controls = Arc::new(Semaphore::new(MAX_PENDING_CONTROLS));
    loop {
        while streams.try_join_next().is_some() {}
        tokio::select! {
            stream = incoming.recv() => {
                let Some(mut stream) = stream else { break };
                let Ok(control) = controls.clone().try_acquire_owned() else {
                    let _ = stream.send.reset(dispatch::RESET_BUSY.into());
                    let _ = stream.recv.stop(dispatch::RESET_BUSY.into());
                    continue;
                };
                let authorization = authorization.clone();
                let targets = targets.clone();
                let resources = resources.clone();
                let tcp_streams = tcp_streams.clone();
                streams.spawn(async move {
                    authorization.guard(true, handle(peer, &targets, &resources, stream, control, tcp_streams)).await
                });
            }
            _ = streams.join_next(), if !streams.is_empty() => {}
        }
    }
    streams.abort_all();
    while streams.join_next().await.is_some() {}
    Ok(())
}

async fn reply(stream: &mut BiStream, id: u64, message: NodeMessage) -> Result<()> {
    wire::write(&mut stream.send, &NodeFrame::new(id, message)).await
}
async fn reject(stream: &mut BiStream, id: u64, code: NodeErrorCode) -> Result<()> {
    reply(stream, id, NodeMessage::Error { code }).await?;
    let _ = stream.send.finish();
    Ok(())
}

async fn handle(
    peer: NodeId,
    targets: &watch::Receiver<Vec<AllowedForwardTarget>>,
    resources: &watch::Receiver<ResourcePolicy>,
    mut stream: BiStream,
    control: OwnedSemaphorePermit,
    tcp_streams: Arc<crate::tunnel::ForwardStreamOwner>,
) -> Result<()> {
    let prepared = tokio::time::timeout(
        RPC_TIMEOUT,
        prepare(peer, targets, resources, &mut stream, &tcp_streams),
    )
    .await;
    // HTTP keep-alive, WebSocket and SMB data flows must not retain a control
    // slot after TcpReady. Their permit belongs to the shared node TCP budget.
    drop(control);
    match prepared {
        Ok(Ok(Some((tcp, _tcp_permit)))) => {
            crate::tunnel::bridge_tcp(tcp, stream.send, stream.recv).await
        }
        Ok(Ok(None)) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(_) => {
            let _ = stream.send.reset(dispatch::RESET_INVALID.into());
            let _ = stream.recv.stop(dispatch::RESET_INVALID.into());
            Err(Error::Protocol("节点服务控制阶段超时".into()))
        }
    }
}

async fn prepare(
    peer: NodeId,
    targets: &watch::Receiver<Vec<AllowedForwardTarget>>,
    resources: &watch::Receiver<ResourcePolicy>,
    stream: &mut BiStream,
    tcp_streams: &crate::tunnel::ForwardStreamOwner,
) -> Result<Option<(tokio::net::TcpStream, OwnedSemaphorePermit)>> {
    let request = tokio::time::timeout(Duration::from_secs(5), wire::read(&mut stream.recv))
        .await
        .map_err(|_| Error::Protocol("服务请求首帧超时".into()))??;
    let id = request.request_id;
    if id == 0 {
        return Err(Error::Protocol("服务请求必须使用非零请求号".into()));
    }
    if resources.borrow().for_peer(peer).disabled {
        return reject(stream, id, NodeErrorCode::Unauthorized)
            .await
            .map(|_| None);
    }
    match request.message {
        NodeMessage::Services => {
            let entries = targets
                .borrow()
                .iter()
                .filter(|entry| visible(entry, peer))
                .take(wire::MAX_SERVICES + 1)
                .map(|entry| ServiceDescriptor {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                })
                .collect::<Vec<_>>();
            if entries.len() > wire::MAX_SERVICES {
                return reject(stream, id, NodeErrorCode::Busy).await.map(|_| None);
            }
            reply(stream, id, NodeMessage::ServiceList { services: entries }).await?;
            let _ = stream.send.finish();
            Ok(None)
        }
        NodeMessage::OpenTcp { service_id } => {
            let target = target_for(&targets.borrow(), peer, &service_id);
            let Some(target) = target else {
                return reject(stream, id, NodeErrorCode::UnknownService)
                    .await
                    .map(|_| None);
            };
            let Some(permit) = tcp_streams.try_reserve() else {
                return reject(stream, id, NodeErrorCode::Busy).await.map(|_| None);
            };
            let tcp = match crate::tunnel::connect_allowed_target(target, &[target]).await {
                Ok(tcp) => tcp,
                Err(_) => {
                    return reject(stream, id, NodeErrorCode::TargetUnavailable)
                        .await
                        .map(|_| None);
                }
            };
            if resources.borrow().for_peer(peer).disabled
                || target_for(&targets.borrow(), peer, &service_id) != Some(target)
            {
                return reject(stream, id, NodeErrorCode::UnknownService)
                    .await
                    .map(|_| None);
            }
            reply(stream, id, NodeMessage::TcpReady).await?;
            Ok(Some((tcp, permit)))
        }
        _ => reject(stream, id, NodeErrorCode::InvalidRequest)
            .await
            .map(|_| None),
    }
}

async fn request(connection: &Connection, message: NodeMessage) -> Result<(BiStream, NodeMessage)> {
    tokio::time::timeout(RPC_TIMEOUT, async {
        let id = rand::random::<u64>().max(1);
        let mut stream = dispatch::open_bi(connection, StreamKind::Service, true).await?;
        wire::write(&mut stream.send, &NodeFrame::new(id, message)).await?;
        let response = wire::read(&mut stream.recv).await?;
        if response.request_id != id {
            return Err(Error::Protocol("服务响应请求号不匹配".into()));
        }
        match response.message {
            NodeMessage::Error { code } => Err(Error::Protocol(code.message().into())),
            message => Ok((stream, message)),
        }
    })
    .await
    .map_err(|_| Error::Transport("节点服务请求超时".into()))?
}

pub async fn list(connection: &Connection) -> Result<Vec<ServiceDescriptor>> {
    let (mut stream, reply) = request(connection, NodeMessage::Services).await?;
    let _ = stream.send.finish();
    match reply {
        NodeMessage::ServiceList { services } => Ok(services),
        _ => Err(Error::Protocol("服务目录响应类型无效".into())),
    }
}

pub async fn open_tcp(connection: &Connection, service_id: &str) -> Result<BiStream> {
    wire::validate_service_id(service_id)?;
    let (stream, reply) = request(
        connection,
        NodeMessage::OpenTcp {
            service_id: service_id.into(),
        },
    )
    .await?;
    match reply {
        NodeMessage::TcpReady => Ok(stream),
        _ => Err(Error::Protocol("服务开流响应类型无效".into())),
    }
}

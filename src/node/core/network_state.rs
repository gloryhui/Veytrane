//! Network-wide session status and peer registry compatibility paths.

use std::time::Duration;

use crate::identity::NodeId;
pub use veytrane_node::peer_registry::{
    BeginPeerAttempt, DEFAULT_MAX_PEERS, MAX_PEER_CAPACITY, MAX_PENDING_PEERS, PEER_LOOKUP_TIMEOUT,
    PeerLifecycle, PeerRegistry, reconnect_delay, should_initiate_quic,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkLifecycle {
    Unconfigured,
    LocalOnly,
    ConnectingSignal,
    SignalOnline,
    ReconnectingSignal {
        attempt: u32,
        delay: Duration,
    },
    PeerPending {
        peer: NodeId,
    },
    Punching {
        peer: NodeId,
    },
    Authenticating {
        peer: NodeId,
    },
    Connected {
        peer: NodeId,
    },
    Disconnected {
        peer: Option<NodeId>,
        detail: String,
    },
    Failed {
        detail: String,
    },
}

impl NetworkLifecycle {
    pub fn label(&self) -> String {
        match self {
            Self::Unconfigured => "未配置网络".into(),
            Self::LocalOnly => "无信令模式：局域网 / IP直连".into(),
            Self::ConnectingSignal => "正在连接信令".into(),
            Self::SignalOnline => "信令在线；等待对端".into(),
            Self::ReconnectingSignal { attempt, .. } => {
                format!("信令断开；正在第 {attempt} 次重连")
            }
            Self::PeerPending { peer } => format!("等待对端 {} 上线", peer.short()),
            Self::Punching { peer } => format!("正在连接对端 {}", peer.short()),
            Self::Authenticating { peer } => format!("正在认证对端 {}", peer.short()),
            Self::Connected { peer } => format!("设备会话已连接 {}", peer.short()),
            Self::Disconnected { peer, detail } => match peer {
                Some(peer) => format!("对端 {} 已断开：{detail}", peer.short()),
                None => format!("网络已断开：{detail}"),
            },
            Self::Failed { detail } => format!("网络启动失败：{detail}"),
        }
    }
}

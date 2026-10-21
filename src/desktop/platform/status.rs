//! Tray display state; contains no embedded session or window owner.
use crate::node::core::network_state;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::desktop) enum CoreState {
    #[default]
    Offline,
    Connecting,
    Online,
    Active,
    Error,
}
impl CoreState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Offline => "未配置 / 离线",
            Self::Connecting => "连接中",
            Self::Online => "在线",
            Self::Active => "传输 / 测速中",
            Self::Error => "需要处理",
        }
    }
    pub fn color(self) -> [u8; 3] {
        match self {
            Self::Offline => [120, 128, 143],
            Self::Connecting => [222, 157, 40],
            Self::Online => [39, 166, 113],
            Self::Active => [50, 117, 227],
            Self::Error => [216, 75, 75],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::desktop) enum SignalState {
    #[default]
    Offline,
    Connecting,
    Online,
    Error,
}
impl SignalState {
    pub fn apply(&mut self, lifecycle: &network_state::NetworkLifecycle) {
        use network_state::NetworkLifecycle as N;
        match lifecycle {
            N::Unconfigured => *self = Self::Offline,
            N::ConnectingSignal | N::ReconnectingSignal { .. } => *self = Self::Connecting,
            N::SignalOnline => *self = Self::Online,
            N::Failed { .. } => *self = Self::Error,
            N::Disconnected { peer: None, .. } => *self = Self::Offline,
            _ => {} // A peer failure never changes the signaling state.
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::desktop) struct StatusSnapshot {
    pub core: CoreState,
    pub device_id: String,
    pub signal: String,
    pub connected_peers: usize,
    pub active_files: usize,
    pub queued_files: usize,
    pub bytes_per_second: u64,
    pub tunnels: usize,
    pub waiting_tunnels: usize,
    pub tunnel_errors: usize,
    pub speed_running: bool,
    pub detail: String,
}
impl StatusSnapshot {
    pub fn tooltip(&self) -> String {
        format!(
            "P2P File · {} · {} 台设备 · {} 个传输 · {} 个隧道",
            self.core.label(),
            self.connected_peers,
            self.active_files,
            self.tunnels
        )
    }
    pub(in crate::desktop) fn project_core(&mut self, signal: SignalState, error: bool) {
        self.core = if error || signal == SignalState::Error || self.tunnel_errors > 0 {
            CoreState::Error
        } else if self.active_files > 0 || self.speed_running {
            CoreState::Active
        } else if self.connected_peers > 0 || signal == SignalState::Online {
            CoreState::Online
        } else if signal == SignalState::Connecting {
            CoreState::Connecting
        } else {
            CoreState::Offline
        };
    }
}

//! Tray display state; contains no embedded session or window owner.

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::platform::tray;
    #[test]
    fn every_core_state_has_a_distinct_monochrome_menu_bar_icon() {
        let icons: Vec<_> = [
            CoreState::Offline,
            CoreState::Connecting,
            CoreState::Online,
            CoreState::Active,
            CoreState::Error,
        ]
        .into_iter()
        .map(|state| tray::icon_rgba(state, true))
        .collect();
        for (i, icon) in icons.iter().enumerate() {
            assert_eq!(icon.len(), 32 * 32 * 4);
            assert!(icons[i + 1..].iter().all(|other| icon != other));
        }
    }
}

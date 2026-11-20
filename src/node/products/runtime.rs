//! Shared product runtime helpers with no product-specific policy.

use std::collections::BTreeSet;

use crate::node::core::control::{NodeControl, NodeSnapshot, SessionEvent};
use crate::node::core::network_state::{NetworkLifecycle, PeerLifecycle};

pub(super) fn authorized_peers(snapshot: NodeSnapshot) -> BTreeSet<String> {
    snapshot
        .peers
        .into_values()
        .filter(|peer| peer.phase == "connected" && peer.inbound_password)
        .map(|peer| peer.node_id)
        .collect()
}

/// Log connection stages, never profiles, passwords, proof material or tokens.
pub(super) fn log_connection_event(event: &SessionEvent) {
    match event {
        SessionEvent::Lifecycle(NetworkLifecycle::ConnectingSignal) => {
            println!("正在连接信令服务器…")
        }
        SessionEvent::Lifecycle(NetworkLifecycle::SignalOnline) => {
            println!("信令服务器已连接，设备已登记。")
        }
        SessionEvent::Lifecycle(NetworkLifecycle::ReconnectingSignal { attempt, delay }) => {
            println!(
                "信令连接中断，第 {attempt} 次重试，等待 {} 秒。",
                delay.as_secs()
            )
        }
        SessionEvent::Lifecycle(NetworkLifecycle::Failed { detail }) => {
            println!("网络启动失败：{detail}")
        }
        SessionEvent::NetworkFamilies {
            ipv4,
            ipv6,
            relay_configured,
        } => println!("网络能力：IPv4={ipv4}，IPv6={ipv6}，中继兜底={relay_configured}"),
        SessionEvent::PeerCandidates {
            peer, ipv4, ipv6, ..
        } => println!(
            "设备 {}：已收到信令地址，IPv4={ipv4}，IPv6={ipv6}",
            peer.short()
        ),
        SessionEvent::PeerTransport {
            peer,
            relay,
            family,
            ..
        } => println!(
            "设备 {}：已建立 {} {family} 通道，正在校验身份。",
            peer.short(),
            if *relay { "中继" } else { "直连" },
        ),
        SessionEvent::PeerState { peer, state, .. } => {
            let detail = match state {
                PeerLifecycle::PeerPending => "等待信令配对".to_owned(),
                PeerLifecycle::Punching => "正在 UDP 打洞".to_owned(),
                PeerLifecycle::Authenticating => "正在建立加密通道并校验设备身份".to_owned(),
                PeerLifecycle::Negotiating => "正在协商服务协议".to_owned(),
                PeerLifecycle::RemoteAuthPending => "正在验证专用访问凭证".to_owned(),
                PeerLifecycle::Connected(_) => "设备通道与访问凭证认证完成".to_owned(),
                PeerLifecycle::Disconnected => "设备通道已断开，等待重新连接".to_owned(),
                PeerLifecycle::Failed(reason) => format!("连接失败：{reason}"),
            };
            println!("设备 {}：{detail}", peer.short());
        }
        SessionEvent::Diagnostic(message) => println!("连接诊断：{message}"),
        _ => {}
    }
}

pub(super) async fn stop_control(control: NodeControl) {
    control.shutdown();
    let _ = tokio::task::spawn_blocking(move || control.shutdown_and_wait()).await;
}

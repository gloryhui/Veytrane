use std::{net::SocketAddr, path::PathBuf};

use p2p_file::cli::{RelayServerOpts, SignalTlsServerOpts};
use p2p_file::discovery::signal::{SignalServerConfig, run_signal_server_on_with};
use p2p_file::error::Result;

pub(super) async fn run(
    listen: SocketAddr,
    short_id_db: PathBuf,
    max_device_ids: u32,
    new_device_ids_per_minute: u32,
    new_device_ids_per_ip_per_minute: u32,
    relay: RelayServerOpts,
    tls: SignalTlsServerOpts,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let config = SignalServerConfig {
        tls: tls.config()?,
        relay: relay.config()?,
        short_id_database: Some(short_id_db),
        max_short_id_mappings: max_device_ids,
        short_id_allocations_per_minute: new_device_ids_per_minute,
        short_id_allocations_per_ip_per_minute: new_device_ids_per_ip_per_minute,
        ..SignalServerConfig::default()
    };
    println!("信令服务器监听 {listen}");
    println!(
        "信令模式：{}",
        if config.tls.is_some() {
            "TLS（客户端校验证书；本端口仅接受 TLS）"
        } else {
            "明文 TCP（兼容模式）"
        }
    );
    if let Some(relay) = &config.relay {
        println!(
            "Relay UDP enabled: {:?}；仅转发已 pairing 且双边认证的加密 datagram。",
            relay.listen
        );
        println!("云安全组还需放通配置的 Relay UDP 端口；QUIC 身份与业务权限仍由对端验证。");
    } else {
        println!("Relay UDP disabled；只交换候选地址，业务数据走 P2P。");
    }
    println!("记得在云主机安全组里放通这个 TCP 端口。");
    println!("按 Ctrl-C 退出。");

    tokio::select! {
        result = run_signal_server_on_with(listener, config) => result,
        _ = super::shutdown_signal() => {
            println!("\n收到中断信号，退出。");
            Ok(())
        }
    }
}

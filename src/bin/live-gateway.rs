//! Isolated broadcaster gateway. MediaMTX remains an unmodified external process.
use clap::Parser;
use p2p_file::node::products::live;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Veytrane Live · 主播端网关（配合官方 MediaMTX）")]
struct Args {
    #[arg(long)]
    data_dir: PathBuf,
    /// Create a new identity and viewer profile. Never overwrites existing files.
    #[arg(long)]
    init: bool,
    #[arg(long, required_if_eq("init", "true"))]
    signal_server: Option<String>,
    #[arg(long)]
    relay_server: Option<String>,
    /// Isolated same-machine test only: no STUN, include loopback candidates.
    #[arg(long)]
    loopback_test: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let root = std::path::absolute(args.data_dir)?;
    if args.init {
        live::initialize_gateway(
            &root,
            args.signal_server.ok_or("--signal-server is required")?,
            args.relay_server,
        )?;
        println!(
            "已生成独立直播身份。朋友使用：{}",
            root.join("live-viewer.json").display()
        );
        return Ok(());
    }
    live::run_gateway(&root, args.loopback_test).await?;
    Ok(())
}

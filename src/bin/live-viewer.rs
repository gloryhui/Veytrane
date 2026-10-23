//! Recipient executable: one adjacent profile, no login and no MediaMTX dependency.
use clap::Parser;
use p2p_file::node::products::live;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Veytrane Live · 观看端，连接后自动打开浏览器")]
struct Args {
    /// Default: live-viewer.json next to this executable.
    #[arg(long)]
    profile: Option<PathBuf>,
    /// For automated tests or machines without a desktop.
    #[arg(long)]
    no_browser: bool,
    #[arg(long)]
    loopback_test: bool,
    /// Separate local identity directory for isolated tests.
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let profile = match args.profile {
        Some(path) => path,
        None => std::env::current_exe()?.with_file_name("live-viewer.json"),
    };
    live::run_viewer(
        &profile,
        args.data_dir.map(std::path::absolute).transpose()?,
        !args.no_browser,
        args.loopback_test,
    )
    .await?;
    Ok(())
}

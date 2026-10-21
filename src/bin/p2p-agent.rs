#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use clap::Parser;
use p2p_file::node::{adapters::cli as management_cli, host::agent, local::paths::AgentPaths};
use std::path::PathBuf;
#[derive(Parser)]
#[command(about = "无图形界面的 P2P 节点宿主", version)]
struct Args {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long, conflicts_with = "initialize")]
    init: bool,
    #[arg(long)]
    build_info: bool,
    /// Explicit first-run lifecycle action: initialize if missing, then run.
    #[arg(long)]
    initialize: bool,
    /// Enable macOS wake/network recovery when an external service restarts us.
    #[arg(long)]
    managed: bool,
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    if args.build_info {
        println!("{}", p2p_file::node::api::build_info::value("p2p-agent"));
        return Ok(());
    }
    let root = match args.data_dir {
        Some(root) => root,
        None => AgentPaths::default_root()?,
    };
    let paths = AgentPaths::new(&root)?;
    if args.init {
        println!("{}", management_cli::initialize(&paths)?);
        return Ok(());
    }
    if args.initialize && !paths.config().try_exists()? {
        management_cli::initialize(&paths)?;
    }
    if args.managed {
        agent::run_managed(paths).await?;
    } else {
        agent::run(paths).await?;
    }
    Ok(())
}

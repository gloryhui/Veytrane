use clap::Parser;
use p2p_file::node::{agent, agent_paths::AgentPaths, management_cli};
use std::path::PathBuf;
#[derive(Parser)]
#[command(about = "无图形界面的 P2P 节点宿主", version)]
struct Args {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long, conflicts_with = "initialize")]
    init: bool,
    /// Explicit first-run lifecycle action: initialize if missing, then run.
    #[arg(long)]
    initialize: bool,
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
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
    agent::run(paths).await?;
    Ok(())
}

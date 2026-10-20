use clap::Parser;
use p2p_file::{
    identity::Identity,
    node::{
        agent, agent_config::ConfigRecord, agent_paths::AgentPaths, instance_lock::InstanceLock,
    },
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "无图形界面的 P2P 节点宿主")]
struct Args {
    #[arg(long)]
    data_dir: PathBuf,
    /// Explicitly create identity/config without starting networking.
    #[arg(long)]
    init: bool,
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let paths = AgentPaths::new(&args.data_dir)?;
    if args.init {
        let _lock = InstanceLock::acquire(&paths.lock())?;
        if paths.config().try_exists()? {
            return Err("节点配置已存在，请使用管理命令修改".into());
        }
        let identity = Identity::load_or_create(&paths.identity())?;
        let _identity_lock = AgentPaths::identity_lock(identity.node_id())?;
        let receive = paths.root.join("receive");
        p2p_file::node::private_access::ensure_directory(&receive)?;
        ConfigRecord::initialize(&paths.config(), receive)?;
        println!(
            "{}",
            serde_json::json!({"node_id":identity.node_id().to_hex(),"config":paths.config()})
        );
        return Ok(());
    }
    agent::run(paths).await?;
    Ok(())
}

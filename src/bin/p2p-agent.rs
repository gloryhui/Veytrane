#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use clap::Parser;
use p2p_file::node::{
    host::{agent, configuration},
    local::paths::AgentPaths,
};
use std::path::PathBuf;
#[derive(Parser)]
#[command(about = "Veytrane Agent · 无图形界面的可信设备互联节点", version)]
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
    /// Enable macOS supervisor fallback if the network core exits.
    #[arg(long)]
    managed: bool,
    /// Serve the navigation UI and API in this Agent process.
    #[arg(long, conflicts_with = "no_navigation")]
    navigation_config: Option<PathBuf>,
    /// Disable the optional navigation adapter even if its config exists.
    #[arg(long)]
    no_navigation: bool,
    /// Create a private, bounded UDP/PUNCH JSONL diagnostic file.
    #[arg(long, value_name = "PATH")]
    diagnostic_log: Option<PathBuf>,
    /// Stop diagnostic writes after this many seconds (default: 90, maximum: 3600).
    #[arg(
        long,
        value_name = "SECONDS",
        requires = "diagnostic_log"
    )]
    diagnostic_seconds: Option<u64>,
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
        println!("{}", configuration::initialize(&paths)?);
        return Ok(());
    }
    if args.initialize && !paths.config().try_exists()? {
        configuration::initialize(&paths)?;
    }
    if let Some(path) = args.diagnostic_log.as_deref() {
        let seconds = args.diagnostic_seconds.unwrap_or(90);
        p2p_file::transport::diagnostic::configure(path, seconds)?;
        p2p_file::transport::diagnostic::startup(seconds);
    }
    let navigation = if args.no_navigation {
        None
    } else if let Some(path) = args.navigation_config {
        Some(path)
    } else {
        paths
            .root
            .parent()
            .map(|root| root.join("navigation/config.json"))
            .filter(|path| path.is_file())
    };
    if let Some(config) = navigation {
        run_with_navigation(paths, args.managed, config).await?;
    } else if args.managed {
        agent::run_managed(paths).await?;
    } else {
        agent::run(paths).await?;
    }
    Ok(())
}

async fn run_with_navigation(
    paths: AgentPaths,
    managed: bool,
    configuration: PathBuf,
) -> std::io::Result<()> {
    let (restart, restart_rx) = tokio::sync::watch::channel(false);
    let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let web_paths = paths.clone();
    let web = tokio::spawn(async move {
        if let Err(error) = p2p_file::node::adapters::navigation::serve(
            web_paths,
            &configuration,
            restart,
            shutdown_rx,
        )
        .await
        {
            // A presentation failure must not tear down established forwards.
            tracing::error!(%error, "内置导航不可用，P2P 核心继续运行");
        }
    });
    let mut restart_rx = restart_rx;
    let result = loop {
        match agent::run_with_restart(paths.clone(), managed, restart_rx.clone()).await {
            Ok(true) => {
                restart_rx.borrow_and_update();
            }
            Ok(false) => break Ok(()),
            Err(error) => break Err(error),
        }
    };
    let _ = shutdown.send(true);
    let abort = web.abort_handle();
    if tokio::time::timeout(std::time::Duration::from_secs(45), web)
        .await
        .is_err()
    {
        abort.abort();
    }
    result
}

//! Native management by default; historical embedded desktop is explicit.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use clap::Parser;
#[derive(Parser)]
#[command(about = "P2P File desktop · 原生节点管理", disable_version_flag = true)]
struct Args {
    #[arg(long)]
    background: bool,
    #[arg(long)]
    legacy: bool,
    #[arg(long, conflicts_with = "legacy")]
    data_dir: Option<std::path::PathBuf>,
    #[arg(long, short = 'V')]
    version: bool,
    #[arg(long)]
    build_info: bool,
}
fn main() {
    let args = Args::parse();
    if args.version {
        println!(
            "p2p-desktop {} ({}; {}; {})",
            env!("CARGO_PKG_VERSION"),
            env!("P2P_BUILD_SHA"),
            env!("P2P_BUILD_TARGET"),
            env!("P2P_BUILD_STATE")
        );
        return;
    }
    if args.build_info {
        println!(
            "{}",
            serde_json::json!({"name":"p2p-desktop","version":env!("CARGO_PKG_VERSION"),"build_sha":env!("P2P_BUILD_SHA"),"target":env!("P2P_BUILD_TARGET"),"source_state":env!("P2P_BUILD_STATE")})
        );
        return;
    }
    if args.legacy {
        p2p_file::desktop::run(args.background);
        return;
    }
    let root = match args.data_dir {
        Some(path) => path,
        None => match p2p_file::node::agent_paths::AgentPaths::default_root() {
            Ok(path) => path,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
    };
    p2p_file::desktop::agent_shell::run(root, args.background);
}

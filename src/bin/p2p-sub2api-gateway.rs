//! Dedicated password-authorized Sub2API gateway, isolated from the user's node.
use clap::Parser;
use p2p_file::node::products::sub2api as restricted_sub2api;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Veytrane Gateway for Sub2API · 专用服务网关")]
struct Args {
    #[arg(long)]
    data_dir: PathBuf,
    /// Initialize a new isolated identity; read the dedicated password from stdin.
    #[arg(long)]
    init: bool,
    #[arg(long, default_value = "")]
    signal_server: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.init {
        let mut password = String::new();
        std::io::stdin().read_line(&mut password)?;
        let id = restricted_sub2api::initialize_gateway(
            &args.data_dir,
            args.signal_server,
            password.trim_end().to_owned(),
        )?;
        println!("{id}");
        return Ok(());
    }
    restricted_sub2api::run_gateway(&args.data_dir).await?;
    Ok(())
}

//! Fixed, console-based client: no CLI configuration, Web UI or management IPC.
#[tokio::main]
async fn main() {
    if std::env::args_os().len() != 1 {
        eprintln!("此专用版不接受配置参数。请直接双击 Start-Sub2API.bat。");
        std::process::exit(2);
    }
    let profile = include_str!(concat!(env!("OUT_DIR"), "/sub2api-profile.json"));
    if let Err(error) = p2p_file::node::products::sub2api::run_client(profile).await {
        eprintln!("连接程序已停止：{error}");
        std::process::exit(1);
    }
}

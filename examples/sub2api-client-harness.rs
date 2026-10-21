//! Operator-only fault-test harness. Never included in recipient ZIPs.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("private test profile path required")?;
    let profile = std::fs::read_to_string(path)?;
    p2p_file::node::products::sub2api::run_client(&profile).await?;
    Ok(())
}

use clap::Parser;
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match p2p_file::node::management_cli::run(p2p_file::node::management_cli::Args::parse()).await {
        Ok(response) => {
            println!(
                "{}",
                serde_json::to_string(&response).expect("JSON response")
            );
            if response.error.is_none() {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            }
        }
        Err(error) => {
            let response = p2p_file::node::ipc::Response::error("local", "local_error", error);
            println!("{}", serde_json::to_string(&response).expect("JSON error"));
            std::process::ExitCode::FAILURE
        }
    }
}

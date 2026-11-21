//! Typed local management; never starts a second networking runtime.
use crate::node::host::configuration::ownership_status;
pub use crate::node::host::configuration::{initialize, offline_apply, read_json};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::{
    io::{self, Read},
    path::PathBuf,
};
#[cfg(feature = "agent-files")]
use veytrane_files::cli::FileAction;
use veytrane_management::client::AgentClient;
use veytrane_management::paths::AgentPaths;
use veytrane_management::protocol::{Command, Request, Response, SpeedtestDirection};
use veytrane_transport::operation_id::TaskId;

#[derive(Parser)]
#[command(about = "Veytrane Node · 管理同一后台 Agent", version)]
pub struct Args {
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Action,
}
#[derive(Subcommand)]
pub enum Action {
    Init,
    /// Prove directory, identity and task ownership are all released.
    OwnershipStatus,
    Web {
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: std::net::SocketAddr,
    },
    Status,
    /// List untrusted LAN discovery hints and the actual direct listening addresses.
    Nearby,
    /// Dial a literal IP:port, pinning the expected full NodeId. IPv6 uses [IP]:port.
    ConnectAddress {
        peer: String,
        #[arg(required = true)]
        addresses: Vec<std::net::SocketAddr>,
        #[arg(long)]
        password_stdin: bool,
    },
    Config,
    Services {
        peer: String,
    },
    Connect {
        peer: String,
        #[arg(long)]
        password_stdin: bool,
    },
    Disconnect {
        peer: String,
    },
    #[cfg(feature = "agent-files")]
    #[command(flatten)]
    File(FileAction),
    Speed {
        peer: String,
        #[arg(long, default_value_t = 30)]
        seconds: u16,
        #[arg(long)]
        download: bool,
    },
    StopSpeed {
        peer: String,
        task: String,
    },
    StartForward {
        rule: String,
    },
    StopForward {
        rule: String,
    },
    Password {
        #[arg(long)]
        revision: u64,
    },
    TrustVerified {
        peer: String,
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        name: String,
    },
    Apply {
        #[arg(long)]
        restart: bool,
        file: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// Agent must be stopped; preserves the existing verifier and identity.
    OfflineApply {
        file: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// JSON request with a caller-controlled ID; '-' reads stdin.
    Request {
        file: PathBuf,
    },
    Stop,
}
fn password_stdin() -> io::Result<String> {
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    io::stdin().take(1025).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 {
        return Err(io::Error::other("密码输入过长"));
    }
    let value = String::from_utf8(bytes.to_vec()).map_err(io::Error::other)?;
    Ok(value.trim_end_matches(['\r', '\n']).to_owned())
}
pub async fn run(args: Args) -> io::Result<Response> {
    let root = match args.data_dir {
        Some(root) => root,
        None => AgentPaths::default_root()?,
    };
    let paths = AgentPaths::new(&root)?;
    let command = match args.command {
        Action::OwnershipStatus => {
            return Ok(Response::ok("ownership_status", ownership_status(&paths)?));
        }
        Action::Web { listen } => {
            veytrane_web_management::web::run(paths, listen).await?;
            return Ok(Response::ok("web", json!({"stopped":true})));
        }
        Action::Init => return Ok(Response::ok("init", initialize(&paths)?)),
        Action::OfflineApply { file, revision } => {
            return Ok(Response::ok(
                "offline_apply",
                offline_apply(&paths, &file, revision)?,
            ));
        }
        Action::Request { file } => {
            return AgentClient::new(paths)
                .request(read_json::<Request>(&file)?)
                .await;
        }
        Action::Status => Command::Status,
        Action::Nearby => Command::Nearby,
        Action::ConnectAddress {
            peer,
            addresses,
            password_stdin: use_password,
        } => Command::ConnectAddress {
            peer,
            addresses,
            password: if use_password {
                Some(password_stdin()?)
            } else {
                None
            },
        },
        Action::Config => Command::Config,
        Action::Services { peer } => Command::Services { peer },
        Action::Connect {
            peer,
            password_stdin: use_password,
        } => Command::Connect {
            peer,
            password: if use_password {
                Some(password_stdin()?)
            } else {
                None
            },
        },
        Action::Disconnect { peer } => Command::Disconnect { peer },
        #[cfg(feature = "agent-files")]
        Action::File(action) => action.into_command()?,
        Action::Speed {
            peer,
            seconds,
            download,
        } => Command::StartSpeed {
            peer,
            seconds,
            direction: if download {
                SpeedtestDirection::Download
            } else {
                SpeedtestDirection::Upload
            },
        },
        Action::StopSpeed { peer, task } => Command::StopSpeed {
            peer,
            task: TaskId::parse(&task).map_err(io::Error::other)?,
        },
        Action::StartForward { rule } => Command::StartForward { rule },
        Action::StopForward { rule } => Command::StopForward { rule },
        Action::Password { revision } => Command::SetPassword {
            expected_revision: revision,
            password: password_stdin()?,
        },
        Action::TrustVerified {
            peer,
            generation,
            revision,
            name,
        } => Command::TrustVerified {
            expected_revision: revision,
            peer,
            generation,
            name,
        },
        Action::Apply {
            file,
            revision,
            restart,
        } => {
            if restart {
                Command::ConfigureRestart {
                    expected_revision: revision,
                    settings: read_json(&file)?,
                }
            } else {
                Command::ApplyConfig {
                    expected_revision: revision,
                    settings: read_json(&file)?,
                }
            }
        }
        Action::Stop => Command::Stop,
    };
    AgentClient::new(paths).execute(command).await
}

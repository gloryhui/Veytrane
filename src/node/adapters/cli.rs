//! Typed local management; never starts a second networking runtime.
use crate::node::api::client::AgentClient;
use crate::node::api::protocol::{self as ipc, Command, Request, Response};
use crate::node::core::settings::{ConfigRecord, NodeSettings};
use crate::node::core::task_model::TaskId;
use crate::node::local::paths::AgentPaths;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "管理同一后台 P2P Agent", version)]
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
    Config,
    Tasks {
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
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
    Send {
        peer: String,
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    Pause {
        task: String,
    },
    PreviewDirectory {
        peer: String,
        path: PathBuf,
    },
    Resume {
        peer: String,
        task: String,
    },
    Cancel {
        task: String,
    },
    ConfirmDirectory {
        peer: String,
        request: String,
    },
    CancelDirectory {
        request: String,
    },
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
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    let input: Box<dyn Read> = if path == Path::new("-") {
        Box::new(io::stdin())
    } else {
        Box::new(std::fs::File::open(path)?)
    };
    input
        .take(ipc::FRAME_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > ipc::FRAME_LIMIT {
        return Err(io::Error::other("输入超过1MiB"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
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
pub fn initialize(paths: &AgentPaths) -> io::Result<Value> {
    let _lock =
        crate::instance_lock::InstanceLock::acquire(&paths.lock()).map_err(io::Error::other)?;
    if paths.config().try_exists()? {
        return Err(io::Error::other("节点配置已存在"));
    }
    let identity =
        crate::identity::Identity::load_or_create(&paths.identity()).map_err(io::Error::other)?;
    let _identity = AgentPaths::identity_lock(identity.node_id())?;
    let receive = paths.root.join("receive");
    crate::storage::permissions::ensure_directory(&receive)?;
    ConfigRecord::initialize(&paths.config(), receive).map_err(io::Error::other)?;
    Ok(json!({"node_id":identity.node_id().to_hex(),"config":paths.config()}))
}
pub fn offline_apply(paths: &AgentPaths, file: &Path, expected: u64) -> io::Result<Value> {
    let _lock =
        crate::instance_lock::InstanceLock::acquire(&paths.lock()).map_err(io::Error::other)?;
    let identity = crate::identity::Identity::load(&paths.identity()).map_err(io::Error::other)?;
    let _identity = AgentPaths::identity_lock(identity.node_id())?;
    let mut record = ConfigRecord::load(&paths.config()).map_err(io::Error::other)?;
    if record.revision != expected {
        return Err(io::Error::other(format!(
            "revision_conflict: 当前revision为{}",
            record.revision
        )));
    }
    let mut value: Value = read_json(file)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| io::Error::other("配置草稿必须是对象"))?;
    if object.contains_key("remote_auth") {
        return Err(io::Error::other("草稿不得包含验证器"));
    }
    object.insert(
        "remote_auth".into(),
        serde_json::to_value(&record.settings.remote_auth).map_err(io::Error::other)?,
    );
    let mut settings: NodeSettings = serde_json::from_value(value).map_err(io::Error::other)?;
    for device in &settings.trusted_devices {
        let source = settings
            .trust_sources
            .get(&device.node_id)
            .ok_or_else(|| io::Error::other("设备缺少来源"))?;
        if record.settings.trust_sources.get(&device.node_id) != Some(source) {
            settings.trust_sources.insert(
                device.node_id.clone(),
                crate::node::core::authorization::TrustSource::Administrator,
            );
        }
    }
    settings.validate().map_err(io::Error::other)?;
    record.settings = settings;
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or_else(|| io::Error::other("revision已耗尽"))?;
    record.last_request = None;
    record.validate().map_err(io::Error::other)?;
    let outcome = crate::storage::atomic::write_json(&paths.config(), &record)?;
    Ok(json!({"revision":record.revision,"durability_warning":outcome.durability_warning}))
}
pub async fn run(args: Args) -> io::Result<Response> {
    let root = match args.data_dir {
        Some(root) => root,
        None => AgentPaths::default_root()?,
    };
    let paths = AgentPaths::new(&root)?;
    let command = match args.command {
        Action::OwnershipStatus => {
            let _directory = crate::instance_lock::InstanceLock::acquire(&paths.lock())
                .map_err(io::Error::other)?;
            let identity = if paths.identity().try_exists()? {
                Some(crate::identity::Identity::load(&paths.identity()).map_err(io::Error::other)?)
            } else {
                None
            };
            let _identity = identity
                .as_ref()
                .map(|identity| AgentPaths::identity_lock(identity.node_id()))
                .transpose()?;
            let _tasks =
                crate::instance_lock::InstanceLock::acquire(&paths.root.join("tasks.lock"))
                    .map_err(io::Error::other)?;
            return Ok(Response::ok(
                "ownership_status",
                json!({"running":false,"node_id":identity.map(|id|id.node_id().to_hex())}),
            ));
        }
        Action::Web { listen } => {
            crate::node::adapters::web::run(paths, listen).await?;
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
        Action::Config => Command::Config,
        Action::Tasks { offset } => Command::Tasks { offset },
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
        Action::Send { peer, paths } => Command::Send {
            peer,
            paths: paths
                .into_iter()
                .map(std::path::absolute)
                .collect::<io::Result<_>>()?,
        },
        Action::Pause { task } => Command::Pause {
            task: TaskId::parse(&task).map_err(io::Error::other)?,
        },
        Action::PreviewDirectory { peer, path } => Command::PreviewDirectory {
            peer,
            path: std::path::absolute(path)?,
        },
        Action::Resume { peer, task } => Command::Resume {
            peer,
            task: TaskId::parse(&task).map_err(io::Error::other)?,
        },
        Action::Cancel { task } => Command::Cancel {
            task: TaskId::parse(&task).map_err(io::Error::other)?,
        },
        Action::ConfirmDirectory { peer, request } => Command::ConfirmDirectory {
            peer,
            request: TaskId::parse(&request).map_err(io::Error::other)?,
        },
        Action::CancelDirectory { request } => Command::CancelDirectory {
            request: TaskId::parse(&request).map_err(io::Error::other)?,
        },
        Action::Speed {
            peer,
            seconds,
            download,
        } => Command::StartSpeed {
            peer,
            seconds,
            direction: if download {
                crate::node::core::config::SpeedtestDirection::Download
            } else {
                crate::node::core::config::SpeedtestDirection::Upload
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

//! Explicit construction of the production session and its capability handles.

use crate::identity::Identity;
use crate::node::composition::business::StaticBusiness;
use crate::node::core::control::{self, ControlEvents, NodeControl, NodeSessionConfig};
use crate::node::core::session::SessionServices;
use std::io;
#[cfg(feature = "agent-files")]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::watch;
#[cfg(feature = "agent-files")]
use veytrane_files::{task_store::TaskStore, transfer::TransferService};
use veytrane_node::{activity::Activity, frame_budget::FrameBudget};
use veytrane_speed::runtime::SpeedService;

struct SharedRuntime {
    activity: Activity,
    changed: watch::Sender<u64>,
    frame_budget: FrameBudget,
}

impl SharedRuntime {
    fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            activity: Activity::new(changed.clone()),
            changed,
            frame_budget: FrameBudget::default(),
        }
    }

    fn speed(&self) -> SpeedService {
        SpeedService::new(
            self.activity.clone(),
            self.changed.clone(),
            self.frame_budget.clone(),
        )
    }
}

pub struct RuntimeAssembly {
    pub control: NodeControl,
    pub events: ControlEvents,
    pub speed: Option<SpeedService>,
    #[cfg(feature = "agent-files")]
    pub file_service: Option<TransferService>,
}

pub fn spawn(
    identity: Identity,
    config: NodeSessionConfig,
) -> io::Result<(NodeControl, ControlEvents)> {
    let assembly = spawn_assembled(identity, config)?;
    Ok((assembly.control, assembly.events))
}

pub(crate) fn spawn_assembled(
    identity: Identity,
    config: NodeSessionConfig,
) -> io::Result<RuntimeAssembly> {
    let speed = config.enable_speed.then(|| SharedRuntime::new().speed());
    let tcp_streams = Arc::new(veytrane_tcp::ForwardStreamOwner::new(
        veytrane_tcp::DEFAULT_MAX_NODE_TCP_STREAMS,
    ));
    let business = Arc::new(StaticBusiness::new(speed.clone(), tcp_streams.clone()));
    let (control, events) = control::spawn(
        identity,
        config,
        SessionServices {
            speed: speed.clone(),
            business,
            tcp_streams,
        },
    )?;
    Ok(RuntimeAssembly {
        control,
        events,
        speed,
        #[cfg(feature = "agent-files")]
        file_service: None,
    })
}

#[cfg(feature = "agent-files")]
pub fn spawn_with_storage(
    identity: Identity,
    mut config: NodeSessionConfig,
    tasks: &Path,
    receive_root: PathBuf,
) -> crate::error::Result<RuntimeAssembly> {
    if !config.enable_files {
        return spawn_assembled(identity, config).map_err(crate::error::Error::Io);
    }
    let shared = SharedRuntime::new();
    let (store, _) =
        TaskStore::open(tasks).map_err(|error| crate::error::Error::Protocol(error.to_string()))?;
    let speed = config.enable_speed.then(|| shared.speed());
    let file_service = TransferService::new_with_shared(
        store,
        receive_root,
        shared.activity,
        shared.changed,
        shared.frame_budget,
    );
    config.transfer = Some(file_service.clone());
    let tcp_streams = Arc::new(veytrane_tcp::ForwardStreamOwner::new(
        veytrane_tcp::DEFAULT_MAX_NODE_TCP_STREAMS,
    ));
    let business = Arc::new(StaticBusiness::with_files(
        speed.clone(),
        Some(file_service.clone()),
        tcp_streams.clone(),
    ));
    let (control, events) = control::spawn(
        identity,
        config,
        SessionServices {
            speed: speed.clone(),
            business,
            tcp_streams,
        },
    )
    .map_err(crate::error::Error::Io)?;
    Ok(RuntimeAssembly {
        control,
        events,
        speed,
        file_service: Some(file_service),
    })
}

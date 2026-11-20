//! Explicit construction of the production session and its capability handles.

use crate::identity::Identity;
use crate::node::core::control::{self, ControlEvents, NodeControl, NodeSessionConfig};
use std::io;
#[cfg(feature = "agent-files")]
use std::path::{Path, PathBuf};
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

pub fn spawn(
    identity: Identity,
    config: NodeSessionConfig,
) -> io::Result<(NodeControl, ControlEvents)> {
    let speed = config.enable_speed.then(|| SharedRuntime::new().speed());
    control::spawn(identity, config, speed)
}

#[cfg(feature = "agent-files")]
pub fn spawn_with_storage(
    identity: Identity,
    mut config: NodeSessionConfig,
    tasks: &Path,
    receive_root: PathBuf,
) -> crate::error::Result<(NodeControl, ControlEvents)> {
    if !config.enable_files {
        return spawn(identity, config).map_err(crate::error::Error::Io);
    }
    let shared = SharedRuntime::new();
    let (store, _) = TaskStore::open(tasks)
        .map_err(|error| crate::error::Error::Protocol(error.to_string()))?;
    let speed = config.enable_speed.then(|| shared.speed());
    config.transfer = Some(TransferService::new_with_shared(
        store,
        receive_root,
        shared.activity,
        shared.changed,
        shared.frame_budget,
    ));
    control::spawn(identity, config, speed).map_err(crate::error::Error::Io)
}

#![cfg(test)]

use std::sync::Arc;

use super::{Identity, NodeSessionConfig, NodeSessionHandle, SessionEvent, SessionServices, mpsc};
use crate::node::composition::business::StaticBusiness;
use crate::node::core::speed::SpeedService;

pub(super) fn spawn(
    identity: Identity,
    config: NodeSessionConfig,
    speed: Option<SpeedService>,
) -> std::io::Result<(NodeSessionHandle, mpsc::Receiver<SessionEvent>)> {
    let tcp_streams = Arc::new(veytrane_tcp::ForwardStreamOwner::new(
        veytrane_tcp::DEFAULT_MAX_NODE_TCP_STREAMS,
    ));
    #[cfg(feature = "agent-files")]
    let business =
        StaticBusiness::with_files(speed.clone(), config.transfer.clone(), tcp_streams.clone());
    #[cfg(not(feature = "agent-files"))]
    let business = StaticBusiness::new(speed.clone(), tcp_streams.clone());
    super::spawn(
        identity,
        config,
        SessionServices {
            speed,
            business: Arc::new(business),
            tcp_streams,
        },
    )
}

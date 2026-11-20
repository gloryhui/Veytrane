use std::future::Future;

use tokio::sync::watch;

use veytrane_node::dispatch::RegisteredIncoming;

use crate::identity::NodeId;
use crate::node::core::authorization::ResourcePolicy;
use crate::node::core::config::AllowedForwardTarget;
use crate::node::core::remote_auth::LiveAuthorization;

pub(crate) struct BusinessContext {
    pub connection: quinn::Connection,
    pub peer: NodeId,
    #[cfg(feature = "node-speed")]
    pub local: NodeId,
    pub authorization: LiveAuthorization,
    pub targets: watch::Receiver<Vec<AllowedForwardTarget>>,
    pub resources: watch::Receiver<ResourcePolicy>,
    pub incoming: RegisteredIncoming,
}

pub(crate) struct BusinessOutcome {
    /// Speed/files ended with an error. TCP errors keep the existing non-diagnostic behavior.
    pub report_end: bool,
}

pub(crate) trait BusinessHandler: Send + Sync + 'static {
    fn serve(&self, context: BusinessContext) -> impl Future<Output = BusinessOutcome> + Send;
}

//! Static inbound business assembly: speed, files and TCP share one authenticated connection.

use std::sync::Arc;

#[cfg(feature = "node-speed")]
use tokio::sync::Semaphore;

use crate::node::core::session::business::{BusinessContext, BusinessHandler, BusinessOutcome};
#[cfg(feature = "node-speed")]
use crate::node::core::speed::{DEFAULT_MAX_PEER_BUSINESS_STREAMS, SpeedService};
#[cfg(feature = "node-speed")]
use veytrane_speed::runtime::AuthenticatedPeer;

#[cfg(feature = "agent-files")]
use veytrane_files::transfer::TransferService;

pub(crate) struct StaticBusiness {
    #[cfg(feature = "node-speed")]
    speed: Option<SpeedService>,
    tcp_streams: Arc<veytrane_tcp::ForwardStreamOwner>,
    #[cfg(feature = "agent-files")]
    files: Option<TransferService>,
}

impl StaticBusiness {
    #[cfg(feature = "node-speed")]
    pub(crate) fn new(
        speed: Option<SpeedService>,
        tcp_streams: Arc<veytrane_tcp::ForwardStreamOwner>,
    ) -> Self {
        Self {
            speed,
            tcp_streams,
            #[cfg(feature = "agent-files")]
            files: None,
        }
    }

    #[cfg(not(feature = "node-speed"))]
    pub(crate) fn new(tcp_streams: Arc<veytrane_tcp::ForwardStreamOwner>) -> Self {
        Self {
            tcp_streams,
            #[cfg(feature = "agent-files")]
            files: None,
        }
    }

    #[cfg(feature = "agent-files")]
    pub(crate) fn with_files(
        speed: Option<SpeedService>,
        files: Option<TransferService>,
        tcp_streams: Arc<veytrane_tcp::ForwardStreamOwner>,
    ) -> Self {
        Self {
            speed,
            files,
            tcp_streams,
        }
    }
}

impl BusinessHandler for StaticBusiness {
    async fn serve(&self, context: BusinessContext) -> BusinessOutcome {
        let BusinessContext {
            connection,
            peer,
            #[cfg(feature = "node-speed")]
            local,
            authorization,
            targets,
            resources,
            incoming,
        } = context;
        let handle_tcp = async {
            if let Some(streams) = incoming.services {
                veytrane_tcp::services::serve(
                    peer,
                    authorization.clone(),
                    targets,
                    resources.clone(),
                    streams,
                    self.tcp_streams.clone(),
                )
                .await
            } else {
                Ok::<(), veytrane_transport::error::Error>(())
            }
        };

        #[cfg(feature = "node-speed")]
        let report_end = {
            let business_budget = (incoming.files.is_some() || incoming.speed.is_some())
                .then(|| Arc::new(Semaphore::new(DEFAULT_MAX_PEER_BUSINESS_STREAMS)));
            let handle_speed = async {
                if let (Some(service), Some(streams), Some(budget)) =
                    (self.speed.as_ref(), incoming.speed, &business_budget)
                {
                    service
                        .serve_peer(
                            AuthenticatedPeer {
                                connection: connection.clone(),
                                local,
                                remote: peer,
                            },
                            authorization.clone(),
                            resources.clone(),
                            budget.clone(),
                            streams,
                        )
                        .await
                } else {
                    Ok(())
                }
            };
            let handle_file = async {
                #[cfg(feature = "agent-files")]
                {
                    if let (Some(service), Some(streams), Some(budget)) =
                        (self.files.as_ref(), incoming.files, &business_budget)
                    {
                        service
                            .serve_files_dispatched(
                                connection.clone(),
                                peer,
                                authorization.clone(),
                                streams,
                                resources.clone(),
                                budget.clone(),
                            )
                            .await
                    } else {
                        Ok(())
                    }
                }
                #[cfg(not(feature = "agent-files"))]
                {
                    drop(incoming.files);
                    Ok::<(), veytrane_transport::error::Error>(())
                }
            };

            let (speed_result, file_result, _tcp_result, _) = tokio::join!(
                biased;
                handle_speed,
                handle_file,
                handle_tcp,
                connection.closed(),
            );
            speed_result.is_err() || file_result.is_err()
        };
        #[cfg(not(feature = "node-speed"))]
        let report_end = {
            drop((incoming.files, incoming.speed));
            let _ = tokio::join!(biased; handle_tcp, connection.closed());
            false
        };
        BusinessOutcome { report_end }
    }
}

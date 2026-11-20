//! Static inbound business assembly: speed, files and TCP share one authenticated connection.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::node::core::session::business::{BusinessContext, BusinessHandler, BusinessOutcome};
use crate::node::core::speed::{DEFAULT_MAX_PEER_BUSINESS_STREAMS, SpeedService};

#[cfg(feature = "agent-files")]
use veytrane_files::transfer::TransferService;

pub(crate) struct StaticBusiness {
    speed: Option<SpeedService>,
    tcp_streams: Arc<veytrane_tcp::ForwardStreamOwner>,
    #[cfg(feature = "agent-files")]
    files: Option<TransferService>,
}

impl StaticBusiness {
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
    fn serve(&self, context: BusinessContext) -> impl Future<Output = BusinessOutcome> + Send {
        async move {
            let BusinessContext {
                connection,
                peer,
                local,
                authorization,
                targets,
                resources,
                incoming,
            } = context;
            let business_budget = (incoming.files.is_some() || incoming.speed.is_some())
                .then(|| Arc::new(Semaphore::new(DEFAULT_MAX_PEER_BUSINESS_STREAMS)));

            let handle_speed = async {
                if let (Some(service), Some(streams), Some(budget)) =
                    (self.speed.as_ref(), incoming.speed, &business_budget)
                {
                    service
                        .serve_peer(
                            connection.clone(),
                            peer,
                            local,
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

            let (speed_result, file_result, _tcp_result, _) = tokio::join!(
                biased;
                handle_speed,
                handle_file,
                handle_tcp,
                connection.closed(),
            );
            BusinessOutcome {
                report_end: speed_result.is_err() || file_result.is_err(),
            }
        }
    }
}

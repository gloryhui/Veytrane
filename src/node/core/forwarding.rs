//! Adapts a node session to the TCP forwarding listener runtime.

use crate::error::{Error, Result};
use crate::node::core::control::NodeControl;
use crate::node::core::settings::NodeSettings;
use veytrane_tcp::forwarding::{PeerConnection, Prepared};

pub(crate) use veytrane_tcp::forwarding::ForwardManager;

pub(crate) async fn prepare(
    manager: &ForwardManager,
    settings: &NodeSettings,
    control: &NodeControl,
    start: Option<&str>,
) -> Result<Prepared> {
    manager
        .prepare(
            &settings.forwards,
            &settings.service_ids,
            start,
            |peer| async move {
                let updates = control
                    .connection_updates(peer)
                    .await
                    .map_err(Error::Protocol)?;
                let control = control.clone();
                Ok(PeerConnection::new(updates, move || {
                    control
                        .request_peer_connection(peer)
                        .map_err(Error::Protocol)
                }))
            },
        )
        .await
}

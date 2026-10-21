//! Persisted host preferences; platform behavior belongs to the host adapter.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundOptions {
    #[serde(default)]
    pub close_to_tray: bool,
    #[serde(default)]
    pub launch_at_login: bool,
}

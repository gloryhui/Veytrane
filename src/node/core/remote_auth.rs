//! Compatibility paths for the dedicated node authentication implementation.

pub use veytrane_node::live_authorization::{
    AuthorizationGrant, AuthorizationPublisher, LiveAuthorization, RemoteAuthorization,
    RevocationEpochs,
};
pub use veytrane_node::remote_auth::{
    AuthContext, AuthorizedSession, Challenge, RemoteVerifier, SecretPassword, TrustedControl,
};

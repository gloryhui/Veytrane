//! Live remote-authorization grants, revocation epochs and watch interruption.

use std::sync::Arc;

use tokio::sync::watch;

use veytrane_transport::error::{Error, Result};

/// Rights belong to the authenticated transport and expire with it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AuthorizationGrant {
    pub password: bool,
    pub trusted_device: bool,
}

impl AuthorizationGrant {
    pub const fn password(password: bool) -> Self {
        Self {
            password,
            trusted_device: false,
        }
    }

    pub const fn authorized(self) -> bool {
        self.password || self.trusted_device
    }

    pub fn label(self) -> &'static str {
        match (self.password, self.trusted_device) {
            (true, true) => "密码及可信设备",
            (true, false) => "密码",
            (false, true) => "可信设备",
            (false, false) => "未授权",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RemoteAuthorization {
    pub inbound: AuthorizationGrant,
    pub outbound: AuthorizationGrant,
}

impl RemoteAuthorization {
    #[cfg(any(test, feature = "test-support"))]
    pub const BOTH: Self = Self {
        inbound: AuthorizationGrant::password(true),
        outbound: AuthorizationGrant::password(true),
    };

    pub const fn inbound_authorized(self) -> bool {
        self.inbound.authorized()
    }

    pub const fn outbound_authorized(self) -> bool {
        self.outbound.authorized()
    }

    pub fn label(self) -> String {
        format!(
            "对端访问本机：{}；本机访问对端：{}",
            self.inbound.label(),
            self.outbound.label()
        )
    }
}

#[derive(Default)]
pub struct RevocationEpochs {
    inbound: std::sync::atomic::AtomicU64,
    outbound: std::sync::atomic::AtomicU64,
}

impl RevocationEpochs {
    fn get(&self, inbound: bool) -> u64 {
        if inbound {
            &self.inbound
        } else {
            &self.outbound
        }
        .load(std::sync::atomic::Ordering::Acquire)
    }

    fn changed(&self, before: RemoteAuthorization, after: RemoteAuthorization) {
        use std::sync::atomic::Ordering;
        if before.inbound_authorized() && !after.inbound_authorized() {
            self.inbound.fetch_add(1, Ordering::AcqRel);
        }
        if before.outbound_authorized() && !after.outbound_authorized() {
            self.outbound.fetch_add(1, Ordering::AcqRel);
        }
    }
}

/// A monotonic revocation epoch prevents watch coalescing from hiding revoke/regrant
/// from a running stream. Effective password grants do not advance that direction.
#[derive(Clone)]
pub struct AuthorizationPublisher {
    sender: watch::Sender<RemoteAuthorization>,
    epochs: Arc<RevocationEpochs>,
}

impl AuthorizationPublisher {
    pub fn new(authorization: RemoteAuthorization) -> Self {
        Self {
            sender: watch::channel(authorization).0,
            epochs: Arc::new(RevocationEpochs::default()),
        }
    }

    pub fn borrow(&self) -> watch::Ref<'_, RemoteAuthorization> {
        self.sender.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<RemoteAuthorization> {
        self.sender.subscribe()
    }

    pub fn live(&self) -> LiveAuthorization {
        LiveAuthorization::Live(self.subscribe(), self.epochs.clone())
    }

    pub fn send_modify(&self, update: impl FnOnce(&mut RemoteAuthorization)) {
        self.sender.send_modify(|authorization| {
            let old = *authorization;
            update(authorization);
            self.epochs.changed(old, *authorization);
        });
    }

    pub fn send_if_modified(&self, update: impl FnOnce(&mut RemoteAuthorization) -> bool) {
        self.sender.send_if_modified(|authorization| {
            let old = *authorization;
            let changed = update(authorization);
            if changed {
                self.epochs.changed(old, *authorization);
            }
            changed
        });
    }

    pub fn send_replace(&self, authorization: RemoteAuthorization) {
        self.send_modify(|a| *a = authorization);
    }
}

/// Readable rights belong to this connection, never to a peer cache or a new generation.
#[derive(Clone)]
pub enum LiveAuthorization {
    Live(watch::Receiver<RemoteAuthorization>, Arc<RevocationEpochs>),
    #[cfg(any(test, feature = "test-support"))]
    Fixed(RemoteAuthorization),
}

impl LiveAuthorization {
    pub fn current(&self) -> RemoteAuthorization {
        match self {
            Self::Live(receiver, _) => *receiver.borrow(),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fixed(a) => *a,
        }
    }

    pub async fn denied(&mut self, inbound: bool) {
        let epoch = match self {
            Self::Live(_, epochs) => epochs.get(inbound),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fixed(_) => 0,
        };
        loop {
            let authorized = if inbound {
                self.current().inbound_authorized()
            } else {
                self.current().outbound_authorized()
            };
            if !authorized {
                return;
            }
            match self {
                Self::Live(receiver, epochs) => {
                    if epochs.get(inbound) != epoch {
                        return;
                    }
                    if receiver.changed().await.is_err() {
                        return;
                    }
                }
                #[cfg(any(test, feature = "test-support"))]
                Self::Fixed(_) => std::future::pending::<()>().await,
            }
        }
    }

    pub async fn guard<T>(
        &self,
        inbound: bool,
        work: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        let mut rights = self.clone();
        tokio::select! { biased; _ = rights.denied(inbound) => Err(Error::Protocol("当前方向的远程授权已撤销".into())), result = work => result }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl From<RemoteAuthorization> for LiveAuthorization {
    fn from(a: RemoteAuthorization) -> Self {
        Self::Fixed(a)
    }
}

//! Per-peer shared/exclusive activity admission.
//!
//! Shared permits may coexist for one peer. An exclusive permit is admitted
//! only when that peer has no shared or exclusive permit. Permits cover cleanup
//! as well as the visible operation, and every release notifies the owner.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};
use tokio::sync::watch;
use veytrane_transport::identity::NodeId;

#[derive(Clone)]
pub struct Activity {
    state: Arc<Mutex<State>>,
    changed: watch::Sender<u64>,
}
#[derive(Default)]
pub struct State {
    peers: HashMap<NodeId, Entry>,
    next_token: u64,
}
#[derive(Default)]
struct Entry {
    shared: usize,
    exclusive: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityError {
    SharedInUse,
    ExclusiveInUse,
    SharedLimit,
    TokenExhausted,
}

pub struct SharedPermit {
    activity: Activity,
    peer: NodeId,
}

pub struct ExclusivePermit {
    activity: Activity,
    peer: NodeId,
    token: u64,
}

impl Activity {
    pub fn new(changed: watch::Sender<u64>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            changed,
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("activity state lock poisoned")
    }

    pub fn shared(&self, peer: NodeId) -> Result<SharedPermit, ActivityError> {
        self.lock().add_shared(peer)?;
        Ok(self.shared_permit(peer))
    }

    // Caller already incremented under the shared admission lock.
    pub fn shared_permit(&self, peer: NodeId) -> SharedPermit {
        SharedPermit {
            activity: self.clone(),
            peer,
        }
    }

    pub fn exclusive(&self, peer: NodeId) -> Result<ExclusivePermit, ActivityError> {
        let token = self.lock().add_exclusive(peer)?;
        Ok(ExclusivePermit {
            activity: self.clone(),
            peer,
            token,
        })
    }

    fn notify(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
}

impl State {
    pub fn can_exclusive(&self, peer: NodeId) -> bool {
        self.peers
            .get(&peer)
            .is_none_or(|entry| entry.shared == 0 && entry.exclusive.is_none())
    }

    pub fn exclusive_active(&self, peer: NodeId) -> bool {
        self.peers
            .get(&peer)
            .is_some_and(|entry| entry.exclusive.is_some())
    }

    pub fn add_shared(&mut self, peer: NodeId) -> Result<(), ActivityError> {
        let entry = self.peers.entry(peer).or_default();
        if entry.exclusive.is_some() {
            return Err(ActivityError::ExclusiveInUse);
        }
        entry.shared = entry
            .shared
            .checked_add(1)
            .ok_or(ActivityError::SharedLimit)?;
        Ok(())
    }

    fn add_exclusive(&mut self, peer: NodeId) -> Result<u64, ActivityError> {
        let token = self
            .next_token
            .checked_add(1)
            .ok_or(ActivityError::TokenExhausted)?;
        let entry = self.peers.entry(peer).or_default();
        if entry.shared != 0 {
            return Err(ActivityError::SharedInUse);
        }
        if entry.exclusive.is_some() {
            return Err(ActivityError::ExclusiveInUse);
        }
        self.next_token = token;
        entry.exclusive = Some(token);
        Ok(token)
    }

    fn prune(&mut self, peer: NodeId) {
        if self
            .peers
            .get(&peer)
            .is_some_and(|entry| entry.shared == 0 && entry.exclusive.is_none())
        {
            self.peers.remove(&peer);
        }
    }
}

impl Drop for SharedPermit {
    fn drop(&mut self) {
        {
            let mut state = self.activity.lock();
            let entry = state
                .peers
                .get_mut(&self.peer)
                .expect("shared activity entry missing");
            debug_assert!(entry.shared > 0, "shared activity count underflow");
            entry.shared -= 1;
            state.prune(self.peer);
        }
        self.activity.notify();
    }
}

impl Drop for ExclusivePermit {
    fn drop(&mut self) {
        {
            let mut state = self.activity.lock();
            if let Some(entry) = state.peers.get_mut(&self.peer)
                && entry.exclusive == Some(self.token)
            {
                entry.exclusive = None;
            }
            state.prune(self.peer);
        }
        self.activity.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(hex: &str) -> NodeId {
        NodeId::from_hex(hex).expect("test node id")
    }

    #[test]
    fn shared_and_exclusive_admission_is_atomic_per_peer_and_cleanup_notifies() {
        let (changed, mut changed_rx) = watch::channel(0);
        let activity = Activity::new(changed);
        let a = peer("00000000000000000000000000000001");
        let b = peer("00000000000000000000000000000002");

        let shared = activity.shared(a).expect("shared permit");
        assert_eq!(
            activity.exclusive(a).err(),
            Some(ActivityError::SharedInUse)
        );
        let exclusive = activity.exclusive(b).expect("exclusive permit");
        assert_eq!(
            activity.shared(b).err(),
            Some(ActivityError::ExclusiveInUse)
        );
        assert_eq!(
            activity.exclusive(b).err(),
            Some(ActivityError::ExclusiveInUse)
        );

        drop(shared);
        drop(exclusive);
        assert_eq!(*changed_rx.borrow_and_update(), 2);

        let exclusive = activity.exclusive(a).expect("exclusive permit");
        let shared = activity.shared(b).expect("shared permit");
        drop(exclusive);
        drop(shared);
        assert_eq!(*changed_rx.borrow_and_update(), 4);
        assert!(activity.lock().peers.is_empty());
    }

    #[test]
    fn stale_exclusive_release_does_not_clear_new_generation() {
        let (changed, _) = watch::channel(0);
        let activity = Activity::new(changed);
        let peer = peer("00000000000000000000000000000003");

        let stale = activity.exclusive(peer).expect("old exclusive permit");
        let stale_token = stale.token;
        // Simulate the old generation leaving the active slot before its
        // permit's Drop runs.
        activity
            .lock()
            .peers
            .get_mut(&peer)
            .expect("exclusive entry")
            .exclusive = None;
        let current = activity.exclusive(peer).expect("new exclusive permit");
        assert_ne!(stale_token, current.token);

        drop(stale);
        assert!(activity.lock().exclusive_active(peer));
        drop(current);
        assert!(activity.lock().peers.is_empty());
    }
}

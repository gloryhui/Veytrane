//! Network changes and bounded transport recovery; never changes persisted policy.
use super::*;
use std::{collections::BTreeSet, time::SystemTime};

pub(super) const SAMPLE: Duration = Duration::from_secs(5);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Attempt {
    generation: u64,
    terminal: bool,
    transport_attempted: bool,
    transport_ok: bool,
    auth_failed: bool,
    connected_before: bool,
    failures: u32,
    retry_at: Duration,
}

#[derive(Clone, Copy)]
enum RefreshReason {
    Wake,
    Interfaces,
    Retry,
}
impl RefreshReason {
    fn message(self) -> &'static str {
        match self {
            Self::Wake => "休眠唤醒或网络调度长时间停顿，刷新网络候选",
            Self::Interfaces => "本机网络接口变化且设备通道已失效，刷新网络候选",
            Self::Retry => "网络刷新失败后重试候选收集",
        }
    }
    fn can_refresh(self, healthy: bool) -> bool {
        !matches!(self, Self::Interfaces) || !healthy
    }
}

pub(super) struct Recovery {
    started: time::Instant,
    last_tick: time::Instant,
    last_wall: SystemTime,
    interfaces: Option<BTreeSet<String>>,
    candidate: Option<BTreeSet<String>>,
    pending_change: Option<RefreshReason>,
    last_refresh: Duration,
    failures: VecDeque<Duration>,
    attempts: HashMap<NodeId, Attempt>,
}
impl Default for Recovery {
    fn default() -> Self {
        Self {
            started: time::Instant::now(),
            last_tick: time::Instant::now(),
            last_wall: SystemTime::now(),
            interfaces: interfaces(),
            candidate: None,
            pending_change: None,
            last_refresh: Duration::ZERO,
            failures: VecDeque::new(),
            attempts: HashMap::new(),
        }
    }
}
impl Recovery {
    pub fn event(&mut self, event: &SessionEvent) {
        let SessionEvent::PeerState {
            peer,
            generation,
            state,
        } = event
        else {
            return;
        };
        if !self.attempts.contains_key(peer) && self.attempts.len() >= MAX_PEERS {
            self.attempts.retain(|_, a| a.connected_before);
            if self.attempts.len() >= MAX_PEERS {
                return;
            }
        }
        let a = self.attempts.entry(*peer).or_default();
        if a.generation > *generation {
            return;
        }
        if a.generation != *generation {
            a.generation = *generation;
            a.terminal = false;
            a.transport_attempted = false;
            a.transport_ok = false;
            a.auth_failed = false;
        }
        if a.terminal {
            return;
        }
        let now = self.started.elapsed();
        match state {
            PeerLifecycle::Punching | PeerLifecycle::Authenticating => a.transport_attempted = true,
            PeerLifecycle::Negotiating | PeerLifecycle::RemoteAuthPending => a.transport_ok = true,
            PeerLifecycle::Connected(_) => {
                a.connected_before = true;
                a.transport_ok = true;
                a.auth_failed = false;
                a.failures = 0;
                self.failures.clear();
            }
            PeerLifecycle::Failed(_) => {
                a.terminal = true;
                a.auth_failed = a.transport_ok;
                a.failures = a.failures.saturating_add(1);
                a.retry_at = now + reconnect_delay(a.failures, 0);
                if a.transport_attempted && !a.transport_ok {
                    if self.failures.len() == 3 {
                        self.failures.pop_front();
                    }
                    self.failures.push_back(now);
                }
            }
            PeerLifecycle::Disconnected => {
                a.terminal = true;
                a.retry_at = now + SAMPLE;
            }
            _ => {}
        }
    }
    pub fn retry_due(&mut self, peer: NodeId) -> bool {
        let now = self.started.elapsed();
        if !self.attempts.contains_key(&peer) && self.attempts.len() >= MAX_PEERS {
            return false;
        }
        let a = self.attempts.entry(peer).or_default();
        if a.auth_failed || now < a.retry_at {
            return false;
        }
        a.retry_at = now + Duration::from_secs(30);
        true
    }
    pub fn retry_refresh(&mut self) {
        self.pending_change = Some(RefreshReason::Retry);
    }
    pub fn poll(&mut self, healthy: bool) -> Option<&'static str> {
        if self.last_tick.elapsed() < SAMPLE {
            return None;
        }
        let wall = SystemTime::now();
        let gap = wall
            .duration_since(self.last_wall)
            .unwrap_or_default()
            .max(self.last_tick.elapsed());
        self.last_tick = time::Instant::now();
        self.last_wall = wall;
        self.sample(gap, interfaces(), healthy)
    }
    fn sample(
        &mut self,
        gap: Duration,
        current: Option<BTreeSet<String>>,
        healthy: bool,
    ) -> Option<&'static str> {
        if gap >= Duration::from_secs(30) {
            self.pending_change = Some(RefreshReason::Wake);
        }
        if let Some(current) = current {
            if self.interfaces.as_ref().is_none_or(|old| *old == current) {
                self.candidate = None;
                self.interfaces = Some(current);
            } else if self.candidate.as_ref() == Some(&current) {
                self.interfaces = Some(current);
                self.candidate = None;
                // A new Docker bridge, VPN adapter or temporary address does not
                // prove that an established QUIC path is broken. Keep live SSH /
                // TCP streams; refresh only after transport failure. A wake or
                // failed preparation has higher priority and must not be masked.
                self.pending_change.get_or_insert(RefreshReason::Interfaces);
            } else {
                self.candidate = Some(current);
            }
        } else {
            self.candidate = None;
        }
        let now = self.started.elapsed();
        self.failures
            .retain(|t| now.saturating_sub(*t) <= Duration::from_secs(90));
        // Interface enumeration is topology evidence, not liveness evidence.
        // Quinn's timeout/close determines when a path actually needs recovery.
        // Wake detection still forces new NAT mappings even with stale handles.
        let reason = self
            .pending_change
            .filter(|reason| reason.can_refresh(healthy))
            .map(RefreshReason::message)
            .or_else(|| {
                (!healthy && self.failures.len() >= 3)
                    .then_some("连续三次设备通道失败，刷新 UDP/STUN 候选")
            });
        if reason.is_some()
            && (self.last_refresh == Duration::ZERO
                || now.saturating_sub(self.last_refresh) >= REFRESH_COOLDOWN)
        {
            self.last_refresh = now.max(Duration::from_nanos(1));
            self.pending_change = None;
            self.failures.clear();
            return reason;
        }
        None
    }
}
fn interfaces() -> Option<BTreeSet<String>> {
    Some(
        if_addrs::get_if_addrs()
            .ok()?
            .into_iter()
            .filter(|i| i.is_oper_up() && !i.is_loopback() && !i.is_link_local())
            .map(|i| format!("{}:{}", i.name, i.ip()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn addresses(s: &str) -> Option<BTreeSet<String>> {
        Some(BTreeSet::from([s.into()]))
    }
    #[tokio::test(start_paused = true)]
    async fn wake_and_debounced_interfaces_refresh_with_cooldown() {
        let mut r = Recovery {
            interfaces: addresses("old"),
            ..Default::default()
        };
        assert!(r.sample(SAMPLE, addresses("new"), true).is_none());
        assert!(r.sample(SAMPLE, addresses("old"), true).is_none());
        assert!(
            r.sample(Duration::from_secs(31), addresses("old"), true)
                .is_some()
        );
        assert!(
            r.sample(Duration::from_secs(31), addresses("old"), true)
                .is_none()
        );
        time::advance(Duration::from_secs(61)).await;
        assert!(r.sample(SAMPLE, addresses("old"), true).is_some());
        assert!(r.sample(SAMPLE, addresses("new"), true).is_none());
        time::advance(Duration::from_secs(61)).await;
        assert!(r.sample(SAMPLE, addresses("new"), true).is_none());
        assert!(r.sample(SAMPLE, addresses("new"), false).is_some());
    }
    #[tokio::test(start_paused = true)]
    async fn transport_failures_refresh_without_looping_on_bad_credentials_or_evicting_healthy_peers()
     {
        for auth in [false, true] {
            let mut r = Recovery::default();
            let peer = Identity::generate().node_id();
            for generation in 1..=3 {
                for state in [
                    PeerLifecycle::Authenticating,
                    if auth {
                        PeerLifecycle::RemoteAuthPending
                    } else {
                        PeerLifecycle::Punching
                    },
                    PeerLifecycle::Failed("test".into()),
                ] {
                    r.event(&SessionEvent::PeerState {
                        peer,
                        generation,
                        state,
                    });
                }
            }
            assert!(r.sample(SAMPLE, None, true).is_none());
            assert_eq!(r.sample(SAMPLE, None, false).is_some(), !auth);
            time::advance(Duration::from_secs(31)).await;
            assert_eq!(r.retry_due(peer), !auth);
            assert!(!r.retry_due(peer));
        }
    }
    #[tokio::test(start_paused = true)]
    async fn stale_or_duplicate_failures_do_not_trigger_refresh_and_auth_retries_need_a_new_attempt()
     {
        let mut r = Recovery::default();
        let peer = Identity::generate().node_id();
        let state = |generation, state| SessionEvent::PeerState {
            peer,
            generation,
            state,
        };
        r.event(&state(2, PeerLifecycle::Punching));
        for _ in 0..4 {
            r.event(&state(1, PeerLifecycle::Failed("stale".into())));
        }
        for _ in 0..4 {
            r.event(&state(2, PeerLifecycle::Failed("duplicate".into())));
        }
        assert!(r.sample(SAMPLE, None, false).is_none());
        r.event(&state(3, PeerLifecycle::RemoteAuthPending));
        r.event(&state(3, PeerLifecycle::Failed("bad password".into())));
        time::advance(Duration::from_secs(120)).await;
        assert!(!r.retry_due(peer));
        r.event(&state(4, PeerLifecycle::PeerPending));
        assert!(r.retry_due(peer));
    }
    #[tokio::test(start_paused = true)]
    async fn docker_bridge_addition_and_removal_never_reset_live_connections() {
        let physical = "enp37s0:192.168.3.192".to_owned();
        let original = BTreeSet::from([physical.clone()]);
        let docker = BTreeSet::from([physical, "br-03e1380e27e7:172.23.0.1".to_owned()]);
        let mut r = Recovery {
            interfaces: Some(original.clone()),
            ..Default::default()
        };
        for current in [&docker, &docker, &docker, &original, &original, &original] {
            assert!(r.sample(SAMPLE, Some(current.clone()), true).is_none());
            time::advance(SAMPLE).await;
        }
        time::advance(Duration::from_secs(120)).await;
        assert!(r.sample(SAMPLE, Some(original), true).is_none());
        assert_eq!(
            r.last_refresh,
            Duration::ZERO,
            "container lifecycle must not tear down SSH"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn real_interface_change_refreshes_after_transport_loss_without_waiting_for_three_failures()
     {
        let mut r = Recovery {
            interfaces: addresses("en0:old"),
            ..Default::default()
        };
        assert!(r.sample(SAMPLE, addresses("en0:new"), true).is_none());
        assert!(r.sample(SAMPLE, addresses("en0:new"), true).is_none());
        assert_eq!(
            r.sample(SAMPLE, addresses("en0:new"), false),
            Some(RefreshReason::Interfaces.message())
        );
        assert!(r.sample(SAMPLE, addresses("en0:new"), false).is_none());
    }
    #[tokio::test(start_paused = true)]
    async fn simultaneous_interface_change_cannot_mask_wake_refresh_or_failed_preparation_retry() {
        for retry in [false, true] {
            let mut r = Recovery {
                interfaces: addresses("old"),
                ..Default::default()
            };
            assert!(r.sample(SAMPLE, addresses("new"), true).is_none());
            if retry {
                r.retry_refresh();
            }
            let gap = if retry {
                SAMPLE
            } else {
                Duration::from_secs(31)
            };
            let expected = if retry {
                RefreshReason::Retry
            } else {
                RefreshReason::Wake
            };
            assert_eq!(
                r.sample(gap, addresses("new"), true),
                Some(expected.message())
            );
        }
    }
}

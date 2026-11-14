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
    transport_failures: VecDeque<Duration>,
    last_rotation: Option<Duration>,
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
            attempts: HashMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryAction {
    Refresh(&'static str),
    Rotate(NodeId),
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
                a.transport_failures.clear();
            }
            PeerLifecycle::Failed(_) => {
                a.terminal = true;
                a.auth_failed = a.transport_ok;
                a.failures = a.failures.saturating_add(1);
                a.retry_at = now + reconnect_delay(a.failures, 0);
                if a.transport_attempted && !a.transport_ok {
                    while a
                        .transport_failures
                        .front()
                        .is_some_and(|at| now.saturating_sub(*at) > Duration::from_secs(90))
                    {
                        a.transport_failures.pop_front();
                    }
                    a.transport_failures.push_back(now);
                }
            }
            PeerLifecycle::Disconnected => {
                a.terminal = true;
                a.retry_at = now + SAMPLE;
                a.transport_failures.clear();
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
    pub fn auth_failed(&self, peer: NodeId) -> bool {
        self.attempts
            .get(&peer)
            .is_some_and(|attempt| attempt.auth_failed)
    }
    pub fn poll(&mut self, healthy: bool) -> Option<RecoveryAction> {
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
    ) -> Option<RecoveryAction> {
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
        for attempt in self.attempts.values_mut() {
            attempt
                .transport_failures
                .retain(|at| now.saturating_sub(*at) <= Duration::from_secs(90));
        }
        let transport_failures = self
            .attempts
            .values()
            .map(|attempt| attempt.transport_failures.len())
            .sum::<usize>();
        // Interface enumeration is topology evidence, not liveness evidence.
        // Quinn's timeout/close determines when a path actually needs recovery.
        // Wake detection still forces new NAT mappings even with stale handles.
        let refresh = self
            .pending_change
            .filter(|reason| reason.can_refresh(healthy))
            .map(RefreshReason::message)
            .or_else(|| {
                (!healthy && transport_failures >= 3)
                    .then_some("连续三次设备通道失败，刷新 UDP/STUN 候选")
            });
        if refresh.is_some()
            && (self.last_refresh == Duration::ZERO
                || now.saturating_sub(self.last_refresh) >= REFRESH_COOLDOWN)
        {
            self.last_refresh = now.max(Duration::from_nanos(1));
            self.pending_change = None;
            for attempt in self.attempts.values_mut() {
                attempt.transport_failures.clear();
            }
            return refresh.map(RecoveryAction::Refresh);
        }
        if healthy {
            let rotation = self
                .attempts
                .iter()
                .filter(|(_, attempt)| {
                    !attempt.auth_failed && attempt.transport_failures.len() >= 3
                })
                .filter(|(_, attempt)| {
                    !attempt
                        .last_rotation
                        .is_some_and(|last| now.saturating_sub(last) < REFRESH_COOLDOWN)
                })
                .min_by_key(|(_, attempt)| {
                    attempt
                        .transport_failures
                        .get(2)
                        .copied()
                        .unwrap_or(Duration::MAX)
                })
                .map(|(peer, _)| *peer);
            if let Some(peer) = rotation {
                let attempt = self.attempts.get_mut(&peer).expect("selected above");
                attempt.last_rotation = Some(now);
                attempt.transport_failures.clear();
                return Some(RecoveryAction::Rotate(peer));
            }
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
    fn peer_event(peer: NodeId, generation: u64, state: PeerLifecycle) -> SessionEvent {
        SessionEvent::PeerState {
            peer,
            generation,
            state,
        }
    }
    fn transport_failure(r: &mut Recovery, peer: NodeId, generation: u64) {
        r.event(&peer_event(peer, generation, PeerLifecycle::Authenticating));
        r.event(&peer_event(
            peer,
            generation,
            PeerLifecycle::Failed("no authenticated path".into()),
        ));
    }
    fn connected(r: &mut Recovery, peer: NodeId, generation: u64) {
        r.event(&peer_event(
            peer,
            generation,
            PeerLifecycle::Connected(crate::node::core::remote_auth::RemoteAuthorization::BOTH),
        ));
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
            assert_eq!(
                r.sample(SAMPLE, None, false),
                (!auth).then_some(RecoveryAction::Refresh(
                    "连续三次设备通道失败，刷新 UDP/STUN 候选"
                ))
            );
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
            Some(RecoveryAction::Refresh(RefreshReason::Interfaces.message()))
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
                Some(RecoveryAction::Refresh(expected.message()))
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn healthy_peer_selects_only_the_failed_peer_for_additive_rotation() {
        let mut r = Recovery::default();
        let healthy = Identity::generate().node_id();
        let failed = Identity::generate().node_id();
        connected(&mut r, healthy, 1);
        for generation in 1..=3 {
            transport_failure(&mut r, failed, generation);
        }
        assert_eq!(
            r.sample(SAMPLE, None, true),
            Some(RecoveryAction::Rotate(failed))
        );
        assert!(r.sample(SAMPLE, None, true).is_none());
        time::advance(Duration::from_secs(61)).await;
        for generation in 4..=6 {
            transport_failure(&mut r, failed, generation);
        }
        assert_eq!(
            r.sample(SAMPLE, None, true),
            Some(RecoveryAction::Rotate(failed))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn healthy_presence_never_turns_auth_failure_into_rotation() {
        let mut r = Recovery::default();
        let peer = Identity::generate().node_id();
        for generation in 1..=3 {
            r.event(&peer_event(
                peer,
                generation,
                PeerLifecycle::RemoteAuthPending,
            ));
            r.event(&peer_event(
                peer,
                generation,
                PeerLifecycle::Failed("bad password".into()),
            ));
        }
        assert!(r.sample(SAMPLE, None, true).is_none());
        assert!(r.auth_failed(peer));
    }

    #[test]
    fn another_peer_connecting_does_not_clear_the_failed_peer_window() {
        let mut r = Recovery::default();
        let failed = Identity::generate().node_id();
        let other = Identity::generate().node_id();
        connected(&mut r, other, 1);
        transport_failure(&mut r, failed, 1);
        transport_failure(&mut r, failed, 2);
        connected(&mut r, other, 2);
        transport_failure(&mut r, failed, 3);
        assert_eq!(
            r.sample(SAMPLE, None, true),
            Some(RecoveryAction::Rotate(failed))
        );
    }

    #[test]
    fn connecting_the_failed_peer_clears_only_that_peers_transport_window() {
        let mut r = Recovery::default();
        let first = Identity::generate().node_id();
        let second = Identity::generate().node_id();
        transport_failure(&mut r, first, 1);
        transport_failure(&mut r, first, 2);
        transport_failure(&mut r, second, 1);
        transport_failure(&mut r, second, 2);
        connected(&mut r, first, 3);
        transport_failure(&mut r, first, 4);
        transport_failure(&mut r, first, 5);
        assert!(r.sample(SAMPLE, None, true).is_none());
        transport_failure(&mut r, second, 3);
        assert_eq!(
            r.sample(SAMPLE, None, true),
            Some(RecoveryAction::Rotate(second))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn failures_older_than_ninety_seconds_do_not_count() {
        let mut r = Recovery::default();
        let peer = Identity::generate().node_id();
        transport_failure(&mut r, peer, 1);
        transport_failure(&mut r, peer, 2);
        time::advance(Duration::from_secs(91)).await;
        transport_failure(&mut r, peer, 3);
        assert!(r.sample(SAMPLE, None, true).is_none());
    }

    #[test]
    fn generation_fence_and_terminal_flag_ignore_stale_and_duplicate_failures() {
        let mut r = Recovery::default();
        let peer = Identity::generate().node_id();
        r.event(&peer_event(peer, 2, PeerLifecycle::Authenticating));
        r.event(&peer_event(peer, 1, PeerLifecycle::Failed("stale".into())));
        for _ in 0..4 {
            r.event(&peer_event(
                peer,
                2,
                PeerLifecycle::Failed("duplicate".into()),
            ));
        }
        assert_eq!(
            r.attempts
                .get(&peer)
                .map(|attempt| attempt.transport_failures.len()),
            Some(1)
        );
        assert!(r.sample(SAMPLE, None, true).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn rotation_cooldown_is_per_peer_and_uses_the_full_sixty_seconds() {
        let mut r = Recovery::default();
        let peer = Identity::generate().node_id();
        for generation in 1..=3 {
            transport_failure(&mut r, peer, generation);
        }
        assert_eq!(
            r.sample(SAMPLE, None, true),
            Some(RecoveryAction::Rotate(peer))
        );
        for generation in 4..=6 {
            transport_failure(&mut r, peer, generation);
        }
        time::advance(Duration::from_secs(59)).await;
        assert!(r.sample(SAMPLE, None, true).is_none());
        time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            r.sample(SAMPLE, None, true),
            Some(RecoveryAction::Rotate(peer))
        );
    }

    #[test]
    fn one_peers_rotation_does_not_delay_another_peers_rotation() {
        let mut r = Recovery::default();
        let first = Identity::generate().node_id();
        let second = Identity::generate().node_id();
        for generation in 1..=3 {
            transport_failure(&mut r, first, generation);
        }
        assert_eq!(
            r.sample(SAMPLE, None, true),
            Some(RecoveryAction::Rotate(first))
        );
        for generation in 1..=3 {
            transport_failure(&mut r, second, generation);
        }
        assert_eq!(
            r.sample(SAMPLE, None, true),
            Some(RecoveryAction::Rotate(second))
        );
    }

    #[test]
    fn rotation_history_is_bounded_by_the_attempt_registry() {
        let mut r = Recovery::default();
        for _ in 0..MAX_PEERS * 3 {
            let peer = Identity::generate().node_id();
            transport_failure(&mut r, peer, 1);
        }
        assert!(r.attempts.len() <= MAX_PEERS);
        assert_eq!(
            r.attempts
                .values()
                .filter(|attempt| attempt.last_rotation.is_some())
                .count(),
            0
        );
    }

    #[test]
    fn manual_disconnect_clears_pending_failures_and_never_rotates_that_peer() {
        let mut r = Recovery::default();
        let peer = Identity::generate().node_id();
        transport_failure(&mut r, peer, 1);
        transport_failure(&mut r, peer, 2);
        r.event(&peer_event(peer, 3, PeerLifecycle::Disconnected));
        assert!(r.sample(SAMPLE, None, true).is_none());
    }
}

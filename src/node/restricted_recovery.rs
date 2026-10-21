//! Bounded recovery decisions for the fixed gateway/client; no configuration writes.
use super::{
    control::{NodeSnapshot, SessionEvent},
    network_state::{NetworkLifecycle, PeerLifecycle},
};
use std::{
    collections::{BTreeMap, VecDeque},
    time::Duration,
};

const MIN_REFRESH_AGE: Duration = Duration::from_secs(60);
const FAILURE_WINDOW: Duration = Duration::from_secs(90);
const SIGNAL_SILENCE: Duration = Duration::from_secs(150);
const IDLE_REFRESH: Duration = Duration::from_secs(300);

#[derive(Default)]
pub(super) struct GatewayRecovery {
    attempts: BTreeMap<String, (u64, bool)>,
    failures: VecDeque<Duration>,
    last_progress: Duration,
    last_signal: Duration,
    idle_since: Option<Duration>,
}
impl GatewayRecovery {
    pub fn event(&mut self, now: Duration, event: &SessionEvent) {
        match event {
            SessionEvent::Lifecycle(NetworkLifecycle::SignalOnline)
            | SessionEvent::PeerCandidates { .. } => self.last_signal = now,
            SessionEvent::Diagnostic(message) if message == "信令心跳已确认" => {
                self.last_signal = now
            }
            SessionEvent::PeerState {
                peer,
                generation,
                state,
            } => {
                let id = peer.to_hex();
                if self
                    .attempts
                    .get(&id)
                    .is_some_and(|(old, _)| old > generation)
                {
                    return;
                }
                self.last_progress = now;
                match state {
                    PeerLifecycle::PeerPending
                    | PeerLifecycle::Punching
                    | PeerLifecycle::Authenticating => {
                        self.attempts
                            .entry(id)
                            .and_modify(|entry| {
                                if entry.0 != *generation {
                                    *entry = (*generation, false)
                                }
                            })
                            .or_insert((*generation, false));
                    }
                    PeerLifecycle::Negotiating | PeerLifecycle::RemoteAuthPending => {
                        self.attempts.insert(id, (*generation, true));
                    }
                    PeerLifecycle::Connected(_) => {
                        self.attempts.insert(id, (*generation, true));
                        self.failures.clear();
                    }
                    PeerLifecycle::Failed(_) => {
                        if self
                            .attempts
                            .remove(&id)
                            .is_some_and(|(_, transport_ok)| !transport_ok)
                        {
                            if self.failures.len() == 3 {
                                self.failures.pop_front();
                            }
                            self.failures.push_back(now);
                        }
                    }
                    PeerLifecycle::Disconnected => {
                        self.attempts.remove(&id);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn reason(&mut self, now: Duration, snapshot: &NodeSnapshot) -> Option<&'static str> {
        if snapshot.peers.values().any(|p| p.phase == "connected") {
            self.idle_since = None;
            return None; // No idle/failure refresh is allowed to evict a healthy user.
        }
        let pending = snapshot.peers.values().any(|p| {
            matches!(
                p.phase.as_str(),
                "pending" | "punching" | "authenticating" | "negotiating" | "authorization_pending"
            )
        });
        if pending {
            self.idle_since = None;
        } else {
            self.idle_since.get_or_insert(now);
        }
        if now < MIN_REFRESH_AGE {
            return None;
        }
        self.failures
            .retain(|t| now.saturating_sub(*t) <= FAILURE_WINDOW);
        if self.failures.len() >= 3 {
            return Some("连续三次未能建立设备通道，重新收集 NAT 地址");
        }
        if now.saturating_sub(self.last_signal) >= SIGNAL_SILENCE {
            return Some("信令长时间未确认，重建连接与网络候选");
        }
        if pending && now.saturating_sub(self.last_progress) >= Duration::from_secs(45) {
            return Some("设备连接阶段长时间无进展，重建网络");
        }
        if self
            .idle_since
            .is_some_and(|t| now.saturating_sub(t) >= IDLE_REFRESH)
        {
            return Some("无人在线闲置五分钟，刷新公网地址与 UDP 映射");
        }
        None
    }
}

#[derive(Default)]
pub(super) struct ClientRecovery {
    missing_since: Option<Duration>,
    service_failures: u8,
}
impl ClientRecovery {
    pub fn sample(
        &mut self,
        now: Duration,
        peer_ready: bool,
        http_ready: bool,
        directory: Option<bool>,
    ) -> Option<&'static str> {
        if http_ready {
            self.missing_since = None;
            self.service_failures = 0;
            return None;
        }
        if !peer_ready {
            self.service_failures = 0;
            let since = *self.missing_since.get_or_insert(now);
            return (now.saturating_sub(since) >= Duration::from_secs(60))
                .then_some("设备通道一分钟未恢复，重新建立本机网络");
        }
        self.missing_since = None;
        if let Some(ready) = directory {
            self.service_failures = if ready {
                0
            } else {
                self.service_failures.saturating_add(1)
            };
            if self.service_failures >= 3 {
                return Some("设备通道无业务响应，重新连接专用接入节点");
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::{control::PeerSnapshot, remote_auth::RemoteAuthorization};
    use super::*;
    use crate::identity::Identity;
    fn seconds(n: u64) -> Duration {
        Duration::from_secs(n)
    }
    fn state(phase: &str) -> NodeSnapshot {
        let mut s = NodeSnapshot {
            signal: "online".into(),
            ..Default::default()
        };
        if !phase.is_empty() {
            s.peers.insert(
                "peer".into(),
                PeerSnapshot {
                    phase: phase.into(),
                    ..Default::default()
                },
            );
        }
        s
    }
    #[test]
    fn idle_refresh_requires_no_users_and_silence_cannot_reset_a_healthy_session() {
        let mut r = GatewayRecovery::default();
        for t in [0, 60, 120, 180, 240, 299] {
            r.last_signal = seconds(t);
            assert!(r.reason(seconds(t), &state("")).is_none());
        }
        r.last_signal = seconds(300);
        assert!(r.reason(seconds(300), &state("")).is_some());
        assert!(r.reason(seconds(999), &state("connected")).is_none());
    }
    #[test]
    fn repeated_transport_failures_refresh_but_bad_credentials_do_not() {
        for auth_stage in [false, true] {
            let mut r = GatewayRecovery::default();
            let peer = Identity::generate().node_id();
            for generation in 1..=3 {
                for state in [
                    PeerLifecycle::Punching,
                    if auth_stage {
                        PeerLifecycle::RemoteAuthPending
                    } else {
                        PeerLifecycle::Authenticating
                    },
                    PeerLifecycle::Failed("test".into()),
                ] {
                    r.event(
                        seconds(20 * generation),
                        &SessionEvent::PeerState {
                            peer,
                            generation,
                            state,
                        },
                    );
                }
            }
            r.last_signal = seconds(60);
            assert_eq!(r.reason(seconds(60), &state("")).is_some(), !auth_stage);
            assert!(r.reason(seconds(61), &state("connected")).is_none());
            r.event(
                seconds(62),
                &SessionEvent::PeerState {
                    peer,
                    generation: 4,
                    state: PeerLifecycle::Connected(RemoteAuthorization::BOTH),
                },
            );
            assert!(r.failures.is_empty());
        }
    }
    #[test]
    fn pending_and_signal_stalls_have_bounded_recovery() {
        let mut r = GatewayRecovery {
            last_signal: seconds(60),
            ..Default::default()
        };
        assert!(r.reason(seconds(59), &state("punching")).is_none());
        assert!(r.reason(seconds(60), &state("punching")).is_some());
        let mut r = GatewayRecovery::default();
        assert!(r.reason(seconds(149), &state("")).is_none());
        assert!(r.reason(seconds(150), &state("")).is_some());
    }
    #[test]
    fn client_refreshes_a_stuck_transport_but_not_a_healthy_directory_with_backend_down() {
        let mut r = ClientRecovery::default();
        assert!(r.sample(seconds(0), false, false, None).is_none());
        assert!(r.sample(seconds(59), false, false, None).is_none());
        assert!(r.sample(seconds(60), false, false, None).is_some());
        for t in [61, 76, 91, 120] {
            assert!(r.sample(seconds(t), true, false, Some(true)).is_none());
        }
        assert!(r.sample(seconds(121), true, false, Some(false)).is_none());
        assert!(r.sample(seconds(136), true, false, Some(false)).is_none());
        assert!(r.sample(seconds(151), true, false, Some(false)).is_some());
        assert!(r.sample(seconds(152), true, true, None).is_none());
        assert!(r.sample(seconds(153), true, false, Some(false)).is_none());
    }
}

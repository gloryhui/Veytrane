//! Small authoritative UI projection. No manifests, tokens or remote absolute paths.
use crate::identity::NodeId;
use crate::node::core::speed::{SpeedSnapshot, SpeedStatus};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[cfg(feature = "agent-files")]
mod file_rows;
#[cfg(feature = "agent-files")]
pub use file_rows::{GroupRow, ListRow, Snapshot, TaskFilter, TaskRow, state_label};
/// Bounded UI diagnostics follow the same generation fence as peer lifecycle.
pub fn project_peer_path(
    paths: &mut HashMap<NodeId, (u64, String)>,
    generations: &HashMap<NodeId, u64>,
    peer: NodeId,
    generation: u64,
    detail: String,
) -> bool {
    if generations
        .get(&peer)
        .is_some_and(|current| generation < *current)
        || paths
            .get(&peer)
            .is_some_and(|(current, _)| generation < *current)
    {
        return false;
    }
    if paths.len() >= crate::node::core::network_state::DEFAULT_MAX_PEERS
        && !paths.contains_key(&peer)
        && let Some(oldest) = paths
            .iter()
            .min_by_key(|(_, (g, _))| *g)
            .map(|(peer, _)| *peer)
    {
        paths.remove(&oldest);
    }
    paths.insert(peer, (generation, detail));
    true
}

#[derive(Clone)]
pub struct SpeedView {
    pub snapshot: SpeedSnapshot,
    pub instant: f64,
    updated: Instant,
}
#[derive(Default)]
pub struct SpeedViews(pub HashMap<NodeId, SpeedView>);
impl SpeedViews {
    pub fn update(&mut self, incoming: HashMap<NodeId, SpeedSnapshot>, now: Instant) {
        for (peer, old) in &mut self.0 {
            if !incoming.contains_key(peer) && old.snapshot.status == SpeedStatus::Running {
                old.snapshot.status = SpeedStatus::Interrupted;
                old.snapshot.bytes_per_second = 0.;
                old.instant = 0.;
            }
        }
        for (peer, s) in incoming {
            let old = self.0.get(&peer);
            let same = old.is_some_and(|o| o.snapshot.test_id == s.test_id);
            let previous = old.filter(|_| same);
            let advanced = previous.is_none_or(|o| s.elapsed > o.snapshot.elapsed);
            let instant = if s.status != SpeedStatus::Running {
                0.
            } else if let Some(o) = previous {
                if advanced {
                    s.bytes.saturating_sub(o.snapshot.bytes) as f64
                        / (s.elapsed - o.snapshot.elapsed).as_secs_f64().max(0.000001)
                } else if now.duration_since(o.updated) > Duration::from_secs(2) {
                    0.
                } else {
                    o.instant
                }
            } else {
                s.bytes as f64 / s.elapsed.as_secs_f64().max(0.000001)
            };
            let updated = if advanced {
                now
            } else {
                previous.map_or(now, |o| o.updated)
            };
            self.0.insert(
                peer,
                SpeedView {
                    snapshot: s,
                    instant,
                    updated,
                },
            );
        }
        while self.0.len() > crate::node::core::network_state::DEFAULT_MAX_PEERS {
            let oldest = self
                .0
                .iter()
                .filter(|(_, v)| v.snapshot.status != SpeedStatus::Running)
                .min_by_key(|(_, v)| v.updated)
                .map(|(p, _)| *p);
            let Some(peer) = oldest else {
                break;
            };
            self.0.remove(&peer);
        }
    }
}
/// GUI eligibility is projected from a verified live session, never Short ID metadata.
pub fn can_trust_peer(
    peer: crate::identity::NodeId,
    local: Option<crate::identity::NodeId>,
    state: Option<&crate::node::core::network_state::PeerLifecycle>,
    devices: &[crate::node::core::trusted_devices::TrustedDevice],
) -> bool {
    local.is_some_and(|local| local != peer)
        && !crate::node::core::trusted_devices::contains(devices, peer)
        && matches!(state, Some(crate::node::core::network_state::PeerLifecycle::Connected(a)) if a.inbound.password)
}

#[cfg(all(test, feature = "agent-files"))]
mod tests {
    use super::*;
    use crate::node::core::queue::QueueMetrics;
    use crate::node::core::task_model::{TaskDirection, TaskId, TaskState};
    use std::collections::HashSet;
    use crate::node::core::protocol::SpeedDirection;
    #[test]
    fn combined_filters_show_matching_children_with_whole_group_truth() {
        let peer = crate::identity::Identity::generate().node_id();
        let group = TaskId::generate();
        let first = TaskRow {
            id: TaskId::generate(),
            group: Some(group),
            name: "Folder/文档.TXT".into(),
            peer,
            direction: TaskDirection::Receive,
            state: TaskState::Completed,
            total: 10,
            confirmed: 10,
            rate: 0.,
            diagnostic: None,
            retryable: false,
            auto_waiting: false,
            can_delete_file: false,
        };
        let mut second = first.clone();
        second.id = TaskId::generate();
        second.name = "Folder/other.bin".into();
        second.state = TaskState::Paused;
        second.confirmed = 2;
        let mut other = first.clone();
        other.id = TaskId::generate();
        other.group = None;
        other.peer = crate::identity::Identity::generate().node_id();
        let view = Snapshot {
            space: Default::default(),
            tasks: vec![first.clone(), second, other],
            queue: crate::node::core::queue::TaskQueue::default().metrics(),
        };
        let devices = [crate::node::core::trusted_devices::TrustedDevice::new(
            peer,
            "Office Mac".into(),
            None,
        )];
        let mut filter = TaskFilter {
            query: "txt OFFICE".into(),
            peer: Some(peer),
            direction: Some(TaskDirection::Receive),
            state: Some(TaskState::Completed),
        };
        let rows = view.filtered_list(&HashSet::new(), &filter, &devices);
        assert_eq!(rows.len(), 2); // Filtering exposes matches even in collapsed groups.
        let ListRow::Group(group) = &rows[0] else {
            panic!()
        };
        assert_eq!(
            (
                group.children,
                group.matched,
                group.complete,
                group.confirmed,
                group.total
            ),
            (2, 1, 1, 12, 20)
        );
        assert!(group.label().contains("进行中"));
        assert!(group.label().contains("筛选命中 1/2"));
        assert!(matches!(&rows[1], ListRow::Task(t) if t.id == first.id));
        filter.direction = Some(TaskDirection::Send);
        assert!(
            view.filtered_list(&HashSet::new(), &filter, &devices)
                .is_empty()
        );
        filter = TaskFilter::default();
        assert!(!filter.active());
        assert_eq!(
            view.filtered_list(&HashSet::new(), &filter, &devices).len(),
            2
        );
        assert_eq!(view.tasks.len(), 3); // Projection never changes source state.
    }
    #[test]
    fn speed_delta_staleness_restart_cancel_and_disconnect_are_truthful() {
        let peer = crate::identity::Identity::generate().node_id();
        let now = Instant::now();
        let mut views = SpeedViews::default();
        let mut s = SpeedSnapshot {
            test_id: TaskId::generate(),
            direction: SpeedDirection::Upload,
            owner: peer,
            seconds: 30,
            status: SpeedStatus::Running,
            bytes: 100,
            elapsed: Duration::from_secs(1),
            bytes_per_second: 100.,
            rtt: Duration::ZERO,
        };
        views.update(HashMap::from([(peer, s.clone())]), now);
        s.bytes = 500;
        s.elapsed = Duration::from_secs(2);
        s.bytes_per_second = 250.;
        views.update(
            HashMap::from([(peer, s.clone())]),
            now + Duration::from_secs(1),
        );
        assert_eq!(views.0[&peer].instant, 400.);
        assert_eq!(views.0[&peer].snapshot.bytes_per_second, 250.);
        views.update(
            HashMap::from([(peer, s.clone())]),
            now + Duration::from_secs(4),
        );
        assert_eq!(views.0[&peer].instant, 0.);
        s.test_id = TaskId::generate();
        s.bytes = 0;
        s.elapsed = Duration::ZERO;
        views.update(
            HashMap::from([(peer, s.clone())]),
            now + Duration::from_secs(5),
        );
        assert_eq!(views.0[&peer].instant, 0.);
        views.update(HashMap::new(), now + Duration::from_secs(6));
        assert_eq!(views.0[&peer].snapshot.status, SpeedStatus::Interrupted);
        assert_eq!(views.0[&peer].snapshot.bytes_per_second, 0.);
        s.status = SpeedStatus::Cancelled;
        s.bytes_per_second = 0.;
        views.update(HashMap::from([(peer, s)]), now + Duration::from_secs(7));
        assert_eq!(views.0[&peer].instant, 0.);
    }
    #[test]
    fn group_partial_failure_and_empty_file_do_not_become_completed() {
        let peer = crate::identity::Identity::generate().node_id();
        let group = TaskId::generate();
        let mut a = TaskRow {
            id: TaskId::generate(),
            group: Some(group.clone()),
            name: "中文/空文件".into(),
            peer,
            direction: TaskDirection::Receive,
            state: TaskState::Paused,
            total: 0,
            confirmed: 0,
            rate: 0.,
            diagnostic: None,
            retryable: false,
            auto_waiting: false,
            can_delete_file: false,
        };
        assert_eq!(a.percent(), 0.);
        assert!(a.can_continue());
        assert!(!a.can_pause());
        a.state = TaskState::Completed;
        assert_eq!(a.percent(), 100.);
        let mut b = a.clone();
        b.id = TaskId::generate();
        b.state = TaskState::Interrupted;
        b.total = 10;
        b.confirmed = 4;
        let view = Snapshot {
            space: Default::default(),
            tasks: vec![a, b],
            queue: QueueMetrics {
                pending: 0,
                active_files: 0,
                active_metadata: 0,
                limit: 1,
                converging: false,
            },
        };
        let collapsed = view.list(&HashSet::new());
        assert_eq!(collapsed.len(), 1);
        let ListRow::Group(g) = &collapsed[0] else {
            panic!()
        };
        assert_eq!(g.complete, 1);
        assert_eq!(g.failures, 1);
        assert!(g.label().contains("部分"));
        assert_eq!(view.list(&HashSet::from([group])).len(), 3);
    }
    #[test]
    fn terminal_speed_history_is_bounded_without_evicting_a_running_test() {
        let now = Instant::now();
        let mut views = SpeedViews::default();
        let oldest = crate::identity::Identity::generate().node_id();
        let template = SpeedSnapshot {
            test_id: TaskId::generate(),
            direction: SpeedDirection::Upload,
            owner: oldest,
            seconds: 30,
            status: SpeedStatus::Completed,
            bytes: 100,
            elapsed: Duration::from_secs(1),
            bytes_per_second: 100.,
            rtt: Duration::ZERO,
        };
        views.update(HashMap::from([(oldest, template.clone())]), now);
        for n in 1..=crate::node::core::network_state::DEFAULT_MAX_PEERS {
            let peer = crate::identity::Identity::generate().node_id();
            let mut s = template.clone();
            s.test_id = TaskId::generate();
            views.update(
                HashMap::from([(peer, s)]),
                now + Duration::from_secs(n as u64),
            );
        }
        let active = crate::identity::Identity::generate().node_id();
        let mut s = template;
        s.test_id = TaskId::generate();
        s.status = SpeedStatus::Running;
        views.update(HashMap::from([(active, s)]), now + Duration::from_secs(30));
        assert_eq!(
            views.0.len(),
            crate::node::core::network_state::DEFAULT_MAX_PEERS
        );
        assert!(!views.0.contains_key(&oldest));
        assert_eq!(views.0[&active].snapshot.status, SpeedStatus::Running);
    }
}

#[cfg(test)]
mod trusted_gui_tests {
    use super::*;
    use crate::{
        identity::Identity,
        node::core::{
            network_state::PeerLifecycle,
            remote_auth::{AuthorizationGrant, RemoteAuthorization},
            trusted_devices::TrustedDevice,
        },
    };
    #[test]
    fn explicit_trust_button_requires_inbound_password_regardless_of_metadata() {
        let local = Identity::generate().node_id();
        let peer = Identity::generate().node_id();
        let trusted = PeerLifecycle::Connected(RemoteAuthorization {
            inbound: AuthorizationGrant {
                password: false,
                trusted_device: true,
            },
            outbound: AuthorizationGrant::default(),
        });
        let outbound_password = PeerLifecycle::Connected(RemoteAuthorization {
            inbound: AuthorizationGrant::default(),
            outbound: AuthorizationGrant::password(true),
        });
        let metadata = TrustedDevice::new(
            Identity::generate().node_id(),
            "same name".into(),
            Some("100000124".into()),
        );
        for state in [
            None,
            Some(&PeerLifecycle::RemoteAuthPending),
            Some(&trusted),
            Some(&outbound_password),
            Some(&PeerLifecycle::Disconnected),
        ] {
            assert!(!can_trust_peer(peer, Some(local), state, &[]));
            assert!(!can_trust_peer(
                peer,
                Some(local),
                state,
                std::slice::from_ref(&metadata)
            ));
        }
        let password = PeerLifecycle::Connected(RemoteAuthorization {
            inbound: AuthorizationGrant::password(true),
            outbound: AuthorizationGrant::default(),
        });
        assert!(can_trust_peer(peer, Some(local), Some(&password), &[]));
        assert!(!can_trust_peer(peer, None, Some(&password), &[]));
        assert!(!can_trust_peer(local, Some(local), Some(&password), &[]));
        assert!(can_trust_peer(
            peer,
            Some(local),
            Some(&password),
            &[metadata]
        ));
        assert!(!can_trust_peer(
            peer,
            Some(local),
            Some(&password),
            &[TrustedDevice::new(peer, "device".into(), None)]
        ));
    }
}

#[cfg(test)]
mod network_path_tests {
    use super::*;
    use crate::identity::Identity;
    #[test]
    fn gui_ipv6_winner_cannot_be_overwritten_by_stale_ipv4_generation() {
        let peer = Identity::generate().node_id();
        let generations = HashMap::from([(peer, 9)]);
        let mut paths = HashMap::new();
        assert!(project_peer_path(
            &mut paths,
            &generations,
            peer,
            9,
            "IPv6 [::1]:9000 已认证".into()
        ));
        assert!(!project_peer_path(
            &mut paths,
            &generations,
            peer,
            8,
            "IPv4 127.0.0.1:9000 已认证".into()
        ));
        assert!(paths[&peer].1.contains("IPv6"));
        assert!(project_peer_path(
            &mut paths,
            &generations,
            peer,
            10,
            "IPv4 新 transport".into()
        ));
        assert_eq!(paths[&peer].0, 10);
    }
    #[test]
    fn gui_peer_path_diagnostics_remain_bounded() {
        let mut paths = HashMap::new();
        let capacity = crate::node::core::network_state::DEFAULT_MAX_PEERS;
        for generation in 1..=(2 * capacity) as u64 {
            let peer = Identity::generate().node_id();
            assert!(project_peer_path(
                &mut paths,
                &HashMap::new(),
                peer,
                generation,
                "IPv6 / IPv4 候选".into()
            ));
        }
        assert_eq!(
            paths.len(),
            crate::node::core::network_state::DEFAULT_MAX_PEERS
        );
        assert!(
            paths
                .values()
                .all(|(generation, _)| *generation > capacity as u64)
        );
    }
}

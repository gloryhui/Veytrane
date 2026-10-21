//! Recover a managed macOS Agent or fixed client after wake/interface changes.
//! A restart re-gathers NAT candidates; signal-only outages stay in the core.
use std::{collections::BTreeSet, time::Duration};

const POLL: Duration = Duration::from_secs(5);
const WAKE_GAP: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Recovery {
    interfaces: Option<BTreeSet<String>>,
    candidate: Option<BTreeSet<String>>,
    stopped_samples: u8,
}
impl Recovery {
    fn observe(
        &mut self,
        gap: Duration,
        interfaces: Option<BTreeSet<String>>,
        core_running: bool,
    ) -> Option<&'static str> {
        if gap >= WAKE_GAP {
            return Some("后台长时间停顿或休眠唤醒，重新收集网络候选");
        }
        self.stopped_samples = if core_running {
            0
        } else {
            self.stopped_samples.saturating_add(1)
        };
        if self.stopped_samples >= 3 {
            return Some("网络核心已退出，重启后台恢复映射");
        }
        let Some(current) = interfaces else {
            // A transient interface enumeration error is not a network change.
            self.candidate = None;
            return None;
        };
        let Some(previous) = &self.interfaces else {
            self.interfaces = Some(current);
            return None;
        };
        if *previous == current {
            self.candidate = None;
        } else if self.candidate.as_ref() == Some(&current) {
            return Some("本机网络接口发生变化，重新收集网络候选");
        } else {
            self.candidate = Some(current);
        }
        None
    }
}

fn interfaces() -> Option<BTreeSet<String>> {
    Some(
        if_addrs::get_if_addrs()
            .ok()?
            .into_iter()
            .filter(|interface| {
                interface.is_oper_up() && !interface.is_loopback() && !interface.is_link_local()
            })
            .map(|interface| format!("{}:{}", interface.name, interface.ip()))
            .collect(),
    )
}

pub(super) async fn wait(control: super::control::NodeControl) -> std::io::Error {
    use std::time::{Instant, SystemTime};
    let mut recovery = Recovery::default();
    let mut last_wall = SystemTime::now();
    let mut last_tick = Instant::now();
    let mut interval = tokio::time::interval(POLL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let now_wall = SystemTime::now();
        let now_tick = Instant::now();
        // macOS monotonic time can exclude sleep. Wall time covers that case;
        // monotonic time also covers a clock correction during a scheduler gap.
        let gap = now_wall
            .duration_since(last_wall)
            .unwrap_or_default()
            .max(now_tick.duration_since(last_tick));
        last_wall = now_wall;
        last_tick = now_tick;
        if let Some(reason) = recovery.observe(gap, interfaces(), control.is_running()) {
            tracing::warn!(reason, "后台将优雅停止，以重新建立连接");
            return std::io::Error::other(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn addresses(address: &str) -> Option<BTreeSet<String>> {
        Some(BTreeSet::from([address.into()]))
    }
    #[test]
    fn steady_sampling_and_transient_changes_do_not_interrupt_the_core() {
        let mut r = Recovery::default();
        assert!(r.observe(POLL, addresses("en0:old"), true).is_none());
        assert!(r.observe(POLL, addresses("en0:new"), true).is_none());
        assert!(r.observe(POLL, addresses("en0:old"), true).is_none());
        assert!(r.observe(POLL, None, true).is_none());
        assert!(r.observe(POLL, addresses("en0:new"), true).is_none());
        assert!(r.observe(POLL, addresses("en0:new"), true).is_some());
    }
    #[test]
    fn wake_with_unchanged_addresses_still_refreshes_nat_candidates() {
        let mut r = Recovery::default();
        assert!(r.observe(POLL, addresses("en0:old"), true).is_none());
        assert!(r.observe(WAKE_GAP, addresses("en0:old"), true).is_some());
    }
    #[test]
    fn core_exit_requires_repeated_samples_and_online_resets_the_counter() {
        let mut r = Recovery::default();
        for live in [false, false, true, false, false] {
            assert!(r.observe(POLL, addresses("en0:old"), live).is_none());
        }
        assert!(r.observe(POLL, addresses("en0:old"), false).is_some());
    }
}

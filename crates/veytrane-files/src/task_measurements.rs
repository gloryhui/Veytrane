//! Bounded current-process measurements for file transfer attempts.

use crate::task_model::{TaskId, TaskRecord};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

const MAX_SAMPLES: usize = 4096;

#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub attempts: u32,
    pub automatic: u32,
    pub elapsed: Duration,
}

struct Measurement {
    sample: Sample,
    since: Option<Instant>,
    automatic: bool,
    serial: u64,
}

#[derive(Default)]
pub struct Measurements {
    entries: HashMap<TaskId, Measurement>,
    serial: u64,
}

impl Measurements {
    pub fn start(&mut self, id: TaskId, now: Instant) {
        if !self.entries.contains_key(&id) && self.entries.len() >= MAX_SAMPLES {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, measurement)| measurement.since.is_none())
                .min_by_key(|(_, measurement)| measurement.serial)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            } else {
                return;
            }
        }
        self.serial = self.serial.wrapping_add(1);
        let measurement = self.entries.entry(id).or_insert(Measurement {
            sample: Sample::default(),
            since: None,
            automatic: false,
            serial: self.serial,
        });
        if measurement.since.is_none() {
            measurement.sample.attempts = measurement.sample.attempts.saturating_add(1);
            measurement.since = Some(now);
            measurement.automatic = false;
            measurement.serial = self.serial;
        }
    }

    pub fn automatic(&mut self, id: &TaskId) {
        if let Some(measurement) = self.entries.get_mut(id)
            && measurement.since.is_some()
            && !measurement.automatic
        {
            measurement.sample.automatic = measurement.sample.automatic.saturating_add(1);
            measurement.automatic = true;
        }
    }

    pub fn finish(&mut self, id: &TaskId, now: Instant) {
        self.serial = self.serial.wrapping_add(1);
        if let Some(measurement) = self.entries.get_mut(id)
            && let Some(since) = measurement.since.take()
        {
            measurement.serial = self.serial;
            measurement.sample.elapsed = measurement
                .sample
                .elapsed
                .saturating_add(now.saturating_duration_since(since));
        }
    }

    pub fn sample(&self, id: &TaskId, now: Instant) -> Option<Sample> {
        self.entries.get(id).map(|measurement| {
            let mut sample = measurement.sample;
            if let Some(since) = measurement.since {
                sample.elapsed = sample
                    .elapsed
                    .saturating_add(now.saturating_duration_since(since));
            }
            sample
        })
    }

    pub fn retain(&mut self, records: &[TaskRecord]) {
        let ids: HashSet<_> = records.iter().map(TaskRecord::task_id).collect();
        self.entries
            .retain(|id, measurement| measurement.since.is_some() || ids.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurements_exclude_pauses_count_real_attempts_and_mark_auto_once() {
        let mut measures = Measurements::default();
        let id = TaskId::generate();
        let now = Instant::now();
        assert!(measures.sample(&id, now).is_none());
        measures.start(id.clone(), now);
        measures.start(id.clone(), now + Duration::from_secs(1));
        measures.finish(&id, now + Duration::from_secs(2));
        measures.finish(&id, now + Duration::from_secs(5));
        assert_eq!(
            measures
                .sample(&id, now + Duration::from_secs(50))
                .unwrap()
                .elapsed,
            Duration::from_secs(2)
        );
        measures.start(id.clone(), now + Duration::from_secs(50));
        measures.automatic(&id);
        measures.automatic(&id);
        let sample = measures.sample(&id, now + Duration::from_secs(53)).unwrap();
        assert_eq!(sample.attempts, 2);
        assert_eq!(sample.automatic, 1);
        assert_eq!(sample.elapsed, Duration::from_secs(5));
        measures.finish(&id, now + Duration::from_secs(55));
        measures.retain(&[]);
        assert!(measures.sample(&id, now).is_none());
        assert!(Measurements::default().sample(&id, now).is_none());
    }

    #[test]
    fn measurements_are_bounded_and_do_not_retire_live_io() {
        let mut measures = Measurements::default();
        let now = Instant::now();
        let active = TaskId::generate();
        measures.start(active.clone(), now);
        for _ in 0..MAX_SAMPLES + 10 {
            let id = TaskId::generate();
            measures.start(id.clone(), now);
            measures.finish(&id, now);
        }
        assert_eq!(measures.entries.len(), MAX_SAMPLES);
        assert!(measures.sample(&active, now).is_some());
        measures.retain(&[]);
        assert_eq!(measures.entries.len(), 1);
    }
}

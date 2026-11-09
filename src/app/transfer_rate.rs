//! Source-timestamped throughput estimates. Display ticks never add samples:
//! retain the last real baseline even when a slow transfer's display expires.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(2);
const BUCKET: Duration = Duration::from_millis(100);
const MAX_SAMPLES: usize = 32;

#[derive(Clone, Copy, Debug)]
struct Sample {
    bytes: u64,
    at: Instant,
}

#[derive(Debug)]
pub(super) struct TransferRate {
    samples: VecDeque<Sample>,
    bucket_start: Instant,
}

impl TransferRate {
    pub(super) fn new(bytes: u64, at: Instant) -> Self {
        Self {
            samples: VecDeque::from([Sample { bytes, at }]),
            bucket_start: at,
        }
    }

    /// Reject reordered samples; a counter rollback starts a new baseline.
    pub(super) fn observe(&mut self, bytes: u64, at: Instant) -> bool {
        let last = *self.samples.back().expect("a baseline always exists");
        if at < last.at {
            return false;
        }
        if bytes < last.bytes {
            *self = Self::new(bytes, at);
            return true;
        }
        let sample = Sample { bytes, at };
        if at == last.at {
            *self.samples.back_mut().unwrap() = sample;
        } else if at.duration_since(self.bucket_start) >= BUCKET {
            self.samples.push_back(sample);
            self.bucket_start = at;
        } else if self.samples.len() == 1 {
            // Preserve the initial baseline inside the first bucket.
            self.samples.push_back(sample);
        } else {
            *self.samples.back_mut().unwrap() = sample;
        }
        if let Some(cutoff) = at.checked_sub(WINDOW) {
            // Keep one anchor at/before the window and at least two samples.
            // A sparse observation must use its actual, possibly longer span.
            while self.samples.len() > 2 && self.samples[1].at <= cutoff {
                self.samples.pop_front();
            }
        }
        while self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
        true
    }

    /// Include the quiet interval since the last event. None denotes stale or
    /// insufficient samples, not a failed or cancelled transfer.
    pub(super) fn bytes_per_second(&self, now: Instant) -> Option<u64> {
        let last = self.samples.back()?;
        if now.checked_duration_since(last.at)? >= WINDOW || self.samples.len() < 2 {
            return None;
        }
        let mut baseline = self.samples.front()?;
        if let Some(cutoff) = now.checked_sub(WINDOW) {
            for sample in &self.samples {
                if sample.at > cutoff {
                    break;
                }
                baseline = sample;
            }
        }
        let elapsed = now.checked_duration_since(baseline.at)?.as_nanos();
        if elapsed == 0 || baseline.at == last.at {
            return None;
        }
        let bytes = last.bytes.checked_sub(baseline.bytes)?;
        let rate = u128::from(bytes) * 1_000_000_000 / elapsed;
        Some(rate.min(u128::from(u64::MAX)) as u64)
    }

    /// Stop ticking when no estimate remains, but keep the real baseline.
    pub(super) fn needs_refresh(&self, now: Instant) -> bool {
        self.bytes_per_second(now).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_progress_decays_and_expires() {
        let start = Instant::now();
        let mut rate = TransferRate::new(0, start);
        assert!(!rate.needs_refresh(start));
        for tick in 1..=40 {
            rate.observe(tick * 100, start + Duration::from_millis(tick * 100));
        }
        assert_eq!(
            rate.bytes_per_second(start + Duration::from_secs(4)),
            Some(1000)
        );
        assert_eq!(
            rate.bytes_per_second(start + Duration::from_millis(4500)),
            Some(750)
        );
        assert!(rate.needs_refresh(start + Duration::from_millis(4500)));
        assert_eq!(rate.bytes_per_second(start + Duration::from_secs(6)), None);
        assert!(!rate.needs_refresh(start + Duration::from_secs(6)));
    }

    #[test]
    fn sparse_progress_reuses_its_baseline_after_expiry() {
        let start = Instant::now();
        let mut rate = TransferRate::new(0, start);
        for block in 1..=2 {
            let at = start + Duration::from_secs(block * 4);
            assert_eq!(rate.bytes_per_second(at), None);
            rate.observe(block * 32768, at);
            assert_eq!(rate.bytes_per_second(at), Some(8192));
        }
    }

    #[test]
    fn late_samples_are_ignored_and_counter_rollback_resets() {
        let start = Instant::now();
        let mut rate = TransferRate::new(0, start);
        rate.observe(1000, start + Duration::from_secs(1));
        assert!(!rate.observe(5, start + Duration::from_millis(500)));
        assert_eq!(rate.bytes_per_second(start), None);
        assert_eq!(
            rate.bytes_per_second(start + Duration::from_secs(1)),
            Some(1000)
        );
        rate.observe(10, start + Duration::from_secs(2));
        assert_eq!(rate.bytes_per_second(start + Duration::from_secs(2)), None);
        rate.observe(20, start + Duration::from_secs(3));
        assert_eq!(
            rate.bytes_per_second(start + Duration::from_secs(3)),
            Some(10)
        );
    }

    #[test]
    fn equal_timestamps_merge_without_inventing_a_duration() {
        let start = Instant::now();
        let mut rate = TransferRate::new(0, start);
        rate.observe(10, start);
        assert_eq!(rate.bytes_per_second(start), None);
        assert_eq!(rate.bytes_per_second(start + Duration::from_secs(1)), None);
        rate.observe(20, start + Duration::from_secs(1));
        assert_eq!(
            rate.bytes_per_second(start + Duration::from_secs(1)),
            Some(10)
        );
    }

    #[test]
    fn zero_bytes_and_large_counts_are_safe() {
        let start = Instant::now();
        let mut rate = TransferRate::new(0, start);
        rate.observe(0, start + Duration::from_secs(1));
        assert_eq!(
            rate.bytes_per_second(start + Duration::from_secs(1)),
            Some(0)
        );
        let mut large = TransferRate::new(0, start);
        large.observe(u64::MAX, start + Duration::from_nanos(1));
        assert_eq!(
            large.bytes_per_second(start + Duration::from_nanos(1)),
            Some(u64::MAX)
        );
    }

    #[test]
    fn high_frequency_samples_remain_bounded_and_recent() {
        let start = Instant::now();
        let mut rate = TransferRate::new(0, start);
        rate.observe(1_000_000, start + Duration::from_millis(1));
        for tick in 2..=100_000 {
            rate.observe(1_000_000 + tick * 100, start + Duration::from_millis(tick));
            assert!(rate.samples.len() <= MAX_SAMPLES);
        }
        assert_eq!(
            rate.bytes_per_second(start + Duration::from_secs(100)),
            Some(100_000)
        );
    }
}

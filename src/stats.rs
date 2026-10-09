// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Rolling statistics persisted in local data storage: the response-length baseline and the
//! two-bucket negative-feedback window.

use serde::{Deserialize, Serialize};

/// Beyond this many samples the baseline becomes an exponential moving estimate, so it tracks
/// drift instead of freezing on old traffic.
const BASELINE_HORIZON: u64 = 1000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub count: u64,
    pub mean: f64,
    pub variance: f64,
}

impl Baseline {
    /// z-score of `x`, once `warmup` samples were seen and the variance is non-degenerate.
    pub fn z_score(&self, x: f64, warmup: u64) -> Option<f64> {
        if self.count < warmup || self.variance <= 1e-9 {
            return None;
        }
        Some((x - self.mean) / self.variance.sqrt())
    }

    /// Welford update capped at `BASELINE_HORIZON` effective samples.
    pub fn update(&mut self, x: f64) {
        self.count = self.count.saturating_add(1);
        let n = self.count.min(BASELINE_HORIZON) as f64;
        let delta = x - self.mean;
        self.mean += delta / n;
        self.variance = (1.0 - 1.0 / n) * (self.variance + delta * delta / n);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FeedbackWindow {
    pub bucket: u64,
    pub total: u64,
    pub negative: u64,
    pub prev_total: u64,
    pub prev_negative: u64,
}

impl FeedbackWindow {
    /// Advances to `bucket`, shifting or clearing counts that fell out of the window.
    pub fn roll(&mut self, bucket: u64) {
        if bucket == self.bucket {
            return;
        }
        if bucket == self.bucket + 1 {
            self.prev_total = self.total;
            self.prev_negative = self.negative;
        } else {
            self.prev_total = 0;
            self.prev_negative = 0;
        }
        self.bucket = bucket;
        self.total = 0;
        self.negative = 0;
    }

    pub fn record(&mut self, negative: bool) {
        self.total = self.total.saturating_add(1);
        if negative {
            self.negative = self.negative.saturating_add(1);
        }
    }

    /// `(samples, negative rate)` across the current and previous bucket.
    pub fn rate(&self) -> (u64, f64) {
        let samples = self.total + self.prev_total;
        if samples == 0 {
            return (0, 0.0);
        }
        (
            samples,
            (self.negative + self.prev_negative) as f64 / samples as f64,
        )
    }
}

pub fn bucket_for(now_secs: u64, window_seconds: u64) -> u64 {
    now_secs / window_seconds.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_baseline_mean_variance_and_warmup() {
        let mut b = Baseline::default();
        for x in [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0] {
            b.update(x);
        }
        assert!((b.mean - 5.0).abs() < 1e-9);
        assert!((b.variance - 4.0).abs() < 1e-9);
        assert_eq!(b.z_score(9.0, 20), None, "not warmed up");
        assert!((b.z_score(9.0, 8).unwrap() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn test_baseline_with_zero_variance_has_no_z_score() {
        let mut b = Baseline::default();
        for _ in 0..50 {
            b.update(3.0);
        }
        assert_eq!(b.z_score(10.0, 1), None);
    }

    #[test]
    fn test_baseline_tracks_drift_after_horizon() {
        let mut b = Baseline::default();
        for _ in 0..2000 {
            b.update(1.0);
        }
        for _ in 0..3000 {
            b.update(10.0);
        }
        assert!(b.mean > 9.0, "mean {}", b.mean);
    }

    #[test]
    fn test_feedback_window_roll_and_rate() {
        let mut w = FeedbackWindow {
            bucket: 5,
            ..Default::default()
        };
        w.record(true);
        w.record(false);
        assert_eq!(w.rate(), (2, 0.5));
        w.roll(6);
        w.record(true);
        assert_eq!(w.rate().0, 3);
        assert!((w.rate().1 - 2.0 / 3.0).abs() < 1e-9);
        w.roll(9);
        assert_eq!(w.rate(), (0, 0.0));
    }

    #[test]
    fn test_bucket_for() {
        assert_eq!(bucket_for(601, 300), 2);
        assert_eq!(bucket_for(5, 0), 5);
    }
}

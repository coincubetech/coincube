use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Instant,
};

pub type SharedMetrics = Arc<Mutex<Metrics>>;

#[derive(Debug, Default)]
pub struct Metrics {
    last_redraw: Option<Instant>,
    intervals: VecDeque<f64>,
    prepare: VecDeque<f64>,
    encode: VecDeque<f64>,
    pub renders: u64,
}

fn push(samples: &mut VecDeque<f64>, value: f64) {
    if samples.len() == 300 {
        samples.pop_front();
    }
    samples.push_back(value);
}

impl Metrics {
    pub fn redraw(&mut self, at: Instant) {
        if let Some(last) = self.last_redraw.replace(at) {
            push(
                &mut self.intervals,
                at.saturating_duration_since(last).as_secs_f64() * 1000.0,
            );
        }
    }
    pub fn prepared(&mut self, ms: f64) {
        push(&mut self.prepare, ms);
    }
    pub fn encoded(&mut self, ms: f64) {
        push(&mut self.encode, ms);
        self.renders += 1;
    }
    pub fn summary(&self) -> String {
        if self.intervals.is_empty() {
            return "Start benchmark to sample redraw cadence".into();
        }
        let mean = self.intervals.iter().sum::<f64>() / self.intervals.len() as f64;
        let mut sorted: Vec<_> = self.intervals.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let p95 = sorted[((sorted.len() - 1) as f64 * 0.95).ceil() as usize];
        let average =
            |samples: &VecDeque<f64>| samples.iter().sum::<f64>() / samples.len().max(1) as f64;
        format!("{:.1} redraws/s  ·  interval {:.2} ms / p95 {:.2} ms  ·  CPU prepare {:.3} ms / encode {:.3} ms  ·  {} scene draws", 1000.0 / mean.max(0.001), mean, p95, average(&self.prepare), average(&self.encode), self.renders)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rolling_metrics_stay_bounded_and_report_known_cadence() {
        let mut metrics = Metrics::default();
        let start = Instant::now();
        for i in 0..1000 {
            metrics.redraw(start + std::time::Duration::from_millis(i * 20));
            metrics.prepared(0.1);
            metrics.encoded(0.2);
        }
        assert_eq!(metrics.intervals.len(), 300);
        assert_eq!(metrics.prepare.len(), 300);
        assert!(metrics.summary().starts_with("50.0 redraws/s"));
    }
}

//! Opt-in frame-interval evidence for the real board acceptance run.
//!
//! This is disabled unless `SOTTO_BOARD_METRICS=1`. When enabled the board requests animation
//! frames continuously and prints non-cumulative intervals at 1/10/20/30 minutes. It does not
//! claim CPU, RSS, GPU, or user-perceived readability; the T035 runbook records those separately.

use std::time::{Duration, Instant};

const DEFAULT_CHECKPOINT_SECONDS: [u64; 4] = [60, 600, 1_200, 1_800];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct BoardMetricsObservation {
    pub total_items: usize,
    pub visible_items: usize,
    /// Visible local frame paths admitted to GPUI's loader, not confirmed decoded assets.
    pub eligible_thumbnail_paths: usize,
    pub following_frontier: bool,
}

pub(super) struct BoardMetrics {
    enabled: bool,
    started: Option<Instant>,
    last_frame: Option<Instant>,
    intervals: Vec<Duration>,
    checkpoints: Vec<Duration>,
    next_checkpoint: usize,
}

impl BoardMetrics {
    pub(super) fn from_environment() -> Self {
        Self::new(
            std::env::var("SOTTO_BOARD_METRICS").is_ok_and(|value| value == "1"),
            DEFAULT_CHECKPOINT_SECONDS.map(Duration::from_secs).to_vec(),
            None,
        )
    }

    fn new(enabled: bool, checkpoints: Vec<Duration>, started: Option<Instant>) -> Self {
        Self {
            enabled,
            started: if enabled { started } else { None },
            last_frame: None,
            intervals: Vec::new(),
            checkpoints,
            next_checkpoint: 0,
        }
    }

    pub(super) const fn is_running(&self) -> bool {
        self.started.is_some()
    }

    pub(super) fn observe(&mut self, observation: BoardMetricsObservation) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        self.observe_at(now, observation, |report| eprintln!("{report}"));
    }

    fn observe_at(
        &mut self,
        now: Instant,
        observation: BoardMetricsObservation,
        mut emit: impl FnMut(String),
    ) {
        if self.started.is_none() {
            if observation.total_items == 0 {
                return;
            }
            self.started = Some(now);
            self.last_frame = Some(now);
            return;
        }
        if let Some(last) = self.last_frame {
            self.intervals.push(now.saturating_duration_since(last));
        }
        self.last_frame = Some(now);
        let Some(checkpoint) = self.checkpoints.get(self.next_checkpoint).copied() else {
            return;
        };
        let Some(started) = self.started else {
            return;
        };
        if now.saturating_duration_since(started) < checkpoint {
            return;
        }
        let mut sorted = self.intervals.clone();
        sorted.sort_unstable();
        let p50 = percentile(&sorted, 50);
        let p95 = percentile(&sorted, 95);
        let max = sorted.last().copied().unwrap_or_default();
        emit(format!(
            "SOTTO_BOARD_METRIC checkpoint_s={} samples={} frame_interval_p50_ms={:.3} frame_interval_p95_ms={:.3} frame_interval_max_ms={:.3} total_items={} visible_items={} eligible_thumbnail_paths={} following_frontier={}",
            checkpoint.as_secs(),
            sorted.len(),
            p50.as_secs_f64() * 1_000.0,
            p95.as_secs_f64() * 1_000.0,
            max.as_secs_f64() * 1_000.0,
            observation.total_items,
            observation.visible_items,
            observation.eligible_thumbnail_paths,
            observation.following_frontier,
        ));
        self.intervals.clear();
        self.next_checkpoint = self.next_checkpoint.saturating_add(1);
        if self.next_checkpoint == self.checkpoints.len() {
            self.started = None;
            self.last_frame = None;
            self.enabled = false;
        }
    }
}

fn percentile(values: &[Duration], percent: usize) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    let rank = values.len().saturating_mul(percent).div_ceil(100);
    let index = rank.saturating_sub(1).min(values.len() - 1);
    values[index]
}

#[cfg(test)]
mod tests {
    use super::{BoardMetrics, BoardMetricsObservation};
    use std::time::{Duration, Instant};

    #[test]
    fn checkpoints_report_non_cumulative_intervals_and_board_counts() {
        let started = Instant::now();
        let mut metrics = BoardMetrics::new(
            true,
            vec![Duration::from_secs(1), Duration::from_secs(2)],
            Some(started),
        );
        let observation = BoardMetricsObservation {
            total_items: 50,
            visible_items: 7,
            eligible_thumbnail_paths: 2,
            following_frontier: true,
        };
        let mut reports = Vec::new();
        metrics.observe_at(started, observation, |report| reports.push(report));
        metrics.observe_at(
            started + Duration::from_millis(500),
            observation,
            |report| reports.push(report),
        );
        metrics.observe_at(started + Duration::from_secs(1), observation, |report| {
            reports.push(report)
        });
        metrics.observe_at(
            started + Duration::from_millis(1_500),
            observation,
            |report| reports.push(report),
        );
        metrics.observe_at(started + Duration::from_secs(2), observation, |report| {
            reports.push(report)
        });

        assert_eq!(reports.len(), 2, "both checkpoints must emit exactly once");
        assert!(
            reports[0].contains("checkpoint_s=1 samples=2"),
            "first checkpoint must contain only its two intervals: {}",
            reports[0]
        );
        assert!(
            reports[1].contains("checkpoint_s=2 samples=2"),
            "second checkpoint must reset rather than accumulate: {}",
            reports[1]
        );
        assert!(
            reports[1].contains(
                "total_items=50 visible_items=7 eligible_thumbnail_paths=2 following_frontier=true"
            ),
            "report must carry board-density evidence: {}",
            reports[1]
        );
        assert!(
            !metrics.is_running(),
            "the harness must stop requesting frames after the last checkpoint"
        );
    }

    #[test]
    fn disabled_metrics_collect_and_emit_nothing() {
        let started = Instant::now();
        let mut metrics = BoardMetrics::new(false, vec![Duration::ZERO], Some(started));
        let mut reports = Vec::new();
        metrics.observe_at(
            started + Duration::from_secs(1),
            BoardMetricsObservation::default(),
            |report| reports.push(report),
        );
        assert!(
            reports.is_empty(),
            "normal board rendering must stay uninstrumented"
        );
        assert!(
            metrics.intervals.is_empty(),
            "disabled mode must not retain samples"
        );
    }

    #[test]
    fn opt_in_metrics_start_on_the_first_board_item() {
        let before_session = Instant::now();
        let mut metrics = BoardMetrics::new(true, vec![Duration::from_secs(1)], None);
        let mut reports = Vec::new();

        metrics.observe_at(
            before_session,
            BoardMetricsObservation::default(),
            |report| reports.push(report),
        );
        assert!(
            !metrics.is_running(),
            "an empty board must not start the clock"
        );

        metrics.observe_at(
            before_session + Duration::from_secs(30),
            BoardMetricsObservation {
                total_items: 1,
                ..BoardMetricsObservation::default()
            },
            |report| reports.push(report),
        );
        assert!(
            metrics.is_running(),
            "the first board item starts the clock"
        );
        assert!(
            reports.is_empty(),
            "the start observation is not a checkpoint"
        );
    }
}

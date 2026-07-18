//! Ceiling-mode load-profile vocabulary (EVT-C7, docs/event-plane-jetstream.md §10).
//!
//! Pure planning/analysis for the ceiling measurement campaigns: the ramp
//! controller (coarse doubling, then a bisect to the knee), the saturation
//! classifier (p99 doubling or achieved-rate divergence), and the CSV shape the
//! §11 provenance ledger points at. The effectful load generation (producer/
//! claimer tasks, DB connections) stays in the bench that drives a campaign
//! (queuebench C7 first; C1/C2 reuse this module).

/// Aggregate stats for one ramp level: `level_secs` of offered load at
/// `offered` runs/sec, measured over the offered window only (drain excluded).
#[derive(Clone, Debug, PartialEq)]
pub struct LevelStats {
    /// The scheduled (open-loop) rate for the level, lifecycles/sec.
    pub offered: f64,
    /// What the producers actually committed, enqueues/sec over the window.
    pub achieved_enqueue: f64,
    /// Completions/sec over the window (the transitions/sec figure).
    pub achieved_complete: f64,
    /// Enqueue→completion sojourn percentiles over window completions, ms.
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    /// Completions inside the offered window (the percentile sample count).
    pub window_completed: u64,
    /// Whether the backlog drained within the level's drain budget afterwards.
    pub drained: bool,
}

/// Whether a level is saturated relative to the ramp's baseline (first) level:
/// the p99 sojourn doubled (with a small absolute floor so a sub-ms baseline
/// doesn't flag on noise), completions diverged from what actually ENTERED the
/// queue, or the backlog never drained. Divergence compares against
/// `achieved_enqueue`, not `offered`: a producer-limited level (the rig fell
/// short of the schedule) is a measurement cap to report, not queue
/// saturation — conflating them under-reports the knee.
pub fn saturated(baseline: &LevelStats, s: &LevelStats) -> bool {
    !s.drained
        || s.achieved_complete < 0.9 * s.achieved_enqueue
        || (s.p99_ms > 2.0 * baseline.p99_ms && s.p99_ms > baseline.p99_ms + 2.0)
}

enum RampState {
    /// Doubling until the first saturated level.
    Coarse {
        next: f64,
    },
    /// Narrowing between the highest good and lowest saturated rate.
    Bisect {
        good: f64,
        bad: f64,
    },
    Done,
}

/// The find-knee ramp controller (§10 methodology: step levels, then a binary
/// search on the level where p99 doubles or throughput diverges).
///
/// Drive it as a loop: `while let Some(rate) = ramp.next_offered() { … run the
/// level … ramp.record(stats); }`, then read [`Ramp::knee`]. The controller
/// doubles from the base rate until a level saturates, then bisects until the
/// good/bad rates are within `tolerance` of each other (or `max_levels` runs
/// have been spent — a wall-clock bound, reported by the caller as a cap).
pub struct Ramp {
    tolerance: f64,
    max_levels: usize,
    levels: Vec<LevelStats>,
    state: RampState,
    /// Index (into `levels`) of the highest-offered unsaturated level.
    best_good: Option<usize>,
}

impl Ramp {
    pub fn new(base_rate: f64, tolerance: f64, max_levels: usize) -> Self {
        Ramp {
            tolerance,
            max_levels,
            levels: Vec::new(),
            state: RampState::Coarse { next: base_rate },
            best_good: None,
        }
    }

    /// The next offered rate to run, or `None` when the ramp is finished.
    pub fn next_offered(&self) -> Option<f64> {
        if self.levels.len() >= self.max_levels {
            return None;
        }
        match self.state {
            RampState::Coarse { next } => Some(next),
            RampState::Bisect { good, bad } if (bad - good) / good > self.tolerance => {
                Some((good + bad) / 2.0)
            }
            _ => None,
        }
    }

    /// Record the stats for the level that `next_offered` scheduled.
    pub fn record(&mut self, stats: LevelStats) {
        let offered = stats.offered;
        // The baseline for p99 doubling is the FIRST level; the first level
        // itself can only saturate on divergence/drain (doubling needs a prior).
        let sat = match self.levels.first() {
            Some(baseline) => saturated(baseline, &stats),
            None => !stats.drained || stats.achieved_complete < 0.9 * stats.achieved_enqueue,
        };
        let idx = self.levels.len();
        self.levels.push(stats);
        self.state = match self.state {
            RampState::Coarse { .. } if !sat => {
                self.best_good = Some(idx);
                RampState::Coarse {
                    next: offered * 2.0,
                }
            }
            RampState::Coarse { .. } => match self.best_good {
                // Saturated at the base rate: no knee above it to find.
                None => RampState::Done,
                Some(good) => RampState::Bisect {
                    good: self.levels[good].offered,
                    bad: offered,
                },
            },
            RampState::Bisect { good, bad } => {
                if sat {
                    RampState::Bisect { good, bad: offered }
                } else {
                    self.best_good = Some(idx);
                    RampState::Bisect { good: offered, bad }
                }
            }
            RampState::Done => RampState::Done,
        };
    }

    /// The knee: the highest-offered level that ran unsaturated (None if the
    /// base rate itself saturated). Publish its `achieved_complete` — the
    /// SUSTAINED transitions/sec — not the offered schedule: at a
    /// producer-limited level the schedule overstates what was demonstrated.
    pub fn knee(&self) -> Option<&LevelStats> {
        self.best_good.map(|i| &self.levels[i])
    }

    /// Every recorded level, in run order (the ramp CSV source).
    pub fn levels(&self) -> &[LevelStats] {
        &self.levels
    }
}

/// The per-level ramp curve as CSV (one row per level, run order).
pub fn ramp_csv(levels: &[LevelStats]) -> String {
    let mut out = String::from(
        "offered_per_s,achieved_enqueue_per_s,achieved_complete_per_s,\
         p50_ms,p99_ms,p999_ms,window_completed,drained\n",
    );
    for s in levels {
        out.push_str(&format!(
            "{:.0},{:.1},{:.1},{:.3},{:.3},{:.3},{},{}\n",
            s.offered,
            s.achieved_enqueue,
            s.achieved_complete,
            s.p50_ms,
            s.p99_ms,
            s.p999_ms,
            s.window_completed,
            s.drained
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A level at `offered` served by a system whose capacity is `cap`: below
    /// capacity it completes what is offered at a flat p99; above, completions
    /// pin at capacity and the sojourn tail explodes.
    fn level(offered: f64, cap: f64) -> LevelStats {
        let saturated = offered > cap;
        LevelStats {
            offered,
            achieved_enqueue: offered,
            achieved_complete: if saturated { cap } else { offered },
            p50_ms: 1.0,
            p99_ms: if saturated { 500.0 } else { 4.0 },
            p999_ms: if saturated { 900.0 } else { 8.0 },
            window_completed: offered as u64 * 60,
            drained: true,
        }
    }

    #[test]
    fn coarse_doubles_then_bisects_to_the_knee() {
        // Capacity 3000: coarse 250→500→1000→2000→4000(sat), then bisect
        // 3000(good)→3500(sat, divergence)→3250(sat via the p99 tail — over
        // capacity the sojourn explodes even though 3000/s ≥ 0.9×3250) and the
        // good/bad gap (3000..3250) is within the 15% tolerance.
        let mut ramp = Ramp::new(250.0, 0.15, 16);
        let mut offered_seq = Vec::new();
        while let Some(rate) = ramp.next_offered() {
            offered_seq.push(rate);
            ramp.record(level(rate, 3000.0));
        }
        assert_eq!(
            offered_seq,
            vec![250.0, 500.0, 1000.0, 2000.0, 4000.0, 3000.0, 3500.0, 3250.0]
        );
        assert_eq!(ramp.knee().map(|k| k.offered), Some(3000.0));
        assert_eq!(ramp.levels().len(), 8);
    }

    #[test]
    fn a_saturated_base_rate_yields_no_knee() {
        let mut ramp = Ramp::new(1000.0, 0.15, 16);
        assert_eq!(ramp.next_offered(), Some(1000.0));
        ramp.record(level(1000.0, 400.0));
        assert_eq!(ramp.next_offered(), None);
        assert!(ramp.knee().is_none());
    }

    #[test]
    fn p99_doubling_alone_is_saturation() {
        // Completions keep up (no divergence) but the tail doubled past the
        // 2 ms absolute floor: the level must classify saturated.
        let baseline = level(250.0, 10_000.0);
        let mut slow = level(500.0, 10_000.0);
        slow.p99_ms = baseline.p99_ms * 2.5;
        assert!(saturated(&baseline, &slow));
        // …but a doubled SUB-MILLISECOND tail under the floor is noise, not a knee.
        let mut tiny = baseline.clone();
        tiny.p99_ms = 0.4;
        let mut tiny_doubled = level(500.0, 10_000.0);
        tiny_doubled.p99_ms = 1.1;
        assert!(!saturated(&tiny, &tiny_doubled));
    }

    #[test]
    fn rate_divergence_alone_is_saturation() {
        // The tail stays flat but completions fall >10% short of what entered
        // the queue: the level must classify saturated on divergence alone.
        let baseline = level(250.0, 10_000.0);
        let mut diverged = level(500.0, 10_000.0);
        diverged.achieved_complete = 400.0;
        assert!(saturated(&baseline, &diverged));
    }

    #[test]
    fn a_producer_limited_level_is_not_queue_saturation() {
        // The rig fell >10% short of the SCHEDULE but the queue completed
        // essentially everything that entered, at a flat tail: that is a
        // measurement cap to report, not a knee.
        let baseline = level(250.0, 10_000.0);
        let mut limited = level(750.0, 10_000.0);
        limited.achieved_enqueue = 660.0;
        limited.achieved_complete = 655.0;
        assert!(!saturated(&baseline, &limited));
    }

    #[test]
    fn a_drain_timeout_is_saturation() {
        let baseline = level(250.0, 10_000.0);
        let mut stuck = level(500.0, 10_000.0);
        stuck.drained = false;
        assert!(saturated(&baseline, &stuck));
    }

    #[test]
    fn max_levels_bounds_the_ramp() {
        let mut ramp = Ramp::new(250.0, 0.0001, 4);
        let mut n = 0;
        while let Some(rate) = ramp.next_offered() {
            ramp.record(level(rate, 3000.0));
            n += 1;
        }
        assert_eq!(n, 4, "the wall-clock bound must stop an unconverged bisect");
    }

    #[test]
    fn ramp_csv_has_a_header_and_one_row_per_level() {
        let levels = vec![level(250.0, 3000.0), level(500.0, 3000.0)];
        let csv = ramp_csv(&levels);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("offered_per_s,"));
        assert!(lines[1].starts_with("250,"));
        assert!(lines[2].contains(",true"));
    }
}

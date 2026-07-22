//! [9.8] Per-component linear-memory metric bridge (wamn-jn6).
//!
//! The D16 fork limiter ([`wash_runtime::engine::ctx::WamnStoreLimiter`]) tracks
//! each budgeted store's high-water linear-memory size and its denied grow count,
//! and logs a `wamn::memory` event on every denial. This module turns that
//! per-store state into OTel `wamn.memory.*` metrics WITHOUT re-parsing the logs
//! and WITHOUT a custom tracing layer (the fork owns subscriber init): the host
//! reads a store's limiter through the read-only accessors the carried fork
//! commit adds and publishes a snapshot into a shared registry that three
//! observable instruments read at export time — the S5 observable-counter shape
//! (`wamn_logging::register_metrics`).
//!
//! Emission model: whoever DRIVES a store with a limiter attached calls
//! [`MemoryMeter::snapshot_from`] after the store runs (the run-worker after each
//! drive). The instruments ride the global meter provider
//! `initialize_observability` installs when `OTEL_*` is set, so with no collector
//! this is inert.
//!
//! Cardinality: keyed only by `component` (bounded — one long-lived component per
//! runner replica; never `run_id`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use opentelemetry::KeyValue;
use wash_runtime::engine::ctx::WamnStoreLimiter;

/// One component's last-published limiter snapshot.
#[derive(Clone, Copy)]
struct MemState {
    high_water: u64,
    denied: u64,
    budget: Option<u64>,
}

#[derive(Default)]
struct Registry {
    by_component: HashMap<String, MemState>,
}

/// A cloneable handle over the shared per-component snapshot registry the
/// `wamn.memory.*` observable callbacks read. Construct the process-wide one via
/// [`global_memory_meter`] (registers the instruments exactly once).
#[derive(Clone)]
pub struct MemoryMeter {
    inner: Arc<Mutex<Registry>>,
}

impl MemoryMeter {
    /// Register the three `wamn.memory.*` observable instruments against the
    /// global meter and return the handle whose registry they read. Call ONCE per
    /// process (observable instruments warn on duplicate registration) — use
    /// [`global_memory_meter`].
    fn register() -> Self {
        let inner = Arc::new(Mutex::new(Registry::default()));
        let meter = opentelemetry::global::meter("wamn-host");

        // Denials: monotonic per limiter — an observable counter reporting the
        // current cumulative total per component.
        let by = inner.clone();
        let _ = meter
            .u64_observable_counter("wamn.memory.denied")
            .with_description(
                "linear-memory grow attempts denied for exceeding the component budget",
            )
            .with_callback(move |o| {
                if let Ok(r) = by.lock() {
                    for (component, s) in &r.by_component {
                        o.observe(s.denied, &[KeyValue::new("component", component.clone())]);
                    }
                }
            })
            .build();

        // High-water: the largest linear memory a component's store reached.
        let by = inner.clone();
        let _ = meter
            .u64_observable_gauge("wamn.memory.high_water_bytes")
            .with_description("largest linear-memory size a component's store reached (bytes)")
            .with_callback(move |o| {
                if let Ok(r) = by.lock() {
                    for (component, s) in &r.by_component {
                        o.observe(
                            s.high_water,
                            &[KeyValue::new("component", component.clone())],
                        );
                    }
                }
            })
            .build();

        // Budget: only budgeted components observe (an unbudgeted tracking
        // limiter has `None` and contributes no series — the ceiling comparison
        // is meaningful only against a real budget).
        let by = inner.clone();
        let _ = meter
            .u64_observable_gauge("wamn.memory.budget_bytes")
            .with_description("per-component linear-memory budget in bytes (budgeted stores only)")
            .with_callback(move |o| {
                if let Ok(r) = by.lock() {
                    for (component, s) in &r.by_component {
                        if let Some(budget) = s.budget {
                            o.observe(budget, &[KeyValue::new("component", component.clone())]);
                        }
                    }
                }
            })
            .build();

        Self { inner }
    }

    /// Publish a store's limiter state for its component. The bridge reads the
    /// HIGH-WATER for the high-water gauge and the BUDGET for the budget gauge —
    /// never conflating the two (the metricbench phase-4 mutant). An unattached
    /// (empty-id) default limiter is skipped so a limiter-less driver never
    /// publishes a bogus `component=""` series.
    pub fn snapshot_from(&self, limiter: &WamnStoreLimiter) {
        let component = limiter.component_id();
        if component.is_empty() {
            return;
        }
        if let Ok(mut r) = self.inner.lock() {
            r.by_component.insert(
                component.to_string(),
                MemState {
                    high_water: limiter.high_water_bytes() as u64,
                    denied: limiter.denied_total(),
                    budget: limiter.budget_bytes().map(|b| b as u64),
                },
            );
        }
    }

    /// The last snapshot published for `component` as
    /// `(high_water_bytes, denied_total, budget_bytes)`. The in-proc readback the
    /// unit test and the metricbench gate assert against (the scrape is the
    /// export-side check).
    pub fn snapshot_of(&self, component: &str) -> Option<(u64, u64, Option<u64>)> {
        let r = self.inner.lock().ok()?;
        r.by_component
            .get(component)
            .map(|s| (s.high_water, s.denied, s.budget))
    }
}

/// The process-wide memory meter, its instruments registered against the global
/// provider on first use. Every driver that snapshots a store's limiter shares
/// this one handle, so the observable instruments are registered exactly once.
pub fn global_memory_meter() -> MemoryMeter {
    static METER: OnceLock<MemoryMeter> = OnceLock::new();
    METER.get_or_init(MemoryMeter::register).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;
    use wash_runtime::wasmtime::ResourceLimiter;

    // The bridge must read HIGH-WATER for the high-water gauge and the DENIED
    // count for the denial counter — a mutant that reads `budget_bytes` where it
    // means `high_water` (or never advances `denied`) is caught here: after a
    // 32 MiB allow then a 128 MiB deny under a 64 MiB budget, the snapshot must
    // read high_water = 32 MiB (NOT the 64 MiB budget) and denied = 1.
    #[test]
    fn snapshot_reads_high_water_and_denied_not_budget() {
        const MIB: usize = 1 << 20;
        let mut limiter = WamnStoreLimiter::new(64 * MIB, StdArc::from("memtest"));
        // An allowed grow sets the high-water; a grow past budget is denied.
        assert_eq!(limiter.memory_growing(0, 32 * MIB, None).unwrap(), true);
        assert_eq!(
            limiter.memory_growing(32 * MIB, 128 * MIB, None).unwrap(),
            false
        );

        // Register a standalone meter (not the global one — no global provider in
        // a unit test) and publish the snapshot.
        let meter = MemoryMeter::register();
        meter.snapshot_from(&limiter);

        let (high_water, denied, budget) =
            meter.snapshot_of("memtest").expect("component published");
        assert_eq!(
            high_water,
            (32 * MIB) as u64,
            "high-water is the allowed size"
        );
        assert_ne!(
            high_water,
            (64 * MIB) as u64,
            "high-water is NOT the budget"
        );
        assert_eq!(denied, 1, "one grow was denied");
        assert_eq!(budget, Some((64 * MIB) as u64));
    }

    // An unattached default limiter (empty component id) publishes nothing — a
    // limiter-less driver must not create a bogus `component=""` series.
    #[test]
    fn empty_component_limiter_is_not_published() {
        let limiter = WamnStoreLimiter::default();
        let meter = MemoryMeter::register();
        meter.snapshot_from(&limiter);
        assert_eq!(meter.snapshot_of(""), None);
    }
}

//! The wiring resolution cache (wamn-0h0g.16.5): resolve once, serve from
//! memory, and go back to the store only when the activation pointer flips.
//!
//! Every case here is in-process — no store, no cluster, no clock. What the
//! cache promises the hot path is that a resolved wiring is handed back as the
//! SAME `Arc`, so `Arc::ptr_eq` is the assertion that a lookup did not go
//! anywhere; and what it promises correctness is that
//! [`WiringCache::invalidate`] is the one thing that can make it.

use std::num::NonZeroUsize;
use std::sync::Arc;

use serde_json::Value;
use wamn_router::{Wiring, WiringCache, WiringNode};

const ENV: &str = "prod";

fn cache(max_entries: usize) -> WiringCache {
    WiringCache::new(NonZeroUsize::new(max_entries).expect("fixture bound is non-zero"))
}

/// A one-node wiring whose entry names `entry`, so two fixtures are tellable
/// apart by [`Wiring::entry`] alone.
fn wiring(entry: &str) -> Wiring {
    Wiring::compile(
        entry,
        vec![WiringNode {
            id: entry.to_string(),
            component: "echo".to_string(),
            operation: "run".to_string(),
            config: Value::Null,
            connection: None,
            terminal: None,
        }],
        vec![],
    )
    .expect("fixture wiring compiles")
}

// ---- resolve once, serve from memory --------------------------------------

#[test]
fn a_resolved_wiring_is_served_from_memory_on_every_later_delivery() {
    let cache = cache(8);
    let installed = cache.insert(ENV, "orders", 7, wiring("v7"));

    for _ in 0..3 {
        let hit = cache.get(ENV, "orders").expect("resident after insert");
        assert_eq!(hit.version, 7);
        assert!(
            Arc::ptr_eq(&hit.wiring, &installed),
            "a hit must hand back the resident graph, not a rebuilt one"
        );
    }
    assert_eq!(cache.len(), 1, "three deliveries, one entry");
}

#[test]
fn an_unresolved_wiring_misses() {
    let cache = cache(8);
    assert!(cache.get(ENV, "orders").is_none());
    assert!(cache.is_empty());
}

/// The pointer is env-scoped: the same wiring id in another environment is a
/// different activation and must not be answered from this one.
#[test]
fn environments_do_not_share_a_pointer() {
    let cache = cache(8);
    cache.insert(ENV, "orders", 7, wiring("prod-v7"));

    assert!(cache.get("staging", "orders").is_none());
    assert_eq!(cache.get(ENV, "orders").expect("prod resident").version, 7);
}

// ---- the pointer flip ------------------------------------------------------

/// The direct proof that `invalidate` works. Its production caller is the
/// doorbell subscriber of wamn-0h0g.18.2, which is NOT wired to it yet — so this
/// test is currently the only thing exercising the seam, and is what stops the
/// method rotting before the joining bead lands.
#[test]
fn invalidate_sends_the_next_lookup_back_to_the_store() {
    let cache = cache(8);
    cache.insert(ENV, "orders", 7, wiring("v7"));
    assert!(cache.get(ENV, "orders").is_some());

    assert!(
        cache.invalidate(ENV, "orders"),
        "invalidating a live pointer reports that it dropped one"
    );

    assert!(
        cache.get(ENV, "orders").is_none(),
        "a flipped pointer must miss, or the flip never takes effect"
    );
    assert!(
        !cache.invalidate(ENV, "orders"),
        "invalidating an already-dropped pointer reports nothing dropped"
    );
}

/// Invalidation drops the POINTER, not the graphs — which is what makes a
/// rollback flip back to a resident version an in-memory hit.
#[test]
fn invalidate_keeps_the_graphs_so_a_rollback_flip_is_a_hit() {
    let cache = cache(8);
    let seven = cache.insert(ENV, "orders", 7, wiring("v7"));
    cache.invalidate(ENV, "orders");
    assert_eq!(cache.len(), 1, "the graph stayed resident");

    let rolled_back = cache.insert(ENV, "orders", 7, wiring("v7-recompiled"));

    assert!(
        Arc::ptr_eq(&rolled_back, &seven),
        "a version is immutable, so the resident graph is still the right answer"
    );
    assert_eq!(cache.len(), 1, "and no second copy was installed");
}

#[test]
fn a_flip_forward_resolves_the_new_version_and_leaves_the_old_resident() {
    let cache = cache(8);
    let seven = cache.insert(ENV, "orders", 7, wiring("v7"));
    cache.invalidate(ENV, "orders");
    cache.insert(ENV, "orders", 8, wiring("v8"));

    let hit = cache.get(ENV, "orders").expect("the new version resolves");
    assert_eq!(hit.version, 8);
    assert_eq!(hit.wiring.entry(), "v8");
    assert_eq!(cache.len(), 2, "both versions are keyed separately");
    assert!(!Arc::ptr_eq(&hit.wiring, &seven));
}

// ---- bounded, deterministic eviction --------------------------------------

#[test]
fn eviction_is_bounded_and_least_recently_used() {
    let cache = cache(2);
    cache.insert(ENV, "a", 1, wiring("a"));
    cache.insert(ENV, "b", 1, wiring("b"));
    // Touch `a`, making `b` the least recently used.
    cache.get(ENV, "a").expect("a is resident");

    cache.insert(ENV, "c", 1, wiring("c"));

    assert_eq!(cache.len(), 2, "the entry bound holds");
    assert!(cache.get(ENV, "a").is_some(), "the touched entry survived");
    assert!(cache.get(ENV, "c").is_some());
    assert!(
        cache.get(ENV, "b").is_none(),
        "the least recently used entry was the one evicted"
    );
}

/// An evicted entry must take its pointer with it: a pointer naming a graph
/// that is gone would read as a resolution and produce nothing.
#[test]
fn an_evicted_entry_leaves_behind_no_pointer() {
    let cache = cache(1);
    cache.insert(ENV, "a", 1, wiring("a"));
    cache.insert(ENV, "b", 1, wiring("b"));

    assert_eq!(cache.len(), 1);
    assert!(cache.get(ENV, "a").is_none());
    assert!(cache.get(ENV, "b").is_some());
}

// ---- the instrument --------------------------------------------------------

/// The cache-hit series is a dashboard contract, so its name and its one
/// attribute are pinned here — the same way `services/executor` pins
/// `wamn.run.drain.failures`. Renaming either breaks a chart, not a build.
#[test]
fn the_cache_reports_hits_and_misses_beside_the_run_instruments() {
    let source = include_str!("../src/resolution.rs");
    assert!(source.contains(r#"u64_counter("wamn.run.wiring.cache.lookups")"#));
    assert!(source.contains(r#"KeyValue::new("#));
    for label in [r#""hit""#, r#""miss""#] {
        assert!(source.contains(label), "the {label} bucket disappeared");
    }
}

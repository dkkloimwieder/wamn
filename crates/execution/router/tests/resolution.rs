//! Immutable wiring cache tests: exact versions share their resident graph,
//! remain isolated by environment, and obey the bounded LRU.

use std::num::NonZeroUsize;
use std::sync::Arc;

use serde_json::Value;
use wamn_router::{CacheInsert, Wiring, WiringCache, WiringNode};

const TENANT: &str = "t1";
const PACKAGE: &str = "shop";
const ENV: &str = "prod";
const RELEASE: u32 = 7;

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
            operation: "echo".to_string(),
            config: Value::Null,
            connection: None,
            terminal: None,
        }],
        vec![],
    )
    .expect("fixture wiring compiles")
}

/// The graph hash a fixture resolves `(wiring_id, version)` under. A version is
/// immutable, so its hash is a function of its identity — a recompilation of the
/// same version must present the same hash, or the cache refuses it as a
/// [`CacheInsert::HashMismatch`].
fn graph_hash(wiring_id: &str, version: u32) -> String {
    format!("sha256:{wiring_id}-v{version}")
}

/// Install a resolved immutable version as the released delivery path does.
fn resolve(
    cache: &WiringCache,
    environment: &str,
    effective_release_id: u32,
    wiring_id: &str,
    version: u32,
    graph: Wiring,
) -> Arc<Wiring> {
    match cache.insert_version(
        TENANT,
        PACKAGE,
        environment,
        effective_release_id,
        wiring_id,
        version,
        graph_hash(wiring_id, version),
        graph,
        (),
    ) {
        CacheInsert::Installed(active) => active.wiring,
        other => {
            panic!("fixture versions use one immutable graph hash: {other:?}")
        }
    }
}

// ---- resolve once, serve from memory --------------------------------------

#[test]
fn a_resolved_wiring_is_served_from_memory_on_every_later_delivery() {
    let cache = cache(8);
    let installed = resolve(&cache, ENV, RELEASE, "orders", 7, wiring("v7"));

    for _ in 0..3 {
        let hit = cache
            .get_version(TENANT, PACKAGE, ENV, RELEASE, "orders", 7)
            .expect("resident after insert");
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
    assert!(
        cache
            .get_version(TENANT, PACKAGE, ENV, RELEASE, "orders", 7)
            .is_none()
    );
    assert!(cache.is_empty());
}

/// The same wiring version in another environment has a separate cache entry.
#[test]
fn environments_do_not_share_an_entry() {
    let cache = cache(8);
    resolve(&cache, ENV, RELEASE, "orders", 7, wiring("prod-v7"));

    assert!(
        cache
            .get_version(TENANT, PACKAGE, "staging", RELEASE, "orders", 7)
            .is_none()
    );
    assert_eq!(
        cache
            .get_version(TENANT, PACKAGE, ENV, RELEASE, "orders", 7)
            .expect("prod resident")
            .version,
        7
    );
}

/// Repeated resolution of an immutable version shares the resident graph.
#[test]
fn repeated_resolution_shares_the_resident_graph() {
    let cache = cache(8);
    let seven = resolve(&cache, ENV, RELEASE, "orders", 7, wiring("v7"));
    assert_eq!(cache.len(), 1, "the graph stayed resident");

    let rolled_back = resolve(&cache, ENV, RELEASE, "orders", 7, wiring("v7-recompiled"));

    assert!(
        Arc::ptr_eq(&rolled_back, &seven),
        "a version is immutable, so the resident graph is still the right answer"
    );
    assert_eq!(cache.len(), 1, "and no second copy was installed");
}

#[test]
fn distinct_versions_remain_resident() {
    let cache = cache(8);
    let seven = resolve(&cache, ENV, RELEASE, "orders", 7, wiring("v7"));
    resolve(&cache, ENV, RELEASE, "orders", 8, wiring("v8"));

    let hit = cache
        .get_version(TENANT, PACKAGE, ENV, RELEASE, "orders", 8)
        .expect("the new version resolves");
    assert_eq!(hit.version, 8);
    assert_eq!(hit.wiring.entry(), "v8");
    assert_eq!(cache.len(), 2, "both versions are keyed separately");
    assert!(!Arc::ptr_eq(&hit.wiring, &seven));
    assert!(Arc::ptr_eq(
        &cache
            .get_version(TENANT, PACKAGE, ENV, RELEASE, "orders", 7)
            .expect("the old version remains resident")
            .wiring,
        &seven
    ));
}

// ---- bounded, deterministic eviction --------------------------------------

#[test]
fn eviction_is_bounded_and_least_recently_used() {
    let cache = cache(2);
    resolve(&cache, ENV, RELEASE, "a", 1, wiring("a"));
    resolve(&cache, ENV, RELEASE, "b", 1, wiring("b"));
    // Touch `a`, making `b` the least recently used.
    cache
        .get_version(TENANT, PACKAGE, ENV, RELEASE, "a", 1)
        .expect("a is resident");

    resolve(&cache, ENV, RELEASE, "c", 1, wiring("c"));

    assert_eq!(cache.len(), 2, "the entry bound holds");
    assert!(
        cache
            .get_version(TENANT, PACKAGE, ENV, RELEASE, "a", 1)
            .is_some(),
        "the touched entry survived"
    );
    assert!(
        cache
            .get_version(TENANT, PACKAGE, ENV, RELEASE, "c", 1)
            .is_some()
    );
    assert!(
        cache
            .get_version(TENANT, PACKAGE, ENV, RELEASE, "b", 1)
            .is_none(),
        "the least recently used entry was the one evicted"
    );
}

/// An evicted version must miss on its next lookup.
#[test]
fn an_evicted_version_misses() {
    let cache = cache(1);
    resolve(&cache, ENV, RELEASE, "a", 1, wiring("a"));
    resolve(&cache, ENV, RELEASE, "b", 1, wiring("b"));

    assert_eq!(cache.len(), 1);
    assert!(
        cache
            .get_version(TENANT, PACKAGE, ENV, RELEASE, "a", 1)
            .is_none()
    );
    assert!(
        cache
            .get_version(TENANT, PACKAGE, ENV, RELEASE, "b", 1)
            .is_some()
    );
}

// ---- the instrument --------------------------------------------------------

// wamn-hopk R5: the cache-hit series was pinned by grepping this crate's source
// for the instrument name. Deleted; a metric-name contract is a live-probe
// question, never a text search.

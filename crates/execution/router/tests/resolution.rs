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
use wamn_router::{CacheInsert, Wiring, WiringCache, WiringNode};

const TENANT: &str = "t1";
const CATALOG: &str = "shop";
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

/// One resolution end to end, exactly as the hot path performs it: miss, read
/// the store, install what the read found under the token that miss handed out.
/// Every fixture below resolves through here, so no test installs a pointer by a
/// route production does not have.
fn resolve(
    cache: &WiringCache,
    environment: &str,
    wiring_id: &str,
    version: u32,
    graph: Wiring,
) -> Arc<Wiring> {
    let token = cache
        .get(TENANT, CATALOG, environment, wiring_id)
        .miss()
        .expect("a fixture resolves only what it has just found missing");
    match cache.insert(
        TENANT,
        CATALOG,
        environment,
        wiring_id,
        version,
        graph_hash(wiring_id, version),
        graph,
        (),
        token,
    ) {
        CacheInsert::Installed(active) => active.wiring,
        other => {
            panic!("nothing invalidated between this fixture's miss and its install: {other:?}")
        }
    }
}

// ---- resolve once, serve from memory --------------------------------------

#[test]
fn a_resolved_wiring_is_served_from_memory_on_every_later_delivery() {
    let cache = cache(8);
    let installed = resolve(&cache, ENV, "orders", 7, wiring("v7"));

    for _ in 0..3 {
        let hit = cache
            .get(TENANT, CATALOG, ENV, "orders")
            .hit()
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
    assert!(cache.get(TENANT, CATALOG, ENV, "orders").hit().is_none());
    assert!(cache.is_empty());
}

/// The pointer is env-scoped: the same wiring id in another environment is a
/// different activation and must not be answered from this one.
#[test]
fn environments_do_not_share_a_pointer() {
    let cache = cache(8);
    resolve(&cache, ENV, "orders", 7, wiring("prod-v7"));

    assert!(
        cache
            .get(TENANT, CATALOG, "staging", "orders")
            .hit()
            .is_none()
    );
    assert_eq!(
        cache
            .get(TENANT, CATALOG, ENV, "orders")
            .hit()
            .expect("prod resident")
            .version,
        7
    );
}

// ---- the pointer flip ------------------------------------------------------

/// The direct proof that `invalidate` works. Its production caller is the
/// doorbell subscriber of wamn-0h0g.18.2, joined to it by wamn-0h0g.16.15 in
/// `crates/platform/runtime/src/wiring_doorbell.rs`.
#[test]
fn invalidate_sends_the_next_lookup_back_to_the_store() {
    let cache = cache(8);
    resolve(&cache, ENV, "orders", 7, wiring("v7"));
    assert!(cache.get(TENANT, CATALOG, ENV, "orders").hit().is_some());

    assert!(
        cache.invalidate(TENANT, CATALOG, ENV, "orders"),
        "invalidating a live pointer reports that it dropped one"
    );

    assert!(
        cache.get(TENANT, CATALOG, ENV, "orders").hit().is_none(),
        "a flipped pointer must miss, or the flip never takes effect"
    );
    assert!(
        !cache.invalidate(TENANT, CATALOG, ENV, "orders"),
        "invalidating an already-dropped pointer reports nothing dropped"
    );
}

// ---- the resolution in flight ---------------------------------------------

/// wamn-0h0g.16.16 — THE INTERLEAVING. A miss reads the store outside the
/// cache's lock, so a flip can commit while that read is in the air. The read
/// then comes back holding a version the doorbell has already invalidated, and
/// installing it would undo the invalidation: the cache would serve the
/// superseded version until the NEXT flip, which may never come.
///
/// Note that nothing is resident when the doorbell rings here — `invalidate`
/// drops NOTHING. The in-flight read is precisely the case where there is no
/// pointer to drop, which is why dropping pointers cannot be the whole of it.
#[test]
fn a_resolution_in_flight_across_an_invalidation_cannot_install_its_stale_pointer() {
    let cache = cache(8);
    // The env-hot store, as a reader sees it. The flip moves it mid-read.
    let mut store = (7, "v7");

    // t0: the delivery misses and goes to the store, which says v7.
    let token = cache
        .get(TENANT, CATALOG, ENV, "orders")
        .miss()
        .expect("nothing is resident yet");
    let read = store;

    // t1: the activation commits. t2: the doorbell rings, before the reader is
    // back.
    store = (8, "v8");
    assert!(
        !cache.invalidate(TENANT, CATALOG, ENV, "orders"),
        "there is no resident pointer to drop — that is the whole difficulty"
    );

    // t3: the reader returns, holding the version from before the flip.
    assert!(
        matches!(
            cache.insert(
                TENANT,
                CATALOG,
                ENV,
                "orders",
                read.0,
                graph_hash("orders", read.0),
                wiring(read.1),
                (),
                token,
            ),
            CacheInsert::Overtaken
        ),
        "a read from before the flip must not become the pointer after it"
    );
    assert!(
        cache.get(TENANT, CATALOG, ENV, "orders").hit().is_none(),
        "the superseded version was installed and will be served until the next flip"
    );

    // And the answer to a refusal is to resolve again, which now sees the flip.
    let retry = cache
        .get(TENANT, CATALOG, ENV, "orders")
        .miss()
        .expect("the refused install left nothing resident");
    assert!(
        matches!(
            cache.insert(
                TENANT,
                CATALOG,
                ENV,
                "orders",
                store.0,
                graph_hash("orders", store.0),
                wiring(store.1),
                (),
                retry,
            ),
            CacheInsert::Installed(_)
        ),
        "a read that started after the flip installs"
    );
    assert_eq!(
        cache
            .get(TENANT, CATALOG, ENV, "orders")
            .hit()
            .expect("the retry resolved")
            .version,
        8
    );
}

/// The same interleaving against the reconnect whole-drop. A subscriber that
/// reconnected knows of no flip in particular, so a resolution begun before it
/// re-established its `LISTEN` is as suspect as a resident pointer is.
#[test]
fn a_resolution_in_flight_across_a_reconnect_cannot_install_its_stale_pointer() {
    let cache = cache(8);

    let token = cache
        .get(TENANT, CATALOG, ENV, "orders")
        .miss()
        .expect("nothing is resident yet");

    assert_eq!(
        cache.invalidate_all(),
        0,
        "a reconnect with nothing resident still drops the gap it cannot see"
    );

    assert!(
        matches!(
            cache.insert(
                TENANT,
                CATALOG,
                ENV,
                "orders",
                7,
                graph_hash("orders", 7),
                wiring("v7"),
                (),
                token,
            ),
            CacheInsert::Overtaken
        ),
        "a read begun before the reconnect may not repopulate the cache"
    );
    assert!(cache.get(TENANT, CATALOG, ENV, "orders").hit().is_none());
}

/// Invalidation drops the POINTER, not the graphs — which is what makes a
/// rollback flip back to a resident version an in-memory hit.
#[test]
fn invalidate_keeps_the_graphs_so_a_rollback_flip_is_a_hit() {
    let cache = cache(8);
    let seven = resolve(&cache, ENV, "orders", 7, wiring("v7"));
    cache.invalidate(TENANT, CATALOG, ENV, "orders");
    assert_eq!(cache.len(), 1, "the graph stayed resident");

    let rolled_back = resolve(&cache, ENV, "orders", 7, wiring("v7-recompiled"));

    assert!(
        Arc::ptr_eq(&rolled_back, &seven),
        "a version is immutable, so the resident graph is still the right answer"
    );
    assert_eq!(cache.len(), 1, "and no second copy was installed");
}

#[test]
fn a_flip_forward_resolves_the_new_version_and_leaves_the_old_resident() {
    let cache = cache(8);
    let seven = resolve(&cache, ENV, "orders", 7, wiring("v7"));
    cache.invalidate(TENANT, CATALOG, ENV, "orders");
    resolve(&cache, ENV, "orders", 8, wiring("v8"));

    let hit = cache
        .get(TENANT, CATALOG, ENV, "orders")
        .hit()
        .expect("the new version resolves");
    assert_eq!(hit.version, 8);
    assert_eq!(hit.wiring.entry(), "v8");
    assert_eq!(cache.len(), 2, "both versions are keyed separately");
    assert!(!Arc::ptr_eq(&hit.wiring, &seven));
}

// ---- the reconnect ---------------------------------------------------------

/// The transport's only loss mode. A subscriber whose `LISTEN` connection
/// dropped missed an unknown set of flips — PostgreSQL queues nothing for an
/// absent session — so it may not resume against ANY pointer, not just the ones
/// it happens to know changed.
#[test]
fn invalidate_all_drops_every_pointer_because_a_reconnect_knows_of_no_flip() {
    let cache = cache(8);
    resolve(&cache, ENV, "orders", 7, wiring("orders-v7"));
    resolve(&cache, ENV, "refunds", 3, wiring("refunds-v3"));
    resolve(&cache, "staging", "orders", 2, wiring("staging-v2"));

    assert_eq!(
        cache.invalidate_all(),
        3,
        "every pointer the process held is suspect after a reconnect"
    );

    for (environment, wiring_id) in [(ENV, "orders"), (ENV, "refunds"), ("staging", "orders")] {
        assert!(
            cache
                .get(TENANT, CATALOG, environment, wiring_id)
                .hit()
                .is_none(),
            "{environment}/{wiring_id} resumed against a pointer no flip was seen for"
        );
    }
    assert_eq!(
        cache.invalidate_all(),
        0,
        "and there is nothing left to drop"
    );
}

/// The reconnect drop is still POINTER-only. A version is immutable, so a missed
/// flip can only have moved a pointer; every graph is still the right answer to
/// its own key, and keeping them makes the re-read a pointer write rather than a
/// recompilation of the whole environment.
#[test]
fn invalidate_all_keeps_the_graphs_so_the_re_read_recompiles_nothing() {
    let cache = cache(8);
    let seven = resolve(&cache, ENV, "orders", 7, wiring("orders-v7"));
    resolve(&cache, ENV, "refunds", 3, wiring("refunds-v3"));

    cache.invalidate_all();

    assert_eq!(cache.len(), 2, "the graphs stayed resident");
    let re_read = resolve(&cache, ENV, "orders", 7, wiring("orders-v7-recompiled"));
    assert!(
        Arc::ptr_eq(&re_read, &seven),
        "the re-read after a reconnect must land on the resident graph"
    );
    assert_eq!(cache.len(), 2, "and install no second copy");
}

// ---- bounded, deterministic eviction --------------------------------------

#[test]
fn eviction_is_bounded_and_least_recently_used() {
    let cache = cache(2);
    resolve(&cache, ENV, "a", 1, wiring("a"));
    resolve(&cache, ENV, "b", 1, wiring("b"));
    // Touch `a`, making `b` the least recently used.
    cache
        .get(TENANT, CATALOG, ENV, "a")
        .hit()
        .expect("a is resident");

    resolve(&cache, ENV, "c", 1, wiring("c"));

    assert_eq!(cache.len(), 2, "the entry bound holds");
    assert!(
        cache.get(TENANT, CATALOG, ENV, "a").hit().is_some(),
        "the touched entry survived"
    );
    assert!(cache.get(TENANT, CATALOG, ENV, "c").hit().is_some());
    assert!(
        cache.get(TENANT, CATALOG, ENV, "b").hit().is_none(),
        "the least recently used entry was the one evicted"
    );
}

/// An evicted entry must take its pointer with it: a pointer naming a graph
/// that is gone would read as a resolution and produce nothing.
#[test]
fn an_evicted_entry_leaves_behind_no_pointer() {
    let cache = cache(1);
    resolve(&cache, ENV, "a", 1, wiring("a"));
    resolve(&cache, ENV, "b", 1, wiring("b"));

    assert_eq!(cache.len(), 1);
    assert!(cache.get(TENANT, CATALOG, ENV, "a").hit().is_none());
    assert!(cache.get(TENANT, CATALOG, ENV, "b").hit().is_some());
}

// ---- the instrument --------------------------------------------------------

// wamn-hopk R5: the cache-hit series was pinned by grepping this crate's source
// for the instrument name. Deleted; a metric-name contract is a live-probe
// question, never a text search.

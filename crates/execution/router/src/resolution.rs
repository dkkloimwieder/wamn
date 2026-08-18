//! The wiring resolution cache — the reason the hot HTTP path never sees
//! Postgres.
//!
//! Resolution reads the env-hot store ONCE per `(environment, wiring)`: which
//! version is active, and that version's compiled graph. Every subsequent
//! delivery is served from memory (`docs/exe-model.md`, ingress path 1 — "no run
//! row, no queue row, no Postgres in the path").
//!
//! ## Two levels, and why
//!
//! Entries are keyed by exact identity — `(environment, wiring id, version)` —
//! because a wiring version is **immutable**: `catalog.wirings.version` is
//! monotonic and its content is pinned by `confirmed_definition_hash`, so a
//! resident entry can never become wrong. A separate pointer index records which
//! version each `(environment, wiring)` currently resolves to, mirroring
//! `catalog.wiring_activation`. That is the only mutable half, and the only half
//! [`WiringCache::invalidate`] touches.
//!
//! The split earns its keep on rollback: flipping back to a version still
//! resident is served from memory, because the superseded entry was never the
//! wrong answer to its own key — it was only the wrong answer to the pointer.
//!
//! ## Eviction and invalidation
//!
//! Bounded and deterministic: an entry-count LRU, evicting the least recently
//! used entry when full, so a tenant enumerating wirings cannot grow the process
//! without limit. Modelled on
//! `crates/platform/runtime/src/plugins/runner_plan_supply.rs`'s
//! `ResolutionPlanCache`, which never invalidates at all because a plan is keyed
//! by its own content hash. A pointer is not content, so this cache does.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;

use crate::wiring::Wiring;

/// One `(environment, wiring)` activation pointer — the key
/// `catalog.wiring_activation` is scoped by, minus the tenant and catalog a
/// serving process already fixes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Pointer {
    environment: Arc<str>,
    wiring_id: Arc<str>,
}

/// One cached graph's exact identity: a pointer plus the version it named.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EntryKey {
    pointer: Pointer,
    version: u32,
}

#[derive(Debug)]
struct CacheState {
    /// The version each pointer currently resolves to.
    active: HashMap<Pointer, u32>,
    /// Compiled graphs by exact identity.
    entries: HashMap<EntryKey, Arc<Wiring>>,
    least_to_most_recent: VecDeque<EntryKey>,
}

/// The active wiring one resolution produced.
#[derive(Debug, Clone)]
pub struct ActiveWiring {
    /// The version the pointer named — what a delivery reports as its
    /// `wiring-version` and scopes an authored dedup key by.
    pub version: u32,
    pub wiring: Arc<Wiring>,
}

/// Entry-bounded, version-keyed cache of resolved wirings.
///
/// Shared across deliveries and threads: `get` on the hot path, `insert` on a
/// miss the caller resolved from the store, [`WiringCache::invalidate`] when the
/// activation pointer flips.
#[derive(Debug)]
pub struct WiringCache {
    max_entries: usize,
    state: Mutex<CacheState>,
    lookups: Counter<u64>,
}

impl WiringCache {
    /// A cache holding at most `max_entries` compiled wirings (a chart value).
    pub fn new(max_entries: NonZeroUsize) -> WiringCache {
        WiringCache {
            max_entries: max_entries.get(),
            state: Mutex::new(CacheState {
                active: HashMap::new(),
                entries: HashMap::with_capacity(max_entries.get()),
                least_to_most_recent: VecDeque::with_capacity(max_entries.get()),
            }),
            // Beside the executor's `wamn.run.*` instruments. One counter split
            // by a two-valued attribute rather than two counters, so the hit
            // RATE — the number that says whether the hot path is actually
            // staying out of Postgres — is one query. Deliberately carries no
            // wiring or tenant attribute: those are unbounded, and this series
            // is read per replica.
            lookups: opentelemetry::global::meter("wamn-router")
                .u64_counter("wamn.run.wiring.cache.lookups")
                .with_description(
                    "wiring resolutions served from memory (hit) or sent to the \
                     env-hot store (miss)",
                )
                .build(),
        }
    }

    /// The wiring this environment's pointer currently names, if both the
    /// pointer and its graph are resident. A miss is the caller's cue to read
    /// the store and [`WiringCache::insert`] what it found.
    pub fn get(&self, environment: &str, wiring_id: &str) -> Option<ActiveWiring> {
        let pointer = Pointer {
            environment: Arc::from(environment),
            wiring_id: Arc::from(wiring_id),
        };
        let mut state = self.state.lock().expect("wiring cache lock poisoned");
        let resolved = state.active.get(&pointer).copied().and_then(|version| {
            let key = EntryKey { pointer, version };
            let wiring = state.entries.get(&key)?.clone();
            touch(&mut state.least_to_most_recent, &key);
            Some(ActiveWiring { version, wiring })
        });
        drop(state);
        self.lookups.add(
            1,
            &[KeyValue::new(
                "result",
                if resolved.is_some() { "hit" } else { "miss" },
            )],
        );
        resolved
    }

    /// Record `version` as this pointer's active wiring and cache its graph.
    ///
    /// Returns the shared graph, which is the already-resident one when this
    /// exact version is cached — two threads racing the same miss share one
    /// compilation rather than each installing its own.
    pub fn insert(
        &self,
        environment: &str,
        wiring_id: &str,
        version: u32,
        wiring: Wiring,
    ) -> Arc<Wiring> {
        let pointer = Pointer {
            environment: Arc::from(environment),
            wiring_id: Arc::from(wiring_id),
        };
        let key = EntryKey {
            pointer: pointer.clone(),
            version,
        };
        let mut state = self.state.lock().expect("wiring cache lock poisoned");
        state.active.insert(pointer, version);
        if let Some(resident) = state.entries.get(&key).cloned() {
            touch(&mut state.least_to_most_recent, &key);
            return resident;
        }
        while state.entries.len() >= self.max_entries {
            let evicted = state
                .least_to_most_recent
                .pop_front()
                .expect("a full cache has an eviction key");
            state.entries.remove(&evicted);
            // A pointer left naming an evicted entry would read as a hit and
            // find nothing; drop it so the next lookup is an honest miss.
            if state.active.get(&evicted.pointer) == Some(&evicted.version) {
                state.active.remove(&evicted.pointer);
            }
        }
        let wiring = Arc::new(wiring);
        state.entries.insert(key.clone(), wiring.clone());
        state.least_to_most_recent.push_back(key);
        wiring
    }

    /// Forget which version this pointer resolves to, sending the next delivery
    /// back to the store for it. **This is the pointer-flip entry point.**
    ///
    /// Returns whether a pointer was actually dropped.
    ///
    /// Its production caller is the doorbell subscriber of wamn-0h0g.18.2 (the
    /// activation verb that writes `catalog.wiring_activation` and notifies),
    /// which **is not wired to this method yet** — joining the two is a
    /// follow-up, and until it lands nothing in the tree calls this outside its
    /// tests. Deliberately does NOT drop the cached graphs: a version is
    /// immutable, so only the pointer can go stale, and keeping the entries is
    /// what makes a rollback flip an in-memory hit.
    pub fn invalidate(&self, environment: &str, wiring_id: &str) -> bool {
        let pointer = Pointer {
            environment: Arc::from(environment),
            wiring_id: Arc::from(wiring_id),
        };
        self.state
            .lock()
            .expect("wiring cache lock poisoned")
            .active
            .remove(&pointer)
            .is_some()
    }

    /// Compiled graphs currently resident, against the entry bound.
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .expect("wiring cache lock poisoned")
            .entries
            .len()
    }

    /// Whether no graph is resident.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Move `key` to the most-recent end of the recency list.
fn touch(order: &mut VecDeque<EntryKey>, key: &EntryKey) {
    if let Some(position) = order.iter().position(|candidate| candidate == key) {
        order.remove(position);
    }
    order.push_back(key.clone());
}

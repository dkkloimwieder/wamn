//! Entry-bounded cache of immutable wiring versions and their release facts.
//!
//! Deliveries carry an exact release and wiring version. Cache hits reuse the
//! resolved graph; misses read that immutable version from the catalog.
//! An entry-count LRU bounds memory and evicts the least recently used version.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;

use crate::wiring::Wiring;

/// The tenant, package, environment, release, and wiring that own an entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Pointer {
    tenant_id: Arc<str>,
    package_id: Arc<str>,
    environment: Arc<str>,
    effective_release_id: u32,
    wiring_id: Arc<str>,
}

/// One cached graph's exact identity, including its immutable version.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EntryKey {
    pointer: Pointer,
    version: u32,
}

#[derive(Debug)]
struct CachedWiring<T> {
    graph_hash: Arc<str>,
    wiring: Arc<Wiring>,
    facts: Arc<T>,
}

impl<T> Clone for CachedWiring<T> {
    fn clone(&self) -> Self {
        Self {
            graph_hash: Arc::clone(&self.graph_hash),
            wiring: Arc::clone(&self.wiring),
            facts: Arc::clone(&self.facts),
        }
    }
}

#[derive(Debug)]
struct CacheState<T> {
    /// Compiled graphs by exact identity.
    entries: HashMap<EntryKey, CachedWiring<T>>,
    least_to_most_recent: VecDeque<EntryKey>,
}

/// The immutable wiring one resolution produced.
#[derive(Debug, Clone)]
pub struct ActiveWiring<T = ()> {
    /// The exact version a delivery reports as its
    /// `wiring-version` and scopes an authored dedup key by.
    pub version: u32,
    /// RFC 8785 digest of the immutable document this entry contains.
    pub graph_hash: Arc<str>,
    pub wiring: Arc<Wiring>,
    /// Host-owned immutable facts resolved in the same store snapshot as the
    /// graph. The router never inspects them; carrying them here prevents a
    /// second cache or a database read on a graph hit.
    pub facts: Arc<T>,
}

/// Process-local cache lifecycle totals, used by bounded operational probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WiringCacheSnapshot {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

/// Result of caching an exact immutable wiring version.
#[derive(Debug, Clone)]
pub enum CacheInsert<T = ()> {
    Installed(ActiveWiring<T>),
    /// The same immutable version is already resident under a different graph
    /// hash. The existing entry is unchanged.
    HashMismatch,
}

/// Entry-bounded, version-keyed cache of resolved wirings.
#[derive(Debug)]
pub struct WiringCache<T = ()> {
    max_entries: usize,
    state: Mutex<CacheState<T>>,
    lookups: Counter<u64>,
    evictions: Counter<u64>,
    hit_total: AtomicU64,
    miss_total: AtomicU64,
    eviction_total: AtomicU64,
}

impl<T> WiringCache<T>
where
    T: Send + Sync + 'static,
{
    /// A cache holding at most `max_entries` compiled wirings.
    ///
    /// The serving process supplies this from its shared wiring-cache capacity
    /// setting. Zero cannot reach this boundary because the type excludes it.
    pub fn new(max_entries: NonZeroUsize) -> WiringCache<T> {
        let meter = opentelemetry::global::meter("wamn-router");
        WiringCache {
            max_entries: max_entries.get(),
            state: Mutex::new(CacheState {
                entries: HashMap::with_capacity(max_entries.get()),
                least_to_most_recent: VecDeque::with_capacity(max_entries.get()),
            }),
            // Beside the executor's `wamn.run.*` instruments. One counter split
            // by a two-valued attribute rather than two counters, so the hit
            // RATE — the number that says whether the hot path is actually
            // staying out of Postgres — is one query. Deliberately carries no
            // wiring or tenant attribute: those are unbounded, and this series
            // is read per replica.
            lookups: meter
                .u64_counter("wamn.run.wiring.cache.lookups")
                .with_description(
                    "wiring resolutions served from memory (hit) or sent to the \
                     env-hot store (miss)",
                )
                .build(),
            evictions: meter
                .u64_counter("wamn.run.wiring.cache.evictions")
                .with_description("compiled wiring entries evicted from the bounded LRU")
                .build(),
            hit_total: AtomicU64::new(0),
            miss_total: AtomicU64::new(0),
            eviction_total: AtomicU64::new(0),
        }
    }

    /// Read one immutable wiring version by its exact release identity.
    pub fn get_version(
        &self,
        tenant_id: &str,
        package_id: &str,
        environment: &str,
        effective_release_id: u32,
        wiring_id: &str,
        version: u32,
    ) -> Option<ActiveWiring<T>> {
        let key = EntryKey {
            pointer: Pointer {
                tenant_id: Arc::from(tenant_id),
                package_id: Arc::from(package_id),
                environment: Arc::from(environment),
                effective_release_id,
                wiring_id: Arc::from(wiring_id),
            },
            version,
        };
        let mut state = self.state.lock().expect("wiring cache lock poisoned");
        let resident = state.entries.get(&key).cloned();
        if resident.is_some() {
            touch(&mut state.least_to_most_recent, &key);
        }
        drop(state);
        self.record_lookup(resident.is_some());
        resident.map(|resident| ActiveWiring {
            version,
            graph_hash: resident.graph_hash,
            wiring: resident.wiring,
            facts: resident.facts,
        })
    }

    /// Cache one exact immutable version, sharing an existing graph when its
    /// identity and hash match. Different bytes under that identity are refused.
    pub fn insert_version(
        &self,
        tenant_id: &str,
        package_id: &str,
        environment: &str,
        effective_release_id: u32,
        wiring_id: &str,
        version: u32,
        graph_hash: impl Into<Arc<str>>,
        wiring: Wiring,
        facts: T,
    ) -> CacheInsert<T> {
        let key = EntryKey {
            pointer: Pointer {
                tenant_id: Arc::from(tenant_id),
                package_id: Arc::from(package_id),
                environment: Arc::from(environment),
                effective_release_id,
                wiring_id: Arc::from(wiring_id),
            },
            version,
        };
        let graph_hash = graph_hash.into();
        let mut state = self.state.lock().expect("wiring cache lock poisoned");
        if let Some(resident) = state.entries.get(&key).cloned() {
            if resident.graph_hash != graph_hash {
                return CacheInsert::HashMismatch;
            }
            touch(&mut state.least_to_most_recent, &key);
            return CacheInsert::Installed(ActiveWiring {
                version,
                graph_hash: resident.graph_hash,
                wiring: resident.wiring,
                facts: resident.facts,
            });
        }
        while state.entries.len() >= self.max_entries {
            self.evict_one(&mut state);
        }
        let wiring = Arc::new(wiring);
        let facts = Arc::new(facts);
        state.entries.insert(
            key.clone(),
            CachedWiring {
                graph_hash: graph_hash.clone(),
                wiring: Arc::clone(&wiring),
                facts: Arc::clone(&facts),
            },
        );
        state.least_to_most_recent.push_back(key);
        CacheInsert::Installed(ActiveWiring {
            version,
            graph_hash,
            wiring,
            facts,
        })
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

    /// Monotonic lifecycle totals for this cache instance.
    pub fn snapshot(&self) -> WiringCacheSnapshot {
        WiringCacheSnapshot {
            hits: self.hit_total.load(Ordering::Relaxed),
            misses: self.miss_total.load(Ordering::Relaxed),
            evictions: self.eviction_total.load(Ordering::Relaxed),
        }
    }

    fn record_lookup(&self, hit: bool) {
        self.lookups.add(
            1,
            &[KeyValue::new("result", if hit { "hit" } else { "miss" })],
        );
        if hit {
            self.hit_total.fetch_add(1, Ordering::Relaxed);
        } else {
            self.miss_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn evict_one(&self, state: &mut CacheState<T>) {
        let evicted = state
            .least_to_most_recent
            .pop_front()
            .expect("a full cache has an eviction key");
        state.entries.remove(&evicted);
        self.evictions.add(1, &[]);
        self.eviction_total.fetch_add(1, Ordering::Relaxed);
    }
}

/// Move `key` to the most-recent end of the recency list.
fn touch(order: &mut VecDeque<EntryKey>, key: &EntryKey) {
    if let Some(position) = order.iter().position(|candidate| candidate == key) {
        order.remove(position);
    }
    order.push_back(key.clone());
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use serde_json::Value;

    use super::{CacheInsert, WiringCache};
    use crate::wiring::{Wiring, WiringNode};

    fn wiring(entry: &str) -> Wiring {
        Wiring::compile(
            entry,
            vec![WiringNode {
                id: entry.to_owned(),
                component: "echo".to_owned(),
                operation: "echo".to_owned(),
                config: Value::Null,
                connection: None,
                terminal: None,
            }],
            Vec::new(),
        )
        .expect("fixture wiring compiles")
    }

    #[test]
    fn effective_releases_do_not_share_resolved_facts() {
        const TENANT: &str = "tenant-a";
        const PACKAGE: &str = "orders";
        const ENVIRONMENT: &str = "prod";
        const WIRING: &str = "create-order";
        let cache = WiringCache::new(NonZeroUsize::new(4).expect("non-zero cache bound"));

        for (release_id, facts) in [(7, "release-7"), (8, "release-8")] {
            assert!(
                cache
                    .get_version(TENANT, PACKAGE, ENVIRONMENT, release_id, WIRING, 1)
                    .is_none(),
                "another release cannot satisfy this lookup"
            );
            let inserted = cache.insert_version(
                TENANT,
                PACKAGE,
                ENVIRONMENT,
                release_id,
                WIRING,
                1,
                "sha256:graph",
                wiring(facts),
                facts,
            );
            assert!(matches!(inserted, CacheInsert::Installed(_)));
        }

        let release_7 = cache
            .get_version(TENANT, PACKAGE, ENVIRONMENT, 7, WIRING, 1)
            .expect("release 7 remains resident");
        let release_8 = cache
            .get_version(TENANT, PACKAGE, ENVIRONMENT, 8, WIRING, 1)
            .expect("release 8 remains resident");
        assert_eq!(*release_7.facts, "release-7");
        assert_eq!(*release_8.facts, "release-8");
    }
}

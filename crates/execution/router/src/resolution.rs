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
//!
//! ## Resolutions in flight
//!
//! A miss reads the store OUTSIDE this lock, so a pointer flip can commit while
//! that read is in the air: the reader saw the old version, the doorbell
//! invalidated, and the reader is then holding a pointer write that is already
//! superseded. Installing it would undo the invalidation and serve the old
//! version until the next flip — indefinitely.
//!
//! So a miss is a two-part transaction. [`WiringCache::get`] hands back a
//! [`ResolutionToken`] stamped with the generation it observed, every
//! invalidation bumps that generation, and [`WiringCache::insert`] refuses any
//! token an invalidation has overtaken. The generation is ONE counter for the
//! whole cache rather than one per pointer, because a reconnect
//! ([`WiringCache::invalidate_all`]) has to overtake every outstanding token
//! anyway; the cost is that one wiring's flip sends other wirings' in-flight
//! reads around again, which is a re-read, not a wrong answer.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;

use crate::wiring::Wiring;

/// One `(environment, wiring)` activation pointer — the key
/// `catalog.wiring_activation` is scoped by, minus the tenant and catalog.
///
/// **HAZARD: this key is NOT tenant-scoped.** It carried a claim that a serving
/// process already fixes the tenant and catalog. That claim is false: the
/// sibling plugin in the SAME process
/// (`crates/platform/runtime/src/plugins/wamn_flow_invocation.rs`) takes
/// tenant/catalog/environment PER WORKLOAD BIND, so one process serves many
/// triples. Two tenants sharing an environment name and a wiring id would
/// collide here, and the second would be served the first's graph. Keying
/// decision owed — see `wamn-0h0g.12.138`. Do not wire a production driver
/// against this path until it is ruled.
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
    /// Bumped by every invalidation, and by nothing else. A pointer write whose
    /// token carries an older generation is a read from before that
    /// invalidation, and is refused.
    generation: u64,
}

/// The active wiring one resolution produced.
#[derive(Debug, Clone)]
pub struct ActiveWiring {
    /// The version the pointer named — what a delivery reports as its
    /// `wiring-version` and scopes an authored dedup key by.
    pub version: u32,
    pub wiring: Arc<Wiring>,
}

/// A miss's receipt: the cache generation the caller began resolving in.
///
/// Handed out by [`WiringCache::get`] on a miss and handed back to
/// [`WiringCache::insert`] with what the store read found. Its whole job is to
/// let `insert` tell a read that started BEFORE an invalidation from one that
/// started after it, and refuse the first. Take it before reading the store, not
/// after: a token stamped after the read proves nothing about the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionToken {
    generation: u64,
}

/// What one lookup produced: the resident wiring, or the receipt to resolve
/// under.
#[derive(Debug, Clone)]
pub enum Lookup {
    /// Both the pointer and the graph it named were resident.
    Hit(ActiveWiring),
    /// Nothing to serve. Read the store, then hand this token back to
    /// [`WiringCache::insert`] with what it said.
    Miss(ResolutionToken),
}

impl Lookup {
    /// The resident wiring, if this lookup was a hit.
    pub fn hit(self) -> Option<ActiveWiring> {
        match self {
            Lookup::Hit(active) => Some(active),
            Lookup::Miss(_) => None,
        }
    }

    /// The token to resolve under, if this lookup was a miss.
    pub fn miss(self) -> Option<ResolutionToken> {
        match self {
            Lookup::Hit(_) => None,
            Lookup::Miss(token) => Some(token),
        }
    }
}

/// Entry-bounded, version-keyed cache of resolved wirings.
///
/// Shared across deliveries and threads: `get` on the hot path, `insert` under
/// the token that miss handed out, [`WiringCache::invalidate`] when the
/// activation pointer flips.
///
/// **HAZARD: not tenant-scoped.** No entry point here takes a tenant — see
/// [`Pointer`]. One shared instance in a multi-tenant process would cross-serve
/// wirings. Keying decision owed — see `wamn-0h0g.12.138`. **No production
/// process may construct one until that is ruled**; today none does, which is
/// the only reason this is latent.
#[derive(Debug)]
pub struct WiringCache {
    max_entries: usize,
    state: Mutex<CacheState>,
    lookups: Counter<u64>,
}

impl WiringCache {
    /// A cache holding at most `max_entries` compiled wirings.
    ///
    /// The bound is a constructor argument and NOTHING SETS IT FROM A CHART.
    /// This said "(a chart value)"; no such knob exists, and every caller today
    /// is a test. Stated plainly rather than aspirationally — `docs/PLAN/PLAN.md`
    /// records the identical doc-comment-as-mechanism defect for pooling.
    pub fn new(max_entries: NonZeroUsize) -> WiringCache {
        WiringCache {
            max_entries: max_entries.get(),
            state: Mutex::new(CacheState {
                active: HashMap::new(),
                entries: HashMap::with_capacity(max_entries.get()),
                least_to_most_recent: VecDeque::with_capacity(max_entries.get()),
                generation: 0,
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
    /// pointer and its graph are resident. A miss carries the
    /// [`ResolutionToken`] to read the store under and hand back to
    /// [`WiringCache::insert`].
    pub fn get(&self, environment: &str, wiring_id: &str) -> Lookup {
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
        // Stamped under the SAME lock hold that found the miss, so no
        // invalidation can slip between deciding to read the store and the
        // generation the read is attributed to.
        let generation = state.generation;
        drop(state);
        self.lookups.add(
            1,
            &[KeyValue::new(
                "result",
                if resolved.is_some() { "hit" } else { "miss" },
            )],
        );
        match resolved {
            Some(active) => Lookup::Hit(active),
            None => Lookup::Miss(ResolutionToken { generation }),
        }
    }

    /// Record `version` as this pointer's active wiring and cache its graph,
    /// on the strength of the `token` the miss handed out.
    ///
    /// Returns the shared graph, which is the already-resident one when this
    /// exact version is cached — two threads racing the same miss share one
    /// compilation rather than each installing its own.
    ///
    /// Returns `None` when an invalidation has overtaken `token`: the store read
    /// behind it began before a pointer flip this cache has already been told
    /// about, so what it found may be the superseded version and NOTHING is
    /// installed — not the pointer, not the graph. The caller's answer is to
    /// resolve again under a fresh token; serving the read it already holds
    /// would be serving a version the cache knows has moved.
    pub fn insert(
        &self,
        environment: &str,
        wiring_id: &str,
        version: u32,
        wiring: Wiring,
        token: ResolutionToken,
    ) -> Option<Arc<Wiring>> {
        let pointer = Pointer {
            environment: Arc::from(environment),
            wiring_id: Arc::from(wiring_id),
        };
        let key = EntryKey {
            pointer: pointer.clone(),
            version,
        };
        let mut state = self.state.lock().expect("wiring cache lock poisoned");
        if token.generation < state.generation {
            return None;
        }
        state.active.insert(pointer, version);
        if let Some(resident) = state.entries.get(&key).cloned() {
            touch(&mut state.least_to_most_recent, &key);
            return Some(resident);
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
        Some(wiring)
    }

    /// Forget which version this pointer resolves to, sending the next delivery
    /// back to the store for it. **This is the pointer-flip entry point.**
    ///
    /// Returns whether a pointer was actually dropped — which is NOT the same
    /// question as whether the flip took effect. It also overtakes every
    /// outstanding [`ResolutionToken`], so a resolution already reading the
    /// store cannot install what it read; that happens even when no pointer was
    /// resident, because the in-flight read is exactly the case where there is
    /// nothing yet to drop.
    ///
    /// Its production caller is the doorbell subscriber of wamn-0h0g.18.2 (the
    /// activation verb that writes `catalog.wiring_activation` and notifies),
    /// joined to this method by wamn-0h0g.16.15:
    /// `crates/platform/runtime/src/wiring_doorbell.rs`. Deliberately does NOT
    /// drop the cached graphs: a version is immutable, so only the pointer can
    /// go stale, and keeping the entries is what makes a rollback flip an
    /// in-memory hit. A subscriber that RECONNECTED has missed an unknown set of
    /// flips and must call [`WiringCache::invalidate_all`] instead.
    pub fn invalidate(&self, environment: &str, wiring_id: &str) -> bool {
        let pointer = Pointer {
            environment: Arc::from(environment),
            wiring_id: Arc::from(wiring_id),
        };
        let mut state = self.state.lock().expect("wiring cache lock poisoned");
        state.generation += 1;
        state.active.remove(&pointer).is_some()
    }

    /// Forget EVERY pointer, sending the next delivery of every wiring back to
    /// the store. **This is the reconnect entry point.**
    ///
    /// Returns how many pointers were dropped, and overtakes every outstanding
    /// [`ResolutionToken`] whether or not that count is zero: a resolution
    /// already reading the store is as much a way to resume against a missed
    /// flip as a resident pointer is.
    ///
    /// PostgreSQL delivers a `NOTIFY` only to sessions holding a `LISTEN` at
    /// commit time and queues nothing for an absent one, so a subscriber whose
    /// connection dropped has missed an unknown set of flips
    /// (`deploy/sql/catalog-schema.sql`, the `wiring_activation_doorbell`
    /// comment). Resuming would serve a stale active version indefinitely with
    /// nothing to signal it, so a reconnect drops the lot instead of trusting
    /// the gap.
    ///
    /// Like [`WiringCache::invalidate`] it keeps the cached graphs: a version is
    /// immutable, so a missed flip can only have moved a POINTER. Every entry is
    /// still the right answer to its own `(environment, wiring, version)` key,
    /// and keeping them is what makes the re-read after a reconnect a pointer
    /// write rather than a recompilation.
    pub fn invalidate_all(&self) -> usize {
        let mut state = self.state.lock().expect("wiring cache lock poisoned");
        state.generation += 1;
        let dropped = state.active.len();
        state.active.clear();
        dropped
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

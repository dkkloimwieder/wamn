//! The pointer-flip doorbell subscriber (wamn-0h0g.16.15) — the production
//! caller of [`WiringCache::invalidate`].
//!
//! `catalog.wiring_activation`'s `wiring_activation_doorbell` trigger fires
//! `pg_notify` on `wamn_wiring_activation` from inside the flip's transaction
//! (`deploy/sql/catalog-schema.sql`), so a serving process learns of an
//! activation or a rollback without a restart, a poll or a TTL. This module is
//! the other end of that wire: one `LISTEN` connection, and every payload it
//! delivers turned into a cache invalidation.
//!
//! # Why it is here and not in the router
//!
//! `crates/execution/router`'s manifest declares no pooling, no runtime, no
//! clock and no DB — "siblings own those" — and a `LISTEN` connection is all
//! four. The cache lives there because the compiled graph is that crate's type;
//! the connection that rings it lives in the process that owns the pipeline,
//! beside the runtime's other database-backed host capabilities.
//!
//! # The reconnect obligation
//!
//! PostgreSQL delivers a notification ONLY to sessions holding a `LISTEN` at
//! commit time and queues NOTHING for an absent one. A subscriber whose
//! connection dropped has therefore missed an unknown set of flips, and
//! resuming would serve a stale active version indefinitely with nothing to
//! signal it. So every established `LISTEN` — the first one included — drops
//! EVERY pointer through [`WiringCache::invalidate_all`] before it serves
//! anything. The order is load-bearing: the `LISTEN` is issued first, so no flip
//! can commit into the gap between dropping the pointers and being able to hear
//! about the next one.
//!
//! # Why it applies inline instead of broadcasting
//!
//! The retired invocation provider fanned run outcomes over a bounded
//! `broadcast` channel, where a slow reader was reported as
//! `RecvError::Lagged` and the hint is merely late — outcomes are re-polled
//! anyway. A dropped doorbell is not late, it is a stale wiring served forever,
//! so there is no queue here: the connection's own driver task applies each
//! payload to the cache directly, which is a `HashMap` remove under a
//! `std::sync::Mutex`. That is also what keeps the shared cluster-wide async
//! notify queue drained, which a listener that stalls can otherwise back up
//! against committing writers.

use std::sync::Arc;
use std::time::Duration;

use wamn_catalog::{WIRING_ACTIVATION_CHANNEL, WiringActivationNotice};
use wamn_router::WiringCache;

use crate::plugins::wamn_postgres::{DEFAULT_PROJECT, PlatformAsyncMessage, WamnPostgres};

/// How long a failed doorbell connection waits before reconnecting. Matches the
/// outcome listener's delay: the same transport with the same failure mode.
const DOORBELL_RECONNECT_DELAY: Duration = Duration::from_millis(250);

/// What one doorbell payload did to the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorbellEffect {
    /// The pointer the notice named was dropped; the next delivery re-reads it.
    Invalidated,
    /// The notice named a pointer this process was not holding — a wiring it has
    /// never served, or one already dropped.
    NotResident,
    /// The payload is not the shape `catalog.notify_wiring_activation()` builds,
    /// so which pointer moved is unknowable. Treated as a missed flip: the whole
    /// cache goes, because guessing "none" would serve a stale version silently.
    Unreadable,
}

/// The cache-side half of the subscriber: everything one connection does to the
/// cache, with no transport in it, so it is provable without a database.
#[derive(Debug)]
pub struct WiringDoorbell<T = ()> {
    cache: Arc<WiringCache<T>>,
}

impl<T> WiringDoorbell<T>
where
    T: Send + Sync + 'static,
{
    pub fn new(cache: Arc<WiringCache<T>>) -> WiringDoorbell<T> {
        WiringDoorbell { cache }
    }

    /// A `LISTEN` has just been established. Drops every pointer and reports how
    /// many went, because an absent session is queued nothing and this process
    /// cannot know which flips it slept through. See the module docs.
    pub fn listening(&self) -> usize {
        let dropped = self.cache.invalidate_all();
        tracing::info!(
            channel = WIRING_ACTIVATION_CHANNEL,
            dropped,
            "wiring activation doorbell listening; dropped every cached pointer"
        );
        dropped
    }

    /// One doorbell payload arrived.
    pub fn rang(&self, payload: &str) -> DoorbellEffect {
        let notice = match serde_json::from_str::<WiringActivationNotice>(payload) {
            Ok(notice) => notice,
            Err(error) => {
                // The payload is a frozen contract pinned against the DDL by
                // `the_notice_shape_is_exactly_the_payload_the_ddl_builds`, so
                // this is deployment skew, not a routine case. The payload
                // itself is not logged: it names another tenant's wirings.
                tracing::warn!(
                    error = %error,
                    "unreadable wiring activation doorbell payload; dropping every cached pointer"
                );
                self.cache.invalidate_all();
                return DoorbellEffect::Unreadable;
            }
        };
        // `enabled` is deliberately not read: taking a wiring dark moves what the
        // read path serves exactly as an activation does, so it is as much an
        // invalidation. The hash is carried for the log — the pointer holds only
        // the hash, so `(wiring, definition-hash)` is the identity the re-read
        // is scoped by, and the version comes back with it.
        let dropped = self.cache.invalidate(
            &notice.tenant_id,
            &notice.package_id,
            &notice.environment,
            &notice.wiring_id,
        );
        tracing::info!(
            tenant_id = %notice.tenant_id,
            package_id = %notice.package_id,
            environment = %notice.environment,
            wiring_id = %notice.wiring_id,
            enabled = notice.enabled,
            confirmed_definition_hash = %notice.confirmed_definition_hash,
            dropped,
            "wiring activation pointer flipped"
        );
        if dropped {
            DoorbellEffect::Invalidated
        } else {
            DoorbellEffect::NotResident
        }
    }
}

/// A running doorbell subscription. Reconnects on failure, and stops when
/// dropped.
pub struct WiringDoorbellListener {
    shutdown: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for WiringDoorbellListener {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        // Do not abort: the task owns a checked-out platform-pool object and
        // must UNLISTEN before returning it. Dropping the JoinHandle detaches
        // that short cleanup rather than cancelling it.
        let _ = self.task.is_finished();
    }
}

impl WiringDoorbellListener {
    /// Subscribe `cache` through the existing platform pool. The listener owns
    /// one checkout for its full LISTEN lifetime; construction refuses if this
    /// `WamnPostgres` already supplied its one async-message receiver.
    pub fn postgres<T>(
        postgres: Arc<WamnPostgres>,
        project: Option<String>,
        cache: Arc<WiringCache<T>>,
    ) -> anyhow::Result<WiringDoorbellListener>
    where
        T: Send + Sync + 'static,
    {
        let messages = postgres.take_platform_messages()?;
        let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(run_doorbell_listener(
            postgres,
            project.unwrap_or_else(|| DEFAULT_PROJECT.to_owned()),
            Arc::new(WiringDoorbell::new(cache)),
            messages,
            shutdown_receiver,
        ));
        Ok(WiringDoorbellListener { shutdown, task })
    }
}

async fn run_doorbell_listener<T>(
    postgres: Arc<WamnPostgres>,
    project: String,
    doorbell: Arc<WiringDoorbell<T>>,
    mut messages: tokio::sync::mpsc::UnboundedReceiver<PlatformAsyncMessage>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) where
    T: Send + Sync + 'static,
{
    loop {
        if *shutdown.borrow() {
            return;
        }
        let (connection, backend_pid) = match postgres.checkout_wiring_listener(&project).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "wiring activation platform-pool LISTEN failed; reconnecting"
                );
                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            return;
                        }
                    }
                    () = tokio::time::sleep(DOORBELL_RECONNECT_DELAY) => {}
                }
                continue;
            }
        };

        // ORDER IS LOAD-BEARING. checkout_wiring_listener issued LISTEN before
        // this whole-pointer drop, so no flip can commit into a gap where it is
        // neither delivered nor covered by the drop.
        doorbell.listening();
        let disconnected = loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        if let Err(error) = connection.batch_execute("UNLISTEN *").await {
                            tracing::warn!(error = %error, "wiring doorbell UNLISTEN failed");
                        }
                        return;
                    }
                }
                message = messages.recv() => match message {
                    Some(PlatformAsyncMessage::Notification {
                        backend_pid: message_pid,
                        channel,
                        payload,
                    }) if message_pid == backend_pid && channel == WIRING_ACTIVATION_CHANNEL => {
                        doorbell.rang(&payload);
                    }
                    Some(PlatformAsyncMessage::Disconnected { backend_pid: message_pid })
                        if message_pid == backend_pid => break true,
                    Some(_) => {}
                    None => break true,
                }
            }
        };
        drop(connection);
        if disconnected {
            tracing::warn!(
                backend_pid,
                "wiring activation platform-pool connection closed; reconnecting"
            );
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
                () = tokio::time::sleep(DOORBELL_RECONNECT_DELAY) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use serde_json::{Value, json};
    use wamn_router::{CacheInsert, Wiring, WiringNode};

    use super::*;

    const TENANT: &str = "t1";
    const PACKAGE: &str = "shop";
    const ENV: &str = "prod";
    const RELEASE: u32 = 7;

    fn cache() -> Arc<WiringCache> {
        Arc::new(WiringCache::new(
            NonZeroUsize::new(8).expect("fixture bound is non-zero"),
        ))
    }

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

    /// A payload with the DDL's exact keys, built from the shared notice type so
    /// a renamed field cannot pass here and fail in production.
    fn payload(tenant: &str, package: &str, environment: &str, wiring_id: &str) -> String {
        serde_json::to_string(&WiringActivationNotice {
            tenant_id: tenant.to_string(),
            package_id: package.to_string(),
            environment: environment.to_string(),
            wiring_id: wiring_id.to_string(),
            enabled: true,
            confirmed_definition_hash: format!("sha256:{}", "b".repeat(64)),
        })
        .expect("the notice serializes")
    }

    fn doorbell(cache: &Arc<WiringCache>) -> WiringDoorbell {
        WiringDoorbell::new(Arc::clone(cache))
    }

    /// The graph hash a fixture resolves `(wiring_id, version)` under. A version
    /// is immutable, so its hash is a function of its identity.
    fn graph_hash(wiring_id: &str, version: u32) -> String {
        format!("sha256:{wiring_id}-v{version}")
    }

    /// One resolution as the serving path performs it: miss, read the store,
    /// install what the read found under the token the miss handed out.
    fn resolve(
        cache: &WiringCache,
        environment: &str,
        wiring_id: &str,
        version: u32,
        graph: Wiring,
    ) -> Arc<Wiring> {
        resolve_release(cache, environment, RELEASE, wiring_id, version, graph)
    }

    fn resolve_release(
        cache: &WiringCache,
        environment: &str,
        effective_release_id: u32,
        wiring_id: &str,
        version: u32,
        graph: Wiring,
    ) -> Arc<Wiring> {
        let token = cache
            .get(
                TENANT,
                PACKAGE,
                environment,
                effective_release_id,
                wiring_id,
            )
            .miss()
            .expect("a fixture resolves only what it has just found missing");
        match cache.insert(
            TENANT,
            PACKAGE,
            environment,
            effective_release_id,
            wiring_id,
            version,
            graph_hash(wiring_id, version),
            graph,
            (),
            token,
        ) {
            CacheInsert::Installed(active) => active.wiring,
            other => {
                panic!("no doorbell rang between this fixture's miss and its install: {other:?}")
            }
        }
    }

    #[test]
    fn a_flip_invalidates_exactly_the_pointer_it_names() {
        let cache = cache();
        resolve(&cache, ENV, "orders", 1, wiring("v1"));
        resolve(&cache, ENV, "refunds", 4, wiring("refunds-v4"));
        let doorbell = doorbell(&cache);

        assert_eq!(
            doorbell.rang(&payload(TENANT, PACKAGE, ENV, "orders")),
            DoorbellEffect::Invalidated
        );

        assert!(
            cache
                .get(TENANT, PACKAGE, ENV, RELEASE, "orders")
                .hit()
                .is_none(),
            "the flipped pointer must send the next delivery back to the store"
        );
        assert_eq!(
            cache
                .get(TENANT, PACKAGE, ENV, RELEASE, "refunds")
                .hit()
                .expect("an unrelated wiring keeps serving")
                .version,
            4
        );
    }

    #[test]
    fn a_flip_invalidates_the_coordinate_across_resident_release_scopes() {
        let cache = cache();
        resolve_release(&cache, ENV, 7, "orders", 1, wiring("release-7"));
        resolve_release(&cache, ENV, 8, "orders", 1, wiring("release-8"));

        assert_eq!(
            doorbell(&cache).rang(&payload(TENANT, PACKAGE, ENV, "orders")),
            DoorbellEffect::Invalidated
        );
        for release_id in [7, 8] {
            assert!(
                cache
                    .get(TENANT, PACKAGE, ENV, release_id, "orders")
                    .hit()
                    .is_none(),
                "the release {release_id} pointer survived one coordinate notice"
            );
        }
        assert_eq!(cache.len(), 2, "immutable release entries remain resident");
    }

    /// The whole point of the seam: after the flip the next resolution installs
    /// the new version and the hot path serves it — a rollback to a version
    /// still resident does it without recompiling.
    #[test]
    fn the_next_delivery_serves_the_new_version_and_a_rollback_is_a_hit() {
        let cache = cache();
        let one = resolve(&cache, ENV, "orders", 1, wiring("v1"));
        let doorbell = doorbell(&cache);

        doorbell.rang(&payload(TENANT, PACKAGE, ENV, "orders"));
        resolve(&cache, ENV, "orders", 2, wiring("v2"));
        let served = cache
            .get(TENANT, PACKAGE, ENV, RELEASE, "orders")
            .hit()
            .expect("the re-read resolved");
        assert_eq!(served.version, 2);
        assert_eq!(served.wiring.entry(), "v2");

        doorbell.rang(&payload(TENANT, PACKAGE, ENV, "orders"));
        let rolled_back = resolve(&cache, ENV, "orders", 1, wiring("v1-recompiled"));
        assert!(
            Arc::ptr_eq(&rolled_back, &one),
            "the rollback flip must land on the resident graph, not a rebuilt one"
        );
    }

    /// wamn-0h0g.16.16 — THE INTERLEAVING, with the real doorbell in it. A
    /// delivery misses and goes to the store; the activation commits; `rang`
    /// applies the flip; only THEN does the store read come back, holding the
    /// version from before it. Installing that would undo the invalidation and
    /// serve the superseded version until the next flip.
    ///
    /// The ring reports `NotResident` here, and that is the point: the pointer
    /// is not resident precisely BECAUSE the resolution that would install it is
    /// still in flight, so dropping resident pointers cannot be the whole of
    /// what a flip has to do.
    #[test]
    fn a_resolution_in_flight_across_a_doorbell_ring_cannot_reinstall_the_stale_pointer() {
        let cache = cache();
        let doorbell = doorbell(&cache);

        // t0: the miss, and the store read it starts. The store says v1.
        let token = cache
            .get(TENANT, PACKAGE, ENV, RELEASE, "orders")
            .miss()
            .expect("nothing is resident yet");
        let read = (1, "v1");

        // t1: the activation commits. t2: the doorbell rings, still before the
        // read has come back.
        assert_eq!(
            doorbell.rang(&payload(TENANT, PACKAGE, ENV, "orders")),
            DoorbellEffect::NotResident,
            "there is nothing to drop yet — the resolution is still in flight"
        );

        // t3: the read returns, holding the version from before the flip.
        assert!(
            matches!(
                cache.insert(
                    TENANT,
                    PACKAGE,
                    ENV,
                    RELEASE,
                    "orders",
                    read.0,
                    graph_hash("orders", read.0),
                    wiring(read.1),
                    (),
                    token,
                ),
                CacheInsert::Overtaken
            ),
            "the flip would be undone by the resolution it interrupted"
        );
        assert!(
            cache
                .get(TENANT, PACKAGE, ENV, RELEASE, "orders")
                .hit()
                .is_none(),
            "the hot path would serve the superseded version until the next flip"
        );

        // The retry starts after the ring, and installs what the flip made
        // active.
        let served = resolve(&cache, ENV, "orders", 2, wiring("v2"));
        assert_eq!(served.entry(), "v2");
        assert_eq!(
            cache
                .get(TENANT, PACKAGE, ENV, RELEASE, "orders")
                .hit()
                .expect("the retry resolved")
                .version,
            2
        );
    }

    /// `pg_notify` is per-database and tenants share a database, so the payload
    /// is the only thing separating one tenant's flip from another's.
    #[test]
    fn another_tenants_or_packages_flip_leaves_this_process_alone() {
        let cache = cache();
        resolve(&cache, ENV, "orders", 1, wiring("v1"));
        let doorbell = doorbell(&cache);

        for (tenant, package) in [("t2", PACKAGE), (TENANT, "warehouse")] {
            assert_eq!(
                doorbell.rang(&payload(tenant, package, ENV, "orders")),
                DoorbellEffect::NotResident,
                "{tenant}/{package} names a pointer this process does not hold"
            );
        }
        assert_eq!(
            cache
                .get(TENANT, PACKAGE, ENV, RELEASE, "orders")
                .hit()
                .expect("this tenant's pointer is untouched")
                .version,
            1
        );
    }

    /// The pointer is environment-scoped, so a staging flip may not take
    /// production's pointer with it.
    #[test]
    fn a_flip_in_another_environment_does_not_touch_this_one() {
        let cache = cache();
        resolve(&cache, ENV, "orders", 1, wiring("prod-v1"));
        let doorbell = doorbell(&cache);

        assert_eq!(
            doorbell.rang(&payload(TENANT, PACKAGE, "staging", "orders")),
            DoorbellEffect::NotResident
        );
        assert_eq!(
            cache
                .get(TENANT, PACKAGE, ENV, RELEASE, "orders")
                .hit()
                .expect("prod still serves")
                .version,
            1
        );
    }

    /// Taking a wiring dark moves what the read path serves exactly as an
    /// activation does, so `enabled = false` is as much an invalidation.
    #[test]
    fn taking_a_wiring_dark_invalidates_like_any_other_flip() {
        let cache = cache();
        resolve(&cache, ENV, "orders", 1, wiring("v1"));
        let doorbell = doorbell(&cache);

        let dark = serde_json::to_string(&WiringActivationNotice {
            tenant_id: TENANT.to_string(),
            package_id: PACKAGE.to_string(),
            environment: ENV.to_string(),
            wiring_id: "orders".to_string(),
            enabled: false,
            confirmed_definition_hash: format!("sha256:{}", "a".repeat(64)),
        })
        .expect("the notice serializes");

        assert_eq!(doorbell.rang(&dark), DoorbellEffect::Invalidated);
        assert!(
            cache
                .get(TENANT, PACKAGE, ENV, RELEASE, "orders")
                .hit()
                .is_none()
        );
    }

    /// A payload this process cannot read is deployment skew, and it hides WHICH
    /// pointer moved. Answering "nothing moved" would serve a stale version
    /// silently, so it is treated as a missed flip.
    #[test]
    fn an_unreadable_payload_drops_the_whole_cache_rather_than_guessing() {
        let complete = payload(TENANT, PACKAGE, ENV, "orders");
        // `deny_unknown_fields`: a DDL that grew a key this build cannot name.
        let surprising = format!(
            r#"{{{},"surprise":1}}"#,
            complete.trim_start_matches('{').trim_end_matches('}')
        );
        for unreadable in ["not json", r#"{"tenant-id":"t1"}"#, &surprising] {
            let cache = cache();
            resolve(&cache, ENV, "orders", 1, wiring("v1"));
            resolve(&cache, "staging", "refunds", 2, wiring("staging-v2"));
            let doorbell = doorbell(&cache);

            assert_eq!(
                doorbell.rang(unreadable),
                DoorbellEffect::Unreadable,
                "{unreadable} must not read as a routine notice"
            );
            assert!(
                cache
                    .get(TENANT, PACKAGE, ENV, RELEASE, "orders")
                    .hit()
                    .is_none()
            );
            assert!(
                cache
                    .get(TENANT, PACKAGE, "staging", RELEASE, "refunds")
                    .hit()
                    .is_none()
            );
            assert_eq!(cache.len(), 2, "the graphs are still immutable and correct");
        }
    }

    /// The kebab-case wire keys are the DDL's, not serde's default. A payload
    /// spelled the way the Rust fields are must NOT parse, or the two halves
    /// could drift and only production would notice.
    #[test]
    fn the_subscriber_reads_the_ddls_kebab_case_keys_and_only_those() {
        let cache = cache();
        resolve(&cache, ENV, "orders", 1, wiring("v1"));
        let doorbell = doorbell(&cache);

        let snake = json!({
            "tenant_id": TENANT,
            "package_id": PACKAGE,
            "environment": ENV,
            "wiring_id": "orders",
            "enabled": true,
            "confirmed_definition_hash": "sha256:00",
        })
        .to_string();
        assert_eq!(doorbell.rang(&snake), DoorbellEffect::Unreadable);

        let kebab = payload(TENANT, PACKAGE, ENV, "orders");
        assert!(kebab.contains(r#""wiring-id""#), "wire keys are kebab-case");
        assert!(kebab.contains(r#""confirmed-definition-hash""#));
    }

    /// THE TRANSPORT'S ONLY LOSS MODE. While the connection was down, flips
    /// committed to sessions that were listening and were queued for nobody. The
    /// reconnected process cannot know which, so it drops every pointer it holds
    /// rather than resuming against any of them. `listening()` is what the
    /// listener loop calls once its `LISTEN` is established.
    #[test]
    fn a_reconnect_drops_every_pointer_because_the_gap_is_unknowable() {
        let cache = cache();
        resolve(&cache, ENV, "orders", 1, wiring("v1"));
        resolve(&cache, ENV, "refunds", 4, wiring("refunds-v4"));
        resolve(&cache, "staging", "orders", 9, wiring("staging-v9"));

        assert_eq!(doorbell(&cache).listening(), 3);

        for (environment, wiring_id) in [(ENV, "orders"), (ENV, "refunds"), ("staging", "orders")] {
            assert!(
                cache
                    .get(TENANT, PACKAGE, environment, RELEASE, wiring_id)
                    .hit()
                    .is_none(),
                "{environment}/{wiring_id} resumed against a pointer no flip was seen for"
            );
        }
        assert_eq!(
            cache.len(),
            3,
            "the graphs survive: a version is immutable, so only pointers can go stale"
        );
    }
}

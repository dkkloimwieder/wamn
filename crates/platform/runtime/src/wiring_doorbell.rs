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
//! beside the only other `LISTEN` in the tree
//! ([`flow_invocation`](crate::flow_invocation), `wamn_run_outcome`).
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
//! [`flow_invocation`](crate::flow_invocation) fans `wamn_run_outcome` out over
//! a bounded `broadcast` channel, where a slow reader is reported as
//! `RecvError::Lagged` and the hint is merely late — outcomes are re-polled
//! anyway. A dropped doorbell is not late, it is a stale wiring served forever,
//! so there is no queue here: the connection's own driver task applies each
//! payload to the cache directly, which is a `HashMap` remove under a
//! `std::sync::Mutex`. That is also what keeps the shared cluster-wide async
//! notify queue drained, which a listener that stalls can otherwise back up
//! against committing writers.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use async_trait::async_trait;
use tokio_postgres::{AsyncMessage, NoTls};
use wamn_catalog::{WIRING_ACTIVATION_CHANNEL, WiringActivationNotice};
use wamn_router::WiringCache;

/// How long a failed doorbell connection waits before reconnecting. Matches the
/// outcome listener's delay: the same transport with the same failure mode.
const DOORBELL_RECONNECT_DELAY: Duration = Duration::from_millis(250);

/// The tenant and catalog a serving process is fixed to.
///
/// [`WiringCache`]'s pointer key is `(environment, wiring)` only, because a
/// serving process already fixes these two. `pg_notify` is per-DATABASE and
/// tenants share a database under row-level security, so without this filter one
/// tenant's flip would drop another tenant's identically-named pointer — not a
/// stale serve, but a store read nobody asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorbellScope {
    pub tenant_id: String,
    pub catalog_id: String,
}

/// What one doorbell payload did to the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorbellEffect {
    /// The pointer the notice named was dropped; the next delivery re-reads it.
    Invalidated,
    /// The notice named a pointer this process was not holding — a wiring it has
    /// never served, or one already dropped.
    NotResident,
    /// The notice named another tenant's or catalog's pointer.
    OutOfScope,
    /// The payload is not the shape `catalog.notify_wiring_activation()` builds,
    /// so which pointer moved is unknowable. Treated as a missed flip: the whole
    /// cache goes, because guessing "none" would serve a stale version silently.
    Unreadable,
}

/// The cache-side half of the subscriber: everything one connection does to the
/// cache, with no transport in it, so it is provable without a database.
#[derive(Debug)]
pub struct WiringDoorbell {
    cache: Arc<WiringCache>,
    scope: DoorbellScope,
}

impl WiringDoorbell {
    pub fn new(cache: Arc<WiringCache>, scope: DoorbellScope) -> WiringDoorbell {
        WiringDoorbell { cache, scope }
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
        if notice.tenant_id != self.scope.tenant_id || notice.catalog_id != self.scope.catalog_id {
            return DoorbellEffect::OutOfScope;
        }
        // `enabled` is deliberately not read: taking a wiring dark moves what the
        // read path serves exactly as an activation does, so it is as much an
        // invalidation. The hash is carried for the log — the pointer holds only
        // the hash, so `(wiring, definition-hash)` is the identity the re-read
        // is scoped by, and the version comes back with it.
        let dropped = self
            .cache
            .invalidate(&notice.environment, &notice.wiring_id);
        tracing::info!(
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

/// One attempt at holding a `LISTEN`, abstracted so the reconnect loop is
/// provable without a database.
#[async_trait]
trait DoorbellConnector: Send + Sync {
    /// Establish a `LISTEN`, call [`WiringDoorbell::listening`] once it is
    /// established, then apply every delivered payload until the connection
    /// fails or `shutdown` flips.
    async fn listen(
        &self,
        doorbell: Arc<WiringDoorbell>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()>;
}

struct PostgresDoorbellConnector {
    database_url: String,
}

#[async_trait]
impl DoorbellConnector for PostgresDoorbellConnector {
    async fn listen(
        &self,
        doorbell: Arc<WiringDoorbell>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let (client, mut connection) = tokio_postgres::connect(&self.database_url, NoTls)
            .await
            .context("connect wiring activation doorbell listener")?;
        let applying = Arc::clone(&doorbell);
        let mut driver = AbortOnDrop(tokio::spawn(async move {
            while let Some(message) =
                futures_util::future::poll_fn(|context| connection.poll_message(context)).await
            {
                match message {
                    // Applied on the connection's own task, with nothing between
                    // the socket and the cache: no channel to lag, and nothing
                    // that can leave the cluster's async notify queue undrained.
                    Ok(AsyncMessage::Notification(notification)) => {
                        applying.rang(notification.payload());
                    }
                    Ok(_) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(anyhow!(
                "wiring activation doorbell LISTEN connection closed"
            ))
        }));
        if let Err(error) = client.batch_execute(&listen_statement()).await {
            driver.abort();
            return Err(error).context("LISTEN wamn_wiring_activation");
        }

        // ORDER IS LOAD-BEARING. The pointers are dropped only once the LISTEN
        // is established, so no flip can commit into a gap where it is neither
        // delivered nor covered by the drop.
        doorbell.listening();

        let result = tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| anyhow!("doorbell listener shutdown owner dropped"))?;
                Ok(())
            }
            result = &mut driver.0 => {
                result.context("join wiring activation doorbell connection")?
            }
        };
        driver.abort();
        drop(client);
        result
    }
}

/// The `LISTEN` this subscriber issues, built from the channel constant the
/// emitter owns so the two can never drift apart.
fn listen_statement() -> String {
    format!("LISTEN {WIRING_ACTIVATION_CHANNEL}")
}

struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> AbortOnDrop<T> {
    fn abort(&self) {
        self.0.abort();
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
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
        self.task.abort();
    }
}

impl WiringDoorbellListener {
    /// Subscribe `cache` to the pointer-flip doorbell on `database_url`.
    pub fn postgres(
        database_url: String,
        cache: Arc<WiringCache>,
        scope: DoorbellScope,
    ) -> WiringDoorbellListener {
        Self::with_connector(
            Arc::new(PostgresDoorbellConnector { database_url }),
            Arc::new(WiringDoorbell::new(cache, scope)),
            DOORBELL_RECONNECT_DELAY,
        )
    }

    fn with_connector(
        connector: Arc<dyn DoorbellConnector>,
        doorbell: Arc<WiringDoorbell>,
        reconnect_delay: Duration,
    ) -> WiringDoorbellListener {
        let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(run_doorbell_listener(
            connector,
            doorbell,
            shutdown_receiver,
            reconnect_delay,
        ));
        WiringDoorbellListener { shutdown, task }
    }
}

async fn run_doorbell_listener(
    connector: Arc<dyn DoorbellConnector>,
    doorbell: Arc<WiringDoorbell>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    reconnect_delay: Duration,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if let Err(error) = connector
            .listen(Arc::clone(&doorbell), shutdown.clone())
            .await
        {
            tracing::warn!(
                error = %error,
                "wiring activation doorbell LISTEN connection failed; reconnecting"
            );
        }
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            () = tokio::time::sleep(reconnect_delay) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{Value, json};
    use wamn_router::{Wiring, WiringNode};

    use super::*;

    const ENV: &str = "prod";

    fn cache() -> Arc<WiringCache> {
        Arc::new(WiringCache::new(
            NonZeroUsize::new(8).expect("fixture bound is non-zero"),
        ))
    }

    fn scope() -> DoorbellScope {
        DoorbellScope {
            tenant_id: "t1".to_string(),
            catalog_id: "shop".to_string(),
        }
    }

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

    /// A payload with the DDL's exact keys, built from the shared notice type so
    /// a renamed field cannot pass here and fail in production.
    fn payload(tenant: &str, catalog: &str, environment: &str, wiring_id: &str) -> String {
        serde_json::to_string(&WiringActivationNotice {
            tenant_id: tenant.to_string(),
            catalog_id: catalog.to_string(),
            environment: environment.to_string(),
            wiring_id: wiring_id.to_string(),
            enabled: true,
            confirmed_definition_hash: format!("sha256:{}", "b".repeat(64)),
        })
        .expect("the notice serializes")
    }

    fn doorbell(cache: &Arc<WiringCache>) -> WiringDoorbell {
        WiringDoorbell::new(Arc::clone(cache), scope())
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
        let token = cache
            .get(environment, wiring_id)
            .miss()
            .expect("a fixture resolves only what it has just found missing");
        cache
            .insert(environment, wiring_id, version, graph, token)
            .expect("no doorbell rang between this fixture's miss and its install")
    }

    #[test]
    fn a_flip_invalidates_exactly_the_pointer_it_names() {
        let cache = cache();
        resolve(&cache, ENV, "orders", 1, wiring("v1"));
        resolve(&cache, ENV, "refunds", 4, wiring("refunds-v4"));
        let doorbell = doorbell(&cache);

        assert_eq!(
            doorbell.rang(&payload("t1", "shop", ENV, "orders")),
            DoorbellEffect::Invalidated
        );

        assert!(
            cache.get(ENV, "orders").hit().is_none(),
            "the flipped pointer must send the next delivery back to the store"
        );
        assert_eq!(
            cache
                .get(ENV, "refunds")
                .hit()
                .expect("an unrelated wiring keeps serving")
                .version,
            4
        );
    }

    /// The whole point of the seam: after the flip the next resolution installs
    /// the new version and the hot path serves it — a rollback to a version
    /// still resident does it without recompiling.
    #[test]
    fn the_next_delivery_serves_the_new_version_and_a_rollback_is_a_hit() {
        let cache = cache();
        let one = resolve(&cache, ENV, "orders", 1, wiring("v1"));
        let doorbell = doorbell(&cache);

        doorbell.rang(&payload("t1", "shop", ENV, "orders"));
        resolve(&cache, ENV, "orders", 2, wiring("v2"));
        let served = cache
            .get(ENV, "orders")
            .hit()
            .expect("the re-read resolved");
        assert_eq!(served.version, 2);
        assert_eq!(served.wiring.entry(), "v2");

        doorbell.rang(&payload("t1", "shop", ENV, "orders"));
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
            .get(ENV, "orders")
            .miss()
            .expect("nothing is resident yet");
        let read = (1, "v1");

        // t1: the activation commits. t2: the doorbell rings, still before the
        // read has come back.
        assert_eq!(
            doorbell.rang(&payload("t1", "shop", ENV, "orders")),
            DoorbellEffect::NotResident,
            "there is nothing to drop yet — the resolution is still in flight"
        );

        // t3: the read returns, holding the version from before the flip.
        assert!(
            cache
                .insert(ENV, "orders", read.0, wiring(read.1), token)
                .is_none(),
            "the flip would be undone by the resolution it interrupted"
        );
        assert!(
            cache.get(ENV, "orders").hit().is_none(),
            "the hot path would serve the superseded version until the next flip"
        );

        // The retry starts after the ring, and installs what the flip made
        // active.
        let served = resolve(&cache, ENV, "orders", 2, wiring("v2"));
        assert_eq!(served.entry(), "v2");
        assert_eq!(
            cache
                .get(ENV, "orders")
                .hit()
                .expect("the retry resolved")
                .version,
            2
        );
    }

    /// `pg_notify` is per-database and tenants share a database, so the payload
    /// is the only thing separating one tenant's flip from another's.
    #[test]
    fn another_tenants_or_catalogs_flip_leaves_this_process_alone() {
        let cache = cache();
        resolve(&cache, ENV, "orders", 1, wiring("v1"));
        let doorbell = doorbell(&cache);

        for (tenant, catalog) in [("t2", "shop"), ("t1", "warehouse")] {
            assert_eq!(
                doorbell.rang(&payload(tenant, catalog, ENV, "orders")),
                DoorbellEffect::OutOfScope,
                "{tenant}/{catalog} is not this process's scope"
            );
        }
        assert_eq!(
            cache
                .get(ENV, "orders")
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
            doorbell.rang(&payload("t1", "shop", "staging", "orders")),
            DoorbellEffect::NotResident
        );
        assert_eq!(
            cache
                .get(ENV, "orders")
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
            tenant_id: "t1".to_string(),
            catalog_id: "shop".to_string(),
            environment: ENV.to_string(),
            wiring_id: "orders".to_string(),
            enabled: false,
            confirmed_definition_hash: format!("sha256:{}", "a".repeat(64)),
        })
        .expect("the notice serializes");

        assert_eq!(doorbell.rang(&dark), DoorbellEffect::Invalidated);
        assert!(cache.get(ENV, "orders").hit().is_none());
    }

    /// A payload this process cannot read is deployment skew, and it hides WHICH
    /// pointer moved. Answering "nothing moved" would serve a stale version
    /// silently, so it is treated as a missed flip.
    #[test]
    fn an_unreadable_payload_drops_the_whole_cache_rather_than_guessing() {
        let complete = payload("t1", "shop", ENV, "orders");
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
            assert!(cache.get(ENV, "orders").hit().is_none());
            assert!(cache.get("staging", "refunds").hit().is_none());
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
            "tenant_id": "t1",
            "catalog_id": "shop",
            "environment": ENV,
            "wiring_id": "orders",
            "enabled": true,
            "confirmed_definition_hash": "sha256:00",
        })
        .to_string();
        assert_eq!(doorbell.rang(&snake), DoorbellEffect::Unreadable);

        let kebab = payload("t1", "shop", ENV, "orders");
        assert!(kebab.contains(r#""wiring-id""#), "wire keys are kebab-case");
        assert!(kebab.contains(r#""confirmed-definition-hash""#));
    }

    /// The statement is built from the emitter's own channel constant, so a
    /// renamed channel cannot leave the subscriber listening to silence.
    #[test]
    fn the_listen_statement_names_the_channel_the_ddl_rings() {
        assert_eq!(listen_statement(), "LISTEN wamn_wiring_activation");
        assert!(listen_statement().ends_with(WIRING_ACTIVATION_CHANNEL));
    }

    // ---- the reconnect obligation ------------------------------------------

    /// A connector that fails its first `listen`, so the loop must reconnect.
    struct Flaky {
        listens: Arc<AtomicUsize>,
        fail_first: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl DoorbellConnector for Flaky {
        async fn listen(
            &self,
            doorbell: Arc<WiringDoorbell>,
            mut shutdown: tokio::sync::watch::Receiver<bool>,
        ) -> anyhow::Result<()> {
            self.listens.fetch_add(1, Ordering::SeqCst);
            if self.fail_first.swap(false, Ordering::SeqCst) {
                // Failed BEFORE the LISTEN was established, so nothing is
                // dropped and nothing is heard.
                return Err(anyhow!("injected doorbell disconnect"));
            }
            doorbell.listening();
            shutdown
                .changed()
                .await
                .map_err(|_| anyhow!("test doorbell shutdown owner dropped"))?;
            Ok(())
        }
    }

    async fn eventually(predicate: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("condition did not become true");
    }

    /// THE TRANSPORT'S ONLY LOSS MODE. While the connection was down, flips
    /// committed to sessions that were listening and were queued for nobody. The
    /// reconnected process cannot know which, so it drops every pointer it holds
    /// rather than resuming against any of them.
    #[tokio::test]
    async fn a_reconnect_drops_every_pointer_because_the_gap_is_unknowable() {
        let cache = cache();
        resolve(&cache, ENV, "orders", 1, wiring("v1"));
        resolve(&cache, ENV, "refunds", 4, wiring("refunds-v4"));
        resolve(&cache, "staging", "orders", 9, wiring("staging-v9"));
        let listens = Arc::new(AtomicUsize::new(0));
        let connector = Arc::new(Flaky {
            listens: Arc::clone(&listens),
            fail_first: std::sync::atomic::AtomicBool::new(true),
        });

        let listener = WiringDoorbellListener::with_connector(
            connector,
            Arc::new(WiringDoorbell::new(Arc::clone(&cache), scope())),
            Duration::from_millis(1),
        );

        eventually(|| listens.load(Ordering::SeqCst) >= 2).await;
        eventually(|| cache.get(ENV, "orders").hit().is_none()).await;

        for (environment, wiring_id) in [(ENV, "orders"), (ENV, "refunds"), ("staging", "orders")] {
            assert!(
                cache.get(environment, wiring_id).hit().is_none(),
                "{environment}/{wiring_id} resumed against a pointer no flip was seen for"
            );
        }
        assert_eq!(
            cache.len(),
            3,
            "the graphs survive: a version is immutable, so only pointers can go stale"
        );
        drop(listener);
    }

    /// The drop happens only once the `LISTEN` is established. The reverse order
    /// leaves a window in which a flip is neither delivered nor covered.
    #[test]
    fn the_listen_is_established_before_the_pointers_are_dropped() {
        let production = include_str!("wiring_doorbell.rs")
            .split_once("#[cfg(test)]")
            .expect("this module has a test module")
            .0;
        let established = production
            .find("client.batch_execute(&listen_statement())")
            .expect("the connector issues the LISTEN");
        let dropped = production
            .find("doorbell.listening();")
            .expect("the connector drops the pointers");
        assert!(
            established < dropped,
            "the pointers may only be dropped once the LISTEN is established"
        );
    }
}

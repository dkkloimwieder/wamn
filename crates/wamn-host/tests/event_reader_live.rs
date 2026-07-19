//! Live gate for the CDC event reader (wamn-l5i9.10, D19 v3 §4).
//!
//! Set `WAMN_READER_PG_URL` to a **superuser** URL (path `/postgres`) of a
//! throwaway Postgres 18 running `wal_level=logical`, and
//! `WAMN_READER_NATS_URL` to a throwaway JetStream-enabled NATS; skipped
//! cleanly when either is unset (recipe: docs/build-and-test.md
//! [EVT-READER]).
//!
//! Stands up the REAL substrate (system schema + registration rows via the
//! wamn-registry builders; role/publication/slot/grants via the wamn-provision
//! builders) and drives `event_reader::run_with_token` — the service body the
//! subcommand runs — through the load-bearing drills:
//!
//! - refusal probes: a disabled registration refuses; a MISSING slot is the
//!   v3 §11 capture-gap incident (the reader never creates slots);
//! - commit order + envelope shape + `Nats-Msg-Id` dedupe on the stream;
//! - confirmed LSN advances only on JetStream ack;
//! - crash (task abort) → restart resumes from the confirmed LSN, no gaps;
//! - JetStream unreachable (a severed TCP proxy) → the LSN HOLDS while
//!   writes continue → restore → delayed, never lost;
//! - the RENAME DRILL (wamn-l5i9.11 / R9b): a catalog entity provisioned +
//!   renamed through the REAL `migrate-catalog` path keeps its stable entity
//!   id on every envelope and subject across `ALTER TABLE RENAME`, in ONE
//!   reader session, with the pg_class OID provably constant; the
//!   `publish-catalog` re-run backfills a wiped map; hand-created and
//!   platform tables publish with `entity` ABSENT (the unmapped marker);
//! - clean shutdown on cancellation; teardown leaves NO slot behind.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_nats::header::NATS_MESSAGE_ID;
use async_nats::jetstream;
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use futures_util::StreamExt as _;
use pg_walstream::CancellationToken;
use tokio_postgres::NoTls;

use wamn_event_wire::{Causation, Envelope, Op, msg_id, subject};
use wamn_host::event_reader::{EventReaderArgs, run_with_token};
use wamn_host::migrate_catalog::MigrateCatalogArgs;
use wamn_host::publish_catalog::PublishCatalogArgs;
use wamn_provision::{cdc_object_name, event_stream_name, sql};
use wamn_registry::sql::{
    upsert_event_reader_sql, upsert_org_sql, upsert_project_env_sql, upsert_project_sql,
};

const SYSTEM_SCHEMA: &str = include_str!("../../../deploy/sql/system-schema.sql");
const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const DB: &str = "wamn_reader_live";
const ORG: &str = "rl0";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const CDC_PW: &str = "wamn_cdc_pw";
const TENANT: &str = "t1";

/// The rename-drill catalog, v1: the entity id `sales_orders` DELIBERATELY
/// differs from its initial table name `orders`, so a mapped envelope proves
/// the map was consulted — never an echo of the table name.
const DRILL_CAT_V1: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "evtdrill",
  "version": 1,
  "name": "event-plane rename drill",
  "entities": [
    { "id": "sales_orders", "name": "orders",
      "fields": [ { "id": "num", "name": "num", "type": { "kind": "text" } } ] }
  ]
}"#;

/// v2: the SAME entity id, renamed table (`orders` → `orders2`) — the R9b
/// migration (`ALTER TABLE RENAME`, pg_class OID preserved).
const DRILL_CAT_V2: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "evtdrill",
  "version": 2,
  "name": "event-plane rename drill",
  "entities": [
    { "id": "sales_orders", "name": "orders2",
      "fields": [ { "id": "num", "name": "num", "type": { "kind": "text" } } ] }
  ]
}"#;

/// Write a drill catalog to a temp file the real subcommand fns can read.
fn catalog_file(name: &str, json: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("wamn_reader_live_{name}.json"));
    std::fs::write(&p, json).expect("write drill catalog");
    p
}

/// Swap the database path segment of a libpq URL (the test controls the URL —
/// no query string).
fn swap_db(url: &str, db: &str) -> String {
    let (base, _) = url.rsplit_once('/').expect("url has a path");
    format!("{base}/{db}")
}

/// `host:port` (with credentials swapped out) → the CDC role's plain URL.
fn cdc_plain_url(super_url: &str, role: &str) -> String {
    let after_scheme = super_url.strip_prefix("postgres://").expect("postgres://");
    let (_, host_and_path) = after_scheme.rsplit_once('@').expect("url has userinfo");
    let (host_port, _) = host_and_path.split_once('/').expect("url has a path");
    format!("postgres://{role}:{CDC_PW}@{host_port}/{DB}")
}

async fn connect(url: &str) -> tokio_postgres::Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .unwrap_or_else(|e| panic!("connect {url}: {e}"));
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

/// A test-owned TCP proxy in front of NATS: severing it is "JetStream
/// unreachable" without touching the NATS server itself.
struct Proxy {
    port: u16,
    severed: Arc<AtomicBool>,
    conns: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Proxy {
    async fn start(upstream: String) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let severed = Arc::new(AtomicBool::new(false));
        let conns: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let (s, c) = (severed.clone(), conns.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    break;
                };
                if s.load(Ordering::Relaxed) {
                    continue; // accept-then-drop: the client sees a dead peer
                }
                let upstream = upstream.clone();
                let h = tokio::spawn(async move {
                    if let Ok(mut outbound) = tokio::net::TcpStream::connect(&upstream).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                c.lock().unwrap().push(h);
            }
        });
        Proxy {
            port,
            severed,
            conns,
        }
    }

    fn sever(&self) {
        self.severed.store(true, Ordering::Relaxed);
        for h in self.conns.lock().unwrap().drain(..) {
            h.abort();
        }
    }

    fn restore(&self) {
        self.severed.store(false, Ordering::Relaxed);
    }
}

fn reader_args(super_url: &str, cdc_name: &str, nats_url: String) -> EventReaderArgs {
    EventReaderArgs {
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENV.into(),
        system_database_url: swap_db(super_url, DB),
        cdc_url: cdc_plain_url(super_url, cdc_name),
        nats_url,
        sslmode: "disable".into(),
        stream_replicas: 1,
        dup_window_secs: 120,
        feedback_secs: 1,
        stall_threshold_secs: 30,
        slot_poll_secs: 0,
        slot_safe_wal_warn_bytes: 268_435_456,
    }
}

/// Drain the whole stream through a fresh ephemeral pull consumer:
/// `(subject, Nats-Msg-Id, envelope)` in delivery order.
async fn read_all(
    js: &jetstream::Context,
    stream: &str,
    expect: usize,
) -> Vec<(String, String, Envelope)> {
    let stream = js.get_stream(stream).await.expect("get stream");
    let consumer = stream
        .create_consumer(PullConfig {
            deliver_policy: DeliverPolicy::All,
            ack_policy: AckPolicy::Explicit,
            num_replicas: 1,
            memory_storage: true,
            ..Default::default()
        })
        .await
        .expect("create pull consumer");
    let mut out = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while out.len() < expect && Instant::now() < deadline {
        let mut batch = consumer
            .fetch()
            .max_messages(expect - out.len())
            .messages()
            .await
            .expect("fetch batch");
        let mut drained_any = false;
        while let Some(msg) = batch.next().await {
            let msg = msg.expect("consume message");
            drained_any = true;
            let id = msg
                .headers
                .as_ref()
                .and_then(|h| h.get(NATS_MESSAGE_ID))
                .map(|v| v.to_string())
                .unwrap_or_default();
            let envelope: Envelope =
                serde_json::from_slice(&msg.payload).expect("envelope deserializes (draft shape)");
            out.push((msg.subject.to_string(), id, envelope));
            msg.ack().await.expect("ack");
        }
        if !drained_any {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    out
}

async fn stream_count(js: &jetstream::Context, name: &str) -> u64 {
    let mut stream = js.get_stream(name).await.expect("get stream");
    stream.info().await.expect("stream info").state.messages
}

async fn wait_for_count(js: &jetstream::Context, name: &str, want: u64, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if stream_count(js, name).await >= want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!(
        "stream {name} never reached {want} messages (have {})",
        stream_count(js, name).await
    );
}

/// Poll until the slot's confirmed_flush_lsn is at/past `lsn` (text form).
async fn wait_confirmed_past(sys: &tokio_postgres::Client, slot: &str, lsn: &str, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let caught_up: bool = sys
            .query_one(
                "SELECT confirmed_flush_lsn >= $2::text::pg_lsn \
                 FROM pg_replication_slots WHERE slot_name = $1",
                &[&slot, &lsn],
            )
            .await
            .expect("read confirmed_flush_lsn")
            .get(0);
        if caught_up {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("confirmed_flush_lsn never reached {lsn} (LSN-advance-on-ack broken?)");
}

async fn confirmed_lsn(sys: &tokio_postgres::Client, slot: &str) -> String {
    sys.query_one(
        "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name = $1",
        &[&slot],
    )
    .await
    .expect("read confirmed_flush_lsn")
    .get(0)
}

async fn insert_lsn(sys: &tokio_postgres::Client) -> String {
    sys.query_one("SELECT pg_current_wal_insert_lsn()::text", &[])
        .await
        .unwrap()
        .get(0)
}

/// The drill entity's map row: `(entity_id, table_name, oid-matches-live-table)`.
async fn drill_map_row(sys: &tokio_postgres::Client) -> (String, String, bool) {
    let r = sys
        .query_one(
            "SELECT entity_id, table_name, \
                    relation_oid = ('app.' || quote_ident(table_name))::regclass::oid \
             FROM app.wamn_entities WHERE entity_id = 'sales_orders'",
            &[],
        )
        .await
        .expect("read the drill entity's map row");
    (r.get(0), r.get(1), r.get(2))
}

/// `(op, id)` — an event's identity for the commit-order program check.
fn key_of(e: &Envelope) -> (Op, String) {
    let row = match e.op {
        Op::Delete => e.old.as_ref().expect("delete carries the old key"),
        _ => e.new.as_ref().expect("insert/update carry new"),
    };
    (e.op, row.get("id").unwrap().as_str().unwrap().to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reader_streams_one_project_env_to_the_evt_stream() {
    let Ok(super_url) = std::env::var("WAMN_READER_PG_URL") else {
        eprintln!("WAMN_READER_PG_URL unset — skipping the event-reader live gate");
        return;
    };
    let Ok(nats_url) = std::env::var("WAMN_READER_NATS_URL") else {
        eprintln!("WAMN_READER_NATS_URL unset — skipping the event-reader live gate");
        return;
    };

    let cdc_name = cdc_object_name(ORG, PROJECT, ENV); // wamn_cdc_rl0__app__dev
    let stream_name = event_stream_name(ORG, ENV); // EVT_rl0_dev

    // --- hermetic preamble (the M2 lesson: leftovers mask mutations) --------
    let admin = connect(&super_url).await;
    // A crashed prior run can leave its walsender ATTACHED — an active slot
    // can't be dropped and blocks DROP DATABASE.
    let _ = admin
        .execute(
            "SELECT pg_terminate_backend(active_pid) FROM pg_replication_slots \
             WHERE slot_name = $1 AND active",
            &[&cdc_name],
        )
        .await;
    let _ = admin
        .execute(
            "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots \
             WHERE slot_name = $1",
            &[&cdc_name],
        )
        .await;
    admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {DB} WITH (FORCE)"))
        .await
        .expect("drop leftover db");
    admin
        .batch_execute(&format!("DROP ROLE IF EXISTS {cdc_name}"))
        .await
        .expect("drop leftover role");
    admin
        .batch_execute(&format!("CREATE DATABASE {DB}"))
        .await
        .expect("create db");

    // --- the REAL substrate, via the real builders --------------------------
    let sys = connect(&swap_db(&super_url, DB)).await;
    sys.batch_execute(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') \
         THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$",
    )
    .await
    .expect("wamn_system role (the schema's owner-grants target)");
    sys.batch_execute(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
         THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' \
           NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$",
    )
    .await
    .expect("wamn_app role (the floor + catalog schema's grant target)");
    sys.batch_execute(SYSTEM_SCHEMA)
        .await
        .expect("apply deploy/sql/system-schema.sql");
    sys.batch_execute(CATALOG_SCHEMA)
        .await
        .expect("apply deploy/sql/catalog-schema.sql (the migrate-catalog metadata store)");
    sys.execute(upsert_org_sql(), &[&ORG, &"pooled", &"wamn-pg"])
        .await
        .expect("org row");
    sys.execute(upsert_project_sql(), &[&ORG, &PROJECT])
        .await
        .expect("project row");
    sys.execute(
        wamn_registry::sql::stamp_env_policy_sql(),
        &[
            &ORG,
            &ENV,
            &r#"{"kind":"pool"}"#,
            &0i32,
            &1i32,
            &"1Gi",
            &"250m",
            &"256Mi",
            &"postgres:18",
            &"",
            &"",
            &"off",
        ],
    )
    .await
    .expect("env-policy row (the project_envs FK target)");
    sys.execute(
        upsert_project_env_sql(),
        &[
            &ORG,
            &PROJECT,
            &ENV,
            &"wamn-db-rl0--app--dev",
            &None::<&str>,
        ],
    )
    .await
    .expect("project-env row");
    let secret = format!("wamn-cdc-{ORG}--{PROJECT}--{ENV}");
    let register = |enabled: bool| {
        let (sys, cdc, stream, secret) = (&sys, &cdc_name, &stream_name, &secret);
        async move {
            sys.execute(
                upsert_event_reader_sql(),
                &[
                    &ORG,
                    &PROJECT,
                    &ENV,
                    cdc,
                    cdc,
                    stream,
                    secret,
                    &None::<&str>,
                    &enabled,
                ],
            )
            .await
            .expect("event_readers row");
        }
    };
    register(false).await;

    sys.batch_execute(&sql::ensure_schema_sql("app"))
        .await
        .expect("schema");
    sys.batch_execute(
        "CREATE TABLE app.receipts (id bigint PRIMARY KEY, val text, big text, note text); \
         ALTER TABLE app.receipts ALTER COLUMN big SET STORAGE EXTERNAL",
    )
    .await
    .expect("table");
    sys.batch_execute(&sql::ensure_replication_role_sql(&cdc_name, CDC_PW))
        .await
        .expect("replication role");
    sys.batch_execute(&sql::create_publication_sql(&cdc_name, "app"))
        .await
        .expect("publication");
    // The entity map precedes the grants (the enable-cdc bundle order): the
    // role's SELECT ON ALL TABLES must cover the reader's decode-time lookup.
    sys.batch_execute(&sql::ensure_entity_map_sql("app"))
        .await
        .expect("entity map");
    sys.batch_execute(&sql::grant_replication_access_sql(DB, &cdc_name, "app"))
        .await
        .expect("grants");
    // The slot is deliberately NOT created yet — the incident probe needs its absence.

    // --- NATS hygiene + the severable proxy ---------------------------------
    let direct = async_nats::connect(&nats_url).await.expect("connect NATS");
    let js = jetstream::new(direct);
    let _ = js.delete_stream(&stream_name).await;
    let upstream = nats_url.trim_start_matches("nats://").to_string();
    let proxy = Proxy::start(upstream).await;
    let proxied_nats = format!("nats://127.0.0.1:{}", proxy.port);

    // --- refusal probes ------------------------------------------------------
    // Both are timeout-bounded: a refusal that HANGS (e.g. a blinded preflight
    // sending the reader into its reopen loop forever) must fail the gate, not
    // stall it — that is exactly the M4 mutant's shape.
    let err = tokio::time::timeout(
        Duration::from_secs(60),
        run_with_token(
            reader_args(&super_url, &cdc_name, proxied_nats.clone()),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the disabled-registration probe must terminate")
    .expect_err("a disabled registration must refuse");
    assert!(err.to_string().contains("disabled"), "got: {err:#}");

    register(true).await;
    let err = tokio::time::timeout(
        Duration::from_secs(60),
        run_with_token(
            reader_args(&super_url, &cdc_name, proxied_nats.clone()),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the missing-slot probe must terminate (a hung reader = the incident path is broken)")
    .expect_err("a missing slot is the v3 §11 incident, never a silent create");
    assert!(err.to_string().contains("CAPTURE GAP"), "got: {err:#}");

    // --- slot (the real builder), then the reader ---------------------------
    sys.batch_execute(&sql::create_failover_slot_sql(&cdc_name))
        .await
        .expect("failover slot");
    let token = CancellationToken::new();
    let handle = tokio::spawn(run_with_token(
        reader_args(&super_url, &cdc_name, proxied_nats.clone()),
        token.clone(),
    ));
    // The walsender attaching proves the session opened with the CDC role.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let active: bool = sys
            .query_one(
                "SELECT active FROM pg_replication_slots WHERE slot_name = $1",
                &[&cdc_name],
            )
            .await
            .unwrap()
            .get(0);
        if active {
            break;
        }
        assert!(
            !handle.is_finished(),
            "reader died at startup: {:?}",
            handle.await
        );
        assert!(Instant::now() < deadline, "walsender never attached");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // --- phase A: commit order + envelope shape + dedupe --------------------
    let mut expected: Vec<(Op, String)> = Vec::new();
    for id in 1..=20i64 {
        sys.execute(
            "INSERT INTO app.receipts (id, val) VALUES ($1, $2)",
            &[&id, &format!("v{id}")],
        )
        .await
        .unwrap();
        expected.push((Op::Insert, id.to_string()));
    }
    sys.batch_execute(
        "BEGIN; \
         INSERT INTO app.receipts (id, val) VALUES (21, 'v21'); \
         INSERT INTO app.receipts (id, val) VALUES (22, 'v22'); \
         INSERT INTO app.receipts (id, val) VALUES (23, 'v23'); \
         COMMIT",
    )
    .await
    .unwrap();
    expected.extend([21, 22, 23].map(|id| (Op::Insert, id.to_string())));
    sys.batch_execute("UPDATE app.receipts SET val = 'u5' WHERE id = 5")
        .await
        .unwrap();
    expected.push((Op::Update, "5".into()));
    sys.batch_execute("DELETE FROM app.receipts WHERE id = 6")
        .await
        .unwrap();
    expected.push((Op::Delete, "6".into()));
    // 22.4KB out-of-line value; the follow-up update must NOT re-ship it.
    sys.batch_execute(
        "INSERT INTO app.receipts (id, val, big) \
         SELECT 30, 'v30', string_agg(md5(i::text), '') FROM generate_series(1, 700) i",
    )
    .await
    .unwrap();
    expected.push((Op::Insert, "30".into()));
    // The advance target: a position BEFORE the last txn — the last commit's
    // end_lsn (where confirmed parks) is necessarily past it, while trailing
    // non-txn WAL records keep the *after* position unreachable forever.
    let wal_a = insert_lsn(&sys).await;
    sys.batch_execute("UPDATE app.receipts SET val = 'u30' WHERE id = 30")
        .await
        .unwrap();
    expected.push((Op::Update, "30".into()));

    wait_for_count(&js, &stream_name, expected.len() as u64, 20).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        stream_count(&js, &stream_name).await,
        expected.len() as u64,
        "no stray events beyond the write program"
    );
    let delivered = read_all(&js, &stream_name, expected.len()).await;
    assert_eq!(
        delivered
            .iter()
            .map(|(_, _, e)| key_of(e))
            .collect::<Vec<_>>(),
        expected,
        "delivery order == commit order, exactly the write program"
    );
    for (subj, id, e) in &delivered {
        assert_eq!(subj, &subject(ORG, PROJECT, ENV, e.entity_segment(), e.op));
        assert_eq!(id, &msg_id(PROJECT, ENV, e.lsn));
        // receipts is hand-created (no catalog entity): the FD unmapped
        // marker — `entity` ABSENT, `table` carries the physical name, the
        // subject falls back to the table segment.
        assert!(e.entity.is_none(), "unmapped table publishes entity-ABSENT");
        assert_eq!(e.table, "receipts");
    }
    let ids: std::collections::BTreeSet<_> = delivered.iter().map(|(_, id, _)| id).collect();
    assert_eq!(ids.len(), delivered.len(), "Nats-Msg-Ids are unique");
    let txids: Vec<u32> = delivered[20..23].iter().map(|(_, _, e)| e.txid).collect();
    assert!(
        txids[0] == txids[1] && txids[1] == txids[2],
        "one txn's rows share the txid"
    );
    assert_ne!(delivered[0].2.txid, txids[0], "txids differ across txns");
    assert_eq!(
        delivered[20].2.commit_ts, delivered[22].2.commit_ts,
        "one txn's rows share the commit_ts"
    );
    let update5 = &delivered[23].2;
    assert!(
        update5.old.is_none(),
        "REPLICA IDENTITY DEFAULT + unchanged key ⇒ no old image"
    );
    let delete6 = &delivered[24].2;
    assert!(delete6.new.is_none(), "a delete has no new image");
    assert_eq!(
        delete6.old.as_ref().unwrap().get("id").unwrap(),
        "6",
        "a delete's old image carries the key"
    );
    let insert30 = &delivered[25].2.new.as_ref().unwrap().clone();
    assert_eq!(
        insert30.get("big").unwrap().as_str().unwrap().len(),
        700 * 32,
        "the insert ships the TOAST value"
    );
    let update30 = &delivered[26].2.new.as_ref().unwrap().clone();
    assert!(
        update30.get("big").is_none(),
        "an unchanged TOAST column is ABSENT from new"
    );
    assert!(
        update30.get("note").unwrap().is_null(),
        "a real NULL is PRESENT as null (distinguishable from TOAST-absent)"
    );
    assert_eq!(update30.get("val").unwrap(), "u30");

    // --- phase B: confirmed LSN advanced on ack -----------------------------
    wait_confirmed_past(&sys, &cdc_name, &wal_a, 75).await;

    // --- phase C: crash → restart resumes from the confirmed LSN ------------
    handle.abort();
    let _ = handle.await;
    let mut wal_c = String::new();
    for id in 40..=44i64 {
        if id == 44 {
            wal_c = insert_lsn(&sys).await; // before the phase's last txn
        }
        sys.execute(
            "INSERT INTO app.receipts (id, val) VALUES ($1, $2)",
            &[&id, &format!("v{id}")],
        )
        .await
        .unwrap();
        expected.push((Op::Insert, id.to_string()));
    }
    let token2 = CancellationToken::new();
    let handle2 = tokio::spawn(run_with_token(
        reader_args(&super_url, &cdc_name, proxied_nats.clone()),
        token2.clone(),
    ));
    wait_for_count(&js, &stream_name, expected.len() as u64, 30).await;
    let delivered = read_all(&js, &stream_name, expected.len()).await;
    assert_eq!(
        delivered
            .iter()
            .map(|(_, _, e)| key_of(e))
            .collect::<Vec<_>>(),
        expected,
        "crash+restart: no gaps, no dupes (dedupe absorbed any redelivery), order kept"
    );
    wait_confirmed_past(&sys, &cdc_name, &wal_c, 75).await;

    // --- phase D: JetStream down ⇒ the LSN holds ⇒ delayed, never lost ------
    let c0 = confirmed_lsn(&sys, &cdc_name).await;
    proxy.sever();
    let count_before = stream_count(&js, &stream_name).await;
    let mut wal_d = String::new();
    for id in 50..=54i64 {
        if id == 54 {
            wal_d = insert_lsn(&sys).await; // before the phase's last txn
        }
        sys.execute(
            "INSERT INTO app.receipts (id, val) VALUES ($1, $2)",
            &[&id, &format!("v{id}")],
        )
        .await
        .unwrap();
        expected.push((Op::Insert, id.to_string()));
    }
    // Past a full idle-keepalive feedback cycle (~30s): a reader that advanced
    // the LSN without a real ack (the M2 fire-and-forget shape) WILL have its
    // advance reach the server by now — while the real reader, stuck inside
    // the publish retry, sends no feedback at all and the LSN provably holds.
    tokio::time::sleep(Duration::from_secs(40)).await;
    let held: bool = sys
        .query_one(
            "SELECT confirmed_flush_lsn = $2::text::pg_lsn \
             FROM pg_replication_slots WHERE slot_name = $1",
            &[&cdc_name, &c0],
        )
        .await
        .unwrap()
        .get(0);
    assert!(held, "JetStream unreachable ⇒ the confirmed LSN must HOLD");
    assert_eq!(
        stream_count(&js, &stream_name).await,
        count_before,
        "nothing lands while severed"
    );
    proxy.restore();
    wait_for_count(&js, &stream_name, expected.len() as u64, 40).await;
    wait_confirmed_past(&sys, &cdc_name, &wal_d, 75).await;
    let delivered = read_all(&js, &stream_name, expected.len()).await;
    assert_eq!(
        delivered
            .iter()
            .map(|(_, _, e)| key_of(e))
            .collect::<Vec<_>>(),
        expected,
        "the severed window's events arrive after restore — delayed, never lost"
    );

    // --- phase F: the RENAME DRILL (wamn-l5i9.11 / R9b) ---------------------
    // The REAL migrate path provisions entity `sales_orders` as table
    // `orders`, the reader session from phase C stays LIVE throughout, and a
    // v2 migration renames the table mid-stream. Every envelope must carry
    // the STABLE entity id — the map is OID-keyed and the cache is never
    // invalidated, so the resolution survives the rename by construction.
    let admin_url = swap_db(&super_url, DB);
    wamn_host::migrate_catalog::run(MigrateCatalogArgs {
        admin_database_url: admin_url.clone(),
        tenant: TENANT.into(),
        environment: ENV.into(),
        schema: "app".into(),
        target: catalog_file("cat_v1", DRILL_CAT_V1),
        base: None,
        dry_run: false,
        confirm_with_backup: false,
    })
    .await
    .expect("migrate-catalog v1 (create the drill entity)");
    assert_eq!(
        drill_map_row(&sys).await,
        ("sales_orders".into(), "orders".into(), true),
        "migrate-catalog maintains the OID-keyed entity map in its transaction"
    );
    let oid_before: u32 = sys
        .query_one("SELECT 'app.orders'::regclass::oid", &[])
        .await
        .unwrap()
        .get(0);

    // Backfill probe: a wiped map (an env CDC-enabled after its catalog was
    // published) repopulates with one publish-catalog re-run.
    sys.batch_execute("DELETE FROM app.wamn_entities")
        .await
        .unwrap();
    wamn_host::publish_catalog::run(PublishCatalogArgs {
        catalog: catalog_file("cat_v1", DRILL_CAT_V1),
        admin_database_url: Some(admin_url.clone()),
        tenant: TENANT.into(),
        schema: "app".into(),
        provision: false,
        runstate: false,
        seed_dataset: None,
        flow: vec![],
    })
    .await
    .expect("publish-catalog (the map backfill path)");
    assert_eq!(
        drill_map_row(&sys).await,
        ("sales_orders".into(), "orders".into(), true),
        "publish-catalog re-run backfills the entity map"
    );

    for num in ["80", "81"] {
        sys.execute(
            "INSERT INTO app.orders (tenant_id, num) VALUES ($1, $2)",
            &[&TENANT, &num],
        )
        .await
        .unwrap();
    }

    // The rename: v2 through the real migrate path (destructive — the rename
    // op is flagged; the drill confirms like an operator with a backup).
    wamn_host::migrate_catalog::run(MigrateCatalogArgs {
        admin_database_url: admin_url.clone(),
        tenant: TENANT.into(),
        environment: ENV.into(),
        schema: "app".into(),
        target: catalog_file("cat_v2", DRILL_CAT_V2),
        base: None,
        dry_run: false,
        confirm_with_backup: true,
    })
    .await
    .expect("migrate-catalog v2 (the rename)");
    let oid_after: u32 = sys
        .query_one("SELECT 'app.orders2'::regclass::oid", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        oid_before, oid_after,
        "ALTER TABLE RENAME preserves the pg_class OID (the property the map rides on)"
    );
    assert_eq!(
        drill_map_row(&sys).await,
        ("sales_orders".into(), "orders2".into(), true),
        "the rename re-upserts the SAME map row: new table_name, same entity id + OID"
    );

    for num in ["90", "91", "92"] {
        sys.execute(
            "INSERT INTO app.orders2 (tenant_id, num) VALUES ($1, $2)",
            &[&TENANT, &num],
        )
        .await
        .unwrap();
    }

    // Deterministic drill accounting on the stream: 5 sales_orders inserts +
    // the map's own unmapped events (insert at migrate v1, delete at the wipe,
    // insert at the backfill, update at the v2 re-upsert) + the wamn_catalog
    // snapshot insert = 10.
    let drill_total = expected.len() as u64 + 10;
    wait_for_count(&js, &stream_name, drill_total, 30).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        stream_count(&js, &stream_name).await,
        drill_total,
        "no stray events beyond the drill program"
    );
    let all = read_all(&js, &stream_name, drill_total as usize).await;
    let drill: Vec<&(String, String, Envelope)> = all
        .iter()
        .filter(|(_, _, e)| e.entity.as_deref() == Some("sales_orders"))
        .collect();
    assert_eq!(
        drill
            .iter()
            .map(|(_, _, e)| {
                (
                    e.op,
                    e.table.clone(),
                    e.new
                        .as_ref()
                        .unwrap()
                        .get("num")
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string(),
                )
            })
            .collect::<Vec<_>>(),
        [
            (Op::Insert, "orders", "80"),
            (Op::Insert, "orders", "81"),
            (Op::Insert, "orders2", "90"),
            (Op::Insert, "orders2", "91"),
            (Op::Insert, "orders2", "92"),
        ]
        .map(|(op, t, n)| (op, t.to_string(), n.to_string())),
        "envelopes carry the stable entity id across the rename (table changes, entity does not)"
    );
    for (subj, _, e) in &drill {
        assert_eq!(
            subj,
            &subject(ORG, PROJECT, ENV, "sales_orders", e.op),
            "the subject is keyed by the entity id, not the table"
        );
    }
    assert!(
        all.iter()
            .all(|(subj, _, _)| { !subj.contains(".orders.") && !subj.contains(".orders2.") }),
        "no mapped event ever falls back to a table-name subject"
    );
    // The platform tables the schema-scoped publication auto-includes are the
    // unmapped probe: entity ABSENT, table-name subject fallback.
    let unmapped: Vec<&(String, String, Envelope)> = all
        .iter()
        .filter(|(_, _, e)| e.table == "wamn_entities")
        .collect();
    assert_eq!(
        unmapped.len(),
        4,
        "the map's own writes publish as unmapped platform-table events"
    );
    for (subj, _, e) in &unmapped {
        assert!(e.entity.is_none(), "platform table publishes entity-ABSENT");
        assert_eq!(subj, &subject(ORG, PROJECT, ENV, "wamn_entities", e.op));
    }

    // --- phase G: CAUSATION stitching (wamn-l5i9.12) ------------------------
    // A transactional `pg_logical_emit_message(true, 'wamn.causation', …)`
    // rides the txn commit; the reader BUFFERS the txn and stamps
    // {run,root,depth} onto every one of its row envelopes — regardless of
    // whether the message frame arrives BEFORE or AFTER the rows (buffer-per-
    // txn, the FD robustness). A txn with no emit carries no causation; a
    // rolled-back txn that emitted one publishes nothing (transactional).
    let base = stream_count(&js, &stream_name).await;
    // message-at-BEGIN: the frame precedes the rows.
    sys.batch_execute(
        "BEGIN; \
         SELECT pg_logical_emit_message(true, 'wamn.causation', '{\"run\":\"r-100\",\"root\":\"root-a\",\"depth\":0}'); \
         INSERT INTO app.receipts (id, val) VALUES (100, 'c100'); \
         INSERT INTO app.receipts (id, val) VALUES (101, 'c101'); \
         COMMIT",
    )
    .await
    .expect("causation txn: message at BEGIN");
    // message-PRE-COMMIT: the frame FOLLOWS the rows — the order-robustness
    // proof that buffer-per-txn stamps rows seen before the message arrived.
    sys.batch_execute(
        "BEGIN; \
         INSERT INTO app.receipts (id, val) VALUES (102, 'c102'); \
         INSERT INTO app.receipts (id, val) VALUES (103, 'c103'); \
         SELECT pg_logical_emit_message(true, 'wamn.causation', '{\"run\":\"r-200\",\"root\":\"root-b\",\"depth\":1}'); \
         COMMIT",
    )
    .await
    .expect("causation txn: message before COMMIT");
    // a plain txn, no emit → causation ABSENT.
    sys.execute(
        "INSERT INTO app.receipts (id, val) VALUES (104, 'c104')",
        &[],
    )
    .await
    .unwrap();
    // a ROLLED-BACK txn that emitted + wrote → NOTHING publishes: the
    // transactional message never reaches the stream, the row is aborted.
    sys.batch_execute(
        "BEGIN; \
         SELECT pg_logical_emit_message(true, 'wamn.causation', '{\"run\":\"r-rolled\",\"root\":\"root-x\",\"depth\":9}'); \
         INSERT INTO app.receipts (id, val) VALUES (105, 'c105'); \
         ROLLBACK",
    )
    .await
    .expect("causation txn: rolled back");

    wait_for_count(&js, &stream_name, base + 5, 30).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        stream_count(&js, &stream_name).await,
        base + 5,
        "5 row events (100-104); the rolled-back txn (105) + the message frames publish nothing"
    );
    let all = read_all(&js, &stream_name, (base + 5) as usize).await;
    let caus: std::collections::BTreeMap<i64, Option<Causation>> = all
        .iter()
        .filter_map(|(_, _, e)| {
            let id: i64 = e.new.as_ref()?.get("id")?.as_str()?.parse().ok()?;
            (100..=104).contains(&id).then(|| (id, e.causation.clone()))
        })
        .collect();
    assert_eq!(caus.len(), 5, "all five phase-G inserts are on the stream");
    let c_a = Causation {
        run: "r-100".into(),
        root: "root-a".into(),
        depth: 0,
    };
    let c_b = Causation {
        run: "r-200".into(),
        root: "root-b".into(),
        depth: 1,
    };
    assert_eq!(
        caus[&100].as_ref(),
        Some(&c_a),
        "message-at-BEGIN stamps the whole txn"
    );
    assert_eq!(caus[&101].as_ref(), Some(&c_a), "…every row of it");
    assert_eq!(
        caus[&102].as_ref(),
        Some(&c_b),
        "message-AFTER-rows still stamps every row (buffer-per-txn robustness)"
    );
    assert_eq!(caus[&103].as_ref(), Some(&c_b), "…every row of it");
    assert_eq!(caus[&104], None, "a txn with no emit carries no causation");
    // 105 rolled back — never on the stream (the exact count above proved it).

    // --- phase E: clean shutdown --------------------------------------------
    token2.cancel();
    let joined = tokio::time::timeout(Duration::from_secs(10), handle2)
        .await
        .expect("reader exits promptly on cancellation")
        .expect("reader task join");
    assert!(joined.is_ok(), "clean shutdown: {joined:?}");

    // --- teardown: NO slot left behind --------------------------------------
    let _ = js.delete_stream(&stream_name).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let active: bool = sys
            .query_one(
                "SELECT active FROM pg_replication_slots WHERE slot_name = $1",
                &[&cdc_name],
            )
            .await
            .unwrap()
            .get(0);
        if !active {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "walsender never released the slot"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    sys.execute("SELECT pg_drop_replication_slot($1)", &[&cdc_name])
        .await
        .expect("drop the slot (never leave one behind)");
    drop(sys);
    admin
        .batch_execute(&format!("DROP DATABASE {DB} WITH (FORCE)"))
        .await
        .expect("drop db");
    admin
        .batch_execute(&format!("DROP ROLE {cdc_name}"))
        .await
        .expect("drop role");
    let slots_left: i64 = admin
        .query_one(
            "SELECT count(*) FROM pg_replication_slots WHERE slot_name = $1",
            &[&cdc_name],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(slots_left, 0, "zero residue: the slot is gone");
}

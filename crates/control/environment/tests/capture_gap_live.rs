//! Live test of `recover-capture-gap` and `close-capture-gap` (`wamn-59z6`,
//! docs/operations/gcp.md#57-cdc-slot-lost).
//!
//! The test starts its own PostgreSQL 18 server with `wal_level=logical`.
//! Supply a Kubernetes target through `KUBECONFIG` and a stamped `WAMN_RELEASE_CHART`.
//! Configure a private JetStream-enabled TLS broker through `WAMN_NATS_URL`,
//! `WAMN_NATS_TLS_CA`, `WAMN_NATS_TLS_CERT`, and `WAMN_NATS_TLS_KEY`.
//! It records a missing slot, an invalidated slot, and a stream with no CDC event.
//! It checks refusal state and retries Deployment restart without changing the gap time.

use serde_json::{Value, json};
use sqlx::{PgConnection as Client, Row as _};
use wamn_kubernetes::{Target, kubectl};

use async_nats::jetstream;
use chrono::{DateTime, Utc};

use wamn_control_provision::{
    cdc_object_name, project_env_database_name, sql, validate_project_env_cdc,
};
use wamn_environment::environment::capture_gap::{
    CloseCaptureGapRequest, RecoverCaptureGapRequest, close, recover,
};
use wamn_event_wire::{Envelope, Op, msg_id, stream_subjects, subject};
use wamn_registry::sql::{
    stamp_env_policy_sql, upsert_event_reader_sql, upsert_org_sql,
    upsert_project_env_with_policy_sql, upsert_project_sql,
};

const PROJECT: &str = "app";
const ENV: &str = "dev";
const INSTANCE: &str = "k3m9x2p7";
const SYSTEM_DB: &str = "wamn_capture_gap_system";

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect the test server");

    client
}

async fn slot_state(db: &mut Client, slot: &str) -> Option<(Option<String>, Option<String>)> {
    sqlx::query(
        "SELECT confirmed_flush_lsn::text, invalidation_reason::text \
         FROM pg_replication_slots WHERE slot_name = $1",
    )
    .bind(&slot)
    .fetch_optional(&mut *db)
    .await
    .expect("read the slot")
    .map(|row| (row.get(0), row.get(1)))
}

async fn publish(js: &jetstream::Context, subject: String, id: &str, body: Vec<u8>) {
    let mut headers = async_nats::HeaderMap::new();
    headers.insert(async_nats::header::NATS_MESSAGE_ID, id);
    js.publish_with_headers(subject, headers, body.into())
        .await
        .expect("publish")
        .await
        .expect("publish ack");
}

async fn deployment(target: &Target, name: &str) -> Value {
    kubectl::get(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        "deployment",
        name,
    )
    .await
    .unwrap()
    .expect("reader Deployment")
}

async fn gap_times(system: &mut Client) -> Vec<(DateTime<Utc>, Option<DateTime<Utc>>)> {
    sqlx::query_as("SELECT created_at, resync_at FROM registry.capture_gap ORDER BY created_at")
        .fetch_all(system)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY, env:KUBECONFIG, env:WAMN_RELEASE_CHART"]
async fn recover_and_close_record_each_capture_gap() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY, env:KUBECONFIG, env:WAMN_RELEASE_CHART",
    );
    let org = format!("cg{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    let kubeconfig = std::path::PathBuf::from(std::env::var_os("KUBECONFIG").unwrap());
    let target = Target {
        context: kubectl::current_context(&kubeconfig).unwrap(),
        kubeconfig,
        namespace: org.clone(),
    };
    let chart = std::path::PathBuf::from(std::env::var_os("WAMN_RELEASE_CHART").unwrap());
    let image = wamn_release_control::release_chart::image_set(&chart)
        .await
        .unwrap()
        .host;
    let nats = wamn_broker::nats::PlatformNats::from_env().unwrap();
    validate_project_env_cdc(&org, PROJECT, ENV).expect("valid names");
    let postgres = wamn_test_postgres::start(&[("wal_level", "logical")])
        .expect("start a PostgreSQL 18 server with wal_level=logical");
    let database = project_env_database_name(&org, PROJECT, ENV, INSTANCE);
    let slot = cdc_object_name(&org, PROJECT, ENV, INSTANCE);
    let stream = wamn_event_wire::stream_name(&org, PROJECT, ENV);
    let super_url = postgres.database("postgres").unwrap().url().to_owned();
    let system_url = postgres.database(SYSTEM_DB).unwrap().url().to_owned();
    let admin_url = postgres.database(&database).unwrap().url().to_owned();

    let mut server = connect(&super_url).await;
    // CREATE DATABASE refuses a transaction block, so each statement runs alone.
    for statement in [
        "CREATE ROLE wamn_system NOLOGIN NOINHERIT".to_owned(),
        format!("CREATE DATABASE {SYSTEM_DB}"),
        format!("CREATE DATABASE \"{database}\""),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe((&statement).to_string()))
            .execute(&mut server)
            .await
            .map(drop)
            .expect("roles and databases");
    }

    // The registry, owned by wamn_system as the verbs expect.
    let mut system = connect(&system_url).await;
    sqlx::raw_sql(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .execute(&mut system)
        .await
        .map(drop)
        .expect("install the system schema");
    sqlx::raw_sql("SET app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'")
        .execute(&mut system)
        .await
        .map(drop)
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe((upsert_org_sql()).to_string()))
        .bind(&org)
        .bind(&"pooled")
        .bind(&"wamn-pg")
        .execute(&mut system)
        .await
        .map(|result| result.rows_affected())
        .expect("org row");
    sqlx::query(sqlx::AssertSqlSafe((upsert_project_sql()).to_string()))
        .bind(&org)
        .bind(&PROJECT)
        .execute(&mut system)
        .await
        .map(|result| result.rows_affected())
        .expect("project row");
    sqlx::query(sqlx::AssertSqlSafe((stamp_env_policy_sql()).to_string()))
        .bind(&org)
        .bind(&ENV)
        .bind(&r#"{"type":"pool"}"#)
        .bind(&0i32)
        .bind(&1i32)
        .bind(&"1Gi")
        .bind(&"250m")
        .bind(&"256Mi")
        .bind(&"postgres:18")
        .bind(&"")
        .bind(&"")
        .bind(&"off")
        .bind(&"standard")
        .execute(&mut system)
        .await
        .map(|result| result.rows_affected())
        .expect("env-policy row");
    sqlx::query(sqlx::AssertSqlSafe(
        (upsert_project_env_with_policy_sql()).to_string(),
    ))
    .bind(&org)
    .bind(&PROJECT)
    .bind(&ENV)
    .bind(&"wamn-db-cg0--dev--app")
    .bind(&None::<&str>)
    .bind(&INSTANCE)
    .bind(&false)
    .bind(&ENV)
    .bind(&Option::<String>::None)
    .execute(&mut system)
    .await
    .map(|result| result.rows_affected())
    .expect("project-env row");
    sqlx::query(sqlx::AssertSqlSafe((upsert_event_reader_sql()).to_string()))
        .bind(&org)
        .bind(&PROJECT)
        .bind(&ENV)
        .bind(&slot)
        .bind(&slot)
        .bind(&stream)
        .bind(&"wamn-cdc-cg0--dev--app")
        .bind(&None::<&str>)
        .bind(&true)
        .bind(&vec!["app"])
        .execute(&mut system)
        .await
        .map(|result| result.rows_affected())
        .expect("event-reader row");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("ALTER TABLE registry.environment_projects OWNER TO wamn_system; \
             ALTER TABLE registry.event_readers OWNER TO wamn_system; \
             ALTER TABLE registry.capture_gap OWNER TO wamn_system")
            .to_string(),
    ))
    .execute(&mut system)
    .await
    .map(drop)
    .expect("registry owner");
    let registered_at: DateTime<Utc> = sqlx::query("SELECT created_at FROM registry.event_readers")
        .fetch_one(&mut system)
        .await
        .unwrap()
        .get(0);

    let mut project_db = connect(&admin_url).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::create_failover_slot_sql(&slot)).to_string(),
    ))
    .execute(&mut project_db)
    .await
    .map(drop)
    .expect("the healthy slot");

    // The source stream: one CDC event, then a newer derived event.
    let js = jetstream::new(nats.connect().await.expect("connect NATS"));
    let _ = js.delete_stream(&stream).await;
    js.create_stream(jetstream::stream::Config {
        name: stream.clone(),
        subjects: vec![stream_subjects(&org, PROJECT, ENV)],
        ..Default::default()
    })
    .await
    .expect("source stream");
    let cdc_lsn = 0x0000_0001_0000_0010_u64;
    let commit_ts: DateTime<Utc> = "2026-09-29T12:00:00Z".parse().unwrap();
    let envelope = Envelope {
        op: Op::Insert,
        old: None,
        new: Some(serde_json::Map::new()),
        package_id: "app".to_owned(),
        entity: "widget".to_owned(),
        table: "widgets".to_owned(),
        identity: wamn_event_wire::CdcIdentity {
            system_id: 1,
            timeline: 1,
            commit_lsn: cdc_lsn,
            row_ordinal: 0,
        },
        txid: 7,
        commit_ts,
        causation: None,
    };
    publish(
        &js,
        subject(&org, PROJECT, ENV, "widget", Op::Insert),
        &msg_id(
            PROJECT,
            ENV,
            &wamn_event_wire::CdcIdentity {
                system_id: 1,
                timeline: 1,
                commit_lsn: cdc_lsn,
                row_ordinal: 0,
            },
        ),
        serde_json::to_vec(&wamn_event_wire::SourceEvent::Cdc(envelope)).unwrap(),
    )
    .await;
    publish(
        &js,
        subject(&org, PROJECT, ENV, "widget", Op::Update),
        "derived:00ff",
        b"{}".to_vec(),
    )
    .await;

    let recovery_request = |admin: &str| RecoverCaptureGapRequest {
        org: org.clone(),
        project: PROJECT.to_owned(),
        env: ENV.to_owned(),
        system_database_url: system_url.clone(),
        admin_database_url: admin.to_owned(),
        nats: nats.clone(),
    };
    let close_request = CloseCaptureGapRequest {
        org: org.clone(),
        project: PROJECT.to_owned(),
        env: ENV.to_owned(),
        system_database_url: system_url.clone(),
    };

    let triple = wamn_registry::Triple::new(&org, ENV, PROJECT);
    let name = wamn_control_provision::project_env_cdc_secret_name(&org, PROJECT, ENV);
    kubectl::apply(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &json!({"apiVersion":"v1", "kind":"Namespace", "metadata":{"name":org}}),
    )
    .await
    .unwrap();
    let mut reader = wamn_events::lifecycle::reader_deployment(&triple, &target.namespace, &image);
    // An idle process exposes Deployment stop/start independently of WAL streaming.
    reader["spec"]["template"]["spec"] = json!({
        "automountServiceAccountToken": false,
        "containers":[{"name":"reader", "image":image, "command":["sleep", "infinity"]}]
    });
    kubectl::apply(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &reader,
    )
    .await
    .unwrap();
    wamn_events::lifecycle::start_reader(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &triple,
    )
    .await
    .unwrap();

    // Refusals preserve the slot, gap records and running Deployment.
    let before_slot = slot_state(&mut project_db, &slot).await;
    let before_gaps = gap_times(&mut system).await;
    assert!(close(&target, &close_request).await.is_err());
    assert_eq!(gap_times(&mut system).await, before_gaps);
    assert_eq!(slot_state(&mut project_db, &slot).await, before_slot);
    assert!(
        recover(&target, &recovery_request(&admin_url))
            .await
            .is_err()
    );
    assert_eq!(gap_times(&mut system).await, before_gaps);
    assert_eq!(slot_state(&mut project_db, &slot).await, before_slot);
    assert!(
        recover(&target, &recovery_request(&system_url))
            .await
            .is_err()
    );
    assert_eq!(gap_times(&mut system).await, before_gaps);
    assert_eq!(slot_state(&mut project_db, &slot).await, before_slot);
    assert_eq!(deployment(&target, &name).await["spec"]["replicas"], 1);

    // A missing slot: the start is the CDC event, and the derived event is skipped.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::drop_replication_slot_sql(&slot)).to_string(),
    ))
    .execute(&mut project_db)
    .await
    .map(drop)
    .expect("lose the slot");
    let row = recover(&target, &recovery_request(&admin_url))
        .await
        .expect("recover a missing slot");
    assert_eq!(row.reason, "missing");
    assert_eq!(row.start_lsn.as_deref(), Some("1/10"));
    assert_eq!(row.start_at, commit_ts);
    let (confirmed, invalidation) = slot_state(&mut project_db, &slot)
        .await
        .expect("the slot exists again");
    assert_eq!(confirmed.as_deref(), Some(row.end_lsn.as_str()));
    assert_eq!(invalidation, None);
    let stored: (String, Option<String>, String, bool) = {
        let r = sqlx::query(
            "SELECT slot, start_lsn::text, end_lsn::text, resync_at IS NULL \
                 FROM registry.capture_gap",
        )
        .fetch_one(&mut system)
        .await
        .unwrap();
        (r.get(0), r.get(1), r.get(2), r.get(3))
    };
    assert_eq!(
        stored,
        (
            slot.clone(),
            Some("1/10".to_owned()),
            row.end_lsn.clone(),
            true
        )
    );
    assert_eq!(deployment(&target, &name).await["spec"]["replicas"], 0);
    let selector = reader["spec"]["selector"]["matchLabels"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(label, value)| format!("{label}={}", value.as_str().unwrap()))
        .collect::<Vec<_>>()
        .join(",");
    let pods: Value = serde_json::from_str(
        &kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            &target.namespace,
            &["get", "pods", "-l", &selector, "-o", "json"],
            None,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(pods["items"].as_array().unwrap().is_empty());
    assert!(
        wamn_events::capture_gap::require_resynchronized(&system_url, &triple)
            .await
            .is_err()
    );
    // A real scaling failure occurs after resynchronization commits.
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &["delete", "deployment", &name, "--wait=true"],
        None,
    )
    .await
    .unwrap();
    assert!(close(&target, &close_request).await.is_err());
    let recorded = gap_times(&mut system).await;
    let resync_at = recorded
        .last()
        .unwrap()
        .1
        .expect("resynchronization committed");
    reader["spec"]["replicas"] = 0.into();
    kubectl::apply(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &reader,
    )
    .await
    .unwrap();
    let closed = close(&target, &close_request)
        .await
        .expect("retry the reader restart");
    assert_eq!(closed.resync_at, resync_at);
    assert_eq!(gap_times(&mut system).await, recorded);
    assert_eq!(
        deployment(&target, &name).await["status"]["readyReplicas"],
        1
    );
    wamn_events::capture_gap::require_resynchronized(&system_url, &triple)
        .await
        .unwrap();
    let actor = wamn_control_provision::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    wamn_test_infrastructure::record_history::assert_stamp(
        &mut system,
        "registry.capture_gap",
        "true",
        &actor,
        &actor,
    )
    .await;
    assert_eq!(closed.created_at, row.created_at);
    let retried = close(&target, &close_request).await.unwrap();
    assert_eq!(retried.resync_at, resync_at);
    assert_eq!(gap_times(&mut system).await, recorded);

    // An invalidated slot: the start is its confirmed position.
    for statement in [
        "ALTER SYSTEM SET max_slot_wal_keep_size = '1MB'",
        "SELECT pg_reload_conf()",
        "CREATE TABLE public.capture_gap_wal (id bigint PRIMARY KEY, pad text)",
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe((statement).to_string()))
            .execute(&mut server)
            .await
            .map(drop)
            .expect("a 1 MB slot limit");
    }
    let mut lost = None;
    for round in 0..20i64 {
        sqlx::query(
            "INSERT INTO public.capture_gap_wal \
                 SELECT g, repeat('x', 1000) \
                 FROM generate_series($1::bigint * 10000 + 1, $1::bigint * 10000 + 10000) g",
        )
        .bind(&round)
        .execute(&mut server)
        .await
        .map(|result| result.rows_affected())
        .expect("write WAL past the limit");
        for statement in ["SELECT pg_switch_wal()", "CHECKPOINT"] {
            sqlx::raw_sql(sqlx::AssertSqlSafe((statement).to_string()))
                .execute(&mut server)
                .await
                .map(drop)
                .expect("switch and checkpoint");
        }
        if let Some((confirmed, Some(reason))) = slot_state(&mut project_db, &slot).await {
            lost = Some((confirmed, reason));
            break;
        }
    }
    let (lost_confirmed, lost_reason) = lost.expect("the slot was never invalidated");
    let row = recover(&target, &recovery_request(&admin_url))
        .await
        .expect("recover an invalidated slot");
    assert_eq!(row.reason, lost_reason);
    assert_eq!(row.start_lsn, lost_confirmed);
    assert_eq!(row.start_at, commit_ts);
    for statement in [
        "ALTER SYSTEM RESET max_slot_wal_keep_size",
        "SELECT pg_reload_conf()",
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe((statement).to_string()))
            .execute(&mut server)
            .await
            .map(drop)
            .expect("restore the slot limit");
    }
    close(&target, &close_request)
        .await
        .expect("close the second gap");

    // A stream with no CDC event: the start is the registration.
    js.get_stream(&stream)
        .await
        .unwrap()
        .purge()
        .await
        .expect("purge the stream");
    publish(
        &js,
        subject(&org, PROJECT, ENV, "widget", Op::Delete),
        "derived:0100",
        b"{}".to_vec(),
    )
    .await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::drop_replication_slot_sql(&slot)).to_string(),
    ))
    .execute(&mut project_db)
    .await
    .map(drop)
    .expect("lose the slot");
    let row = recover(&target, &recovery_request(&admin_url))
        .await
        .expect("recover with no CDC event");
    assert_eq!(row.reason, "missing");
    assert_eq!(row.start_lsn, None);
    assert_eq!(row.start_at, registered_at);
    let gaps: i64 = sqlx::query("SELECT count(*) FROM registry.capture_gap")
        .fetch_one(&mut system)
        .await
        .unwrap()
        .get(0);
    assert_eq!(gaps, 3, "one row per gap");

    let _ = js.delete_stream(&stream).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::drop_replication_slot_sql(&slot)).to_string(),
    ))
    .execute(&mut project_db)
    .await
    .map(drop)
    .expect("drop the slot");
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &["delete", "namespace", &target.namespace, "--wait=true"],
        None,
    )
    .await
    .unwrap();
}

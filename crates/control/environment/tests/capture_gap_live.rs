//! Live test of `recover-capture-gap` and `close-capture-gap` (`wamn-59z6`,
//! docs/operations/gcp.md#57-cdc-slot-lost).
//!
//! The test starts its own PostgreSQL 18 server with `wal_level=logical`.
//! Configure a private JetStream-enabled TLS broker through `WAMN_NATS_URL`,
//! `WAMN_NATS_TLS_CA`, `WAMN_NATS_TLS_CERT`, and `WAMN_NATS_TLS_KEY`.
//! It records a missing slot, an invalidated slot, and a stream with no CDC event.
//! It drives every refusal except the active slot, which needs a streaming reader.

use sqlx::{Connection as _, Row as _};

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use sqlx::{Connection as _, Row as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

use async_nats::jetstream;
use chrono::{DateTime, Utc};

use wamn_control_provision::{
    cdc_object_name, project_env_database_name, sql, validate_project_env_cdc,
};
use wamn_environment::capture_gap::{
    CloseCaptureGapRequest, RecoverCaptureGapRequest, close_capture_gap, recover_capture_gap,
};
use wamn_event_wire::{Envelope, Op, msg_id, stream_subjects, subject};
use wamn_registry::sql::{
    stamp_env_policy_sql, upsert_event_reader_sql, upsert_org_sql,
    upsert_project_env_with_policy_sql, upsert_project_sql,
};

const ORG: &str = "cg0";
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

fn refusal(result: anyhow::Result<impl std::fmt::Debug>) -> String {
    format!("{:#}", result.expect_err("the verb must refuse"))
}

#[tokio::test]
#[ignore = "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY"]
async fn recover_and_close_record_each_capture_gap() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY",
    );
    let nats = wamn_broker::nats::PlatformNats::from_env().unwrap();
    validate_project_env_cdc(ORG, PROJECT, ENV).expect("valid names");
    let postgres = wamn_test_postgres::start(&[("wal_level", "logical")])
        .expect("start a PostgreSQL 18 server with wal_level=logical");
    let mut database = project_env_database_name(ORG, PROJECT, ENV, INSTANCE);
    let slot = cdc_object_name(ORG, PROJECT, ENV, INSTANCE);
    let stream = wamn_event_wire::stream_name(ORG, PROJECT, ENV);
    let super_url = postgres.database("postgres").unwrap().url().to_owned();
    let system_url = postgres.database(SYSTEM_DB).unwrap().url().to_owned();
    let admin_url = postgres.database(&mut database).unwrap().url().to_owned();

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
        .bind(&ORG)
        .bind(&"pooled")
        .bind(&"wamn-pg")
        .execute(&mut system)
        .await
        .map(|result| result.rows_affected())
        .expect("org row");
    sqlx::query(sqlx::AssertSqlSafe((upsert_project_sql()).to_string()))
        .bind(&ORG)
        .bind(&PROJECT)
        .execute(&mut system)
        .await
        .map(|result| result.rows_affected())
        .expect("project row");
    sqlx::query(sqlx::AssertSqlSafe((stamp_env_policy_sql()).to_string()))
        .bind(&ORG)
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
    .bind(&ORG)
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
        .bind(&ORG)
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
        subjects: vec![stream_subjects(ORG, PROJECT, ENV)],
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
        lsn: cdc_lsn,
        txid: 7,
        commit_ts,
        causation: None,
    };
    publish(
        &js,
        subject(ORG, PROJECT, ENV, "widget", Op::Insert),
        &msg_id(PROJECT, ENV, cdc_lsn),
        serde_json::to_vec(&wamn_event_wire::SourceEvent::Cdc(envelope)).unwrap(),
    )
    .await;
    publish(
        &js,
        subject(ORG, PROJECT, ENV, "widget", Op::Update),
        "derived:00ff",
        b"{}".to_vec(),
    )
    .await;

    let directory = tempfile_directory();
    let password_file = directory.join("observer-password");
    std::fs::write(&password_file, "observer").unwrap();
    std::fs::set_permissions(&password_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let recover = |admin: &str| RecoverCaptureGapRequest {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: ENV.to_owned(),
        system_database_url: system_url.clone(),
        admin_database_url: admin.to_owned(),
        nats: nats.clone(),
    };
    let close = CloseCaptureGapRequest {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: ENV.to_owned(),
        system_database_url: system_url.clone(),
    };

    // Refusals before any gap.
    let message = refusal(close_capture_gap(&close).await);
    assert!(message.contains("has no capture gap row"), "{message}");
    let message = refusal(recover_capture_gap(&recover(&admin_url)).await);
    assert!(message.contains("is healthy"), "{message}");
    let message = refusal(recover_capture_gap(&recover(&system_url)).await);
    assert!(
        message.contains("not the project-env database"),
        "{message}"
    );

    // A missing slot: the start is the CDC event, and the derived event is skipped.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::drop_replication_slot_sql(&slot)).to_string(),
    ))
    .execute(&mut project_db)
    .await
    .map(drop)
    .expect("lose the slot");
    let row = recover_capture_gap(&recover(&admin_url))
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
    let closed = close_capture_gap(&close).await.expect("close the gap");
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
    let message = refusal(close_capture_gap(&close).await);
    assert!(message.contains("already has resync_at"), "{message}");

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
    let row = recover_capture_gap(&recover(&admin_url))
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
    close_capture_gap(&close)
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
        subject(ORG, PROJECT, ENV, "widget", Op::Delete),
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
    let row = recover_capture_gap(&recover(&admin_url))
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
    std::fs::remove_file(&password_file).unwrap();
    std::fs::remove_dir(&directory).unwrap();
}

fn tempfile_directory() -> std::path::PathBuf {
    let directory =
        std::env::temp_dir().join(format!("wamn-capture-gap-live-{}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("private test directory");
    directory
}

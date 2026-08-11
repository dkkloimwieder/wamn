//! Optional PostgreSQL proof for immutable inline test-set storage.
//!
//! Set `WAMN_TEST_SET_STORE_PG_URL` to a superuser URL for a disposable
//! database. The test resets one dedicated schema and skips cleanly when the
//! URL is absent.

use tokio_postgres::{Client, NoTls};
use wamn_authoring_model::TestSetInput;
use wamn_scenario_worker::store::test_sets::store_test_set;
use wamn_schema_control::{BareSchemaName, rewrite_schema};

const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const FLOW_TESTS_SQL: &str = include_str!("../../../deploy/sql/flow-tests.sql");
const SCHEMA: &str = "test_set_store_live";
const TENANT: &str = "test-set-store-tenant";

async fn connect(url: &str) -> (Client, tokio::task::JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    (client, task)
}

fn schema_sql(record: &str) -> String {
    let schema = BareSchemaName::new(SCHEMA).expect("live-test schema name is valid");
    rewrite_schema(record, &schema)
}

fn input() -> TestSetInput {
    TestSetInput {
        definition: concat!(
            "{\"schema-version\":\"0.1\",\"cases\":[",
            "{\"case-id\":\"one\",\"input\":{},",
            "\"expect\":[{\"run-terminal-outcome\":{\"status\":\"completed\"}}]}",
            "]}"
        )
        .into(),
    }
}

fn assert_database_code(error: &tokio_postgres::Error, expected: &str, context: &str) {
    let actual = error
        .as_db_error()
        .map(|database| database.code().code())
        .unwrap_or("non-database-error");
    assert_eq!(actual, expected, "{context}: {error}");
}

async fn direct_insert(client: &Client, tenant: &str, definition: &str) -> tokio_postgres::Error {
    let exact_bytes = definition.as_bytes();
    client
        .execute(
            "INSERT INTO authoring_test_sets \
               (tenant_id, test_set_hash, schema_version, exact_bytes, byte_length) \
             SELECT $1, 'sha256:' || encode(sha256($2::bytea), 'hex'), \
                    '0.1', $2::bytea, octet_length($2::bytea)",
            &[&tenant, &exact_bytes],
        )
        .await
        .expect_err("invalid self-described schema version must be refused")
}

#[tokio::test]
async fn authoring_test_set_store_enforces_exact_immutable_bytes_on_postgres() {
    let Ok(url) = std::env::var("WAMN_TEST_SET_STORE_PG_URL") else {
        eprintln!(
            "skipping authoring_test_set_store_enforces_exact_immutable_bytes_on_postgres \
             (set WAMN_TEST_SET_STORE_PG_URL to run)"
        );
        return;
    };

    let (client, connection_task) = connect(&url).await;
    client
        .batch_execute(&format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$; \
             DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             CREATE SCHEMA {SCHEMA};"
        ))
        .await
        .expect("reset test-set store schema and roles");
    client
        .batch_execute(&schema_sql(FLOWS_SQL))
        .await
        .expect("apply flow registry prerequisite");
    client
        .batch_execute(&schema_sql(FLOW_TESTS_SQL))
        .await
        .expect("apply authoring test-set DDL");
    client
        .batch_execute(&format!("SET search_path TO {SCHEMA}"))
        .await
        .expect("scope store statements to the test schema");

    for (tenant, definition) in [
        ("missing-version", r#"{"cases":[]}"#),
        ("null-version", r#"{"schema-version":null,"cases":[]}"#),
    ] {
        let error = direct_insert(&client, tenant, definition).await;
        assert_database_code(
            &error,
            "23514",
            "missing or null self-described schema version",
        );
    }

    let input = input();
    let first = store_test_set(&client, TENANT, &input)
        .await
        .expect("store exact test-set bytes");
    let second = store_test_set(&client, TENANT, &input)
        .await
        .expect("repeat exact test-set store");
    assert_eq!(first, second);

    let row = client
        .query_one(
            "SELECT count(*)::bigint, min(byte_length)::int \
               FROM authoring_test_sets \
              WHERE tenant_id = $1 AND test_set_hash = $2",
            &[&TENANT, &first.hash],
        )
        .await
        .expect("count exact test-set rows");
    assert_eq!(row.get::<_, i64>(0), 1, "duplicate store keeps one row");
    assert_eq!(
        row.get::<_, i32>(1) as usize,
        input.definition.len(),
        "stored byte length remains exact"
    );
    let stored_bytes: Vec<u8> = client
        .query_one(
            "SELECT exact_bytes FROM authoring_test_sets \
              WHERE tenant_id = $1 AND test_set_hash = $2",
            &[&TENANT, &first.hash],
        )
        .await
        .expect("read exact test-set bytes")
        .get(0);
    assert_eq!(stored_bytes, input.definition.as_bytes());

    let update_error = client
        .execute(
            "UPDATE authoring_test_sets SET exact_bytes = exact_bytes \
              WHERE tenant_id = $1 AND test_set_hash = $2",
            &[&TENANT, &first.hash],
        )
        .await
        .expect_err("authoring test-set update must be refused");
    assert_database_code(&update_error, "55000", "immutable update");
    let delete_error = client
        .execute(
            "DELETE FROM authoring_test_sets \
              WHERE tenant_id = $1 AND test_set_hash = $2",
            &[&TENANT, &first.hash],
        )
        .await
        .expect_err("authoring test-set delete must be refused");
    assert_database_code(&delete_error, "55000", "immutable delete");

    client
        .batch_execute(&format!("DROP SCHEMA {SCHEMA} CASCADE"))
        .await
        .expect("remove test-set store schema");
    connection_task.abort();
}

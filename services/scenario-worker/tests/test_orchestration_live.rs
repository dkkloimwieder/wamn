//! Optional PostgreSQL 18 proof for durable test-case orchestration.
//!
//! Set `WAMN_TEST_ORCHESTRATION_PG_URL` to a superuser URL for a disposable
//! database. The proof resets dedicated schemas and skips when the URL is absent.

use tokio_postgres::{Client, NoTls};
use wamn_scenario_worker::store::test_orchestration::{
    TestReportReconciliation, TestReportReservation, reconcile_test_report, reserve_test_report,
};
use wamn_schema_control::{BareSchemaName, rewrite_schema};

const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const AUTHORING_TESTS_SQL: &str = include_str!("../../../deploy/sql/authoring-tests.sql");
const SCHEMA: &str = "test_orchestration_live";
const TENANT: &str = "test-orchestration-tenant";

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
    rewrite_schema(
        record,
        &BareSchemaName::new(SCHEMA).expect("live schema name is valid"),
    )
}

fn command_hash(marker: char) -> String {
    format!("sha256:{}", marker.to_string().repeat(64))
}

fn reservation(report_id: &str) -> TestReportReservation {
    TestReportReservation {
        tenant_id: TENANT.into(),
        report_id: report_id.into(),
        command_hash: command_hash('a'),
        validated_draft_id: "validated-draft-a".into(),
        catalog_id: "catalog-a".into(),
        catalog_version: 1,
        case_ids: vec!["case-a".into()],
    }
}

fn assert_database_code(error: &tokio_postgres::Error, expected: &str) {
    assert_eq!(
        error.as_db_error().map(|database| database.code().code()),
        Some(expected),
        "unexpected database error: {error}"
    );
}

#[tokio::test]
async fn durable_test_orchestration_enforces_restart_deadline_and_report_invariants() {
    let Ok(url) = std::env::var("WAMN_TEST_ORCHESTRATION_PG_URL") else {
        eprintln!(
            "skipping durable_test_orchestration_enforces_restart_deadline_and_report_invariants \
             (set WAMN_TEST_ORCHESTRATION_PG_URL to run)"
        );
        return;
    };
    let (mut client, connection_task) = connect(&url).await;
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN; \
               END IF; \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN; \
               END IF; \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN; \
               END IF; \
             END $$;"
        ))
        .await
        .expect("reset orchestration schemas and roles");
    client
        .batch_execute(CATALOG_SQL)
        .await
        .expect("apply catalog prerequisite");
    for record in [RUN_STATE_SQL, FLOWS_SQL, AUTHORING_TESTS_SQL] {
        client
            .batch_execute(&schema_sql(record))
            .await
            .expect("apply orchestration record");
    }
    client
        .batch_execute(&format!(
            "SET search_path TO {SCHEMA}; \
             SELECT set_config('app.tenant', '{TENANT}', false);"
        ))
        .await
        .expect("scope orchestration session");

    let accepted = reserve_test_report(&mut client, &reservation("restart-report"))
        .await
        .expect("reserve accepted report");
    let retried = reserve_test_report(&mut client, &reservation("restart-report"))
        .await
        .expect("retry accepted report");
    assert_eq!(accepted, retried);
    let mappings: Vec<(i32, String)> = client
        .query(
            "SELECT ordinal, run_id FROM authoring_test_case_runs \
              WHERE tenant_id = $1 AND report_id = $2",
            &[&TENANT, &accepted.report_id],
        )
        .await
        .expect("read one normalized mapping")
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(mappings.len(), 1);

    let mutation = client
        .execute(
            "UPDATE authoring_test_case_runs SET run_id = 'replacement' \
              WHERE tenant_id = $1 AND report_id = $2 AND ordinal = 0",
            &[&TENANT, &accepted.report_id],
        )
        .await
        .expect_err("an ordinal's run identity is immutable");
    assert_database_code(&mutation, "55000");
    let premature_deadline = client
        .execute(
            "UPDATE authoring_test_case_runs \
                SET state = 'finalized', passed = false, \
                    failure_kind = 'deadline-exhausted', summary = '{}', \
                    finalized_at = clock_timestamp() \
              WHERE tenant_id = $1 AND report_id = $2 AND ordinal = 0",
            &[&TENANT, &accepted.report_id],
        )
        .await
        .expect_err("a live per-case deadline cannot be fabricated");
    assert_database_code(&premature_deadline, "55000");

    let test_set_relation: Option<String> = client
        .query_one("SELECT to_regclass('authoring_test_sets')::text", &[])
        .await
        .expect("probe the deleted test-set store")
        .get(0);
    assert_eq!(test_set_relation, None);

    // A second accepted report is made immediately deadline-eligible without
    // mutating any reserved identity; reconciliation must fail/finalize it.
    client
        .execute(
            "INSERT INTO authoring_test_run_reservations \
               (tenant_id, report_id, command_hash, validated_draft_id, \
                catalog_id, catalog_version, case_count, created_at, whole_deadline_at) \
             VALUES ($1, 'deadline-report', $2, 'validated-draft-a', \
                     'catalog-a', 1, 1, clock_timestamp() - interval '2 minutes', \
                     clock_timestamp() - interval '1 minute')",
            &[&TENANT, &command_hash('b')],
        )
        .await
        .expect("reserve past whole-set deadline");
    client
        .execute(
            "INSERT INTO authoring_test_case_runs \
               (tenant_id, report_id, ordinal, case_id, run_id, catalog_id, \
                catalog_version, validated_draft_id, case_deadline_at, created_at) \
             VALUES ($1, 'deadline-report', 0, 'case-a', 'deadline-run', \
                     'catalog-a', 1, 'validated-draft-a', \
                     clock_timestamp() - interval '90 seconds', \
                     clock_timestamp() - interval '2 minutes')",
            &[&TENANT],
        )
        .await
        .expect("reserve past per-case deadline");
    assert_eq!(
        reconcile_test_report(&mut client, TENANT, "deadline-report")
            .await
            .expect("reconcile expired report"),
        TestReportReconciliation::Finalized { passed: false }
    );
    let deadline_kind: String = client
        .query_one(
            "SELECT failure_kind FROM authoring_test_case_runs \
              WHERE tenant_id = $1 AND report_id = 'deadline-report'",
            &[&TENANT],
        )
        .await
        .expect("read deadline outcome")
        .get(0);
    assert_eq!(deadline_kind, "deadline-exhausted");

    // Effect uncertainty uses an ordinary run row and never enters operator
    // recovery.
    let plan_bytes = b"{}".as_slice();
    let plan_hash: String = client
        .query_one(
            "SELECT 'sha256:' || encode(sha256($1::bytea), 'hex')",
            &[&plan_bytes],
        )
        .await
        .expect("hash exact plan bytes")
        .get(0);
    client
        .execute(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ($1,'catalog-a',1,'dev','0.1','applied')",
            &[&TENANT],
        )
        .await
        .expect("seed pinned catalog");
    client
        .execute(
            "INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version,members_json) \
             VALUES ($1,'catalog-a',1,'[]')",
            &[&TENANT],
        )
        .await
        .expect("seed pinned release");
    client
        .execute(
            "INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ($1,$2,'0.1',$3,octet_length($3::bytea))",
            &[&TENANT, &plan_hash, &plan_bytes],
        )
        .await
        .expect("seed exact execution bundle");
    let effect_report = reservation("uncertain-report");
    reserve_test_report(&mut client, &effect_report)
        .await
        .expect("reserve uncertain report");
    let run_id: String = client
        .query_one(
            "SELECT run_id FROM authoring_test_case_runs \
              WHERE tenant_id = $1 AND report_id = 'uncertain-report'",
            &[&TENANT],
        )
        .await
        .expect("read uncertain run mapping")
        .get(0);
    client
        .execute(
            "INSERT INTO runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                environment,execution_bundle_hash,status,trigger_source,invocation_context) \
             VALUES ($1,$2,'flow-a',1,'catalog-a',1,'dev',$3, \
                     'effect-uncertain','scenario-draft',jsonb_build_object( \
                         'principal', jsonb_build_object( \
                             'validated-draft-hash', 'validated-draft-a'), \
                         'source', jsonb_build_object( \
                             'report-id', 'uncertain-report', 'case-id', 'case-a'))) ",
            &[&TENANT, &run_id, &plan_hash],
        )
        .await
        .expect("seed effect-uncertain run");
    client
        .execute(
            "INSERT INTO node_runs \
               (tenant_id,run_id,frame_id,current_plan_hash,local_node_id, \
                occurrence,seq,status,error_kind,ended_at) \
             VALUES ($1,$2,0,$3,'write',0,0,'error','terminal',clock_timestamp())",
            &[&TENANT, &run_id, &plan_hash],
        )
        .await
        .expect("seed frame-keyed terminal fact");
    assert_eq!(
        reconcile_test_report(&mut client, TENANT, "uncertain-report")
            .await
            .expect("reconcile effect uncertainty"),
        TestReportReconciliation::Finalized { passed: false }
    );
    assert_eq!(
        reserve_test_report(&mut client, &effect_report)
            .await
            .expect("retry finalized report identity"),
        wamn_scenario_worker::store::test_orchestration::AcceptedTestReport {
            report_id: "uncertain-report".into(),
        }
    );

    let immutable = client
        .execute(
            "UPDATE authoring_test_reports SET passed = true \
              WHERE tenant_id = $1 AND report_id = 'uncertain-report'",
            &[&TENANT],
        )
        .await
        .expect_err("final report is immutable");
    assert_database_code(&immutable, "55000");

    client
        .batch_execute(&format!(
            "DROP SCHEMA {SCHEMA} CASCADE; DROP SCHEMA catalog CASCADE;"
        ))
        .await
        .expect("remove orchestration proof schemas");
    connection_task.abort();
}

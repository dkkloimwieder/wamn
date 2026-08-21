//! PostgreSQL 18 proof for the project-admin effect-uncertain terminalization.
//!
//! Set `WAMN_OPERATOR_TERMINALIZE_PG18_URL` to the superuser URL of a disposable
//! database. The gate is skipped when the variable is absent.

use tokio_postgres::{Client, NoTls};
use wamn_ctl::{reconcile_run_plane, terminalize_effect_uncertain};
use wamn_run_state::operator_action::{OperatorActionBasis, OperatorTerminalizeResult};
use wamn_schema_control::BareSchemaName;

const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../test-support/fixtures/sql/current-database-public-connect.sql");
const SCHEMA: &str = "operator_live";
const HASH: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL 18");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn reset_and_install(client: &Client) -> BareSchemaName {
    client
        .batch_execute(&format!(
            "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $roles$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN NOSUPERUSER NOBYPASSRLS; END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; ELSE \
                 ALTER ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; ELSE \
                 ALTER ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; \
             END $roles$; \
             DO $database$ BEGIN \
               EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM wamn_effect_writer', \
                              current_database()); \
               EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_app', current_database()); \
             END $database$;"
        ))
        .await
        .expect("reset terminalization schema");
    let schema = BareSchemaName::new(SCHEMA).expect("static schema name");
    reconcile_run_plane::reconcile(client, &schema, true)
        .await
        .expect("install current run plane");
    client
        .batch_execute(&format!(
            "INSERT INTO {SCHEMA}.environment_policies \
               (tenant_id,expected_environment,durability_class) \
             VALUES ('t1','dev','standard'); \
             INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ('t1','cat',1,'dev','0.1','applied'); \
             INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ('t1','{HASH}','0.1',''::bytea,0); \
             INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version) \
             VALUES ('t1','cat',1);"
        ))
        .await
        .expect("seed admission pin parents");
    schema
}

async fn seed_run(client: &Client, run: &str, status: &str, fail_kind: Option<&str>) {
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                    environment,execution_bundle_hash,status,capture_mode,input_json, \
                    result_json,state_json,caller_outcome_kind,caller_outcome_json, \
                    caller_release_node_id,caller_outcome_hash,caller_released_at,fail_kind, \
                    trigger_source) \
                 VALUES ('t1',$1,'flow',1,'cat',1,'dev','{HASH}',$2,'full', \
                         '{{\"secret\":\"scrubbed\"}}', '{{\"result\":1}}', \
                         '{{\"pc\":7}}','failed','{{\"caller\":true}}','release-node', \
                         'sha256:caller','2026-01-01 UTC',$3,'scenario-draft')"
            ),
            &[&run, &status, &fail_kind],
        )
        .await
        .expect("seed run");
}

async fn seed_attempt(client: &Client, run: &str, node: &str, occurrence: i32) {
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempts \
                   (tenant_id,run_id,root_plan_hash,current_plan_hash,frame_id,local_node_id, \
                    source_artifact_hash,requirement_name,occurrence,seq,generation_fact_kind, \
                    attempt_deadline_at,attempt_input_ref) \
                 VALUES ('t1',$1,'{HASH}','{HASH}',0,$2,'{HASH}','database',$3,$3, \
                         'not-required',now()+interval '5 minutes','sha256:input')"
            ),
            &[&run, &node, &occurrence],
        )
        .await
        .expect("seed immutable attempt");
}

async fn seed_started_node(client: &Client, run: &str, node: &str, occurrence: i32) {
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.node_runs \
                   (tenant_id,run_id,frame_id,current_plan_hash,local_node_id,occurrence,seq, \
                    status,input_json,output_json,output_size,payload_hash) \
                 VALUES ('t1',$1,0,'{HASH}',$2,$3::int,$3::bigint,'started', \
                         '{{\"in\":1}}','{{\"out\":2}}',9,'sha256:payload')"
            ),
            &[&run, &node, &occurrence],
        )
        .await
        .expect("seed started node");
}

fn request<'a>(
    run: &'a str,
    correlation: &'a str,
) -> terminalize_effect_uncertain::TerminalizeEffectUncertain<'a> {
    terminalize_effect_uncertain::TerminalizeEffectUncertain {
        tenant: "t1",
        run,
        basis: OperatorActionBasis::ExternalEvidence,
        evidence_ref: "case:operator-live",
        correlation_id: correlation,
    }
}

#[tokio::test]
async fn terminalize_effect_uncertain_is_atomic_exact_and_authority_closed_live() {
    let Some(url) = std::env::var("WAMN_OPERATOR_TERMINALIZE_PG18_URL").ok() else {
        eprintln!(
            "WAMN_OPERATOR_TERMINALIZE_PG18_URL unset — skipping operator terminalization gate"
        );
        return;
    };
    let mut client = connect(&url).await;
    let schema = reset_and_install(&client).await;

    seed_run(
        &client,
        "concurrent",
        "effect-uncertain",
        Some("effect-uncertain"),
    )
    .await;
    seed_attempt(&client, "concurrent", "effect", 0).await;
    let holder = connect(&url).await;
    holder
        .batch_execute(&format!(
            "BEGIN; SELECT 1 FROM {SCHEMA}.runs \
              WHERE tenant_id='t1' AND run_id='concurrent' FOR UPDATE;"
        ))
        .await
        .expect("hold run row lock");
    let worker_url = url.clone();
    let worker_schema = schema.clone();
    let blocked_first = tokio::spawn(async move {
        let mut worker = connect(&worker_url).await;
        terminalize_effect_uncertain::terminalize(
            &mut worker,
            &worker_schema,
            &request("concurrent", "corr-concurrent"),
        )
        .await
    });
    let worker_url = url.clone();
    let worker_schema = schema.clone();
    let blocked_retry = tokio::spawn(async move {
        let mut worker = connect(&worker_url).await;
        terminalize_effect_uncertain::terminalize(
            &mut worker,
            &worker_schema,
            &request("concurrent", "corr-concurrent"),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !blocked_first.is_finished() && !blocked_retry.is_finished(),
        "concurrent terminalizations wait on the run lock"
    );
    holder
        .batch_execute("ROLLBACK")
        .await
        .expect("release run row lock");
    let concurrent_results = [
        blocked_first
            .await
            .expect("join first blocked terminalization")
            .expect("first terminalization after lock release"),
        blocked_retry
            .await
            .expect("join blocked retry")
            .expect("retry after lock release"),
    ];
    assert!(
        concurrent_results.contains(&OperatorTerminalizeResult::Terminalized)
            && concurrent_results.contains(&OperatorTerminalizeResult::IdenticalRetry),
        "concurrent exact retry has one winner and one exact retry: {concurrent_results:?}"
    );

    seed_run(
        &client,
        "concurrent-conflict",
        "effect-uncertain",
        Some("effect-uncertain"),
    )
    .await;
    seed_attempt(&client, "concurrent-conflict", "effect", 0).await;
    holder
        .batch_execute(&format!(
            "BEGIN; SELECT 1 FROM {SCHEMA}.runs \
              WHERE tenant_id='t1' AND run_id='concurrent-conflict' FOR UPDATE;"
        ))
        .await
        .expect("hold conflicting run row lock");
    let worker_url = url.clone();
    let worker_schema = schema.clone();
    let blocked_a = tokio::spawn(async move {
        let mut worker = connect(&worker_url).await;
        terminalize_effect_uncertain::terminalize(
            &mut worker,
            &worker_schema,
            &request("concurrent-conflict", "corr-concurrent-a"),
        )
        .await
    });
    let worker_url = url.clone();
    let worker_schema = schema.clone();
    let blocked_b = tokio::spawn(async move {
        let mut worker = connect(&worker_url).await;
        terminalize_effect_uncertain::terminalize(
            &mut worker,
            &worker_schema,
            &request("concurrent-conflict", "corr-concurrent-b"),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !blocked_a.is_finished() && !blocked_b.is_finished(),
        "divergent terminalizations wait on the run lock"
    );
    holder
        .batch_execute("ROLLBACK")
        .await
        .expect("release conflicting run row lock");
    let conflict_results = [
        blocked_a
            .await
            .expect("join first divergent terminalization")
            .expect("first divergent terminalization after lock release"),
        blocked_b
            .await
            .expect("join second divergent terminalization")
            .expect("second divergent terminalization after lock release"),
    ];
    assert!(
        conflict_results.contains(&OperatorTerminalizeResult::Terminalized)
            && conflict_results.contains(&OperatorTerminalizeResult::CorrelationConflict),
        "concurrent divergent requests have one winner and one conflict: {conflict_results:?}"
    );

    seed_run(
        &client,
        "one-node",
        "effect-uncertain",
        Some("effect-uncertain"),
    )
    .await;
    seed_started_node(&client, "one-node", "effect", 0).await;
    seed_attempt(&client, "one-node", "effect", 0).await;
    let run_before: String = client
        .query_one(
            &format!(
                "SELECT (to_jsonb(r) - ARRAY['status','terminal_reason','updated_at']::text[])::text \
                   FROM {SCHEMA}.runs r WHERE tenant_id='t1' AND run_id='one-node'"
            ),
            &[],
        )
        .await
        .expect("snapshot run evidence")
        .get(0);
    let attempt_before: String = client
        .query_one(
            &format!(
                "SELECT to_jsonb(a)::text FROM {SCHEMA}.effect_attempts a \
                  WHERE tenant_id='t1' AND run_id='one-node'"
            ),
            &[],
        )
        .await
        .expect("snapshot attempt evidence")
        .get(0);

    assert_eq!(
        terminalize_effect_uncertain::terminalize(
            &mut client,
            &schema,
            &request("one-node", "corr-one"),
        )
        .await
        .expect("terminalize one node"),
        OperatorTerminalizeResult::Terminalized
    );
    let run_after = client
        .query_one(
            &format!(
                "SELECT status,fail_kind,terminal_reason, \
                        (to_jsonb(r) - ARRAY['status','terminal_reason','updated_at']::text[])::text \
                   FROM {SCHEMA}.runs r WHERE tenant_id='t1' AND run_id='one-node'"
            ),
            &[],
        )
        .await
        .expect("read terminal run");
    assert_eq!(run_after.get::<_, String>(0), "failed");
    assert_eq!(
        run_after.get::<_, Option<String>>(1).as_deref(),
        Some("effect-uncertain")
    );
    assert_eq!(
        run_after.get::<_, Option<String>>(2).as_deref(),
        Some("operator-terminalized-effect-uncertain")
    );
    assert_eq!(run_after.get::<_, String>(3), run_before);
    let attempt_after: String = client
        .query_one(
            &format!(
                "SELECT to_jsonb(a)::text FROM {SCHEMA}.effect_attempts a \
                  WHERE tenant_id='t1' AND run_id='one-node'"
            ),
            &[],
        )
        .await
        .expect("read preserved attempt evidence")
        .get(0);
    assert_eq!(attempt_after, attempt_before);
    let node = client
        .query_one(
            &format!(
                "SELECT status,error_kind,error_detail->>'code',ended_at IS NOT NULL, \
                        input_json::text,output_json::text,output_size,payload_hash \
                   FROM {SCHEMA}.node_runs WHERE tenant_id='t1' AND run_id='one-node'"
            ),
            &[],
        )
        .await
        .expect("read terminal node");
    assert_eq!(node.get::<_, String>(0), "error");
    assert_eq!(
        node.get::<_, Option<String>>(1).as_deref(),
        Some("terminal")
    );
    assert_eq!(
        node.get::<_, Option<String>>(2).as_deref(),
        Some("effect-uncertain")
    );
    assert!(node.get::<_, bool>(3));
    assert_eq!(
        node.get::<_, Option<String>>(4).as_deref(),
        Some("{\"in\": 1}")
    );
    assert_eq!(
        node.get::<_, Option<String>>(5).as_deref(),
        Some("{\"out\": 2}")
    );
    assert_eq!(node.get::<_, Option<i64>>(6), Some(9));
    assert_eq!(
        node.get::<_, Option<String>>(7).as_deref(),
        Some("sha256:payload")
    );

    let action = client
        .query_one(
            &format!(
                "SELECT correlation_id,action_kind,basis,evidence_ref,principal=session_user, \
                        principal_kind,prior_run_status,prior_started_node_frame_id, \
                        prior_started_node_local_node_id,prior_started_node_occurrence, \
                        prior_started_node_status \
                   FROM {SCHEMA}.operator_run_actions \
                  WHERE tenant_id='t1' AND run_id='one-node'"
            ),
            &[],
        )
        .await
        .expect("read immutable action");
    assert_eq!(action.get::<_, String>(0), "corr-one");
    assert_eq!(action.get::<_, String>(1), "terminalize-effect-uncertain");
    assert_eq!(action.get::<_, String>(2), "external-evidence");
    assert_eq!(action.get::<_, String>(3), "case:operator-live");
    assert!(action.get::<_, bool>(4));
    assert_eq!(action.get::<_, String>(5), "database-role");
    assert_eq!(action.get::<_, String>(6), "effect-uncertain");
    assert_eq!(action.get::<_, Option<i64>>(7), Some(0));
    assert_eq!(
        action.get::<_, Option<String>>(8).as_deref(),
        Some("effect")
    );
    assert_eq!(action.get::<_, Option<i32>>(9), Some(0));
    assert_eq!(
        action.get::<_, Option<String>>(10).as_deref(),
        Some("started")
    );

    assert_eq!(
        terminalize_effect_uncertain::terminalize(
            &mut client,
            &schema,
            &request("one-node", "corr-one"),
        )
        .await
        .expect("identical retry"),
        OperatorTerminalizeResult::IdenticalRetry
    );
    assert_eq!(
        terminalize_effect_uncertain::terminalize(
            &mut client,
            &schema,
            &request("one-node", "different-correlation"),
        )
        .await
        .expect("divergent run reuse"),
        OperatorTerminalizeResult::CorrelationConflict
    );

    seed_run(
        &client,
        "zero-node",
        "effect-uncertain",
        Some("effect-uncertain"),
    )
    .await;
    seed_attempt(&client, "zero-node", "effect", 0).await;
    assert_eq!(
        terminalize_effect_uncertain::terminalize(
            &mut client,
            &schema,
            &request("zero-node", "corr-zero"),
        )
        .await
        .expect("zero-node terminalization"),
        OperatorTerminalizeResult::Terminalized
    );

    seed_run(
        &client,
        "two-node",
        "effect-uncertain",
        Some("effect-uncertain"),
    )
    .await;
    for (node, occurrence) in [("effect-a", 0), ("effect-b", 1)] {
        seed_started_node(&client, "two-node", node, occurrence).await;
        seed_attempt(&client, "two-node", node, occurrence).await;
    }
    assert_eq!(
        terminalize_effect_uncertain::terminalize(
            &mut client,
            &schema,
            &request("two-node", "corr-two"),
        )
        .await
        .expect("cardinality refusal"),
        OperatorTerminalizeResult::RunStateInvariant
    );
    let unchanged_row = client
        .query_one(
            &format!(
                "SELECT status,(SELECT count(*) FROM {SCHEMA}.operator_run_actions \
                  WHERE run_id='two-node') FROM {SCHEMA}.runs \
                  WHERE tenant_id='t1' AND run_id='two-node'"
            ),
            &[],
        )
        .await
        .expect("cardinality refusal is atomic");
    let unchanged = (
        unchanged_row.get::<_, String>(0),
        unchanged_row.get::<_, i64>(1),
    );
    assert_eq!(unchanged, ("effect-uncertain".to_string(), 0));

    for role in ["wamn_app", "wamn_scenario_author", "wamn_effect_writer"] {
        let privileges: Vec<bool> = client
            .query_one(
                "SELECT ARRAY[has_table_privilege($1,$2,'INSERT'), \
                              has_table_privilege($1,$2,'UPDATE'), \
                              has_table_privilege($1,$2,'DELETE')]",
                &[&role, &format!("{SCHEMA}.operator_run_actions")],
            )
            .await
            .expect("probe action DML authority")
            .get(0);
        assert_eq!(
            privileges,
            [false, false, false],
            "{role} has no action DML"
        );
    }
    for verb in ["UPDATE", "DELETE"] {
        let sql = if verb == "UPDATE" {
            format!(
                "UPDATE {SCHEMA}.operator_run_actions SET evidence_ref=evidence_ref WHERE run_id='one-node'"
            )
        } else {
            format!("DELETE FROM {SCHEMA}.operator_run_actions WHERE run_id='one-node'")
        };
        let error = client
            .execute(&sql, &[])
            .await
            .expect_err("operator action is immutable even to its owner");
        assert_eq!(
            error.as_db_error().map(|database| database.message()),
            Some("operator-run-action-immutable")
        );
    }
}

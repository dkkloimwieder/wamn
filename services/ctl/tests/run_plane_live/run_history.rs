use super::{
    CATALOG_SCHEMA_SQL, Client, RUN_QUEUE_SQL, RUN_STATE_SQL, RunPlaneActionKind, SCHEMA,
    assert_db_code, assert_db_code_in_chain, column_exists, connect, indexdef,
    install_current_run_plane, reconcile_run_plane, reset, rewrite_schema, schema,
    seed_run_admission_facts, support, table_exists,
};

async fn install_legacy_child_run_state(su: &Client) {
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs \
           ADD COLUMN parent_run_id text, \
           ADD COLUMN parent_node_id text, \
           ADD COLUMN parent_occurrence int, \
           ADD COLUMN invoke_depth int NOT NULL DEFAULT 0, \
           ADD COLUMN invoke_root_run_id text, \
           ADD COLUMN waiting_child_run_id text, \
           ADD COLUMN waiting_child_occurrence int, \
           ADD COLUMN wait_generation bigint, \
           ADD CONSTRAINT runs_invoke_depth_check CHECK (invoke_depth >= 0), \
           ADD CONSTRAINT runs_check3 CHECK ( \
             (parent_run_id IS NULL) = (parent_node_id IS NULL) AND \
             (parent_run_id IS NULL) = (parent_occurrence IS NULL)), \
           ADD CONSTRAINT runs_check4 CHECK ( \
             (parent_run_id IS NULL) = (invoke_root_run_id IS NULL)), \
           ADD CONSTRAINT runs_check5 CHECK ( \
             (waiting_child_run_id IS NULL) = (waiting_child_occurrence IS NULL) AND \
             (waiting_child_run_id IS NULL) = (wait_generation IS NULL)); \
         CREATE UNIQUE INDEX runs_parent_occurrence ON {SCHEMA}.runs \
           (tenant_id,parent_run_id,parent_node_id,parent_occurrence) \
           WHERE parent_run_id IS NOT NULL; \
         CREATE INDEX runs_invoke_root ON {SCHEMA}.runs \
           (tenant_id,invoke_root_run_id) WHERE invoke_root_run_id IS NOT NULL; \
         CREATE INDEX runs_waiting_child ON {SCHEMA}.runs \
           (tenant_id,waiting_child_run_id) WHERE waiting_child_run_id IS NOT NULL;"
    ))
    .await
    .expect("install retired child-run state");
}

async fn install_legacy_failure_detail(su: &Client) {
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs \
           ADD COLUMN fail_node text, \
           ADD COLUMN fail_reason text;"
    ))
    .await
    .expect("install retired failure-detail columns");
}

async fn seed_failure_detail_run(su: &Client, run_id: &str) {
    seed_run_admission_facts(su, "failure-detail", "cat", 1, "dev", "standard").await;
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.runs \
               (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
                environment,status,fail_kind,terminal_reason, \
                caller_outcome_kind,caller_outcome_json,caller_http_status, \
                caller_released_at,fail_node,fail_reason) \
             VALUES ('failure-detail',$1,'f',1,'cat',1,'dev','failed','terminal', \
                     'typed-caller-failure','failed', \
                     '{{\"class\":\"terminal\",\"detail\":\"typed-caller\"}}'::jsonb,500, \
                     now(),'deleted-plan-node','obsolete coordinate detail')"
        ),
        &[&run_id],
    )
    .await
    .expect("seed populated retired failure detail");
}

async fn assert_retained_failure_record(su: &Client, run_id: &str) {
    let row = su
        .query_one(
            &format!(
                "SELECT fail_kind,terminal_reason,caller_outcome_kind, \
                        caller_outcome_json::text,caller_http_status,status \
                   FROM {SCHEMA}.runs \
                  WHERE tenant_id='failure-detail' AND run_id=$1"
            ),
            &[&run_id],
        )
        .await
        .expect("read retained failure record");
    assert_eq!(row.get::<_, Option<String>>(0).as_deref(), Some("terminal"));
    assert_eq!(
        row.get::<_, Option<String>>(1).as_deref(),
        Some("typed-caller-failure")
    );
    assert_eq!(row.get::<_, Option<String>>(2).as_deref(), Some("failed"));
    assert_eq!(
        row.get::<_, Option<String>>(3).as_deref(),
        Some("{\"class\": \"terminal\", \"detail\": \"typed-caller\"}")
    );
    assert_eq!(row.get::<_, Option<i32>>(4), Some(500));
    assert_eq!(row.get::<_, String>(5), "failed");
}

async fn failure_detail_snapshot(su: &Client) -> String {
    su.query_one(
        &format!(
            "SELECT jsonb_build_object( \
               'columns', COALESCE(( \
                 SELECT jsonb_agg(jsonb_build_array(attribute.attname, \
                                                    attribute.attnotnull, \
                                                    pg_catalog.format_type( \
                                                      attribute.atttypid,attribute.atttypmod)) \
                                  ORDER BY attribute.attnum) \
                   FROM pg_catalog.pg_attribute AS attribute \
                  WHERE attribute.attrelid='{SCHEMA}.runs'::regclass \
                    AND attribute.attnum > 0 AND NOT attribute.attisdropped), '[]'::jsonb), \
               'rows', COALESCE(( \
                 SELECT jsonb_agg(to_jsonb(run_row) ORDER BY tenant_id,run_id) \
                   FROM {SCHEMA}.runs AS run_row), '[]'::jsonb), \
               'view', pg_catalog.pg_get_viewdef( \
                 pg_catalog.to_regclass('{SCHEMA}.retired_failure_detail_dependency'),true))::text"
        ),
        &[],
    )
    .await
    .expect("snapshot failure-detail dependency and rows")
    .get(0)
}

#[tokio::test]
async fn child_run_cutover_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the child-run cutover gate");
        return;
    };
    let su = connect(&url).await;
    child_run_cutover_leg(&su).await;
}

#[tokio::test]
async fn rerun_lineage_cutover_live() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the rerun-lineage cutover gate");
        return;
    };
    let su = connect(&url).await;
    rerun_lineage_cutover_leg(&su).await;
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn failure_detail_cutover_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    failure_detail_cutover_leg(&su).await;
}

/// wamn-0h0g.26.3.1 (204220e8) retired the node-runs projection. A schema
/// provisioned before it still carries the relation, so the reconciler plans
/// ONE `RetireNodeRuns` action, executes it before role bootstrap, and returns
/// — every other repair waits for the next pass. Without this leg the arm has
/// no live watcher.
pub(super) async fn node_runs_retirement_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog for node-runs retirement");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply current run-state for node-runs retirement");
    su.batch_execute(&format!(
        "CREATE TABLE {SCHEMA}.node_runs ( \
           tenant_id text NOT NULL, run_id text NOT NULL, node_id text NOT NULL, \
           occurrence int NOT NULL DEFAULT 0, seq int NOT NULL, status text NOT NULL, \
           PRIMARY KEY (tenant_id,run_id,node_id,occurrence)); \
         INSERT INTO {SCHEMA}.node_runs(tenant_id,run_id,node_id,occurrence,seq,status) \
           VALUES ('t1','r1','n1',0,0,'success'); \
         GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_run_projection_writer; \
         GRANT SELECT, INSERT, UPDATE, DELETE ON {SCHEMA}.node_runs \
           TO wamn_run_projection_writer; \
         GRANT SELECT ON {SCHEMA}.runs TO wamn_run_projection_writer;"
    ))
    .await
    .expect("install a surviving node-runs projection");

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("a surviving projection retires");
    assert_eq!(
        plan.actions
            .iter()
            .map(|action| action.kind)
            .collect::<Vec<_>>(),
        vec![RunPlaneActionKind::RetireNodeRuns],
        "retirement is a one-action plan: {:#?}",
        plan.actions
    );
    assert!(
        !table_exists(su, SCHEMA, "node_runs").await,
        "populated projection rows are discarded with their relation"
    );
    let projection_authority: bool = su
        .query_one(
            &format!(
                "SELECT has_schema_privilege('wamn_run_projection_writer','{SCHEMA}','USAGE')"
            ),
            &[],
        )
        .await
        .expect("read retired projection-writer authority")
        .get(0);
    assert!(
        !projection_authority,
        "the projection writer keeps schema authority after its relation is gone"
    );
    let projection_table_authority: bool = su
        .query_one(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class AS relation \
                   CROSS JOIN LATERAL pg_catalog.aclexplode(relation.relacl) AS acl \
                   JOIN pg_catalog.pg_roles AS grantee ON grantee.oid = acl.grantee \
                  WHERE relation.relnamespace = to_regnamespace('{SCHEMA}') \
                    AND grantee.rolname = 'wamn_run_projection_writer')"
            ),
            &[],
        )
        .await
        .expect("read retired projection-writer table authority")
        .get(0);
    // The grant on the SURVIVING `runs` is what makes this observable: dropping
    // node_runs takes its own ACL with it, so only a grant on a relation that
    // outlives the projection can witness the `ON ALL TABLES` revoke.
    assert!(
        !projection_table_authority,
        "the projection writer keeps a table grant somewhere in the schema"
    );

    // The next pass sees an ordinary schema and reaches everything the
    // one-action plan deferred.
    assert!(
        !reconcile_run_plane::reconcile(su, &schema, true)
            .await
            .expect("the pass after retirement proceeds")
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::RetireNodeRuns),
        "retirement is idempotent"
    );
}

pub(super) async fn shared_runner_legacy_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(&format!(
        "CREATE SCHEMA {SCHEMA}; GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app; \
         CREATE TABLE {SCHEMA}.runs ( \
           tenant_id text NOT NULL CHECK (tenant_id <> ''), run_id text NOT NULL, \
           flow_id text NOT NULL, flow_version int NOT NULL, \
           status text NOT NULL DEFAULT 'running' CHECK (status IN \
             ('dispatched','running','completed','failed','infrastructure-failure','effect-uncertain')), \
           trigger_source text, input_json jsonb, result_json jsonb, state_json jsonb, \
           idempotency_key text, replay_of text, root_run_id text, \
           fail_kind text CHECK (fail_kind IN \
             ('terminal','retry-exhausted','invalid-input','runaway-budget')), \
           created_at timestamptz NOT NULL DEFAULT now(), \
           updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (tenant_id,run_id)); \
         CREATE TABLE {SCHEMA}.run_queue (tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           run_id text NOT NULL,partition_key text,partition_policy text NOT NULL DEFAULT 'blocking' \
             CHECK(partition_policy IN ('blocking','leapfrog')),priority int NOT NULL DEFAULT 0, \
           available_at timestamptz NOT NULL DEFAULT now(),stream_seq bigint NOT NULL DEFAULT 0, \
           lease_owner text,lease_expires_at timestamptz,attempts int NOT NULL DEFAULT 0, \
           max_attempts int NOT NULL DEFAULT 20,enqueued_at timestamptz NOT NULL DEFAULT now(), \
           PRIMARY KEY(tenant_id,run_id),FOREIGN KEY(tenant_id,run_id) \
             REFERENCES {SCHEMA}.runs(tenant_id,run_id) ON DELETE CASCADE); \
         CREATE INDEX run_queue_claimable ON {SCHEMA}.run_queue \
           (tenant_id,available_at,stream_seq,lease_expires_at); \
         CREATE INDEX run_queue_partition ON {SCHEMA}.run_queue(tenant_id,partition_key) \
           WHERE partition_key IS NOT NULL; \
         CREATE TABLE {SCHEMA}.partition_owner (tenant_id text NOT NULL CHECK(tenant_id <> ''), \
           partition_key text NOT NULL,lease_owner text NOT NULL,lease_expires_at timestamptz NOT NULL, \
           acquired_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(tenant_id,partition_key)); \
         CREATE TABLE {SCHEMA}.run_dead_letters (tenant_id text NOT NULL CHECK(tenant_id <> ''), \
           run_id text NOT NULL,partition_key text NOT NULL,flow_id text NOT NULL,reason text NOT NULL, \
           failed_at timestamptz NOT NULL DEFAULT now(),PRIMARY KEY(tenant_id,run_id), \
           FOREIGN KEY(tenant_id,run_id) REFERENCES {SCHEMA}.runs(tenant_id,run_id) ON DELETE CASCADE);"
    ))
    .await
    .expect("build shared-runner legacy run plane");
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog schema");
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs(tenant_id,run_id,flow_id,flow_version,status) \
           VALUES ('t1','history-run','f',1,'completed');"
    ))
    .await
    .expect("seed compatible shared history");

    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("populated shared-runner legacy fixture must refuse the pin carriers");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    let database = postgres
        .as_db_error()
        .expect("shared-runner refusal is a database refusal");
    // The bespoke `execution-pin-cutover-requires-empty-run-and-release-membership`
    // refusal no longer exists anywhere in the tree. The RULE it enforced does:
    // the package/release admission pins are NOT NULL in the record, so
    // PostgreSQL itself refuses the ADD against populated legacy rows rather
    // than let the reconciler fabricate provenance for them. That is the
    // documented contract — "a legacy row that violates the canonical contract
    // aborts reconciliation rather than being rewritten or deleted".
    assert_eq!(database.code().code(), "23502");
    assert!(
        database.message().contains("package_id") && database.message().contains("runs"),
        "the refusal names the pin carrier it could not fabricate: {}",
        database.message()
    );

    // What the refusal must preserve is the HISTORY and the absence of
    // fabricated provenance. It is NOT a pre-mutation refusal: only
    // `PRE_ROLE_BOOTSTRAP_ACTIONS` run ahead of everything else, and creating a
    // missing record table is not one of them, so earlier actions in the same
    // pass legitimately land before this one aborts.
    let retained = su
        .query_one(
            &format!("SELECT count(*), min(flow_id), min(status) FROM {SCHEMA}.runs"),
            &[],
        )
        .await
        .expect("read the refusal-preserved legacy history");
    assert_eq!(retained.get::<_, i64>(0), 1);
    assert_eq!(retained.get::<_, Option<String>>(1).as_deref(), Some("f"));
    assert_eq!(
        retained.get::<_, Option<String>>(2).as_deref(),
        Some("completed")
    );
    for column in ["package_id", "effective_release_id", "environment"] {
        assert!(
            !column_exists(su, "runs", column).await,
            "refusal leaves pin column {column} absent"
        );
    }
}

/// Retired child/wait state is removed only when every durable row is ordinary.
pub(super) async fn child_run_cutover_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_child_run_state(su).await;
    seed_run_admission_facts(su, "child-cutover", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
            environment,trigger_source, \
            event_source_run_id,event_root_run_id,event_depth) \
         VALUES ('child-cutover','retained-run','f',1,'cat',1,'dev', \
                 'event','source-run','event-root',3);"
    ))
    .await
    .expect("seed retained ordinary run");

    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("empty child state cuts over around retained ordinary facts");
    assert_eq!(
        plan.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::ChildRunCutover),
        "child deletion is the leading action: {:#?}",
        plan.actions
    );
    for column in [
        "parent_run_id",
        "parent_node_id",
        "parent_occurrence",
        "waiting_child_run_id",
        "waiting_child_occurrence",
        "wait_generation",
        "invoke_depth",
        "invoke_root_run_id",
    ] {
        assert!(
            !column_exists(su, "runs", column).await,
            "retired child-run column remains: {column}"
        );
    }
    for index in [
        "runs_parent_occurrence",
        "runs_invoke_root",
        "runs_waiting_child",
    ] {
        assert!(
            indexdef(su, index).await.is_none(),
            "retired index remains: {index}"
        );
    }
    let retained = su
        .query_one(
            &format!(
                "SELECT r.trigger_source, r.event_source_run_id, r.event_root_run_id, r.event_depth \
                   FROM {SCHEMA}.runs AS r \
                  WHERE r.tenant_id='child-cutover' AND r.run_id='retained-run'"
            ),
            &[],
        )
        .await
        .expect("read retained root facts");
    assert_eq!(
        retained.get::<_, Option<String>>(0).as_deref(),
        Some("event")
    );
    assert_eq!(
        retained.get::<_, Option<String>>(1).as_deref(),
        Some("source-run")
    );
    assert_eq!(
        retained.get::<_, Option<String>>(2).as_deref(),
        Some("event-root")
    );
    assert_eq!(retained.get::<_, Option<i32>>(3), Some(3));
    assert!(
        reconcile_run_plane::reconcile(su, &schema(), false)
            .await
            .expect("observe converged child cutover")
            .actions
            .iter()
            .all(|action| action.kind != RunPlaneActionKind::ChildRunCutover),
        "child cutover is idempotent"
    );

    install_legacy_child_run_state(su).await;
    su.execute(
        &format!(
            "UPDATE {SCHEMA}.runs \
                SET waiting_child_run_id='child',waiting_child_occurrence=2,wait_generation=7 \
              WHERE tenant_id='child-cutover' AND run_id='retained-run'"
        ),
        &[],
    )
    .await
    .expect("restore a populated legacy wait fact");
    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("populated child state must refuse cutover");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    assert_db_code(postgres, "55000", "populated child state refusal");
    for column in [
        "parent_run_id",
        "parent_node_id",
        "parent_occurrence",
        "waiting_child_run_id",
        "waiting_child_occurrence",
        "wait_generation",
        "invoke_depth",
        "invoke_root_run_id",
    ] {
        assert!(
            column_exists(su, "runs", column).await,
            "refusal atomically preserves legacy column: {column}"
        );
    }
    for index in [
        "runs_parent_occurrence",
        "runs_invoke_root",
        "runs_waiting_child",
    ] {
        assert!(
            indexdef(su, index).await.is_some(),
            "refusal atomically preserves legacy index: {index}"
        );
    }

    su.execute(
        &format!(
            "UPDATE {SCHEMA}.runs \
                SET waiting_child_run_id=NULL,waiting_child_occurrence=NULL,wait_generation=NULL \
              WHERE tenant_id='child-cutover' AND run_id='retained-run'"
        ),
        &[],
    )
    .await
    .expect("restore the refused mutation to ordinary state");
    let restored = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("restored ordinary state cuts over");
    assert_eq!(
        restored.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::ChildRunCutover)
    );
    assert!(!column_exists(su, "runs", "waiting_child_run_id").await);
}

/// Retired execution-lineage metadata is removable on a populated schema. The
/// exact legacy index is the only same-name object the cutover may destroy.
pub(super) async fn rerun_lineage_cutover_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    seed_run_admission_facts(su, "rerun-cutover", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs \
           ADD COLUMN replay_of text, ADD COLUMN root_run_id text; \
         CREATE INDEX runs_root ON {SCHEMA}.runs (tenant_id,root_run_id) \
           WHERE root_run_id IS NOT NULL; \
         INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
            environment,trigger_source,input_json,state_json,replay_of,root_run_id, \
            event_source_run_id,event_root_run_id,event_depth) \
         VALUES ('rerun-cutover','retained-run','f',1,'cat',1,'dev', \
                 'event','{{\"payload\":7}}','{{\"cursor\":9}}', \
                 'legacy-parent','legacy-root','event-source','event-root',4);"
    ))
    .await
    .expect("install populated retired rerun lineage");

    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("populated rerun lineage cuts over without rewriting the run");
    assert_eq!(
        plan.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::RerunLineageCutover),
        "rerun lineage deletion is leading: {:#?}",
        plan.actions
    );
    assert!(!column_exists(su, "runs", "replay_of").await);
    assert!(!column_exists(su, "runs", "root_run_id").await);
    assert!(indexdef(su, "runs_root").await.is_none());
    assert!(
        indexdef(su, "runs_event_root").await.is_some(),
        "event-causation traversal index survives"
    );
    let retained_row = su
        .query_one(
            &format!(
                "SELECT trigger_source,event_source_run_id,event_root_run_id,event_depth, \
                        input_json::text,state_json::text \
                   FROM {SCHEMA}.runs \
                  WHERE tenant_id='rerun-cutover' AND run_id='retained-run'"
            ),
            &[],
        )
        .await
        .expect("read retained run after rerun-lineage cutover");
    let retained = (
        retained_row.get::<_, String>(0),
        retained_row.get::<_, String>(1),
        retained_row.get::<_, String>(2),
        retained_row.get::<_, i32>(3),
        retained_row.get::<_, String>(4),
        retained_row.get::<_, String>(5),
    );
    assert_eq!(
        retained,
        (
            "event".to_string(),
            "event-source".to_string(),
            "event-root".to_string(),
            4,
            "{\"payload\": 7}".to_string(),
            "{\"cursor\": 9}".to_string(),
        )
    );
    let retained_event_contract = su
        .query_one(
            &format!(
                "SELECT \
                   EXISTS (SELECT FROM pg_trigger t \
                     JOIN pg_class c ON c.oid=t.tgrelid \
                     JOIN pg_namespace n ON n.oid=c.relnamespace \
                    WHERE n.nspname='{SCHEMA}' AND c.relname='runs' \
                      AND t.tgname='runs_event_lineage_immutable' AND NOT t.tgisinternal), \
                   EXISTS (SELECT FROM pg_constraint con \
                     JOIN pg_class c ON c.oid=con.conrelid \
                     JOIN pg_namespace n ON n.oid=c.relnamespace \
                    WHERE n.nspname='{SCHEMA}' AND c.relname='runs' \
                      AND pg_get_constraintdef(con.oid,true) LIKE '%event_source_run_id%' \
                      AND pg_get_constraintdef(con.oid,true) LIKE '%event_root_run_id%' \
                      AND pg_get_constraintdef(con.oid,true) LIKE '%event_depth%'), \
                   NOT has_any_column_privilege('wamn_app','{SCHEMA}.runs','INSERT') \
                     AND NOT has_any_column_privilege('wamn_app','{SCHEMA}.runs','UPDATE') \
                     AND has_table_privilege('wamn_app','{SCHEMA}.runs','SELECT') \
                     AND has_table_privilege('wamn_app','{SCHEMA}.runs','DELETE')"
            ),
            &[],
        )
        .await
        .expect("read retained event-lineage contract");
    assert!(retained_event_contract.get::<_, bool>(0));
    assert!(retained_event_contract.get::<_, bool>(1));
    // The guest role writes no run column at all since wamn-0h0g.22.7
    // (b1d42599): `wamn_app` holds table SELECT and DELETE and nothing else, so
    // event lineage is unreachable to it by ACL as well as by the immutability
    // trigger above. This used to demand column INSERT, an authority the
    // reconciler now revokes.
    assert!(retained_event_contract.get::<_, bool>(2));
    assert!(
        reconcile_run_plane::reconcile(su, &schema(), false)
            .await
            .expect("observe converged rerun-lineage cutover")
            .actions
            .iter()
            .all(|action| action.kind != RunPlaneActionKind::RerunLineageCutover)
    );

    // Restore the retired columns with a foreign same-name index. The action's
    // lock + exact definition guard must roll back before any column or row is
    // changed, then the canonical restored definition must cut over cleanly.
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs \
           ADD COLUMN replay_of text, ADD COLUMN root_run_id text; \
         UPDATE {SCHEMA}.runs SET replay_of='restored-parent',root_run_id='restored-root' \
          WHERE tenant_id='rerun-cutover' AND run_id='retained-run'; \
         CREATE INDEX runs_root ON {SCHEMA}.runs (tenant_id,flow_id);"
    ))
    .await
    .expect("install unknown same-name runs_root mutant");
    let before = indexdef(su, "runs_root")
        .await
        .expect("mutant index exists");
    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("unknown same-name runs_root must refuse");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    assert_db_code(postgres, "55000", "unknown runs_root refusal");
    assert!(column_exists(su, "runs", "replay_of").await);
    assert!(column_exists(su, "runs", "root_run_id").await);
    assert_eq!(
        indexdef(su, "runs_root").await.as_deref(),
        Some(before.as_str())
    );
    let lineage_row = su
        .query_one(
            &format!(
                "SELECT replay_of,root_run_id,event_root_run_id FROM {SCHEMA}.runs \
                  WHERE tenant_id='rerun-cutover' AND run_id='retained-run'"
            ),
            &[],
        )
        .await
        .expect("refusal preserved row");
    let lineage = (
        lineage_row.get::<_, Option<String>>(0),
        lineage_row.get::<_, Option<String>>(1),
        lineage_row.get::<_, Option<String>>(2),
    );
    assert_eq!(
        lineage,
        (
            Some("restored-parent".to_string()),
            Some("restored-root".to_string()),
            Some("event-root".to_string()),
        )
    );

    su.batch_execute(&format!(
        "DROP INDEX {SCHEMA}.runs_root; \
         CREATE INDEX runs_root ON {SCHEMA}.runs (tenant_id,root_run_id) \
           WHERE root_run_id IS NOT NULL;"
    ))
    .await
    .expect("restore canonical legacy index definition");
    let restored = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("restored canonical legacy shape cuts over");
    assert_eq!(
        restored.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::RerunLineageCutover)
    );
    assert!(!column_exists(su, "runs", "replay_of").await);
    assert!(!column_exists(su, "runs", "root_run_id").await);
    assert!(indexdef(su, "runs_root").await.is_none());
    assert!(indexdef(su, "runs_event_root").await.is_some());
}

/// The retired per-node failure detail is deliberately discarded: its node
/// coordinate no longer has a live representation. The retained failure class
/// and typed caller outcome remain on the same run row. `RESTRICT` makes an
/// unlisted dependency a loud, atomic refusal before role bootstrap.
pub(super) async fn failure_detail_cutover_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_failure_detail(su).await;
    seed_failure_detail_run(su, "retained-run").await;

    let before_dry_run = failure_detail_snapshot(su).await;
    let dry = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("retired failure detail plans a row-preserving cutover");
    assert_eq!(
        dry.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::FailureDetailCutover),
        "failure-detail deletion is leading on the current legacy shape: {:#?}",
        dry.actions
    );
    assert_eq!(
        dry.actions
            .iter()
            .filter(|action| action.kind == RunPlaneActionKind::FailureDetailCutover)
            .count(),
        1,
        "one legacy shape must plan one cutover: {:#?}",
        dry.actions
    );
    assert!(dry.extra_columns.is_empty());
    assert_eq!(
        failure_detail_snapshot(su).await,
        before_dry_run,
        "dry-run mutated populated failure history"
    );

    let applied = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("populated retired failure detail is deliberately discarded");
    assert_eq!(
        applied.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::FailureDetailCutover)
    );
    for retired in ["fail_node", "fail_reason"] {
        assert!(
            !column_exists(su, "runs", retired).await,
            "retired failure detail remains: {retired}"
        );
    }
    assert_retained_failure_record(su, "retained-run").await;
    let again = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("failure-detail second reconcile plans");
    assert!(
        again.is_noop(),
        "failure-detail cutover did not converge: {:#?}",
        again.actions
    );

    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_failure_detail(su).await;
    seed_failure_detail_run(su, "dependent-run").await;
    su.batch_execute(&format!(
        "CREATE VIEW {SCHEMA}.retired_failure_detail_dependency AS \
           SELECT tenant_id,run_id,fail_reason FROM {SCHEMA}.runs; \
         GRANT wamn_scenario_author TO wamn_app;"
    ))
    .await
    .expect("install dependent-view and role-bootstrap sentinels");

    let before_refusal = failure_detail_snapshot(su).await;
    let dry = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("dependent failure detail still plans a cutover");
    assert_eq!(
        dry.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::FailureDetailCutover),
        "dependent refusal must lead before role repair: {:#?}",
        dry.actions
    );
    assert_eq!(failure_detail_snapshot(su).await, before_refusal);
    let membership_before: bool = su
        .query_one(
            "SELECT pg_catalog.pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
            &[],
        )
        .await
        .expect("read pre-bootstrap membership sentinel after dry-run")
        .get(0);
    assert!(membership_before, "dry-run mutated role membership");

    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("dependent view must refuse failure-detail retirement");
    assert_db_code_in_chain(&error, "2BP01", "dependent failure-detail view refusal");
    assert_eq!(
        failure_detail_snapshot(su).await,
        before_refusal,
        "RESTRICT refusal changed columns, view, or populated row"
    );
    for retired in ["fail_node", "fail_reason"] {
        assert!(
            column_exists(su, "runs", retired).await,
            "atomic refusal lost {retired}"
        );
    }
    let membership_retained: bool = su
        .query_one(
            "SELECT pg_catalog.pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
            &[],
        )
        .await
        .expect("read role-bootstrap sentinel after refusal")
        .get(0);
    assert!(
        membership_retained,
        "dependent-object refusal must precede role bootstrap"
    );

    su.batch_execute(&format!(
        "DROP VIEW {SCHEMA}.retired_failure_detail_dependency;"
    ))
    .await
    .expect("remove the external dependency only");
    let recovered = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("dependency-free failure detail cuts over");
    assert_eq!(
        recovered.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::FailureDetailCutover)
    );
    for retired in ["fail_node", "fail_reason"] {
        assert!(!column_exists(su, "runs", retired).await);
    }
    assert_retained_failure_record(su, "dependent-run").await;
    let membership_repaired: bool = su
        .query_one(
            "SELECT pg_catalog.pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
            &[],
        )
        .await
        .expect("read repaired role membership")
        .get(0);
    assert!(
        !membership_repaired,
        "role repair did not converge after cutover"
    );
    assert!(
        reconcile_run_plane::reconcile(su, &schema(), false)
            .await
            .expect("observe recovered failure-detail cutover")
            .is_noop()
    );
}

/// Reconcile repairs drifted persisted vocabularies and converges in one pass.
pub(super) async fn persisted_literal_check_drift_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    // Provision the CURRENT run plane (fresh 5-literal fail_kind CHECK)…
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");
    // Regress the run failure vocabulary to the predecessor constraint. The
    // node-error vocabulary that stood beside it left CHECK_SPECS with the
    // projection (wamn-0h0g.26.3.1, 204220e8).
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.runs DROP CONSTRAINT runs_fail_kind_check; \
         ALTER TABLE {SCHEMA}.runs ADD CONSTRAINT runs_fail_kind_check \
             CHECK (fail_kind IN ('terminal', 'retry-exhausted', 'invalid-input'));"
    ))
    .await
    .expect("regress the persisted failure vocabulary");
    // A run whose runaway verdict we will try to record.
    seed_run_admission_facts(su, "t1", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
              environment) \
             VALUES ('t1','r-budget','f',1,'cat',1,'dev');"
    ))
    .await
    .expect("seed a run");
    // Under the legacy CHECK the runaway verdict is REJECTED (the fqg.16 bug).
    let rejected = su
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET fail_kind = 'runaway-budget' \
                 WHERE tenant_id = 't1' AND run_id = 'r-budget'"
            ),
            &[],
        )
        .await;
    assert!(
        rejected.is_err(),
        "legacy 3-literal CHECK rejects the runaway verdict"
    );

    // Reconcile: exactly the fail_kind CHECK repair is planned + applied.
    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("reconcile applies");
    assert!(
        plan.actions
            .iter()
            .any(|a| a.kind == RunPlaneActionKind::RepairConstraint
                && a.target == "runs.runs_fail_kind_check"),
        "the fail_kind CHECK repair is planned: {:#?}",
        plan.actions
    );

    // (i) the canonical constraint def now admits 'runaway-budget'.
    let def: String = su
        .query_one(
            "SELECT pg_get_constraintdef(con.oid) FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = 'runs' \
               AND con.conname = 'runs_fail_kind_check'",
            &[&SCHEMA],
        )
        .await
        .expect("read fail_kind constraintdef")
        .get(0);
    assert!(
        def.contains("runaway-budget"),
        "reconciled CHECK admits runaway-budget: {def}"
    );

    // (ii) the runaway `mark_failed` UPDATE now SUCCEEDS — the verdict lands.
    let updated = su
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET fail_kind = 'runaway-budget' \
                 WHERE tenant_id = 't1' AND run_id = 'r-budget'"
            ),
            &[],
        )
        .await
        .expect("runaway verdict now accepted");
    assert_eq!(updated, 1, "the runaway verdict lands on the audit row");

    // (iii) a second reconcile plans nothing (idempotence + convergence).
    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan");
    assert!(again.is_noop(), "re-run is a no-op: {:#?}", again.actions);
}

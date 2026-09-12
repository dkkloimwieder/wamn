use super::{
    Client, RunPlaneActionKind, SCHEMA, column_exists, connect, indexdef,
    install_current_run_plane, install_legacy_flow_registry, reconcile_run_plane, reset, schema,
    seed_run_admission_facts, support, table_exists,
};

async fn install_legacy_partition_plane(su: &Client) {
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.run_queue \
           ADD COLUMN partition_key text, \
           ADD COLUMN partition_policy text NOT NULL DEFAULT 'blocking', \
           ADD CONSTRAINT run_queue_partition_policy_check \
             CHECK (partition_policy IN ('blocking','leapfrog')); \
         CREATE INDEX run_queue_partition ON {SCHEMA}.run_queue \
           (tenant_id,partition_key) WHERE partition_key IS NOT NULL; \
         CREATE TABLE {SCHEMA}.partition_owner ( \
           tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           partition_key text NOT NULL, lease_owner text NOT NULL, \
           lease_expires_at timestamptz NOT NULL, \
           acquired_at timestamptz NOT NULL DEFAULT now(), \
           PRIMARY KEY (tenant_id,partition_key)); \
         CREATE TABLE {SCHEMA}.run_dead_letters ( \
           tenant_id text NOT NULL CHECK (tenant_id <> ''), \
           run_id text NOT NULL, partition_key text NOT NULL, \
           flow_id text NOT NULL, reason text NOT NULL, \
           failed_at timestamptz NOT NULL DEFAULT now(), \
           PRIMARY KEY (tenant_id,run_id), \
           FOREIGN KEY (tenant_id,run_id) REFERENCES {SCHEMA}.runs (tenant_id,run_id) \
             ON DELETE CASCADE);"
    ))
    .await
    .expect("install retired partition plane");
}

async fn partition_plane_schema_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'relations', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,c.relkind) ORDER BY c.relname) \
               FROM pg_class c \
              WHERE c.relnamespace=to_regnamespace($1::text) \
                AND c.relname IN ('run_queue','partition_owner','run_dead_letters')), \
             '[]'::jsonb), \
           'columns', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,a.attname,a.attnotnull, \
                                                pg_get_expr(d.adbin,d.adrelid)) \
                              ORDER BY c.relname,a.attnum) \
               FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid \
               LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum \
              WHERE c.relnamespace=to_regnamespace($1::text) \
                AND c.relname IN ('run_queue','partition_owner','run_dead_letters') \
                AND a.attnum > 0 AND NOT a.attisdropped), '[]'::jsonb), \
           'constraints', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,p.conname, \
                                                pg_get_constraintdef(p.oid,true)) \
                              ORDER BY c.relname,p.conname) \
               FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid \
              WHERE p.connamespace=to_regnamespace($1::text) \
                AND c.relname IN ('run_queue','partition_owner','run_dead_letters')), \
             '[]'::jsonb), \
           'indexes', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(indexname,indexdef) ORDER BY indexname) \
               FROM pg_indexes WHERE schemaname=$1 \
                AND tablename IN ('run_queue','partition_owner','run_dead_letters')), \
             '[]'::jsonb))::text",
        &[&SCHEMA],
    )
    .await
    .expect("snapshot partition-plane schema")
    .get(0)
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn partition_plane_cutover_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    partition_plane_cutover_leg(&su).await;
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn partition_plane_active_lease_refusal_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    partition_plane_active_lease_refusal_leg(&su).await;
}

/// Persisted authored ordering bytes have no lossless global-FIFO backfill.
/// Refuse under a flow-table lock before DDL, preserve the bytes, and converge
/// once only default-omitted current graphs remain.
pub(super) async fn partition_plane_authored_ordering_refusal_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_flow_registry(su).await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.flows \
           (tenant_id,flow_id,version,graph_json) VALUES \
           ('retired-order','ordered',1, \
            '{{\"schema-version\":\"0.1\",\"ordering\":{{\"key\":\"serial\"}}}}'), \
           ('retired-order','policy',1, \
            '{{\"schema-version\":\"0.1\",\"partition-policy\":\"blocking\"}}'); \
         GRANT wamn_scenario_author TO wamn_app;"
    ))
    .await
    .expect("seed persisted retired flow ordering keys");

    let before: String = su
        .query_one(
            &format!(
                "SELECT jsonb_agg(graph_json ORDER BY flow_id)::text \
                   FROM {SCHEMA}.flows WHERE tenant_id='retired-order'"
            ),
            &[],
        )
        .await
        .expect("snapshot persisted flow bytes")
        .get(0);
    let dry = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("retired authored ordering plans a guarded cutover");
    let cutover = dry.actions.first().expect("leading partition cutover");
    assert_eq!(cutover.kind, RunPlaneActionKind::PartitionPlaneCutover);
    assert!(cutover.sql.contains(&format!(
        "LOCK TABLE \"{SCHEMA}\".\"run_queue\", \"{SCHEMA}\".\"flows\" IN ACCESS EXCLUSIVE MODE"
    )));

    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("persisted authored ordering requires reprovision");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    let database = postgres
        .as_db_error()
        .expect("typed authored-order refusal");
    assert_eq!(database.code().code(), "55000");
    assert_eq!(
        database.message(),
        "retired-authored-ordering-requires-environment-reprovision"
    );
    let after: String = su
        .query_one(
            &format!(
                "SELECT jsonb_agg(graph_json ORDER BY flow_id)::text \
                   FROM {SCHEMA}.flows WHERE tenant_id='retired-order'"
            ),
            &[],
        )
        .await
        .expect("read refusal-preserved flow bytes")
        .get(0);
    assert_eq!(after, before);
    assert!(!column_exists(su, "run_queue", "partition_key").await);
    assert!(!table_exists(su, SCHEMA, "partition_owner").await);
    let membership_retained: bool = su
        .query_one(
            "SELECT pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
            &[],
        )
        .await
        .expect("read authored-ordering refusal mutation sentinel")
        .get(0);
    assert!(membership_retained, "refusal must be the leading action");

    su.batch_execute(&format!(
        "REVOKE wamn_scenario_author FROM wamn_app; \
         DELETE FROM {SCHEMA}.flows WHERE tenant_id='retired-order'; \
         INSERT INTO {SCHEMA}.flows (tenant_id,flow_id,version,graph_json) \
         VALUES ('current-order','default-omitted',1, \
                 '{{\"schema-version\":\"0.1\",\"nodes\":[]}}');"
    ))
    .await
    .expect("replace refusal fixture with current default-omitted graph");
    let converged = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("default-omitted flow graph needs no partition cutover");
    assert!(converged.is_noop(), "actions: {:#?}", converged.actions);
}

/// A populated queue is retained when no worker holds a live lease. Only the
/// retired partition state is removed, and the global FIFO claim index lands
/// in its exact record shape.
pub(super) async fn partition_plane_cutover_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_partition_plane(su).await;
    seed_run_admission_facts(su, "partition", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
            environment) \
         VALUES ('partition','retained-run','f',1,'cat',1,'dev'); \
         INSERT INTO {SCHEMA}.run_queue \
           (tenant_id,run_id,partition_key,partition_policy,stream_seq) \
         VALUES ('partition','retained-run','serial','blocking',7);"
    ))
    .await
    .expect("seed an unleased legacy queue row");

    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("drained partition plane converges");
    assert_eq!(
        plan.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::PartitionPlaneCutover),
        "partition deletion is the leading action: {:#?}",
        plan.actions
    );
    assert_eq!(
        plan.actions
            .iter()
            .filter(|action| action.kind == RunPlaneActionKind::PartitionPlaneCutover)
            .count(),
        1
    );

    let columns: Vec<String> = su
        .query(
            "SELECT column_name FROM information_schema.columns \
              WHERE table_schema=$1 AND table_name='run_queue' \
              ORDER BY ordinal_position",
            &[&SCHEMA],
        )
        .await
        .expect("read global queue columns")
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        columns,
        [
            "tenant_id",
            "run_id",
            "priority",
            "available_at",
            "stream_seq",
            "lease_owner",
            "lease_expires_at",
            "lease_generation",
            "attempts",
            "max_attempts",
            "enqueued_at",
        ]
        .map(str::to_string)
    );
    let retained = su
        .query_one(
            &format!(
                "SELECT stream_seq,lease_owner,lease_generation,attempts \
                   FROM {SCHEMA}.run_queue \
                  WHERE tenant_id='partition' AND run_id='retained-run'"
            ),
            &[],
        )
        .await
        .expect("read retained queue row");
    assert_eq!(retained.get::<_, i64>(0), 7);
    assert_eq!(retained.get::<_, Option<String>>(1), None);
    assert_eq!(retained.get::<_, i64>(2), 0);
    assert_eq!(retained.get::<_, i32>(3), 0);
    assert!(!table_exists(su, SCHEMA, "partition_owner").await);
    assert!(!table_exists(su, SCHEMA, "run_dead_letters").await);
    assert!(indexdef(su, "run_queue_partition").await.is_none());
    let claimable = indexdef(su, "run_queue_claimable")
        .await
        .expect("global claim index exists");
    assert!(
        claimable.contains("(tenant_id, available_at, stream_seq, run_id, lease_expires_at)"),
        "global FIFO index shape: {claimable}"
    );
    assert!(
        !claimable.contains("WHERE"),
        "claim index is global: {claimable}"
    );

    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("partition cutover idempotence");
    assert!(
        again.is_noop(),
        "second partition reconcile: {:#?}",
        again.actions
    );
}

/// Either source of live partition ownership refuses before any DDL or later
/// role repair. The fixed-lock cutover is therefore safe against both queue and
/// owner-table workers.
pub(super) async fn partition_plane_active_lease_refusal_leg(su: &Client) {
    for lease_source in ["run_queue", "partition_owner"] {
        reset(su).await;
        install_current_run_plane(su).await;
        install_legacy_partition_plane(su).await;
        su.batch_execute("GRANT wamn_scenario_author TO wamn_app")
            .await
            .expect("install later authority-repair sentinel");
        match lease_source {
            "run_queue" => {
                seed_run_admission_facts(su, "leased", "cat", 1, "dev", "standard").await;
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.runs \
                       (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
                        environment) \
                     VALUES ('leased','active-run','f',1,'cat',1,'dev'); \
                     INSERT INTO {SCHEMA}.run_queue \
                       (tenant_id,run_id,lease_owner,lease_expires_at) \
                     VALUES ('leased','active-run','worker','infinity');"
                ))
                .await
                .expect("seed active queue lease");
            }
            "partition_owner" => {
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.partition_owner \
                       (tenant_id,partition_key,lease_owner,lease_expires_at) \
                     VALUES ('leased','serial','worker','infinity');"
                ))
                .await
                .expect("seed active partition-owner lease");
            }
            _ => unreachable!(),
        }

        let before = partition_plane_schema_snapshot(su).await;
        let dry = reconcile_run_plane::reconcile(su, &schema(), false)
            .await
            .expect("active lease dry-run plans a refusal guard");
        assert_eq!(
            dry.actions.first().map(|action| action.kind),
            Some(RunPlaneActionKind::PartitionPlaneCutover)
        );
        let error = reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect_err("active partition lease refuses cutover");
        let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
        let database = postgres.as_db_error().expect("typed lease refusal");
        assert_eq!(database.code().code(), "55000");
        assert_eq!(
            database.message(),
            "partition-plane-cutover-requires-drained-workers"
        );
        assert_eq!(
            partition_plane_schema_snapshot(su).await,
            before,
            "{lease_source}: active-lease refusal leaves the schema unchanged"
        );
        let membership_retained: bool = su
            .query_one(
                "SELECT pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
                &[],
            )
            .await
            .expect("read authority-repair sentinel")
            .get(0);
        assert!(
            membership_retained,
            "{lease_source}: leading refusal precedes unrelated repair"
        );
    }
}

/// A populated legacy table whose lease state cannot be read is not assumed
/// drained. The cutover refuses byte-for-byte before DDL; once the ambiguous
/// table is empty, the same partial schema converges to the retained record.
pub(super) async fn partition_plane_unobservable_lease_refusal_leg(su: &Client) {
    for lease_source in ["run_queue", "partition_owner"] {
        reset(su).await;
        install_current_run_plane(su).await;
        install_legacy_partition_plane(su).await;
        let expected_message = match lease_source {
            "run_queue" => {
                seed_run_admission_facts(su, "ambiguous", "cat", 1, "dev", "standard").await;
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.runs \
                       (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
                        environment) \
                     VALUES ('ambiguous','queue-run','f',1,'cat',1,'dev'); \
                     INSERT INTO {SCHEMA}.run_queue (tenant_id,run_id,partition_key) \
                     VALUES ('ambiguous','queue-run','serial'); \
                     ALTER TABLE {SCHEMA}.run_queue DROP COLUMN lease_owner;"
                ))
                .await
                .expect("seed populated queue with unobservable lease shape");
                "partition-plane-cutover-requires-observable-run-queue-leases-or-empty-queue"
            }
            "partition_owner" => {
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.partition_owner \
                       (tenant_id,partition_key,lease_owner,lease_expires_at) \
                     VALUES ('ambiguous','serial','worker','infinity'); \
                     ALTER TABLE {SCHEMA}.partition_owner DROP COLUMN lease_expires_at;"
                ))
                .await
                .expect("seed populated owner table with unobservable lease shape");
                "partition-plane-cutover-requires-observable-partition-leases-or-empty-owner-table"
            }
            _ => unreachable!(),
        };

        let before = partition_plane_schema_snapshot(su).await;
        let error = reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect_err("unobservable populated lease shape refuses cutover");
        let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
        let database = postgres.as_db_error().expect("typed lease-shape refusal");
        assert_eq!(database.code().code(), "55000");
        assert_eq!(database.message(), expected_message);
        assert_eq!(
            partition_plane_schema_snapshot(su).await,
            before,
            "{lease_source}: ambiguous lease refusal rolls back every DDL change"
        );

        su.batch_execute(&format!("DELETE FROM {SCHEMA}.{lease_source}"))
            .await
            .expect("drain ambiguous legacy table");
        reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect("empty partial lease shape converges");
        assert!(!table_exists(su, SCHEMA, "partition_owner").await);
        assert!(!table_exists(su, SCHEMA, "run_dead_letters").await);
        assert!(!column_exists(su, "run_queue", "partition_key").await);
        assert!(!column_exists(su, "run_queue", "partition_policy").await);
        assert!(column_exists(su, "run_queue", "lease_owner").await);
        assert!(column_exists(su, "run_queue", "lease_expires_at").await);
    }
}

/// Retired dead-letter history has no in-place conversion. Any retained row
/// requires an archive or whole-environment reprovision and rolls the cutover
/// back before a table, column, index, CHECK, or unrelated grant changes.
pub(super) async fn partition_plane_dead_letter_refusal_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    install_legacy_partition_plane(su).await;
    seed_run_admission_facts(su, "dead-letter", "cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
            environment) \
         VALUES ('dead-letter','failed-run','f',1,'cat',1,'dev'); \
         INSERT INTO {SCHEMA}.run_dead_letters \
           (tenant_id,run_id,partition_key,flow_id,reason) \
         VALUES ('dead-letter','failed-run','serial','f','legacy-history'); \
         GRANT wamn_scenario_author TO wamn_app;"
    ))
    .await
    .expect("seed retained dead-letter history");

    let before = partition_plane_schema_snapshot(su).await;
    let dry = reconcile_run_plane::reconcile(su, &schema(), false)
        .await
        .expect("dead-letter dry-run plans the guarded cutover");
    let cutover = dry.actions.first().expect("leading partition cutover");
    assert_eq!(cutover.kind, RunPlaneActionKind::PartitionPlaneCutover);
    let guard = cutover
        .sql
        .find("retired-run-dead-letter-history-requires-archive-or-environment-reprovision")
        .expect("archive-or-reprovision refusal diagnostic");
    let destructive = cutover
        .sql
        .find("DROP INDEX")
        .expect("guarded destructive statements");
    assert!(
        guard < destructive,
        "history guard precedes destructive DDL"
    );

    let error = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect_err("nonempty retired dead-letter history refuses cutover");
    let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
    let database = postgres.as_db_error().expect("typed history refusal");
    assert_eq!(database.code().code(), "55000");
    assert_eq!(
        database.message(),
        "retired-run-dead-letter-history-requires-archive-or-environment-reprovision"
    );
    assert_eq!(
        partition_plane_schema_snapshot(su).await,
        before,
        "history refusal leaves the partition schema unchanged"
    );
    assert_eq!(
        su.query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.run_dead_letters"),
            &[],
        )
        .await
        .expect("dead-letter history remains")
        .get::<_, i64>(0),
        1
    );
    let membership_retained: bool = su
        .query_one(
            "SELECT pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
            &[],
        )
        .await
        .expect("read authority-repair sentinel")
        .get(0);
    assert!(membership_retained, "history refusal is the leading action");
}

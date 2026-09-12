use super::{
    CATALOG_SCHEMA_SQL, Client, EMPTY_EXECUTION_BUNDLE_HASH, RUN_STATE_SQL, RunPlaneActionKind,
    SCHEMA, column_exists, connect, reconcile_run_plane, reset, rewrite_schema, schema,
    seed_run_admission_facts, support,
};

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn frame_identity_cutover_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    frame_identity_cutover_leg(&su).await;
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL 18 database via WAMN_CTL_PG_URL"]
async fn effect_writer_cutover_live() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let su = connect(&url).await;
    effect_writer_cutover_leg(&su).await;
}

async fn retired_shape_schema_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'constraints', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,p.conname, \
                                                pg_get_constraintdef(p.oid,true)) \
                              ORDER BY c.relname,p.conname) \
               FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid \
              WHERE p.connamespace=to_regnamespace($1::text) \
                AND c.relname = 'effect_attempts'), '[]'::jsonb), \
           'indexes', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(indexname,indexdef) ORDER BY indexname) \
               FROM pg_indexes WHERE schemaname=$1 \
                AND tablename = 'effect_attempts'), '[]'::jsonb), \
           'columns', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,a.attname,a.attnotnull, \
                                                pg_get_expr(d.adbin,d.adrelid)) \
                              ORDER BY c.relname,a.attnum) \
               FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid \
               LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum \
              WHERE c.relnamespace=to_regnamespace($1::text) \
                AND c.relname = 'effect_attempts' \
                AND a.attnum > 0 AND NOT a.attisdropped), '[]'::jsonb))::text",
        &[&SCHEMA],
    )
    .await
    .expect("read retired schema snapshot")
    .get(0)
}

async fn create_old_frame_identity_tables(su: &Client, effect: bool, populated: bool) {
    reset(su).await;
    su.batch_execute(&format!("CREATE SCHEMA {SCHEMA};"))
        .await
        .expect("create frame-cutover schema");
    if effect {
        su.batch_execute(&format!(
            "CREATE TABLE {SCHEMA}.effect_attempts ( \
               tenant_id text NOT NULL, attempt_id uuid NOT NULL, run_id text NOT NULL, \
               node_id text NOT NULL, occurrence int NOT NULL, seq int NOT NULL, \
               generation_fact_kind text NOT NULL, attempt_started_at timestamptz NOT NULL, \
               attempt_deadline_at timestamptz NOT NULL, attempt_input_ref text NOT NULL, \
               PRIMARY KEY (tenant_id,attempt_id), \
               UNIQUE (tenant_id,attempt_id,attempt_started_at), \
               CONSTRAINT effect_attempts_occurrence_key \
                 UNIQUE (tenant_id,run_id,node_id,occurrence));"
        ))
        .await
        .expect("create old effect_attempts");
        if populated {
            su.batch_execute(&format!(
                "INSERT INTO {SCHEMA}.effect_attempts \
                   (tenant_id,attempt_id,run_id,node_id,occurrence,seq,generation_fact_kind, \
                    attempt_started_at,attempt_deadline_at,attempt_input_ref) \
                 VALUES ('t1','00000000-0000-0000-0000-000000000413','r1','n1',0,0, \
                         'not-required','2026-01-01 UTC','2026-01-02 UTC','sha256:input');"
            ))
            .await
            .expect("seed old effect_attempts");
        }
    }
}

pub(super) async fn frame_identity_cutover_leg(su: &Client) {
    {
        let label = "effect-only";
        create_old_frame_identity_tables(su, true, true).await;
        su.batch_execute("GRANT wamn_scenario_author TO wamn_app")
            .await
            .expect("seed role-membership mutation sentinel");
        let before = retired_shape_schema_snapshot(su).await;
        let error = reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect_err("populated legacy identity must refuse before DDL");
        assert!(
            format!("{error:#}").contains("effect-writer-cutover-requires-empty-ledger"),
            "{label}: wrong refusal: {error:#}"
        );
        assert_eq!(
            retired_shape_schema_snapshot(su).await,
            before,
            "{label}: refusal must leave schema unchanged"
        );
        let membership_retained: bool = su
            .query_one(
                "SELECT pg_has_role('wamn_app', 'wamn_scenario_author', 'MEMBER')",
                &[],
            )
            .await
            .expect("read role-membership mutation sentinel")
            .get(0);
        assert!(
            membership_retained,
            "{label}: refusal must precede role bootstrap"
        );
    }

    // Late frame-identity drift on a CURRENT, populated schema. Since
    // wamn-0h0g.26.3.1 (204220e8) `effect_attempts` is the only frame-identity
    // target, so each case drifts it: the relation CHECK, the occurrence
    // identity, and a resurrected legacy `node_id` carrier.
    for (label, drift_sql) in [
        (
            "drifted-frame-check",
            format!(
                "ALTER TABLE {SCHEMA}.effect_attempts \
                   DROP CONSTRAINT effect_attempts_frame_relation_check, \
                   ADD CONSTRAINT effect_attempts_frame_relation_check CHECK (frame_id >= 0);"
            ),
        ),
        (
            "drifted-occurrence-key",
            format!(
                "ALTER TABLE {SCHEMA}.effect_attempts \
                   DROP CONSTRAINT effect_attempts_occurrence_key, \
                   ADD CONSTRAINT effect_attempts_occurrence_key \
                     UNIQUE (tenant_id,run_id,local_node_id,occurrence);"
            ),
        ),
        (
            "retained-legacy-node-id",
            format!(
                "ALTER TABLE {SCHEMA}.effect_attempts \
                   ADD COLUMN node_id text NOT NULL DEFAULT 'legacy';"
            ),
        ),
    ] {
        reset(su).await;
        su.batch_execute(CATALOG_SCHEMA_SQL)
            .await
            .expect("apply catalog for populated frame drift refusal");
        su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema()))
            .await
            .expect("apply current run-state for populated frame drift refusal");
        seed_run_admission_facts(su, "t1", "frame_cat", 1, "dev", "standard").await;
        su.batch_execute(&format!(
            "INSERT INTO {SCHEMA}.runs \
               (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
                status) \
             VALUES ('t1',$${label}$$,'f',1,'frame_cat',1,'dev','running'); \
             INSERT INTO {SCHEMA}.effect_attempts \
               (tenant_id,attempt_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                local_node_id,source_artifact_hash,requirement_name,occurrence,seq, \
                generation_fact_kind,attempt_deadline_at,attempt_input_ref) \
             VALUES ('t1','00000000-0000-0000-0000-000000000413',$${label}$$, \
                     $${EMPTY_EXECUTION_BUNDLE_HASH}$$,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,0, \
                     'n',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'manager',0,0, \
                     'not-required','2099-01-02 UTC','sha256:input'); \
             {drift_sql}"
        ))
        .await
        .expect("seed populated frame drift");
        let before = retired_shape_schema_snapshot(su).await;
        let error = reconcile_run_plane::reconcile(su, &schema(), true)
            .await
            .expect_err("populated late frame identity drift must refuse before DDL");
        assert!(
            format!("{error:#}").contains("requires-empty"),
            "{label}: wrong refusal: {error:#}"
        );
        assert_eq!(
            retired_shape_schema_snapshot(su).await,
            before,
            "{label}: refusal must leave schema unchanged"
        );
    }

    create_old_frame_identity_tables(su, true, false).await;
    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("empty old frame identity cutover succeeds");
    assert!(
        plan.actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
    );
    let old_identity_residue: i64 = su
        .query_one(
            "SELECT count(*) FROM information_schema.columns \
              WHERE table_schema=$1 AND table_name='effect_attempts' \
                AND column_name='node_id'",
            &[&SCHEMA],
        )
        .await
        .expect("read upgraded frame identity columns")
        .get(0);
    assert_eq!(
        old_identity_residue, 0,
        "empty cutover retained legacy node_id"
    );
    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("frame identity cutover idempotence");
    assert!(
        !again
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
    );

    reset(su).await;
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog for combined frame/writer cutover");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema()))
        .await
        .expect("apply current run-state for combined frame/writer cutover");
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.effect_attempt_dispatches \
           DROP CONSTRAINT effect_attempt_dispatches_attempt_fk, \
           DROP CONSTRAINT effect_attempt_dispatches_occurrence_key, \
           DROP COLUMN run_id, DROP COLUMN frame_id, \
           DROP COLUMN local_node_id, DROP COLUMN occurrence; \
         ALTER TABLE {SCHEMA}.effect_attempts \
           DROP CONSTRAINT effect_attempts_current_plan_hash_check, \
           ADD CONSTRAINT effect_attempts_current_plan_hash_check \
             CHECK (current_plan_hash <> '');"
    ))
    .await
    .expect("install combined frame and dispatch-coordinate drift");
    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("combined frame/writer cutover converges in one pass");
    let frame_position = plan
        .actions
        .iter()
        .position(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
        .expect("combined frame cutover action");
    let writer_position = plan
        .actions
        .iter()
        .position(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("combined writer cutover action");
    assert!(frame_position < writer_position);
    assert!(
        plan.actions[frame_position]
            .sql
            .contains("DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_attempt_fk")
    );
    assert!(
        !plan.actions[frame_position]
            .sql
            .contains("ADD CONSTRAINT effect_attempt_dispatches_attempt_fk")
    );
    assert!(
        plan.actions[writer_position]
            .sql
            .contains("ADD CONSTRAINT effect_attempt_dispatches_attempt_fk")
    );
    for column in ["run_id", "frame_id", "local_node_id", "occurrence"] {
        assert!(
            column_exists(su, "effect_attempt_dispatches", column).await,
            "combined cutover did not restore dispatch coordinate {column}"
        );
    }
    let dispatch_fk: String = su
        .query_one(
            "SELECT pg_get_constraintdef(oid,true) FROM pg_constraint \
              WHERE connamespace=to_regnamespace($1::text) \
                AND conname='effect_attempt_dispatches_attempt_fk'",
            &[&SCHEMA],
        )
        .await
        .expect("read combined-cutover dispatch FK")
        .get(0);
    assert!(dispatch_fk.contains(
        "FOREIGN KEY (tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)"
    ));
    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("combined frame/writer cutover reapply");
    assert!(!again.actions.iter().any(|action| matches!(
        action.kind,
        RunPlaneActionKind::FrameIdentityCutover | RunPlaneActionKind::EffectWriterCutover
    )));

    reset(su).await;
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog for current effect-frame drift");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema()))
        .await
        .expect("apply current run-state for effect-frame drift");
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.effect_attempts \
           DROP CONSTRAINT effect_attempts_current_plan_hash_check, \
           ADD CONSTRAINT effect_attempts_current_plan_hash_check CHECK (current_plan_hash <> '');"
    ))
    .await
    .expect("install current-schema effect-frame drift");
    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("empty effect-frame drift converges with dispatch FK present");
    let action = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
        .expect("effect-frame cutover action");
    assert!(action.sql.contains(&format!(
        "LOCK TABLE \"{SCHEMA}\".effect_attempt_dispatches IN ACCESS EXCLUSIVE MODE"
    )));
    assert!(
        action
            .sql
            .contains("DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_attempt_fk")
    );
    assert!(
        action
            .sql
            .contains("ADD CONSTRAINT effect_attempt_dispatches_attempt_fk")
    );
    let dispatch_fk: String = su
        .query_one(
            "SELECT pg_get_constraintdef(oid,true) FROM pg_constraint \
              WHERE connamespace=to_regnamespace($1::text) \
                AND conname='effect_attempt_dispatches_attempt_fk'",
            &[&SCHEMA],
        )
        .await
        .expect("read restored dispatch-to-attempt FK")
        .get(0);
    assert!(dispatch_fk.contains(
        "FOREIGN KEY (tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)"
    ));
    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("effect-frame cutover reapply");
    assert!(
        !again
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
    );

    reset(su).await;
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog for current single-target proof");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema()))
        .await
        .expect("apply current run-state");
    su.batch_execute(&format!("DROP TABLE {SCHEMA}.effect_attempts CASCADE;"))
        .await
        .expect("remove effect peer");
    seed_run_admission_facts(su, "t1", "frame_cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            status) \
         VALUES ('t1','framed-current','f',1,'frame_cat',1,'dev','running');"
    ))
    .await
    .expect("seed a current populated run");
    let plan = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("an absent effect peer is recreated without a frame refusal");
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
    );
    let exists: bool = su
        .query_one(
            "SELECT to_regclass($1::text) IS NOT NULL",
            &[&format!("{SCHEMA}.effect_attempts")],
        )
        .await
        .expect("read recreated effect peer")
        .get(0);
    assert!(exists, "missing current peer was not recreated");
}

async fn effect_writer_schema_snapshot(su: &Client) -> String {
    su.query_one(
        "SELECT jsonb_build_object( \
           'columns', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(table_name,column_name,is_nullable,column_default) \
                              ORDER BY table_name,ordinal_position) \
               FROM information_schema.columns \
              WHERE table_schema=$1 AND table_name IN \
                    ('effect_attempts','effect_attempt_dispatches','effect_attempt_outcomes')), \
             '[]'::jsonb), \
           'constraints', COALESCE(( \
             SELECT jsonb_agg(jsonb_build_array(c.relname,p.conname, \
                                                pg_get_constraintdef(p.oid,true)) \
                              ORDER BY c.relname,p.conname) \
               FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid \
              WHERE p.connamespace=to_regnamespace($1::text) \
                AND c.relname IN \
                    ('effect_attempts','effect_attempt_dispatches','effect_attempt_outcomes')), \
             '[]'::jsonb))::text",
        &[&SCHEMA],
    )
    .await
    .expect("snapshot effect-writer schema")
    .get(0)
}

async fn install_empty_incompatible_effect_writer_shape(su: &Client) {
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.effect_attempt_dispatches \
             DROP CONSTRAINT effect_attempt_dispatches_attempt_fk, \
             DROP CONSTRAINT effect_attempt_dispatches_occurrence_key, \
             DROP COLUMN run_id, DROP COLUMN frame_id, \
             DROP COLUMN local_node_id, DROP COLUMN occurrence; \
         ALTER TABLE {SCHEMA}.effect_attempts \
             DROP CONSTRAINT effect_attempts_dispatch_identity_key, \
             ALTER COLUMN attempt_started_at DROP DEFAULT, \
             ADD COLUMN attempt_key text;"
    ))
    .await
    .expect("install incompatible empty writer-ledger shape");
}

pub(super) async fn effect_writer_cutover_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog for writer cutover");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply current run-state for writer cutover");
    seed_run_admission_facts(su, "t1", "writer_cat", 1, "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id,environment, \
            status) \
         VALUES ('t1','writer-projection','f',1,'writer_cat',1,'dev', \
                 'running');"
    ))
    .await
    .expect("seed the run the writer cutover reconciles around");
    install_empty_incompatible_effect_writer_shape(su).await;

    su.batch_execute("ALTER ROLE wamn_effect_writer LOGIN")
        .await
        .expect("make stable writer role invalid");
    let before_role_refusal = effect_writer_schema_snapshot(su).await;
    let error = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("invalid stable role refuses before empty cutover");
    let postgres: tokio_postgres::Error = error.downcast().expect("role refusal");
    let database = postgres.as_db_error().expect("typed role refusal");
    assert_eq!(database.code().code(), "42501");
    assert_eq!(database.message(), "effect-writer-role-out-of-bounds");
    assert_eq!(
        effect_writer_schema_snapshot(su).await,
        before_role_refusal,
        "role verification precedes empty structural cutover"
    );
    su.batch_execute("ALTER ROLE wamn_effect_writer NOLOGIN")
        .await
        .expect("restore stable writer role");

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("empty writer-ledger cutover succeeds");
    let action = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("effect writer cutover action");
    assert_eq!(
        action.sql.matches("LOCK TABLE").count(),
        3,
        "the three incompatible ledgers are locked"
    );
    let preflight = action
        .sql
        .find("effect-writer-cutover-requires-empty-ledger")
        .expect("frozen cutover preflight");
    let first_ddl = ["ALTER TABLE", "DROP TRIGGER", "DROP FUNCTION"]
        .into_iter()
        .filter_map(|needle| action.sql.find(needle))
        .min()
        .expect("cutover structural DDL");
    assert!(
        preflight < first_ddl,
        "all preflight precedes structural DDL"
    );
    assert!(!action.sql.contains("UPDATE "));
    assert!(!action.sql.contains("INSERT INTO "));

    assert!(!column_exists(su, "effect_attempts", "attempt_key").await);
    for column in ["run_id", "frame_id", "local_node_id", "occurrence"] {
        assert!(
            column_exists(su, "effect_attempt_dispatches", column).await,
            "missing dispatch coordinate {column}"
        );
    }
    let occurrence: String = su
        .query_one(
            "SELECT pg_get_constraintdef(oid,true) FROM pg_constraint \
              WHERE connamespace=to_regnamespace($1::text) \
                AND conname='effect_attempt_dispatches_occurrence_key'",
            &[&SCHEMA],
        )
        .await
        .expect("read exact dispatch occurrence identity")
        .get(0);
    assert_eq!(
        occurrence,
        "UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence)"
    );
    let foreign_key: String = su
        .query_one(
            "SELECT pg_get_constraintdef(oid,true) FROM pg_constraint \
              WHERE connamespace=to_regnamespace($1::text) \
                AND conname='effect_attempt_dispatches_attempt_fk'",
            &[&SCHEMA],
        )
        .await
        .expect("read coordinate-bound attempt FK")
        .get(0);
    assert!(foreign_key.contains(
        "FOREIGN KEY (tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)"
    ));

    let second = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("writer-ledger cutover reapply");
    assert!(
        !second
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
    );

    su.batch_execute(&format!(
        "GRANT CREATE ON SCHEMA {SCHEMA} TO wamn_effect_writer; \
         GRANT SELECT ON {SCHEMA}.effect_attempts TO wamn_scenario_author; \
         GRANT UPDATE (attempt_input_ref) ON {SCHEMA}.effect_attempts TO wamn_app; \
         GRANT INSERT ON {SCHEMA}.effect_attempt_dispatches TO wamn_effect_writer; \
         GRANT INSERT ON {SCHEMA}.effect_attempt_outcomes TO wamn_effect_writer; \
         GRANT SELECT ON {SCHEMA}.runs TO wamn_effect_writer; \
         GRANT UPDATE (status) ON {SCHEMA}.runs TO wamn_effect_writer; \
         ALTER TABLE {SCHEMA}.run_queue DROP COLUMN lease_owner; \
         REVOKE SELECT (lease_expires_at) ON {SCHEMA}.run_queue FROM wamn_effect_writer; \
         GRANT SELECT (lease_generation) ON {SCHEMA}.run_queue TO wamn_effect_writer;"
    ))
    .await
    .expect("install schema/table/column ACL drift");
    let repair = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("repair effect-writer ACL drift");
    assert!(repair.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
            && action.target == format!("{SCHEMA}.usage")
    }));
    assert!(repair.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
            && action.target == format!("{SCHEMA}.effect_attempts")
    }));
    // THE CONVERGE PATH for the two sibling ledgers (wamn-0h0g.20.32): an append
    // granted directly to the stable role on an ALREADY-PROVISIONED database is
    // drift the reconciler must REMOVE. The DDL alone cannot show this — it only
    // shows birth — so the drift above is installed on purpose.
    for table in ["effect_attempt_dispatches", "effect_attempt_outcomes"] {
        assert!(
            repair.actions.iter().any(|action| {
                action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
                    && action.target == format!("{SCHEMA}.{table}")
            }),
            "the reconciler did not plan to remove the sibling ledger append on {table}"
        );
    }
    for table in ["runs", "run_queue"] {
        assert!(repair.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
                && action.target == format!("{SCHEMA}.{table}.effect-read")
        }));
    }
    let add_lease_owner = repair
        .actions
        .iter()
        .position(|action| {
            action.kind == RunPlaneActionKind::AddColumn && action.target == "run_queue.lease_owner"
        })
        .expect("partial queue adds the missing writer-read column");
    let repair_queue_read = repair
        .actions
        .iter()
        .position(|action| action.target == format!("{SCHEMA}.run_queue.effect-read"))
        .unwrap();
    assert!(add_lease_owner < repair_queue_read);
    let privileges = su
        .query_one(
            &format!(
                "SELECT has_schema_privilege('wamn_effect_writer','{SCHEMA}','USAGE'), \
                        has_schema_privilege('wamn_effect_writer','{SCHEMA}','CREATE'), \
                        has_column_privilege('wamn_app','{SCHEMA}.effect_attempts', \
                                             'attempt_input_ref','UPDATE'), \
                        has_table_privilege('wamn_scenario_author', \
                                            '{SCHEMA}.effect_attempts','SELECT'), \
                        has_table_privilege('wamn_effect_writer', \
                                            '{SCHEMA}.effect_attempts','INSERT'), \
                        has_table_privilege('wamn_effect_writer', \
                                            '{SCHEMA}.effect_attempts','SELECT'), \
                        has_any_column_privilege('wamn_effect_writer', \
                                                 '{SCHEMA}.effect_attempts','INSERT'), \
                        has_table_privilege('wamn_effect_writer', \
                                            '{SCHEMA}.effect_attempt_dispatches','INSERT'), \
                        has_table_privilege('wamn_effect_writer', \
                                            '{SCHEMA}.effect_attempt_outcomes','INSERT'), \
                        EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_effect_writer' \
                                 AND NOT (rolsuper OR rolbypassrls))"
            ),
            &[],
        )
        .await
        .expect("read converged effect-writer ACL boundary");
    assert!(privileges.get::<_, bool>(0));
    assert!(!privileges.get::<_, bool>(1));
    assert!(!privileges.get::<_, bool>(2));
    assert!(!privileges.get::<_, bool>(3));
    // BORN PARKED (wamn-0h0g.20.30). THE SERVER'S OWN ANSWER, not the DDL text:
    // once the reconciler converges, the stable writer role holds READ on the
    // attempt ledger and NO append — at table level or at any column. Every
    // provisioned generation login inherits this role with INHERIT TRUE, so this
    // is exactly what a fresh project environment is born holding, and — because
    // the drift above was installed first — what an UPGRADED one converges to.
    assert!(
        !privileges.get::<_, bool>(4),
        "the reconciler re-minted a LIVE append authority on a parked ledger"
    );
    assert!(privileges.get::<_, bool>(5), "the writer keeps its read");
    assert!(
        !privileges.get::<_, bool>(6),
        "column-level append survived the table-level park"
    );
    // wamn-0h0g.20.32: both sibling ledgers are parked on the same footing, and
    // the append DIRECTLY granted to the stable role above is gone — the
    // reconciler removed it rather than re-granting it.
    assert!(
        !privileges.get::<_, bool>(7),
        "the reconciler left a LIVE append on effect_attempt_dispatches"
    );
    assert!(
        !privileges.get::<_, bool>(8),
        "the reconciler left a LIVE append on effect_attempt_outcomes"
    );
    // A superuser or RLS-bypassing role would mask every refusal asserted above.
    assert!(privileges.get::<_, bool>(9));
    let run_reads = su
        .query_one(
            &format!(
                "SELECT \
                    has_table_privilege('wamn_effect_writer','{SCHEMA}.runs','SELECT'), \
                    has_table_privilege('wamn_effect_writer','{SCHEMA}.runs','UPDATE'), \
                    has_column_privilege('wamn_effect_writer','{SCHEMA}.runs','tenant_id','SELECT') \
                      AND has_column_privilege('wamn_effect_writer','{SCHEMA}.runs','run_id','SELECT') \
                      AND has_column_privilege('wamn_effect_writer','{SCHEMA}.runs','status','SELECT'), \
                    has_column_privilege('wamn_effect_writer','{SCHEMA}.runs','flow_id','SELECT'), \
                    has_table_privilege('wamn_effect_writer','{SCHEMA}.run_queue','SELECT'), \
                    has_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','tenant_id','SELECT') \
                      AND has_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','run_id','SELECT') \
                      AND has_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','lease_owner','SELECT') \
                      AND has_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','lease_expires_at','SELECT'), \
                    has_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','lease_generation','SELECT'), \
                    has_any_column_privilege('wamn_effect_writer','{SCHEMA}.run_queue','INSERT,UPDATE,REFERENCES')"
            ),
            &[],
        )
        .await
        .expect("read exact effect-writer runnable-state privileges");
    assert!(!run_reads.get::<_, bool>(0));
    assert!(!run_reads.get::<_, bool>(1));
    assert!(run_reads.get::<_, bool>(2));
    assert!(!run_reads.get::<_, bool>(3));
    assert!(!run_reads.get::<_, bool>(4));
    assert!(run_reads.get::<_, bool>(5));
    assert!(run_reads.get::<_, bool>(6));
    assert!(!run_reads.get::<_, bool>(7));
    // wamn-0h0g.26.3.1 (204220e8) retired the node-runs projection, and with it
    // the retired projection's ACL target and every rogue-projection-authority
    // path this leg used to close. `wamn_projection_rogue_member` survives only
    // as the transitive-membership witness the generation contract below needs.
    su.batch_execute(
        "DO $roles$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_projection_rogue_member') THEN \
             CREATE ROLE wamn_projection_rogue_member NOLOGIN INHERIT; \
           END IF; \
         END $roles$;",
    )
    .await
    .expect("install the transitive-membership witness");

    // wamn-0h0g.12.178: both hand-built generations below carry the option shape
    // the prepare path emits, so each refusal stays attributable to the ONE
    // cause its assertion names rather than also tripping the edge-option term.
    let generation = "wamn_effect_writer_0000000000000000000000000000000000000000_a";
    su.batch_execute(&format!(
        "DO $generation$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='{generation}') THEN \
             CREATE ROLE {generation} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               INHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $generation$; \
         GRANT wamn_effect_writer TO {generation} \
           WITH ADMIN FALSE, INHERIT TRUE, SET FALSE; \
         GRANT {generation} TO wamn_projection_rogue_member;"
    ))
    .await
    .expect("install unexpected transitive generation membership");
    let inherited = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("unexpected stable-role membership must fail closed");
    assert!(
        format!("{inherited:#}").contains("effect-writer-role-out-of-bounds"),
        "wrong inherited-authority refusal: {inherited:#}"
    );
    assert!(
        su.query_one(
            &format!("SELECT pg_has_role('wamn_projection_rogue_member','{generation}','MEMBER')"),
            &[],
        )
        .await
        .expect("read retained refused membership")
        .get::<_, bool>(0),
        "refusal is atomic and does not silently rewrite role membership"
    );
    su.batch_execute(&format!(
        "REVOKE {generation} FROM wamn_projection_rogue_member; \
         REVOKE wamn_effect_writer FROM {generation};"
    ))
    .await
    .expect("remove disposable rogue memberships");

    let impostor = "wamn_effect_writer_1111111111111111111111111111111111111111_b";
    su.batch_execute(&format!(
        "DO $impostor$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='{impostor}') THEN \
             CREATE ROLE {impostor} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               INHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $impostor$; \
         ALTER ROLE {impostor} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
           INHERIT NOREPLICATION NOBYPASSRLS; \
         GRANT wamn_run_projection_writer TO {impostor} \
           WITH ADMIN FALSE, INHERIT TRUE, SET FALSE; \
         DO $connect$ BEGIN EXECUTE format( \
           'GRANT CONNECT ON DATABASE %I TO {impostor}', current_database()); \
         END $connect$;"
    ))
    .await
    .expect("install projection-only connected generation impostor");
    let impostor_refusal = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect_err("projection-only connected generation impostor must fail closed");
    assert!(
        format!("{impostor_refusal:#}").contains("effect-writer-role-out-of-bounds"),
        "wrong connected-generation refusal: {impostor_refusal:#}"
    );
    let impostor_retained: bool = su
        .query_one(
            &format!(
                "SELECT has_database_privilege('{impostor}',current_database(),'CONNECT') \
                    AND pg_has_role('{impostor}','wamn_run_projection_writer','MEMBER') \
                    AND NOT pg_has_role('{impostor}','wamn_effect_writer','MEMBER')"
            ),
            &[],
        )
        .await
        .expect("read atomically retained connected impostor")
        .get(0);
    assert!(impostor_retained);
    su.batch_execute(&format!(
        "DO $disconnect$ BEGIN EXECUTE format( \
           'REVOKE CONNECT ON DATABASE %I FROM {impostor}', current_database()); \
         END $disconnect$; \
         REVOKE wamn_run_projection_writer FROM {impostor}; \
         ALTER ROLE {impostor} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';"
    ))
    .await
    .expect("remove disposable connected generation impostor authority");
    let clean = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("the writer ACL converges after authority removal");
    assert!(!clean.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
            && action.target == format!("{SCHEMA}.effect_attempts")
    }));
}

pub(super) async fn effect_writer_populated_refusal_leg(su: &Client) {
    for populated in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        reset(su).await;
        let schema = schema();
        su.batch_execute(CATALOG_SCHEMA_SQL)
            .await
            .expect("apply catalog for writer refusal");
        su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
            .await
            .expect("apply current run-state for writer refusal");
        su.batch_execute("SELECT set_config('app.tenant','t1',false)")
            .await
            .expect("prepare isolated incompatible ledger fact");
        match populated {
            "effect_attempts" => {
                su.batch_execute(&format!(
                    "INSERT INTO {SCHEMA}.effect_attempts \
                       (tenant_id,attempt_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                        local_node_id,source_artifact_hash,requirement_name,occurrence,seq, \
                        generation_fact_kind,attempt_deadline_at,attempt_input_ref) \
                     VALUES ('t1','00000000-0000-0000-0000-000000000491','r', \
                        $${EMPTY_EXECUTION_BUNDLE_HASH}$$,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,0, \
                        'n',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'manager',0,1, \
                        'not-required','2099-01-02 UTC','sha256:input');"
                ))
                .await
                .expect("seed incompatible attempt fact");
            }
            "effect_attempt_dispatches" => {
                su.batch_execute(&format!(
                    "ALTER TABLE {SCHEMA}.effect_attempt_dispatches \
                       DROP CONSTRAINT effect_attempt_dispatches_attempt_fk; \
                     INSERT INTO {SCHEMA}.effect_attempt_dispatches \
                       (tenant_id,attempt_id,attempt_started_at,run_id,frame_id, \
                        local_node_id,occurrence,dispatched_at) \
                     VALUES ('t1','00000000-0000-0000-0000-000000000492', \
                        '2026-01-01 UTC','r',0,'n',0,'2026-01-01 00:01 UTC');"
                ))
                .await
                .expect("seed incompatible dispatch fact");
            }
            "effect_attempt_outcomes" => {
                su.batch_execute(&format!(
                    "ALTER TABLE {SCHEMA}.effect_attempt_outcomes \
                       DROP CONSTRAINT effect_attempt_outcomes_dispatch_fk; \
                     INSERT INTO {SCHEMA}.effect_attempt_outcomes \
                       (tenant_id,attempt_id,dispatched_at,outcome_status,recorded_at) \
                     VALUES ('t1','00000000-0000-0000-0000-000000000493', \
                        '2026-01-01 UTC','success','2026-01-01 00:01 UTC');"
                ))
                .await
                .expect("seed incompatible outcome fact");
            }
            _ => unreachable!(),
        }
        su.batch_execute(&format!(
            "ALTER TABLE {SCHEMA}.effect_attempts ADD COLUMN attempt_key text;"
        ))
        .await
        .expect("install incompatible accepted-residue column");
        su.batch_execute("GRANT wamn_scenario_author TO wamn_app")
            .await
            .expect("install unrelated mutation sentinel");
        let before = effect_writer_schema_snapshot(su).await;
        let error = reconcile_run_plane::reconcile(su, &schema, true)
            .await
            .expect_err("populated incompatible writer ledger refuses");
        let postgres: tokio_postgres::Error = error.downcast().expect("postgres refusal");
        let database = postgres.as_db_error().expect("typed cutover refusal");
        assert_eq!(database.code().code(), "55000");
        assert_eq!(
            database.message(),
            "effect-writer-cutover-requires-empty-ledger"
        );
        assert_eq!(
            effect_writer_schema_snapshot(su).await,
            before,
            "{populated}: refusal leaves schema unchanged"
        );
        let membership_retained: bool = su
            .query_one(
                "SELECT pg_has_role('wamn_app','wamn_scenario_author','MEMBER')",
                &[],
            )
            .await
            .expect("read unrelated mutation sentinel")
            .get(0);
        assert!(
            membership_retained,
            "refusal precedes unrelated role repair"
        );
    }
}

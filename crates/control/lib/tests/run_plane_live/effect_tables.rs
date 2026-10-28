use super::{
    CATALOG_SCHEMA_SQL, Client, EMPTY_EXECUTION_BUNDLE_HASH, RUN_STATE_SQL, RunPlaneActionKind,
    SCHEMA, column_exists, connect, install_current_run_plane, locked_database,
    reconcile_run_plane, reset, rewrite_schema, schema, seed_run_admission_facts,
};

#[tokio::test]
async fn effect_table_privileges_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
    effect_table_privileges_leg(&su).await;
}

#[tokio::test]
async fn current_frame_identity_drift_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
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
                     UNIQUE (env_key,run_id,local_node_id,occurrence);"
            ),
        ),
    ] {
        reset(&su).await;
        su.batch_execute(CATALOG_SCHEMA_SQL)
            .await
            .expect("apply catalog for populated frame drift refusal");
        su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema()))
            .await
            .expect("apply current run-state for populated frame drift refusal");
        seed_run_admission_facts(&su, "t1", "frame_cat", "dev", "standard").await;
        su.batch_execute(&format!(
            "INSERT INTO {SCHEMA}.runs \
               (env_key,run_id,flow_id,flow_version,package_id,environment, \
                status) \
             VALUES ('t1',$${label}$$,'f',1,'frame_cat','dev','running'); \
             INSERT INTO {SCHEMA}.effect_attempts \
               (env_key,attempt_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                local_node_id,source_artifact_hash,requirement_name,occurrence,seq, \
                generation_fact_type,attempt_deadline_at,attempt_input_ref) \
             VALUES ('t1','00000000-0000-0000-0000-000000000413',$${label}$$, \
                     $${EMPTY_EXECUTION_BUNDLE_HASH}$$,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,0, \
                     'n',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'manager',0,0, \
                     'not-required','2099-01-02 UTC','sha256:input'); \
             {drift_sql}"
        ))
        .await
        .expect("seed populated frame drift");
        let before = effect_schema_snapshot(&su).await;
        let error = reconcile_run_plane::reconcile(&su, &schema(), true)
            .await
            .expect_err("populated late frame identity drift must refuse before DDL");
        assert!(
            format!("{error:#}").contains("effect-ledger-schema-drift"),
            "{label}: wrong refusal: {error:#}"
        );
        assert_eq!(
            effect_schema_snapshot(&su).await,
            before,
            "{label}: refusal must leave schema unchanged"
        );
    }
}

async fn effect_schema_snapshot(su: &Client) -> String {
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
    .expect("read current effect schema snapshot")
    .get(0)
}

pub(super) async fn effect_table_privileges_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    let schema = schema();
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
        "UNIQUE (env_key, run_id, frame_id, local_node_id, occurrence)"
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
        "FOREIGN KEY (env_key, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)"
    ));

    su.batch_execute(&format!(
        "GRANT SELECT ON {SCHEMA}.effect_attempts TO wamn_scenario_author; \
         GRANT UPDATE (attempt_input_ref) ON {SCHEMA}.effect_attempts TO wamn_app; \
         GRANT INSERT ON {SCHEMA}.effect_attempt_dispatches TO wamn_app; \
         GRANT INSERT ON {SCHEMA}.effect_attempt_outcomes TO wamn_app;"
    ))
    .await
    .expect("install table/column ACL drift");
    let repair = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("repair effect-table ACL drift");
    assert!(repair.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectTablePrivilege
            && action.target == format!("{SCHEMA}.effect_attempts")
    }));
    // THE CONVERGE PATH for the two sibling tables (wamn-0h0g.20.32): an append
    // granted directly on an ALREADY-PROVISIONED database is drift the
    // reconciler must REMOVE. The DDL alone cannot show this — it only shows
    // birth — so the drift above is installed on purpose.
    for table in ["effect_attempt_dispatches", "effect_attempt_outcomes"] {
        assert!(
            repair.actions.iter().any(|action| {
                action.kind == RunPlaneActionKind::RepairEffectTablePrivilege
                    && action.target == format!("{SCHEMA}.{table}")
            }),
            "the reconciler did not plan to remove the sibling table append on {table}"
        );
    }
    let privileges = su
        .query_one(
            &format!(
                "SELECT has_column_privilege('wamn_app','{SCHEMA}.effect_attempts', \
                                             'attempt_input_ref','UPDATE'), \
                        has_table_privilege('wamn_scenario_author', \
                                            '{SCHEMA}.effect_attempts','SELECT'), \
                        has_table_privilege('wamn_app', \
                                            '{SCHEMA}.effect_attempts','INSERT'), \
                        has_table_privilege('wamn_app', \
                                            '{SCHEMA}.effect_attempts','SELECT'), \
                        has_any_column_privilege('wamn_app', \
                                                 '{SCHEMA}.effect_attempts','INSERT'), \
                        has_table_privilege('wamn_app', \
                                            '{SCHEMA}.effect_attempt_dispatches','INSERT'), \
                        has_table_privilege('wamn_app', \
                                            '{SCHEMA}.effect_attempt_outcomes','INSERT'), \
                        EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app' \
                                 AND NOT (rolsuper OR rolbypassrls))"
            ),
            &[],
        )
        .await
        .expect("read converged effect-table ACL boundary");
    assert!(!privileges.get::<_, bool>(0));
    assert!(!privileges.get::<_, bool>(1));
    // BORN PARKED (wamn-0h0g.20.30). THE SERVER'S OWN ANSWER, not the DDL text:
    // once the reconciler converges, the guest role holds READ on the attempt
    // table and NO append — at table level or at any column. This is exactly
    // what a fresh project environment is born holding, and — because the drift
    // above was installed first — what an UPGRADED one converges to.
    assert!(
        !privileges.get::<_, bool>(2),
        "the reconciler re-minted a LIVE append authority on a parked table"
    );
    assert!(privileges.get::<_, bool>(3), "the guest keeps its read");
    assert!(
        !privileges.get::<_, bool>(4),
        "column-level append survived the table-level park"
    );
    // wamn-0h0g.20.32: both sibling tables are parked on the same footing, and
    // the append DIRECTLY granted above is gone — the reconciler removed it
    // rather than re-granting it.
    assert!(
        !privileges.get::<_, bool>(5),
        "the reconciler left a LIVE append on effect_attempt_dispatches"
    );
    assert!(
        !privileges.get::<_, bool>(6),
        "the reconciler left a LIVE append on effect_attempt_outcomes"
    );
    // A superuser or RLS-bypassing role would mask every refusal asserted above.
    assert!(privileges.get::<_, bool>(7));
    su.batch_execute("SET SESSION AUTHORIZATION wamn_app")
        .await
        .expect("enter the guest session");
    let direct_fact_append = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempts \
                   (env_key,attempt_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                    local_node_id,source_artifact_hash,requirement_name,occurrence,seq,generation_fact_type, \
                    attempt_started_at,attempt_deadline_at,attempt_input_ref) \
                 VALUES ('acme--billing--dev','00000000-0000-0000-0000-000000000509', \
                         'forged',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,$${EMPTY_EXECUTION_BUNDLE_HASH}$$,0, \
                         'effect',$${EMPTY_EXECUTION_BUNDLE_HASH}$$,'manager',0,0,'not-required', \
                         now(),now()+interval '1 minute','sha256:forged')"
            ),
            &[],
        )
        .await;
    su.batch_execute("RESET SESSION AUTHORIZATION")
        .await
        .expect("leave the guest session");
    let fact_error = direct_fact_append.expect_err("ordinary app cannot append immutable facts");
    super::assert_db_code(&fact_error, "42501", "effect-table ACL refusal");
    let clean = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("the effect-table ACL stays converged");
    assert!(!clean.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairEffectTablePrivilege
            && action.target == format!("{SCHEMA}.effect_attempts")
    }));
}

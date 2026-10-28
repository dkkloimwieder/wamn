use super::{
    CATALOG_SCHEMA_SQL, Client, RUN_QUEUE_SQL, RUN_STATE_SQL, SCHEMA, reconcile_run_plane, reset,
    rewrite_schema, schema, seed_run_admission_facts,
};

pub(super) async fn persisted_literal_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    // Install the current run plane.
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");
    // A run whose runaway verdict we will try to record.
    seed_run_admission_facts(su, "t1", "cat", "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (env_key,run_id,flow_id,flow_version,package_id, \
              environment) \
             VALUES ('t1','r-budget','f',1,'cat','dev');"
    ))
    .await
    .expect("seed a run");
    // The current constraint admits runaway-budget.
    let def: String = su
        .query_one(
            "SELECT pg_get_constraintdef(con.oid) FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = 'runs' \
               AND con.conname = 'runs_fail_type_check'",
            &[&SCHEMA],
        )
        .await
        .expect("read fail_type constraintdef")
        .get(0);
    assert!(
        def.contains("runaway-budget"),
        "reconciled CHECK admits runaway-budget: {def}"
    );

    // Store the runaway verdict.
    let updated = su
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET fail_type = 'runaway-budget' \
                 WHERE env_key = 't1' AND run_id = 'r-budget'"
            ),
            &[],
        )
        .await
        .expect("runaway verdict now accepted");
    assert_eq!(updated, 1, "the runaway verdict lands on the audit row");

    // The current plane needs no repairs.
    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan");
    assert!(again.is_noop(), "re-run is a no-op: {:#?}", again.actions);
}

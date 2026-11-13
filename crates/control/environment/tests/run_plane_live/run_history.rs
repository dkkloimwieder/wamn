use super::{
    CATALOG_SCHEMA_SQL, Client, RUN_QUEUE_SQL, RUN_STATE_SQL, SCHEMA, reconcile_run_plane, reset,
    rewrite_schema, schema, seed_run_admission_facts,
};
use sqlx::Row as _;

pub(super) async fn persisted_literal_leg(su: &mut Client) {
    reset(su).await;
    let schema = schema();
    sqlx::raw_sql(CATALOG_SCHEMA_SQL)
        .execute(&mut *su)
        .await
        .map(drop)
        .expect("apply catalog-schema");
    // Install the current run plane.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&rewrite_schema(RUN_STATE_SQL, &schema)).to_string(),
    ))
    .execute(&mut *su)
    .await
    .map(drop)
    .expect("apply run-state");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&rewrite_schema(RUN_QUEUE_SQL, &schema)).to_string(),
    ))
    .execute(&mut *su)
    .await
    .map(drop)
    .expect("apply run-queue");
    // A run whose runaway verdict we will try to record.
    seed_run_admission_facts(su, "t1", "cat", "dev").await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {SCHEMA}.runs \
             (project_env_key,run_id,wiring_id,wiring_version,wiring_hash,binding_world_json,package_id, \
              environment) \
             VALUES ('t1','r-budget','fixture-wiring',1,'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'cat','dev');"
    ))).execute(&mut *su).await.map(drop)
    .expect("seed a run");
    // The current constraint admits runaway-budget.
    let def: String = sqlx::query(
        "SELECT pg_get_constraintdef(con.oid) FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = 'runs' \
               AND con.conname = 'runs_fail_type_check'",
    )
    .bind(&SCHEMA)
    .fetch_one(&mut *su)
    .await
    .expect("read fail_type constraintdef")
    .get(0);
    assert!(
        def.contains("runaway-budget"),
        "reconciled CHECK admits runaway-budget: {def}"
    );

    // Store the runaway verdict.
    let updated = sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "UPDATE {SCHEMA}.runs SET fail_type = 'runaway-budget' \
                 WHERE project_env_key = 't1' AND run_id = 'r-budget'"
        ))
            .to_string(),
    ))
    .execute(&mut *su)
    .await
    .map(|result| result.rows_affected())
    .expect("runaway verdict now accepted");
    assert_eq!(updated, 1, "the runaway verdict lands on the audit row");

    // The current plane needs no repairs.
    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan");
    assert!(again.is_noop(), "re-run is a no-op: {:#?}", again.actions);
}

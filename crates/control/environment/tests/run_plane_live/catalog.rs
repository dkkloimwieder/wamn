use super::{
    CATALOG_SCHEMA_SQL, Client, RUN_QUEUE_SQL, RUN_STATE_SQL, SCHEMA, assert_db_code,
    drop_generation_role, mint_guest_generation, reconcile_run_plane, reset, rewrite_schema,
    schema, seed_run_admission_facts,
};
use sqlx::Row as _;

pub(super) async fn capture_mode_security_leg(su: &mut Client, url: &str) {
    reset(su).await;
    let schema = schema();
    sqlx::raw_sql(CATALOG_SCHEMA_SQL)
        .execute(&mut *su)
        .await
        .map(drop)
        .expect("apply catalog-schema");
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
    seed_run_admission_facts(su, "t1", "capture", "dev").await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {SCHEMA}.runs \
           (project_env_key,run_id,wiring_id,wiring_version,wiring_hash,binding_world_json,package_id,environment, \
            status) \
         VALUES ('t1','capture-off','fixture-wiring',1,'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'capture','dev', \
                 'completed');"
    ))).execute(&mut *su).await.map(drop)
    .expect("seed a completed run");

    let mode: String = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT capture_mode FROM {SCHEMA}.runs WHERE run_id='capture-off'"
    )))
    .fetch_one(&mut *su)
    .await
    .expect("read the default capture mode")
    .get(0);
    assert_eq!(mode, "off");

    let immutable = sqlx::query(sqlx::AssertSqlSafe(
        (&format!("UPDATE {SCHEMA}.runs SET capture_mode='full' WHERE run_id='capture-off'"))
            .to_string(),
    ))
    .execute(&mut *su)
    .await
    .map(|result| result.rows_affected())
    .expect_err("post-admission capture mutation refused");
    assert_db_code(&immutable, "55000", "capture mode is admission-immutable");
    let invalid = sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO {SCHEMA}.runs \
                   (project_env_key,run_id,wiring_id,wiring_version,wiring_hash,binding_world_json,package_id,environment, \
                    status,trigger_source,capture_mode) \
                 VALUES ('t1','published-full','fixture-wiring',1,'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'capture','dev', \
                         'completed','http','full')"
            ))).execute(&mut *su).await.map(|result| result.rows_affected())
        .expect_err("published full capture refused");
    assert_db_code(&invalid, "23514", "only direct draft rows may capture full");
    sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {SCHEMA}.runs \
               (project_env_key,run_id,wiring_id,wiring_version,wiring_hash,binding_world_json,package_id,environment, \
                status,trigger_source,capture_mode) \
             VALUES ('t1','draft-full','fixture-wiring',1,'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'capture','dev', \
                     'completed','scenario-draft','full')"
        ))).execute(&mut *su).await.map(|result| result.rows_affected())
    .expect("canonical direct draft may carry full");

    // Reconcile an out-of-band write grant before observing the application role.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "GRANT INSERT, UPDATE, REFERENCES ON {SCHEMA}.runs TO wamn_app"
    )))
    .execute(&mut *su)
    .await
    .map(drop)
    .expect("widen the application role for repair");
    reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("repair application write authority");
    let writable: bool = sqlx::query(
        "SELECT EXISTS (SELECT FROM pg_catalog.pg_attribute a \
              WHERE a.attrelid = $1::regclass AND a.attnum > 0 AND NOT a.attisdropped \
                AND (has_column_privilege('wamn_app', a.attrelid, a.attnum, 'INSERT') \
                  OR has_column_privilege('wamn_app', a.attrelid, a.attnum, 'UPDATE') \
                  OR has_column_privilege('wamn_app', a.attrelid, a.attnum, 'REFERENCES')))",
    )
    .bind(&format!("{SCHEMA}.runs"))
    .fetch_one(&mut *su)
    .await
    .expect("observe every surviving column privilege")
    .get(0);
    assert!(
        !writable,
        "reconciliation retained application write authority"
    );
    let (app_generation, mut app) = mint_guest_generation(su, url, "t1").await;
    for (label, refused) in [
        (
            "admission",
            format!(
                "INSERT INTO {SCHEMA}.runs \
                   (project_env_key,run_id,wiring_id,wiring_version,wiring_hash,binding_world_json,package_id,environment, \
                    status,trigger_source) \
                 VALUES ('t1','app-forged','fixture-wiring',1,'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'capture','dev','dispatched','test')"
            ),
        ),
        (
            "capture-mode update",
            format!("UPDATE {SCHEMA}.runs SET capture_mode='full' WHERE run_id='draft-full'"),
        ),
        (
            "ordinary update",
            format!("UPDATE {SCHEMA}.runs SET status='running' WHERE run_id='draft-full'"),
        ),
    ] {
        let denied = sqlx::query(sqlx::AssertSqlSafe((&refused).to_string()))
            .execute(&mut app)
            .await
            .map(|result| result.rows_affected())
            .expect_err(&format!("{label} must be refused"));
        assert_db_code(&denied, "42501", label);
    }
    let readable: i64 = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {SCHEMA}.runs WHERE run_id='draft-full'"
    )))
    .fetch_one(&mut app)
    .await
    .expect("the application role retains its project read")
    .get(0);
    assert_eq!(readable, 1);
    drop(app);
    drop_generation_role(su, &app_generation).await;

    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("capture carrier second reconcile plans");
    assert!(
        again.is_noop(),
        "capture carrier converged: {:#?}",
        again.actions
    );
}

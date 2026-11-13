use super::{
    CATALOG_SCHEMA_SQL, Client, RUN_QUEUE_SQL, RUN_STATE_SQL, SCHEMA, assert_db_code,
    drop_generation_role, mint_guest_generation, reconcile_run_plane, reset, rewrite_schema,
    schema, seed_run_admission_facts,
};

pub(super) async fn capture_mode_security_leg(su: &Client, url: &str) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");
    seed_run_admission_facts(su, "t1", "capture", "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (env_key,run_id,flow_id,flow_version,package_id,environment, \
            status) \
         VALUES ('t1','capture-off','f',1,'capture','dev', \
                 'completed');"
    ))
    .await
    .expect("seed a completed run");

    let mode: String = su
        .query_one(
            &format!("SELECT capture_mode FROM {SCHEMA}.runs WHERE run_id='capture-off'"),
            &[],
        )
        .await
        .expect("read the default capture mode")
        .get(0);
    assert_eq!(mode, "off");

    let immutable = su
        .execute(
            &format!("UPDATE {SCHEMA}.runs SET capture_mode='full' WHERE run_id='capture-off'"),
            &[],
        )
        .await
        .expect_err("post-admission capture mutation refused");
    assert_db_code(&immutable, "55000", "capture mode is admission-immutable");
    let invalid = su
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (env_key,run_id,flow_id,flow_version,package_id,environment, \
                    status,trigger_source,capture_mode) \
                 VALUES ('t1','published-full','f',1,'capture','dev', \
                         'completed','http','full')"
            ),
            &[],
        )
        .await
        .expect_err("published full capture refused");
    assert_db_code(&invalid, "23514", "only direct draft rows may capture full");
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.runs \
               (env_key,run_id,flow_id,flow_version,package_id,environment, \
                status,trigger_source,capture_mode) \
             VALUES ('t1','draft-full','f',1,'capture','dev', \
                     'completed','scenario-draft','full')"
        ),
        &[],
    )
    .await
    .expect("canonical direct draft may carry full");

    // wamn-0h0g.22.7 (b1d42599) replaced the capture-mode COLUMN confinement
    // with a whole-relation one: `wamn_app` holds table SELECT and DELETE on
    // `runs` and no write of any shape. The admission that used to show the
    // `off` default moved to the private management path, so what this leg
    // still shows live is the confinement the reconciler restores.
    // A MINTED GENERATION LOGIN, not the bare `wamn_app` ACL role: the read
    // below is governed by the tenant floor, under which the ACL role derives a
    // NULL key and matches nothing in silence (`wamn-0h0g.22.36`). The three
    // refusals are grant-level and hold either way; the generation inherits
    // exactly the ACL role's grants, so it is the same authority under test.
    let (app_generation, app) = mint_guest_generation(su, url, "t1").await;
    for (label, refused) in [
        (
            "admission",
            format!(
                "INSERT INTO {SCHEMA}.runs \
                   (env_key,run_id,flow_id,flow_version,package_id,environment, \
                    status,trigger_source) \
                 VALUES ('t1','app-forged','f',1,'capture','dev','dispatched','test')"
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
        let denied = app
            .execute(&refused, &[])
            .await
            .expect_err(&format!("{label} must be refused"));
        assert_db_code(&denied, "42501", label);
    }
    let readable: i64 = app
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE run_id='draft-full'"),
            &[],
        )
        .await
        .expect("the application role retains its tenant read")
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

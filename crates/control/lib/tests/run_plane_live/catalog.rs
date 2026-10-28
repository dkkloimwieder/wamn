use super::{
    CATALOG_SCHEMA_SQL, CONTROL_PORTABLE_STORE_SQL, Client, RUN_QUEUE_SQL, RUN_STATE_SQL,
    RunPlaneActionKind, SCHEMA, assert_db_code, column_exists, connect, drop_generation_role,
    install_current_run_plane, locked_database, mint_guest_generation, reconcile_run_plane, reset,
    rewrite_schema, schema, seed_run_admission_facts, table_exists,
};

/// Own entry so the two-plane post-check can be run — and mutated — alone.
#[tokio::test]
async fn two_plane_residency_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
    two_plane_residency_leg(&su).await;
}

/// The reconciler plans NOTHING on the author's project-plane surface that the
/// FRESH INSTALL just produced (wamn-0h0g.22.20, wamn-0h0g.22.43).
///
/// `AUTHORING_PRIVILEGE_SPECS` is the third emitter of the author's catalog
/// grants and the only one that runs on every reconcile, so a revoke landed in
/// `deploy/sql/catalog-schema.sql` and `ensure_authoring_catalog_privileges`
/// alone is silently RE-GRANTED here, and `authoring_privileges_drifted` would
/// report the revoked state AS DRIFT on every provisioned environment. The two
/// must therefore be read against each other on a real server rather than
/// against one another's constants.
///
/// Its own entry, and DRY-RUN only, on purpose. `current_noop_leg` makes the
/// same reading inside `run_plane_reconcile_live`, whose leg chain stops at its
/// first failing leg. Planning executes no action, so this reading stays
/// reachable on its own.
#[tokio::test]
async fn authoring_privileges_at_record_plan_no_repair_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
    reset(&su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    for ddl in [RUN_STATE_SQL, RUN_QUEUE_SQL] {
        su.batch_execute(&rewrite_schema(ddl, &schema))
            .await
            .expect("apply the run-plane record");
    }

    let dry = reconcile_run_plane::reconcile(&su, &schema, false)
        .await
        .expect("dry-run plans");
    let repairs: Vec<&str> = dry
        .actions
        .iter()
        .filter(|action| action.kind == RunPlaneActionKind::RepairAuthoringPrivilege)
        .map(|action| action.target.as_str())
        .collect();
    assert!(
        repairs.is_empty(),
        "the reconciler disagrees with the fresh install about the authoring \
         surface and would re-grant on every pass: {repairs:?}"
    );

    // The reading has to be able to SEE every converge path, or an empty plan
    // for any reason at all reads as agreement. Re-open one catalog read plus
    // both retired run-plane reads and require all three repairs.
    su.batch_execute(&format!(
        "GRANT SELECT ON catalog.releases TO wamn_scenario_author; \
         GRANT SELECT ON {SCHEMA}.environment_policies TO wamn_scenario_author; \
         GRANT SELECT ON {SCHEMA}.runs TO wamn_scenario_author"
    ))
    .await
    .expect("re-open the dormant author reads");
    let drifted = reconcile_run_plane::reconcile(&su, &schema, false)
        .await
        .expect("dry-run plans over the drifted surface");
    let mut drifted_repairs: Vec<&str> = drifted
        .actions
        .iter()
        .filter(|action| {
            matches!(
                action.kind,
                RunPlaneActionKind::RepairAuthoringPrivilege
                    | RunPlaneActionKind::RepairRunCapturePrivilege
            )
        })
        .map(|action| action.target.as_str())
        .collect();
    drifted_repairs.sort_unstable();
    assert_eq!(
        drifted_repairs,
        vec![
            "catalog.releases",
            "rp_live.environment_policies",
            "runs.capture_mode",
        ],
        "the three re-opened author reads must plan exactly three repairs"
    );

    reconcile_run_plane::reconcile(&su, &schema, true)
        .await
        .expect("apply the three author-read repairs");
    for relation in [
        "catalog.releases",
        "rp_live.environment_policies",
        "rp_live.runs",
    ] {
        let retains_read: bool = su
            .query_one(
                "SELECT has_table_privilege('wamn_scenario_author', $1, 'SELECT')",
                &[&relation],
            )
            .await
            .expect("read the repaired author privilege")
            .get(0);
        assert!(!retains_read, "author SELECT survived on {relation}");
    }
}

/// The CONTROL plane's `wamn_run` residency, created from the deployed control
/// store's OWN declaration text rather than a hand copy that could drift from it.
///
/// `wamn_run.gate_reports` is the relation `protected_relations_live.rs` names
/// CONTROL-ONLY: the control portable store installs it and no project installer
/// recreates it. Both planes spell the schema `wamn_run`, so a project-plane
/// reconciler pointed at a control database sees this relation sitting in the
/// schema it is about to converge.
async fn install_control_plane_residency(su: &Client) {
    let declaration = CONTROL_PORTABLE_STORE_SQL
        .split_once("CREATE TABLE wamn_run.gate_reports (")
        .expect("the control portable store declares the gate-report relation")
        .1
        .split_once("\n);")
        .expect("the gate-report declaration is terminated")
        .0;
    su.batch_execute(&format!(
        "CREATE TABLE {SCHEMA}.gate_reports ({declaration});"
    ))
    .await
    .expect("install the control plane's same-schema residency");
}

/// TWO-PLANE RESIDENCY (wamn-0h0g.12.177). The reconciler's post-check must call
/// the PROJECT record — `run-state.sql` plus `run-queue.sql` — and nothing else,
/// on a database that also carries the CONTROL plane's `wamn_run` residency.
///
/// **Why the drift is installed first.** `current_noop_leg` shows the FRESH
/// path: a schema already at record plans nothing. That is the R55 shape
/// wamn-0h0g.12.1 measured on the control store's own gate — a virgin install
/// whose every convergence arm sits behind a presence probe that is false both
/// times. This leg therefore starts from a DRIFTED (already-provisioned)
/// schema, so the post-check is read off a CONVERGED database, and the second
/// run's no-op is read from `pg_class` and `pg_attribute` rather than from an
/// exit status.
///
/// **Why the residency is the discriminator.** A post-check that reached the
/// other plane's record could pass while the plane it guards has drifted. So
/// this asserts both directions: the seven PROJECT record tables are at target,
/// and the control-plane relation is neither counted as a record table, nor
/// named by any action, nor amputated — the project reconciler must leave the
/// other plane's record exactly as it found it.
pub(super) async fn two_plane_residency_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    install_current_run_plane(su).await;
    install_control_plane_residency(su).await;

    // Real drift, so this is an UPGRADE and not a fresh install: a record column
    // the reconciler must restore.
    su.batch_execute(&format!(
        "ALTER TABLE {SCHEMA}.run_queue DROP COLUMN lease_owner;"
    ))
    .await
    .expect("install converge-path drift");

    let converge = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("converge the drifted project plane");
    assert!(
        !converge.is_noop(),
        "the drift did not take — this leg would check the fresh path only"
    );
    assert!(
        converge.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::AddColumn && action.target == "run_queue.lease_owner"
        }),
        "the converge pass did not restore the record column: {:#?}",
        converge.actions
    );
    // THE PLANE LINE. No action may name the other plane's relation.
    {
        let plan = &converge;
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.target.contains("gate_reports")),
            "the project reconciler planned against the CONTROL plane's record: {:#?}",
            plan.actions
        );
        assert!(
            !plan.at_target.iter().any(|table| table == "gate_reports"),
            "the control-plane relation was counted as a project record table: {:?}",
            plan.at_target
        );
    }

    // R55: the SECOND run is a no-op, in apply mode and in dry-run mode.
    let again = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("second reconcile on the converged database");
    assert!(
        again.is_noop(),
        "the converged database still plans work: {:#?}",
        again.actions
    );
    let dry = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("read-only third reconcile");
    assert!(dry.is_noop(), "dry-run drift: {:#?}", dry.actions);
    // …and the post-check names the PROJECT record exactly: the six run-state
    // tables plus run_queue, with the co-resident control relation excluded.
    let mut at_target = dry.at_target.clone();
    at_target.sort();
    assert_eq!(
        at_target,
        [
            "effect_attempt_dispatches",
            "effect_attempt_outcomes",
            "effect_attempts",
            "environment_policies",
            "operator_run_actions",
            "run_queue",
            "runs",
        ],
        "the converged post-check did not name exactly the project record"
    );

    // POST-STATE FROM THE CATALOG, not from an exit status. Every project record
    // table is present…
    for table in [
        "environment_policies",
        "runs",
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
        "operator_run_actions",
        "run_queue",
    ] {
        assert!(
            table_exists(su, SCHEMA, table).await,
            "project record table {table} is absent after convergence"
        );
    }
    assert!(
        column_exists(su, "run_queue", "lease_owner").await,
        "the restored record column is absent from the catalog"
    );
    // …and the other plane's relation survives, with its own columns, untouched.
    assert!(
        table_exists(su, SCHEMA, "gate_reports").await,
        "the project reconciler amputated the CONTROL plane's relation"
    );
    let residency_columns: String = su
        .query_one(
            "SELECT string_agg(attribute.attname, ',' ORDER BY attribute.attnum) \
               FROM pg_catalog.pg_attribute AS attribute \
              WHERE attribute.attrelid = pg_catalog.to_regclass($1::text) \
                AND attribute.attnum > 0 AND NOT attribute.attisdropped",
            &[&format!("{SCHEMA}.gate_reports")],
        )
        .await
        .expect("read the co-resident control relation from the catalog")
        .get(0);
    assert_eq!(
        residency_columns,
        "created_at,created_by,updated_at,updated_by,env_key,wiring_hash,passed,summary,gated_at",
        "the project reconciler rewrote the CONTROL plane's record"
    );

    reset(su).await;
}

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

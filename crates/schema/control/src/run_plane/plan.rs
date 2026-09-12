//! Order schema changes and privilege repairs from observed database facts.

use std::collections::{
    BTreeMap, BTreeSet,
};

use wamn_catalog::CATALOG_SCHEMA_SQL;

use super::schema::{
    index_definition_stale, index_statements, normalize_observed_schema, quote_ident,
    record_columns, record_table_names, record_tables, schema_header_section, table_section,
    table_section_carries_trigger,
};

use super::{
    BareSchemaName, EFFECT_WRITER_ROLE, LEGACY_OUTBOX_TABLES, OUTBOX_TRIGGER_NAME,
    RUN_PLANE_FILES, RUN_STATE_SQL, RowPolicyObservation, RowSecurityObservation,
    RunPlaneAction, RunPlaneActionKind, RunPlaneObservation, RunPlanePlan,
    SCENARIO_AUTHOR_ROLE, ensure_scenario_author_role_sql,
    generation_role_contract_violation_sql, rewrite_schema,
    strip_retired_registration_keys_sql,
};

use super::declarations::{
    AUTHORING_PRIVILEGE_SPECS, AuthoringTableSchema, CHECK_SPECS, CheckOrigin,
    EFFECT_DISPATCH_ATTEMPT_FK_DEF, EFFECT_DISPATCH_ATTEMPT_FK_NAME,
    EFFECT_DISPATCH_ATTEMPT_FK_SQL, EFFECT_FRAME_COLUMNS, EFFECT_OUTCOME_DISPATCH_FK_DEF,
    EFFECT_OUTCOME_DISPATCH_FK_NAME, EFFECT_OUTCOME_DISPATCH_FK_SQL,
    EFFECT_WRITER_RUN_READ_COLUMNS, ENVIRONMENT_POLICY_TENANT_QUAL,
    RETIRED_EFFECT_ATTEMPT_COLUMNS, TABLE_PRIVILEGE_TYPES, helper_specs, trigger_specs,
};

use super::schema_changes::{
    RETIRED_CHILD_RUN_COLUMNS, RETIRED_FAILURE_DETAIL_COLUMNS, RETIRED_PARTITION_CHECK,
    RETIRED_PARTITION_COLUMNS, RETIRED_RERUN_LINEAGE_COLUMNS,
    RETIRED_TEST_SET_REFERENCE_COLUMN, RETIRED_TEST_SET_REFERENCE_TABLES,
    child_run_cutover_needed, child_run_cutover_sql, effect_writer_cutover_owned_check,
    effect_writer_cutover_sql, effect_writer_ledger_cutover_needed,
    execution_bundle_cutover_needed, execution_bundle_cutover_sql,
    failure_detail_cutover_needed, failure_detail_cutover_sql, frame_identity_check,
    frame_identity_column, frame_identity_cutover_sql, frame_identity_cutover_targets,
    partition_plane_cutover_needed, partition_plane_cutover_sql, rerun_lineage_cutover_needed,
    rerun_lineage_cutover_sql, retired_child_run_check,
    retired_effect_disposition_cutover_needed, retired_effect_disposition_cutover_sql,
    run_queue_claim_index_ready, run_wiring_identity_contract_complete,
    stored_suite_cutover_needed, stored_suite_cutover_sql, wiring_identity_cutover_sql,
};

pub(super) fn environment_policy_row_security_at_record() -> RowSecurityObservation {
    RowSecurityObservation {
        enabled: true,
        forced: true,
        policies: BTreeMap::from([
            (
                "environment_policies_tenant".to_string(),
                RowPolicyObservation {
                    command: "select".to_string(),
                    permissive: true,
                    roles: BTreeSet::from(["wamn_app".to_string()]),
                    using_expression: Some(ENVIRONMENT_POLICY_TENANT_QUAL.to_string()),
                    check_expression: None,
                },
            ),
            (
                "environment_policies_platform".to_string(),
                RowPolicyObservation {
                    command: "select".to_string(),
                    permissive: true,
                    roles: BTreeSet::from(["wamn_platform".to_string()]),
                    using_expression: Some("true".to_string()),
                    check_expression: None,
                },
            ),
        ]),
    }
}

/// The convergent mirror of `deploy/sql/run-state.sql`'s `runs` ACL block.
///
/// The `wamn_run_retention` arm (`wamn-0h0g.12.69`) is EXISTENCE-GUARDED rather
/// than emitted bare: the stable retention ACL role is created by the schema
/// header section and by provisioning, and a repair that names a role the target
/// cluster has not been bootstrapped with would fail the whole batch instead of
/// converging the privileges it CAN. Its `REVOKE`s precede its `GRANT` for the
/// same reason every other arm's do — the batch must narrow a widened grant, not
/// merely add the intended one — and BOTH a column-wide and a table-wide
/// `REVOKE` are issued, because a table-level `GRANT SELECT` and a per-column
/// one are separate ACL entries and revoking either alone leaves the other
/// standing. The re-grant is column-scoped to exactly the three columns the
/// prune statement's `WHERE` clause reads: retention is a `wamn_platform`
/// member, that group's floor arm is `USING (true)`, and a table-level `SELECT`
/// would therefore expose every tenant's run payload columns.
fn repair_run_capture_privilege_sql(
    schema: &BareSchemaName,
    available_columns: impl IntoIterator<Item = String>,
) -> String {
    let available_columns = available_columns.into_iter().collect::<BTreeSet<_>>();
    debug_assert!(available_columns.contains("capture_mode"));
    let all_columns = available_columns
        .iter()
        .map(|column| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");
    let qualified = format!("{}.runs", schema.quoted());
    format!(
        "LOCK TABLE {qualified} IN ACCESS EXCLUSIVE MODE; \
         REVOKE SELECT ({all_columns}), INSERT ({all_columns}), \
                UPDATE ({all_columns}), REFERENCES ({all_columns}) \
           ON TABLE {qualified} FROM PUBLIC, wamn_app, {SCENARIO_AUTHOR_ROLE}; \
         REVOKE ALL PRIVILEGES ON TABLE {qualified} \
           FROM PUBLIC, wamn_app, {SCENARIO_AUTHOR_ROLE}; \
         GRANT SELECT, DELETE ON TABLE {qualified} TO wamn_app; \
         DO $run_retention_acl$ BEGIN \
           IF EXISTS (SELECT FROM pg_catalog.pg_roles \
                       WHERE rolname = 'wamn_run_retention') THEN \
             EXECUTE 'REVOKE ALL PRIVILEGES ({all_columns}) ON TABLE {qualified} \
                        FROM wamn_run_retention'; \
             EXECUTE 'REVOKE ALL PRIVILEGES ON TABLE {qualified} FROM wamn_run_retention'; \
             EXECUTE 'GRANT SELECT (\"tenant_id\", \"status\", \"created_at\"), DELETE \
                        ON TABLE {qualified} TO wamn_run_retention'; \
           END IF; \
         END $run_retention_acl$; \
         DO $run_capture_acl$ BEGIN \
           IF EXISTS ( \
                SELECT 1 \
                  FROM unnest(ARRAY['wamn_app','{SCENARIO_AUTHOR_ROLE}']) actor, \
                       unnest(ARRAY['INSERT','UPDATE']) privilege \
                 WHERE pg_catalog.has_any_column_privilege( \
                   actor, '{qualified}', privilege)) \
              OR EXISTS ( \
                   SELECT 1 \
                     FROM pg_catalog.pg_class relation \
                     JOIN pg_catalog.pg_namespace namespace \
                       ON namespace.oid = relation.relnamespace \
                     JOIN pg_catalog.pg_attribute attribute \
                       ON attribute.attrelid = relation.oid \
                      AND attribute.attname = 'capture_mode' \
                     CROSS JOIN LATERAL \
                       pg_catalog.aclexplode(attribute.attacl) acl \
                    WHERE relation.oid = pg_catalog.to_regclass('{qualified}') \
                      AND acl.grantee = 0 \
                      AND acl.privilege_type IN ('INSERT','UPDATE')) \
              OR NOT pg_catalog.has_table_privilege( \
                   'wamn_app', '{qualified}', 'SELECT') \
              OR NOT pg_catalog.has_table_privilege( \
                   'wamn_app', '{qualified}', 'DELETE') \
              OR EXISTS ( \
                   SELECT 1 \
                     FROM unnest(ARRAY[ \
                       'INSERT','UPDATE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                    WHERE pg_catalog.has_table_privilege( \
                      'wamn_app', '{qualified}', privilege)) \
              OR EXISTS ( \
                   SELECT 1 \
                     FROM unnest(ARRAY[ \
                       'SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                    WHERE pg_catalog.has_table_privilege( \
                      '{SCENARIO_AUTHOR_ROLE}', '{qualified}', privilege)) \
              OR pg_catalog.has_any_column_privilege( \
                   '{SCENARIO_AUTHOR_ROLE}', '{qualified}', \
                   'SELECT,INSERT,UPDATE,REFERENCES') \
              OR EXISTS ( \
                   SELECT 1 \
                     FROM pg_catalog.pg_class relation \
                     CROSS JOIN LATERAL pg_catalog.aclexplode( \
                       COALESCE(relation.relacl, \
                                pg_catalog.acldefault('r', relation.relowner))) acl \
                    WHERE relation.oid = pg_catalog.to_regclass('{qualified}') \
                      AND acl.grantee = 0) \
              OR (SELECT owner.rolname \
                    FROM pg_catalog.pg_class relation \
                    JOIN pg_catalog.pg_roles owner ON owner.oid = relation.relowner \
                   WHERE relation.oid = pg_catalog.to_regclass('{qualified}')) \
                   IN ('wamn_app', '{SCENARIO_AUTHOR_ROLE}') \
           THEN RAISE EXCEPTION USING ERRCODE = '42501', \
                MESSAGE = 'run-capture-author-sql-write-authority'; \
           END IF; \
         END $run_capture_acl$"
    )
}

fn run_capture_privileges_drifted(schema: &BareSchemaName, obs: &RunPlaneObservation) -> bool {
    if !obs.tables.contains_key("runs") {
        return false;
    }

    let expected = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_string())
            .collect::<BTreeSet<_>>()
    };
    let key = |grantee: &str| {
        (
            schema.as_str().to_string(),
            "runs".to_string(),
            grantee.to_string(),
        )
    };
    let observed =
        |map: &BTreeMap<_, _>, grantee: &str| map.get(&key(grantee)).cloned().unwrap_or_default();

    observed(&obs.authoring_table_privileges, "PUBLIC") != BTreeSet::new()
        || observed(&obs.authoring_table_privileges, "wamn_app") != expected(&["SELECT", "DELETE"])
        || observed(&obs.authoring_table_privileges, SCENARIO_AUTHOR_ROLE) != BTreeSet::new()
        || observed(&obs.authoring_effective_table_privileges, "wamn_app")
            != expected(&["SELECT", "DELETE"])
        || observed(
            &obs.authoring_effective_table_privileges,
            SCENARIO_AUTHOR_ROLE,
        ) != BTreeSet::new()
        || observed(&obs.authoring_effective_column_privileges, "wamn_app") != expected(&["SELECT"])
        || observed(
            &obs.authoring_effective_column_privileges,
            SCENARIO_AUTHOR_ROLE,
        ) != BTreeSet::new()
        || obs
            .authoring_table_owners
            .get(&(schema.as_str().to_string(), "runs".to_string()))
            .is_some_and(|owner| owner == "wamn_app" || owner == SCENARIO_AUTHOR_ROLE)
        || obs.app_run_capture_privileges.0
        || obs.app_run_capture_privileges.1
        || !obs.app_run_capture_privileges.2
}

/// The generation-role floor the PROVISIONER mints, restated as a refusal.
///
/// The membership edge term is `SET FALSE`, not `SET TRUE`: `INHERIT TRUE, SET
/// FALSE` gives the login generation the stable ACL role's privileges while
/// denying it `SET ROLE` to BECOME that role — the "rotating login generations
/// with no `SET ROLE` escape" of `docs/exe-model.md`, and the posture that keeps
/// `current_user` an honest RLS input. `wamn-0h0g.12.178`: this predicate was
/// authored against the `SET TRUE` grant of `f0d18024`; `358f6792` tightened the
/// provisioner's grant, its state probe, and its own violation check to `SET
/// FALSE` without reaching here, leaving the reconciler demanding the LOOSER
/// shape and refusing every environment `provision-project-env --prepare`
/// actually mints. `provisioner_minted_generation_leg` is the arm that meets
/// the real prepared shape.
/// Reconcile one project-env's run-plane schema (+ the per-database `catalog`
/// metadata schema) against the schema of record. Pure: `obs` is what the
/// driver read; the returned plan is what it should execute, in order.
pub fn plan_run_plane(schema: &BareSchemaName, obs: &RunPlaneObservation) -> RunPlanePlan {
    let mut plan = RunPlanePlan::default();
    if obs.tables.contains_key("node_runs") {
        let target = schema.quoted();
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RetireNodeRuns,
            target: format!("{}.node_runs", schema.as_str()),
            sql: format!(
                r#"LOCK TABLE {target}.node_runs IN ACCESS EXCLUSIVE MODE;
-- Populated rows are deliberately discarded without archive: node_runs held
-- only dead mutable projection coordinates, while runs retains run history.
DROP TABLE IF EXISTS {target}.node_runs RESTRICT;
DO $retire_run_projection_authority$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_catalog.pg_roles
                WHERE rolname = 'wamn_run_projection_writer') THEN
        EXECUTE pg_catalog.format(
            'REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA %I FROM wamn_run_projection_writer',
            '{schema}'
        );
        EXECUTE pg_catalog.format(
            'REVOKE ALL PRIVILEGES ON SCHEMA %I FROM wamn_run_projection_writer',
            '{schema}'
        );
    END IF;
END
$retire_run_projection_authority$;"#,
                schema = schema.as_str(),
            ),
        });
        return plan;
    }
    if execution_bundle_cutover_needed(obs) {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RetireExecutionBundles,
            target: "catalog.execution-bundles".to_string(),
            sql: execution_bundle_cutover_sql(schema, obs),
        });
        return plan;
    }
    let has_runs = obs.tables.contains_key("runs");
    let capture_mode_present = obs
        .tables
        .get("runs")
        .is_some_and(|columns| columns.contains("capture_mode"));
    let run_capture_privileges_drifted = run_capture_privileges_drifted(schema, obs);
    let wiring_identity_cutover_needed = has_runs && !run_wiring_identity_contract_complete(obs);

    let effect_writer_ledger_cutover_needed = effect_writer_ledger_cutover_needed(schema, obs);
    if effect_writer_ledger_cutover_needed && obs.effect_ledger_rows != 0 {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EffectWriterCutover,
            target: "effect-ledgers.coordinate-writer-boundary".to_string(),
            sql: effect_writer_cutover_sql(schema, obs),
        });
        return plan;
    }

    let failure_detail_cutover_needed = failure_detail_cutover_needed(obs);
    let partition_plane_cutover_needed = partition_plane_cutover_needed(obs);
    if partition_plane_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::PartitionPlaneCutover,
            target: "run_queue.partition-plane".to_string(),
            sql: partition_plane_cutover_sql(schema, obs),
        });
    }

    let child_run_cutover_needed = child_run_cutover_needed(obs);
    if child_run_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::ChildRunCutover,
            target: "runs.durable-child-state".to_string(),
            sql: child_run_cutover_sql(schema, obs),
        });
    }

    let rerun_lineage_cutover_needed = rerun_lineage_cutover_needed(obs);
    if rerun_lineage_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RerunLineageCutover,
            target: "runs.rerun-lineage".to_string(),
            sql: rerun_lineage_cutover_sql(schema),
        });
    }

    let stored_suite_cutover_needed = stored_suite_cutover_needed(obs);
    if stored_suite_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::StoredSuiteCutover,
            target: "stored-suite-persistence".to_string(),
            sql: stored_suite_cutover_sql(schema, obs),
        });
    }

    if retired_effect_disposition_cutover_needed(obs) {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RetiredEffectDispositionCutover,
            target: "effect-disposition-persistence".to_string(),
            sql: retired_effect_disposition_cutover_sql(schema, obs),
        });
    }

    if obs
        .effect_writer_role
        .is_none_or(|role| !role.is_acl_only())
    {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::VerifyEffectWriterRole,
            target: EFFECT_WRITER_ROLE.to_string(),
            sql: format!("DO $effect_writer_role$ \
                  DECLARE role_oid oid; \
                  BEGIN \
                    SELECT oid INTO role_oid FROM pg_catalog.pg_roles \
                     WHERE rolname = 'wamn_effect_writer' AND NOT rolcanlogin \
                       AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole \
                       AND NOT rolinherit AND NOT rolreplication AND NOT rolbypassrls; \
                    IF role_oid IS NULL \
                       OR pg_catalog.has_database_privilege(role_oid, current_database(), 'CONNECT') \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_class WHERE relowner = role_oid) \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspowner = role_oid) \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_proc WHERE proowner = role_oid) \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datdba = role_oid) \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members WHERE member = role_oid) \
                       OR EXISTS ( \
                            SELECT 1 FROM pg_catalog.pg_auth_members AS membership \
                            JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member \
                            WHERE membership.roleid = role_oid \
                              AND (member.rolname !~ '^wamn_effect_writer_[0-9a-f]{{40}}_[ab]$' \
                                   OR NOT member.rolcanlogin OR member.rolsuper \
                                   OR member.rolcreatedb OR member.rolcreaterole \
                                   OR NOT member.rolinherit OR member.rolreplication \
                                   OR member.rolbypassrls)) \
                       OR {generation_contract} \
                    THEN RAISE EXCEPTION USING ERRCODE = '42501', \
                         MESSAGE = 'effect-writer-role-out-of-bounds'; \
                    END IF; \
                  END $effect_writer_role$",
                generation_contract = generation_role_contract_violation_sql(),
            ),
        });
    }
    let frame_cutover_targets = frame_identity_cutover_targets(obs, schema);
    if frame_cutover_targets.needed() {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::FrameIdentityCutover,
            target: "effect_attempts.frame-identity".to_string(),
            sql: frame_identity_cutover_sql(schema, frame_cutover_targets),
        });
    }
    if effect_writer_ledger_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EffectWriterCutover,
            target: "effect-ledgers.coordinate-writer-boundary".to_string(),
            sql: effect_writer_cutover_sql(schema, obs),
        });
    }
    // This is the final pre-role-bootstrap migration. Keeping it behind the
    // older guarded cutovers preserves their refusal ordering on compound
    // legacy schemas, while `RESTRICT` dependency failures still happen before
    // the shell creates/hardens roles or repairs membership.
    if failure_detail_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::FailureDetailCutover,
            target: "runs.failure-detail".to_string(),
            sql: failure_detail_cutover_sql(schema),
        });
    }
    let retire_invocation_admissions = obs.tables.contains_key("invocation_admissions");
    let retire_catalog_head_lock = obs.helper_functions.contains_key("lock_catalog_head");
    if retire_invocation_admissions || retire_catalog_head_lock {
        let mut statements = Vec::new();
        if retire_invocation_admissions {
            statements.push(format!(
                "DROP TABLE {}.invocation_admissions RESTRICT",
                schema.quoted()
            ));
        }
        if retire_catalog_head_lock {
            statements.push(format!(
                "DROP FUNCTION {}.lock_catalog_head(text, text, text) RESTRICT",
                schema.quoted()
            ));
        }
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RetireLegacyAdmissionSurface,
            target: "legacy-admission-surface".to_string(),
            sql: statements.join("; "),
        });
    }

    // The standalone record files grant schema visibility to this reserved
    // role, so it must exist (and remain non-login/non-bypass) before a missing
    // schema header executes. It intentionally owns no project-plane table read.
    if obs
        .scenario_author_role
        .is_none_or(|role| !role.is_host_only())
    {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EnsureScenarioAuthorRole,
            target: SCENARIO_AUTHOR_ROLE.to_string(),
            sql: ensure_scenario_author_role_sql().to_string(),
        });
    }
    if obs.app_is_scenario_author_member {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairAuthoringPrivilege,
            target: "wamn_app-membership".to_string(),
            sql: format!("REVOKE {SCENARIO_AUTHOR_ROLE} FROM wamn_app"),
        });
    }
    if capture_mode_present && run_capture_privileges_drifted {
        let available_columns = obs
            .tables
            .get("runs")
            .expect("capture_mode is present only on a present runs table")
            .iter()
            .filter(|column| {
                (!child_run_cutover_needed || !RETIRED_CHILD_RUN_COLUMNS.contains(&column.as_str()))
                    && (!rerun_lineage_cutover_needed
                        || !RETIRED_RERUN_LINEAGE_COLUMNS.contains(&column.as_str()))
                    && (!failure_detail_cutover_needed
                        || !RETIRED_FAILURE_DETAIL_COLUMNS.contains(&column.as_str()))
            })
            .cloned();
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairRunCapturePrivilege,
            target: "runs.capture_mode".to_string(),
            sql: repair_run_capture_privilege_sql(schema, available_columns),
        });
    }

    // 1. Missing run-plane tables → EnsureSchema once, then per-table sections
    //    in file order so retained foreign keys resolve.
    let mut any_missing = false;
    let mut creates = Vec::new();
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            if obs.tables.contains_key(&table) {
                continue;
            }
            any_missing = true;
            creates.push(RunPlaneAction {
                kind: RunPlaneActionKind::CreateTable,
                target: table.clone(),
                sql: rewrite_schema(&table_section(file, "wamn_run", &table), schema),
            });
        }
    }
    if any_missing {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EnsureSchema,
            target: schema.to_string(),
            sql: rewrite_schema(&schema_header_section(RUN_STATE_SQL, "wamn_run"), schema),
        });
    }
    // Helpers precede table sections: missing-table sections carry triggers,
    // and those triggers must resolve their functions at CREATE time.
    let helper_specs = helper_specs();
    for spec in &helper_specs {
        if obs
            .helper_functions
            .get(spec.name)
            .is_none_or(|definition| {
                normalize_observed_schema(definition, schema) != spec.definition.as_ref()
            })
        {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::RepairHelperFunction,
                target: spec.name.to_string(),
                sql: rewrite_schema(&spec.sql, schema),
            });
        }
    }
    // Catalog storage converges before run-plane constraint reconciliation:
    // attested rows derive their portable connection name from the pinned flow
    // graph, and the next runtime child reads the nullable provenance columns.
    if !obs.catalog_schema_present {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EnsureCatalogSchema,
            target: "catalog".to_string(),
            sql: CATALOG_SCHEMA_SQL.to_string(),
        });
    } else {
        for table in record_tables(CATALOG_SCHEMA_SQL, "catalog") {
            if !obs.catalog_tables.contains(&table) {
                plan.actions.push(RunPlaneAction {
                    kind: RunPlaneActionKind::CreateCatalogTable,
                    target: table.clone(),
                    sql: table_section(CATALOG_SCHEMA_SQL, "catalog", &table),
                });
            }
        }
    }

    // `runs` now has immediate catalog FKs, so catalog creation must precede
    // every missing run-table section.
    plan.actions.extend(creates);

    if obs.app_run_queue_authority {
        let columns = obs
            .tables
            .get("run_queue")
            .into_iter()
            .flatten()
            .filter(|column| {
                !partition_plane_cutover_needed
                    || !RETIRED_PARTITION_COLUMNS.contains(&column.as_str())
            })
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ");
        let column_revoke = if columns.is_empty() {
            String::new()
        } else {
            format!(
                "; REVOKE SELECT ({columns}), INSERT ({columns}), UPDATE ({columns}), \
                 REFERENCES ({columns}) ON TABLE {}.run_queue FROM PUBLIC, wamn_app",
                schema.quoted()
            )
        };
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RemoveAppRunQueueAuthority,
            target: format!("{}.run_queue.app-authority", schema.as_str()),
            sql: format!(
                "REVOKE ALL PRIVILEGES ON TABLE {}.run_queue FROM PUBLIC, wamn_app{column_revoke}",
                schema.quoted()
            ),
        });
    }

    if obs.effect_writer_schema_privileges != (true, false) {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairEffectWriterPrivilege,
            target: format!("{}.usage", schema.as_str()),
            sql: format!(
                "REVOKE ALL PRIVILEGES ON SCHEMA {} FROM PUBLIC, {EFFECT_WRITER_ROLE}; \
                 GRANT USAGE ON SCHEMA {} TO {EFFECT_WRITER_ROLE}; \
                 DO $effect_writer_schema_acl$ BEGIN \
                   IF NOT pg_catalog.has_schema_privilege('{EFFECT_WRITER_ROLE}', '{}', 'USAGE') \
                      OR pg_catalog.has_schema_privilege('{EFFECT_WRITER_ROLE}', '{}', 'CREATE') \
                   THEN RAISE EXCEPTION USING ERRCODE = '42501', \
                        MESSAGE = 'effect-writer-schema-privilege-out-of-bounds'; \
                   END IF; \
                 END $effect_writer_schema_acl$",
                schema.quoted(),
                schema.quoted(),
                schema.as_str(),
                schema.as_str(),
            ),
        });
    }
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        if !obs.tables.contains_key(table) {
            continue;
        }
        // BORN PARKED (owner ruling on wamn-0h0g.20.28, widened to the two sibling
        // ledgers by wamn-0h0g.20.32). NO effect ledger's APPEND authority is part
        // of the record: the writer primitive is unwired, and every generation
        // login inherits this role with INHERIT TRUE. So a live INSERT on any of
        // the three is DRIFT, and this convergent step REMOVES it rather than
        // re-granting it. Whoever wires the writer grants those INSERTs here.
        let writer_privileges: &[&str] = &["SELECT"];
        let expected = |grantee: &str| -> BTreeSet<String> {
            match grantee {
                "wamn_app" => ["SELECT"].into_iter().map(str::to_string).collect(),
                EFFECT_WRITER_ROLE => writer_privileges
                    .iter()
                    .copied()
                    .map(str::to_string)
                    .collect(),
                "PUBLIC" | SCENARIO_AUTHOR_ROLE => BTreeSet::new(),
                _ => unreachable!("closed effect-ledger grantee set"),
            }
        };
        let direct_drifted = [
            "PUBLIC",
            "wamn_app",
            SCENARIO_AUTHOR_ROLE,
            EFFECT_WRITER_ROLE,
        ]
        .into_iter()
        .any(|grantee| {
            obs.effect_ledger_table_privileges
                .get(&(table.to_string(), grantee.to_string()))
                .cloned()
                .unwrap_or_default()
                != expected(grantee)
        });
        let effective_drifted = ["wamn_app", SCENARIO_AUTHOR_ROLE, EFFECT_WRITER_ROLE]
            .into_iter()
            .any(|grantee| {
                obs.effect_ledger_effective_privileges
                    .get(&(table.to_string(), grantee.to_string()))
                    .cloned()
                    .unwrap_or_default()
                    != expected(grantee)
            });
        let effective_column_drifted = ["wamn_app", SCENARIO_AUTHOR_ROLE, EFFECT_WRITER_ROLE]
            .into_iter()
            .any(|grantee| {
                let expected_columns: BTreeSet<String> = expected(grantee)
                    .into_iter()
                    .filter(|privilege| {
                        ["SELECT", "INSERT", "UPDATE", "REFERENCES"].contains(&privilege.as_str())
                    })
                    .collect();
                obs.effect_ledger_effective_column_privileges
                    .get(&(table.to_string(), grantee.to_string()))
                    .cloned()
                    .unwrap_or_default()
                    != expected_columns
            });
        let boundary_owned = obs.effect_ledger_owners.get(table).is_some_and(|owner| {
            matches!(
                owner.as_str(),
                "wamn_app" | SCENARIO_AUTHOR_ROLE | EFFECT_WRITER_ROLE
            )
        });
        if !direct_drifted && !effective_drifted && !effective_column_drifted && !boundary_owned {
            continue;
        }
        let qualified = format!("{}.{}", schema.quoted(), quote_ident(table));
        let columns = obs
            .tables
            .get(table)
            .expect("present effect ledger")
            .iter()
            .filter(|column| {
                if table != "effect_attempts" {
                    return true;
                }
                let frame_owned = frame_cutover_targets.effect
                    && (column.as_str() == "node_id"
                        || EFFECT_FRAME_COLUMNS.contains(&column.as_str()));
                let writer_owned = effect_writer_ledger_cutover_needed
                    && RETIRED_EFFECT_ATTEMPT_COLUMNS.contains(&column.as_str());
                !frame_owned && !writer_owned
            })
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ");
        // The grant and the self-check move together: whatever APPEND authority
        // this ledger does not carry becomes a privilege the block REFUSES to see
        // the server still report, so a parked table shows its own denial.
        let writer_grant = writer_privileges.join(", ");
        let writer_forbidden_table = "'INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'";
        let writer_forbidden_columns = "INSERT,UPDATE,REFERENCES";
        plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::RepairEffectWriterPrivilege,
                target: format!("{}.{}", schema.as_str(), table),
                sql: format!(
                    "REVOKE SELECT ({columns}), INSERT ({columns}), UPDATE ({columns}), \
                            REFERENCES ({columns}) ON TABLE {qualified} \
                       FROM PUBLIC, wamn_app, {SCENARIO_AUTHOR_ROLE}, {EFFECT_WRITER_ROLE}; \
                     REVOKE ALL PRIVILEGES ON TABLE {qualified} \
                       FROM PUBLIC, wamn_app, {SCENARIO_AUTHOR_ROLE}, {EFFECT_WRITER_ROLE}; \
                     GRANT SELECT ON TABLE {qualified} TO wamn_app; \
                     GRANT {writer_grant} ON TABLE {qualified} TO {EFFECT_WRITER_ROLE}; \
                     DO $effect_ledger_acl$ BEGIN \
                       IF EXISTS (SELECT 1 FROM unnest(ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                                   WHERE pg_catalog.has_table_privilege('wamn_app', '{qualified}', privilege)) \
                          OR EXISTS (SELECT 1 FROM unnest(ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                                   WHERE pg_catalog.has_table_privilege('{SCENARIO_AUTHOR_ROLE}', '{qualified}', privilege)) \
                          OR EXISTS (SELECT 1 FROM unnest(ARRAY[{writer_forbidden_table}]) privilege \
                                   WHERE pg_catalog.has_table_privilege('{EFFECT_WRITER_ROLE}', '{qualified}', privilege)) \
                          OR pg_catalog.has_any_column_privilege('wamn_app', '{qualified}', 'INSERT,UPDATE,REFERENCES') \
                          OR pg_catalog.has_any_column_privilege('{SCENARIO_AUTHOR_ROLE}', '{qualified}', 'SELECT,INSERT,UPDATE,REFERENCES') \
                          OR pg_catalog.has_any_column_privilege('{EFFECT_WRITER_ROLE}', '{qualified}', '{writer_forbidden_columns}') \
                          OR (SELECT owner.rolname FROM pg_catalog.pg_class relation \
                              JOIN pg_catalog.pg_roles owner ON owner.oid = relation.relowner \
                             WHERE relation.oid = pg_catalog.to_regclass('{qualified}')) \
                             IN ('wamn_app', '{SCENARIO_AUTHOR_ROLE}', '{EFFECT_WRITER_ROLE}') \
                       THEN RAISE EXCEPTION USING ERRCODE = '42501', \
                            MESSAGE = 'effect-ledger-effective-privilege-out-of-bounds:{table}'; \
                       END IF; \
                     END $effect_ledger_acl$"
                ),
            });
    }

    let mut effect_writer_run_read_repairs = Vec::new();
    for (table, allowed) in EFFECT_WRITER_RUN_READ_COLUMNS {
        let Some(live_columns) = obs.tables.get(table) else {
            continue;
        };
        let table_drifted = obs
            .effect_writer_run_table_privileges
            .get(table)
            .is_some_and(|privileges| !privileges.is_empty());
        let column_drifted = allowed.iter().any(|column| !live_columns.contains(*column))
            || live_columns.iter().any(|column| {
                let actual = obs
                    .effect_writer_run_column_privileges
                    .get(&(table.to_string(), column.clone()))
                    .cloned()
                    .unwrap_or_default();
                let expected: BTreeSet<String> = if allowed.contains(&column.as_str()) {
                    ["SELECT".to_string()].into_iter().collect()
                } else {
                    BTreeSet::new()
                };
                actual != expected
            });
        if !table_drifted && !column_drifted {
            continue;
        }

        let qualified = format!("{}.{}", schema.quoted(), quote_ident(table));
        let all_columns = live_columns
            .iter()
            .filter(|column| {
                !(table == "run_queue"
                    && partition_plane_cutover_needed
                    && RETIRED_PARTITION_COLUMNS.contains(&column.as_str()))
                    && !(table == "runs"
                        && rerun_lineage_cutover_needed
                        && RETIRED_RERUN_LINEAGE_COLUMNS.contains(&column.as_str()))
                    && !(table == "runs"
                        && failure_detail_cutover_needed
                        && RETIRED_FAILURE_DETAIL_COLUMNS.contains(&column.as_str()))
            })
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ");
        let allowed_columns = allowed
            .iter()
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ");
        let allowed_literals = allowed
            .iter()
            .map(|column| format!("'{}'", column.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        effect_writer_run_read_repairs.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairEffectWriterPrivilege,
            target: format!("{}.{}.effect-read", schema.as_str(), table),
            sql: format!(
                "REVOKE SELECT ({all_columns}), INSERT ({all_columns}), \
                        UPDATE ({all_columns}), REFERENCES ({all_columns}) \
                   ON TABLE {qualified} FROM PUBLIC, {EFFECT_WRITER_ROLE}; \
                 REVOKE ALL PRIVILEGES ON TABLE {qualified} \
                   FROM PUBLIC, {EFFECT_WRITER_ROLE}; \
                 GRANT SELECT ({allowed_columns}) ON TABLE {qualified} \
                   TO {EFFECT_WRITER_ROLE}; \
                 DO $effect_writer_run_read_acl$ BEGIN \
                   IF EXISTS ( \
                        SELECT 1 FROM unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE', \
                                                   'TRUNCATE','REFERENCES','TRIGGER']) privilege \
                         WHERE pg_catalog.has_table_privilege( \
                               '{EFFECT_WRITER_ROLE}', '{qualified}', privilege)) \
                      OR EXISTS ( \
                        SELECT 1 FROM pg_catalog.pg_attribute AS attribute \
                        CROSS JOIN unnest(ARRAY['INSERT','UPDATE','REFERENCES']) privilege \
                         WHERE attribute.attrelid=pg_catalog.to_regclass('{qualified}') \
                           AND attribute.attnum > 0 AND NOT attribute.attisdropped \
                           AND pg_catalog.has_column_privilege( \
                               '{EFFECT_WRITER_ROLE}', '{qualified}', \
                               attribute.attname, privilege)) \
                      OR EXISTS ( \
                        SELECT 1 FROM pg_catalog.pg_attribute AS attribute \
                         WHERE attribute.attrelid=pg_catalog.to_regclass('{qualified}') \
                           AND attribute.attnum > 0 AND NOT attribute.attisdropped \
                           AND NOT (attribute.attname = ANY (ARRAY[{allowed_literals}])) \
                           AND pg_catalog.has_column_privilege( \
                               '{EFFECT_WRITER_ROLE}', '{qualified}', \
                               attribute.attname, 'SELECT')) \
                      OR EXISTS ( \
                        SELECT 1 FROM unnest(ARRAY[{allowed_literals}]) column_name \
                         WHERE NOT pg_catalog.has_column_privilege( \
                               '{EFFECT_WRITER_ROLE}', '{qualified}', \
                               column_name, 'SELECT')) \
                   THEN RAISE EXCEPTION USING ERRCODE='42501', \
                        MESSAGE='effect-writer-run-read-privilege-out-of-bounds:{table}'; \
                   END IF; \
                 END $effect_writer_run_read_acl$"
            ),
        });
    }

    if wiring_identity_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::AddColumn,
            target: "runs.wiring-identity".to_string(),
            sql: wiring_identity_cutover_sql(schema),
        });
    }

    // The host-only role needs schema visibility even when an existing schema
    // receives only a newly missing table section (record table sections do
    // not replay file headers). Catalog from-zero carries its header grant;
    // an existing catalog is repaired explicitly.
    if !obs.scenario_author_schema_usage.contains(schema.as_str()) {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairAuthoringPrivilege,
            target: format!("{}.usage", schema.as_str()),
            sql: format!(
                "GRANT USAGE ON SCHEMA {} TO {SCENARIO_AUTHOR_ROLE}",
                schema.quoted()
            ),
        });
    }
    if obs.catalog_schema_present && !obs.scenario_author_schema_usage.contains("catalog") {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairAuthoringPrivilege,
            target: "catalog.usage".to_string(),
            sql: format!("GRANT USAGE ON SCHEMA catalog TO {SCENARIO_AUTHOR_ROLE}"),
        });
    }

    // Converge direct grants exactly on the narrow authoring surface. PUBLIC
    // and the guest-visible role are part of the expected map so a stale grant
    // cannot survive an otherwise current schema.
    for spec in AUTHORING_PRIVILEGE_SPECS {
        if matches!(spec.schema, AuthoringTableSchema::RunPlane) && spec.table == "runs" {
            continue;
        }
        let (schema_name, present) = match spec.schema {
            AuthoringTableSchema::Catalog => ("catalog", obs.catalog_tables.contains(spec.table)),
            AuthoringTableSchema::RunPlane => {
                (schema.as_str(), obs.tables.contains_key(spec.table))
            }
        };
        if !present {
            continue;
        }
        let is_environment_policy = matches!(spec.schema, AuthoringTableSchema::RunPlane)
            && spec.table == "environment_policies";
        let direct_grantees: &[&str] = if is_environment_policy {
            &[
                "PUBLIC",
                "wamn_app",
                SCENARIO_AUTHOR_ROLE,
                EFFECT_WRITER_ROLE,
            ]
        } else {
            &["PUBLIC", "wamn_app", SCENARIO_AUTHOR_ROLE]
        };
        let effective_grantees: &[&str] = if is_environment_policy {
            &["wamn_app", SCENARIO_AUTHOR_ROLE, EFFECT_WRITER_ROLE]
        } else {
            &["wamn_app", SCENARIO_AUTHOR_ROLE]
        };
        let expected_for = |grantee: &str| -> BTreeSet<String> {
            let privileges = match grantee {
                "wamn_app" => spec.app,
                SCENARIO_AUTHOR_ROLE => spec.author,
                "PUBLIC" | EFFECT_WRITER_ROLE => &[],
                _ => unreachable!("closed authoring grantee set"),
            };
            privileges
                .iter()
                .map(|value| (*value).to_string())
                .collect()
        };
        let direct_drifted = direct_grantees.iter().copied().any(|grantee| {
            obs.authoring_table_privileges
                .get(&(
                    schema_name.to_string(),
                    spec.table.to_string(),
                    grantee.to_string(),
                ))
                .cloned()
                .unwrap_or_default()
                != expected_for(grantee)
        });
        let effective_drifted = effective_grantees.iter().copied().any(|grantee| {
            obs.authoring_effective_table_privileges
                .get(&(
                    schema_name.to_string(),
                    spec.table.to_string(),
                    grantee.to_string(),
                ))
                .cloned()
                .unwrap_or_default()
                != expected_for(grantee)
        });
        let effective_column_drifted = effective_grantees.iter().copied().any(|grantee| {
            let expected_columns: BTreeSet<String> = expected_for(grantee)
                .into_iter()
                .filter(|privilege| {
                    ["SELECT", "INSERT", "UPDATE", "REFERENCES"].contains(&privilege.as_str())
                })
                .collect();
            obs.authoring_effective_column_privileges
                .get(&(
                    schema_name.to_string(),
                    spec.table.to_string(),
                    grantee.to_string(),
                ))
                .cloned()
                .unwrap_or_default()
                != expected_columns
        });
        let boundary_owned = obs
            .authoring_table_owners
            .get(&(schema_name.to_string(), spec.table.to_string()))
            .is_some_and(|owner| effective_grantees.contains(&owner.as_str()));
        if !direct_drifted && !effective_drifted && !effective_column_drifted && !boundary_owned {
            continue;
        }

        let qualified = format!("{}.{}", quote_ident(schema_name), quote_ident(spec.table));
        let mut sql = String::new();
        if direct_drifted {
            sql = direct_grantees
                .iter()
                .map(|grantee| format!("REVOKE ALL PRIVILEGES ON TABLE {qualified} FROM {grantee}"))
                .collect::<Vec<_>>()
                .join("; ");
            for (grantee, privileges) in
                [("wamn_app", spec.app), (SCENARIO_AUTHOR_ROLE, spec.author)]
            {
                if !privileges.is_empty() {
                    sql.push_str(&format!(
                        "; GRANT {} ON TABLE {qualified} TO {grantee}",
                        privileges.join(", ")
                    ));
                }
            }
        }
        // Direct REVOKEs cannot safely repair an unrelated inherited group or
        // table ownership. Verify the effective postcondition and fail loudly
        // instead of claiming convergence while either boundary role retains
        // authority outside its spec.
        let mut forbidden_checks = Vec::new();
        for grantee in effective_grantees.iter().copied() {
            let expected = match grantee {
                "wamn_app" => spec.app,
                SCENARIO_AUTHOR_ROLE => spec.author,
                EFFECT_WRITER_ROLE => &[],
                _ => unreachable!("closed effective grantee set"),
            };
            for privilege in TABLE_PRIVILEGE_TYPES {
                if !expected.contains(&privilege) {
                    forbidden_checks.push(format!(
                        "pg_catalog.has_table_privilege(\
                         '{grantee}', '{qualified}', '{privilege}')"
                    ));
                }
            }
            for privilege in ["SELECT", "INSERT", "UPDATE", "REFERENCES"] {
                if !expected.contains(&privilege) {
                    forbidden_checks.push(format!(
                        "pg_catalog.has_any_column_privilege(\
                         '{grantee}', '{qualified}', '{privilege}')"
                    ));
                }
            }
        }
        forbidden_checks.push(format!(
            "(SELECT owner.rolname \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_roles AS owner ON owner.oid = relation.relowner \
              WHERE relation.oid = pg_catalog.to_regclass('{qualified}')) \
             IN ({})",
            effective_grantees
                .iter()
                .map(|grantee| format!("'{grantee}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if !sql.is_empty() {
            sql.push_str("; ");
        }
        sql.push_str(&format!(
            "DO $effective_acl$ BEGIN IF {} THEN RAISE EXCEPTION USING \
             ERRCODE = '42501', MESSAGE = \
             'authoring-effective-privilege-out-of-bounds:{schema_name}.{}'; \
             END IF; END $effective_acl$",
            forbidden_checks.join(" OR "),
            spec.table,
        ));
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairAuthoringPrivilege,
            target: format!("{schema_name}.{}", spec.table),
            sql,
        });
    }

    // 2. Column drift on PRESENT record tables: add what the record has and the
    //    live table lacks (record order); surface unknown extras, never drop
    //    them. Explicit cutover-owned retired columns are handled above.
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            let Some(live_cols) = obs.tables.get(&table) else {
                continue;
            };
            let record_cols = record_columns(file, "wamn_run", &table);
            for (record_column_index, (col, def)) in record_cols.iter().enumerate() {
                if wiring_identity_cutover_needed
                    && table == "runs"
                    && matches!(
                        col.as_str(),
                        "flow_id"
                            | "flow_version"
                            | "wiring_id"
                            | "wiring_version"
                            | "wiring_hash"
                            | "binding_world_json"
                    )
                {
                    continue;
                }
                if frame_cutover_targets.needed() && frame_identity_column(&table, col) {
                    continue;
                }
                if effect_writer_ledger_cutover_needed
                    && table == "effect_attempt_dispatches"
                    && matches!(
                        col.as_str(),
                        "run_id" | "frame_id" | "local_node_id" | "occurrence"
                    )
                {
                    continue;
                }
                if !live_cols.contains(col) {
                    let add_column_sql = format!(
                        "ALTER TABLE {}.{} ADD COLUMN {def}",
                        schema.quoted(),
                        quote_ident(&table),
                    );
                    let sql = if table == "runs"
                        && col == "capture_mode"
                        && obs.app_run_capture_privileges.0
                    {
                        let available_columns = live_cols
                            .iter()
                            .cloned()
                            .chain(
                                record_cols[..=record_column_index]
                                    .iter()
                                    .map(|(column, _)| column.clone()),
                            )
                            .filter(|column| {
                                (!child_run_cutover_needed
                                    || !RETIRED_CHILD_RUN_COLUMNS.contains(&column.as_str()))
                                    && (!failure_detail_cutover_needed
                                        || !RETIRED_FAILURE_DETAIL_COLUMNS
                                            .contains(&column.as_str()))
                            });
                        format!(
                            "LOCK TABLE {}.runs IN ACCESS EXCLUSIVE MODE; {add_column_sql}; {}",
                            schema.quoted(),
                            repair_run_capture_privilege_sql(schema, available_columns),
                        )
                    } else {
                        add_column_sql
                    };
                    plan.actions.push(RunPlaneAction {
                        kind: RunPlaneActionKind::AddColumn,
                        target: format!("{table}.{col}"),
                        sql,
                    });
                }
            }
            let known: BTreeSet<&str> = record_cols.iter().map(|(c, _)| c.as_str()).collect();
            for col in live_cols {
                if partition_plane_cutover_needed
                    && table == "run_queue"
                    && RETIRED_PARTITION_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if frame_cutover_targets.includes_table(&table)
                    && frame_identity_column(&table, col)
                {
                    continue;
                }
                if effect_writer_ledger_cutover_needed
                    && table == "effect_attempts"
                    && RETIRED_EFFECT_ATTEMPT_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if child_run_cutover_needed
                    && table == "runs"
                    && RETIRED_CHILD_RUN_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if rerun_lineage_cutover_needed
                    && table == "runs"
                    && RETIRED_RERUN_LINEAGE_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if failure_detail_cutover_needed
                    && table == "runs"
                    && RETIRED_FAILURE_DETAIL_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if stored_suite_cutover_needed
                    && RETIRED_TEST_SET_REFERENCE_TABLES.contains(&table.as_str())
                    && col == RETIRED_TEST_SET_REFERENCE_COLUMN
                {
                    continue;
                }
                if !known.contains(col.as_str()) {
                    plan.extra_columns.push((table.clone(), col.clone()));
                }
            }
        }
    }

    // A missing table's record section carries its complete RLS apparatus.
    // Existing tables are compared at the PostgreSQL catalog grain: both
    // relation flags and the sole tenant SELECT policy must match exactly.
    if obs.tables.contains_key("environment_policies")
        && obs.environment_policy_row_security.as_ref()
            != Some(&environment_policy_row_security_at_record())
    {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairRowSecurity,
            target: "environment_policies.row-security".to_string(),
            sql: repair_environment_policy_row_security_sql(schema),
        });
    }

    // Required columns are added before the exact writer read boundary names
    // them. This also lets one reconcile turn converge a partial queue shape.
    plan.actions.extend(effect_writer_run_read_repairs);

    // A broad legacy grant is narrowed after the record column exists.
    if !capture_mode_present && run_capture_privileges_drifted && !obs.app_run_capture_privileges.0
    {
        let record_columns = record_columns(RUN_STATE_SQL, "wamn_run", "runs")
            .into_iter()
            .map(|(column, _)| column);
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairRunCapturePrivilege,
            target: "runs.capture_mode".to_string(),
            sql: repair_run_capture_privilege_sql(schema, record_columns),
        });
    }

    // 2b. Exact CHECK convergence. AddColumn carries its own inline CHECK, so
    // skip that spec when its column is absent in the observation. Table-level
    // checks run after AddColumn and therefore may safely name newly-added
    // columns. PostgreSQL validates every ADD against existing rows; a legacy
    // row that violates the canonical contract aborts reconciliation rather
    // than being rewritten or deleted.
    let expected_checks: BTreeSet<(&str, &str)> = CHECK_SPECS
        .iter()
        .map(|spec| (spec.table, spec.name))
        .collect();
    for spec in CHECK_SPECS {
        if spec.table == "runs" && spec.name == "runs_check" {
            continue;
        }
        if wiring_identity_cutover_needed
            && spec.table == "runs"
            && matches!(
                spec.name,
                "runs_wiring_identity_check" | "runs_execution_grain_check"
            )
        {
            continue;
        }
        if frame_cutover_targets.includes_table(spec.table)
            && frame_identity_check(spec.table, spec.name)
        {
            continue;
        }
        if effect_writer_ledger_cutover_needed
            && spec.table == "effect_attempt_dispatches"
            && matches!(
                spec.name,
                "effect_attempt_dispatches_frame_check"
                    | "effect_attempt_dispatches_local_node_check"
                    | "effect_attempt_dispatches_occurrence_check"
            )
        {
            continue;
        }
        let Some(columns) = obs.tables.get(spec.table) else {
            continue;
        };
        if matches!(spec.origin, CheckOrigin::Inline(column) if !columns.contains(column)) {
            continue;
        }
        let key = (spec.table.to_string(), spec.name.to_string());
        if obs
            .checks
            .get(&key)
            .is_some_and(|def| def == spec.definition)
        {
            continue;
        }
        let drop = if obs.checks.contains_key(&key) {
            format!("DROP CONSTRAINT {}, ", quote_ident(spec.name))
        } else {
            String::new()
        };
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairConstraint,
            target: format!("{}.{}", spec.table, spec.name),
            sql: format!(
                "ALTER TABLE {}.{} {drop}ADD CONSTRAINT {} {}",
                schema.quoted(),
                quote_ident(spec.table),
                quote_ident(spec.name),
                spec.definition,
            ),
        });
    }
    for ((table, name), definition) in &obs.checks {
        if obs.tables.contains_key(table)
            && record_table_names().contains(table.as_str())
            && !expected_checks.contains(&(table.as_str(), name.as_str()))
            && !(effect_writer_ledger_cutover_needed
                && effect_writer_cutover_owned_check(table, name))
            && !(frame_cutover_targets.includes_table(table) && frame_identity_check(table, name))
            && !(partition_plane_cutover_needed
                && table == "run_queue"
                && name == RETIRED_PARTITION_CHECK)
            && !(child_run_cutover_needed && table == "runs" && retired_child_run_check(definition))
            && !(table == "runs" && name == "runs_environment_check")
        {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::DropExtraConstraint,
                target: format!("{table}.{name}"),
                sql: format!(
                    "ALTER TABLE {}.{} DROP CONSTRAINT {}",
                    schema.quoted(),
                    quote_ident(table),
                    quote_ident(name),
                ),
            });
        }
    }

    // 2d. The effect-ledger FKs remain exact. A missing table's canonical
    // CREATE section carries these, so repair only observed tables.
    for (table, name, definition, sql) in [
        (
            "effect_attempt_dispatches",
            EFFECT_DISPATCH_ATTEMPT_FK_NAME,
            EFFECT_DISPATCH_ATTEMPT_FK_DEF,
            EFFECT_DISPATCH_ATTEMPT_FK_SQL,
        ),
        (
            "effect_attempt_outcomes",
            EFFECT_OUTCOME_DISPATCH_FK_NAME,
            EFFECT_OUTCOME_DISPATCH_FK_DEF,
            EFFECT_OUTCOME_DISPATCH_FK_SQL,
        ),
    ] {
        if effect_writer_ledger_cutover_needed
            && table == "effect_attempt_dispatches"
            && obs.tables.contains_key("effect_attempts")
        {
            continue;
        }
        if !obs.tables.contains_key(table) {
            continue;
        }
        let key = (table.to_string(), name.to_string());
        if obs
            .foreign_keys
            .get(&key)
            .is_some_and(|observed| normalize_observed_schema(observed, schema) == definition)
        {
            continue;
        }
        let drop = if obs.foreign_keys.contains_key(&key) {
            format!(
                "ALTER TABLE {}.{} DROP CONSTRAINT {}; ",
                schema.quoted(),
                quote_ident(table),
                quote_ident(name),
            )
        } else {
            String::new()
        };
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairForeignKey,
            target: format!("{table}.{name}"),
            sql: format!("{drop}{}", rewrite_schema(sql, schema)),
        });
    }

    // 2e. User triggers are explicit record objects. A missing table's section
    // may carry its trigger; triggers placed after a shared helper are separate
    // actions because the section parser deliberately stops at that helper.
    // Present tables are repaired exactly, and immutable-ledger triggers are
    // never mistaken for extras.
    let trigger_specs = trigger_specs();
    let expected_triggers: BTreeSet<(&str, &str)> = trigger_specs
        .iter()
        .map(|spec| (spec.table.as_str(), spec.name.as_str()))
        .collect();
    for spec in &trigger_specs {
        if !obs.tables.contains_key(&spec.table)
            && table_section_carries_trigger(&spec.table, &spec.name)
        {
            continue;
        }
        let key = (spec.table.clone(), spec.name.clone());
        if obs.triggers.get(&key).is_some_and(|definition| {
            normalize_observed_schema(definition, schema) == spec.definition
        }) {
            continue;
        }
        let drop = if obs.triggers.contains_key(&key) {
            format!(
                "DROP TRIGGER {} ON {}.{}; ",
                quote_ident(&spec.name),
                schema.quoted(),
                quote_ident(&spec.table),
            )
        } else {
            String::new()
        };
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairTrigger,
            target: format!("{}.{}", spec.table, spec.name),
            sql: format!("{drop}{}", rewrite_schema(&spec.sql, schema)),
        });
    }
    for (table, name) in obs.triggers.keys() {
        if record_table_names().contains(table.as_str())
            && !expected_triggers.contains(&(table.as_str(), name.as_str()))
            && name != OUTBOX_TRIGGER_NAME
            && !(effect_writer_ledger_cutover_needed
                && matches!(
                    (table.as_str(), name.as_str()),
                    ("effect_attempts", "effect_attempts_insert_guard")
                        | (
                            "effect_attempt_dispatches",
                            "effect_attempt_dispatches_insert_guard"
                        )
                        | (
                            "effect_attempt_outcomes",
                            "effect_attempt_outcomes_insert_guard"
                        )
                ))
        {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::DropExtraTrigger,
                target: format!("{table}.{name}"),
                sql: format!(
                    "DROP TRIGGER {} ON {}.{}",
                    quote_ident(name),
                    schema.quoted(),
                    quote_ident(table),
                ),
            });
        }
    }

    // 3. Index drift on PRESENT tables only (a created section carries its own
    //    indexes): absent → create from record; present but the live definition
    //    lost a record column the record definition names → drop + recreate.
    for file in RUN_PLANE_FILES {
        for (name, table, stmt) in index_statements(file, "wamn_run") {
            if !obs.tables.contains_key(&table) {
                continue;
            }
            if matches!(name.as_str(), "runs_release" | "runs_execution_bundle") {
                continue;
            }
            if effect_writer_ledger_cutover_needed
                && matches!(
                    name.as_str(),
                    "effect_attempts_dispatch_identity_key"
                        | "effect_attempt_dispatches_occurrence_key"
                )
            {
                continue;
            }
            if partition_plane_cutover_needed
                && run_queue_claim_index_ready(obs)
                && name == "run_queue_claimable"
            {
                continue;
            }
            match obs.indexes.get(&name) {
                None => plan.actions.push(RunPlaneAction {
                    kind: RunPlaneActionKind::CreateIndex,
                    target: name.clone(),
                    sql: rewrite_schema(&stmt, schema),
                }),
                Some(live_def) if index_definition_stale(file, &table, &stmt, live_def) => {
                    plan.actions.push(RunPlaneAction {
                        kind: RunPlaneActionKind::RecreateIndex,
                        target: name.clone(),
                        sql: format!(
                            "DROP INDEX {}.{}; {}",
                            schema.quoted(),
                            quote_ident(&name),
                            rewrite_schema(&stmt, schema),
                        ),
                    });
                }
                Some(_) => {}
            }
        }
    }

    // 4. Legacy outbox-era teardown: tables, then triggers BEFORE the function
    //    (DROP FUNCTION is RESTRICT while a trigger still references it).
    for legacy in LEGACY_OUTBOX_TABLES {
        if obs.tables.contains_key(legacy) {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::DropLegacyTable,
                target: legacy.to_string(),
                sql: format!(
                    "DROP TABLE IF EXISTS {}.{}",
                    schema.quoted(),
                    quote_ident(legacy),
                ),
            });
        }
    }
    for table in &obs.outbox_trigger_tables {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::DropLegacyTrigger,
            target: table.clone(),
            sql: format!(
                "DROP TRIGGER IF EXISTS {OUTBOX_TRIGGER_NAME} ON {}.{}",
                schema.quoted(),
                quote_ident(table),
            ),
        });
    }
    if obs.outbox_function_present {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::DropLegacyFunction,
            target: OUTBOX_TRIGGER_NAME.to_string(),
            sql: format!(
                "DROP FUNCTION IF EXISTS {}.{OUTBOX_TRIGGER_NAME}()",
                schema.quoted(),
            ),
        });
    }

    // 5. Registration payload cleanup follows structural convergence.
    if obs.stale_registration_key_rows > 0 {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::StripRetiredRegistrationKeys,
            target: format!("{} registrations", obs.stale_registration_key_rows),
            sql: strip_retired_registration_keys_sql().to_string(),
        });
    }

    // Report the run-plane tables that needed nothing at all.
    let touched: BTreeSet<&str> = plan
        .actions
        .iter()
        .map(|a| a.target.split('.').next().unwrap_or(&a.target))
        .collect();
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            let index_touched = index_statements(file, "wamn_run")
                .iter()
                .any(|(name, t, _)| *t == table && touched.contains(name.as_str()));
            if obs.tables.contains_key(&table)
                && !touched.contains(table.as_str())
                && !index_touched
            {
                plan.at_target.push(table);
            }
        }
    }

    plan
}

fn repair_environment_policy_row_security_sql(schema: &BareSchemaName) -> String {
    let qualified = format!("{}.environment_policies", schema.quoted());
    format!(
        "ALTER TABLE {qualified} ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {qualified} FORCE ROW LEVEL SECURITY; \
         DO $environment_policy_rows$ DECLARE policy_name text; BEGIN \
           FOR policy_name IN \
             SELECT policy.polname FROM pg_catalog.pg_policy AS policy \
              WHERE policy.polrelid = pg_catalog.to_regclass('{qualified}') \
           LOOP \
             EXECUTE pg_catalog.format( \
               'DROP POLICY %I ON {qualified}', policy_name); \
           END LOOP; \
         END $environment_policy_rows$; \
         CREATE POLICY environment_policies_tenant ON {qualified} \
           FOR SELECT TO wamn_app USING (wamn_authority.tenant_key(tenant_id) \
             = wamn_authority.current_tenant_key()); \
         CREATE POLICY environment_policies_platform ON {qualified} \
           AS PERMISSIVE FOR SELECT TO wamn_platform USING (true)"
    )
}

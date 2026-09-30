//! The kind → type column cutover on an installed database
//! (docs/plan/kind-to-type.md §4.3.2).
//!
//! Each case installs today's schema and renames the P8 and P10 to P12 names
//! back to their old spelling, which is the state of a database installed
//! before the rename.

use super::{
    Client, RunPlaneActionKind, SCHEMA, connect, install_current_run_plane, locked_database,
    reconcile_run_plane, reset, schema,
};

/// Rename one constraint only when it exists: the `NOT NULL` names exist only
/// on PostgreSQL 18.
fn rename_constraint(table: &str, from: &str, to: &str) -> String {
    format!(
        "DO $old$ BEGIN \
           IF EXISTS (SELECT FROM pg_catalog.pg_constraint \
                       WHERE conrelid = '{table}'::regclass AND conname = '{from}') THEN \
             ALTER TABLE {table} RENAME CONSTRAINT {from} TO {to}; \
           END IF; \
         END $old$;"
    )
}

/// Put the installed database back to the names it had before the rename.
async fn install_old_type_columns(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    let owners = "catalog.package_definition_owners";
    let runs = format!("{SCHEMA}.runs");
    let attempts = format!("{SCHEMA}.effect_attempts");
    let actions = format!("{SCHEMA}.operator_run_actions");
    let mut sql = vec![
        format!("ALTER TABLE {owners} RENAME COLUMN definition_type TO definition_kind;"),
        rename_constraint(
            owners,
            "package_definition_owners_definition_type_check",
            "package_definition_owners_definition_kind_check",
        ),
        rename_constraint(
            owners,
            "package_definition_owners_definition_type_not_null",
            "package_definition_owners_definition_kind_not_null",
        ),
        format!("ALTER TABLE {runs} RENAME COLUMN caller_outcome_type TO caller_outcome_kind;"),
        format!(
            "ALTER TABLE {runs} RENAME CONSTRAINT runs_caller_outcome_type_check \
             TO runs_caller_outcome_kind_check;"
        ),
        format!("ALTER TABLE {runs} RENAME COLUMN fail_type TO fail_kind;"),
        format!(
            "ALTER TABLE {runs} RENAME CONSTRAINT runs_fail_type_check TO runs_fail_kind_check;"
        ),
        format!(
            "ALTER TABLE {attempts} RENAME COLUMN generation_fact_type TO generation_fact_kind;"
        ),
        rename_constraint(
            &attempts,
            "effect_attempts_generation_fact_type_not_null",
            "effect_attempts_generation_fact_kind_not_null",
        ),
        format!("ALTER TABLE {actions} RENAME COLUMN action_type TO action_kind;"),
        format!(
            "ALTER TABLE {actions} RENAME CONSTRAINT operator_run_actions_type_check \
             TO operator_run_actions_kind_check;"
        ),
        format!("ALTER TABLE {actions} RENAME COLUMN principal_type TO principal_kind;"),
        format!(
            "ALTER TABLE {actions} RENAME CONSTRAINT operator_run_actions_principal_type_check \
             TO operator_run_actions_principal_kind_check;"
        ),
    ];
    for (from, to) in [
        (
            "operator_run_actions_action_type_not_null",
            "operator_run_actions_action_kind_not_null",
        ),
        (
            "operator_run_actions_principal_type_not_null",
            "operator_run_actions_principal_kind_not_null",
        ),
    ] {
        sql.push(rename_constraint(&actions, from, to));
    }
    su.batch_execute(&sql.join("\n"))
        .await
        .expect("rename the installed database back to its old names");
}

/// The names of these four tables that still hold `kind`: the check query of
/// docs/plan/kind-to-type.md §4.3, narrowed to them.
async fn kind_names(su: &Client) -> Vec<String> {
    su.query(
        "SELECT a.attname::text FROM pg_catalog.pg_attribute AS a \
          WHERE a.attrelid = ANY ($1::text[]::regclass[]) AND a.attname LIKE '%kind%' \
            AND a.attnum > 0 AND NOT a.attisdropped \
         UNION ALL \
         SELECT con.conname::text FROM pg_catalog.pg_constraint AS con \
          WHERE con.conrelid = ANY ($1::text[]::regclass[]) AND con.conname LIKE '%kind%' \
         ORDER BY 1",
        &[&vec![
            "catalog.package_definition_owners".to_string(),
            format!("{SCHEMA}.runs"),
            format!("{SCHEMA}.effect_attempts"),
            format!("{SCHEMA}.operator_run_actions"),
        ]],
    )
    .await
    .expect("read the kind names")
    .iter()
    .map(|row| row.get(0))
    .collect()
}

#[tokio::test]
async fn type_column_cutover_renames_an_installed_database_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
    install_old_type_columns(&su).await;
    assert!(
        !kind_names(&su).await.is_empty(),
        "the old names are installed"
    );

    let plan = reconcile_run_plane::reconcile(&su, &schema(), true)
        .await
        .expect("the cutover renames the old names");
    assert_eq!(plan.actions[0].kind, RunPlaneActionKind::TypeColumnCutover);
    assert_eq!(kind_names(&su).await, Vec::<String>::new());

    let again = reconcile_run_plane::reconcile(&su, &schema(), false)
        .await
        .expect("a second plan reads the renamed database");
    assert!(
        again.actions.is_empty(),
        "a second plan is a no-op: {:#?}",
        again.actions
    );
}

#[tokio::test]
async fn type_column_cutover_refuses_a_missing_definition_owner_check_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
    install_old_type_columns(&su).await;
    su.batch_execute(
        "ALTER TABLE catalog.package_definition_owners \
         DROP CONSTRAINT package_definition_owners_definition_kind_check",
    )
    .await
    .expect("drop the old P8 check");
    let before = kind_names(&su).await;

    let error = reconcile_run_plane::reconcile(&su, &schema(), true)
        .await
        .expect_err("the cutover refuses a missing P8 check");
    super::assert_db_code_in_chain(&error, "55000", "missing P8 check");
    assert_eq!(kind_names(&su).await, before, "nothing is renamed");
}

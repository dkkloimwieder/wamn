//! The platform configuration rows on the admin-hierarchy path
//! (docs/plan/platform-config.md, Issues for C1, 3; issue `wamn-aqd0m.3`).
//!
//! `reconcile-run-plane` writes the identity rows of the tenant first, as env
//! apply does. Then an administration generation login, minted by the
//! production builder, writes one row of each of the six tables in one
//! transaction that binds a caller and an operation, as the control host
//! writes the application rows of an environment. The login reaches neither
//! a superuser nor `BYPASSRLS`, so the row security of each table applies.
//! Each write appends its history entry, a guest generation of the tenant
//! reads each row back through `wamn_app`'s `SELECT`, the same rows again
//! write none, a changed setting writes one row, and no rows remove none.

use super::{
    CLI_ENV, CLI_INSTANCE, CLI_ORG, CLI_PROJECT, Client, CredentialGeneration,
    ReconcileRunPlaneRequest, SCHEMA, WorkloadRoleFamily, WorkloadRoleScope, connect, connect_as,
    database_url, drop_database, drop_generation_role, locked_database, mint_guest_generation,
    project_env_database_name, provision_sql, reconcile_run_plane, recreate_database, reset,
    seed_system_env_policy, workload_generation_role,
};
use serde_json::json;
use wamn_platform_identity::auth_changes::AuthChanges;
use wamn_platform_identity::configuration::PlatformConfiguration;

#[path = "../support/platform_configuration.rs"]
mod rows;

const TENANT: &str = "t1";
const ADMINISTRATION_PASSWORD: &str = "administration-generation-configuration-probe";

/// Mint generation a of the environment's administration login through the
/// production builder, and dial it.
async fn mint_administration_generation(
    su: &Client,
    url: &str,
    database: &str,
) -> (String, Client) {
    let generation = workload_generation_role(
        WorkloadRoleFamily::Administration,
        WorkloadRoleScope::ProjectEnvironment {
            org: CLI_ORG,
            project: CLI_PROJECT,
            environment: CLI_ENV,
            database,
        },
        CredentialGeneration::A,
    )
    .expect("the administration family takes a project-environment scope");
    drop_generation_role(su, &generation).await;
    // The stable surface revokes in the production run-plane schema.
    su.batch_execute("CREATE SCHEMA IF NOT EXISTS wamn_run")
        .await
        .expect("create the production run-plane schema");
    su.batch_execute(&provision_sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::Administration,
        database,
        &generation,
        ADMINISTRATION_PASSWORD,
        "2100-01-01T00:00:00Z",
    ))
    .await
    .expect("mint the administration generation login through the production builder");
    let masking: Vec<String> = su
        .query(
            "SELECT rolname FROM pg_catalog.pg_roles \
              WHERE (rolsuper OR rolbypassrls) AND pg_has_role($1, oid, 'USAGE')",
            &[&generation],
        )
        .await
        .expect("probe the generation's superuser/bypassrls reach")
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert!(
        masking.is_empty(),
        "the administration generation reaches an RLS-masking role: {masking:?}"
    );
    let client = connect_as(url, &generation, ADMINISTRATION_PASSWORD).await;
    (generation, client)
}

async fn platform_configuration_leg(su: &Client, system_url: &str) {
    let database = project_env_database_name(CLI_ORG, CLI_PROJECT, CLI_ENV, CLI_INSTANCE);
    recreate_database(su, &database).await;
    let target_url = database_url(system_url, &database);
    let target = connect(&target_url).await;
    reset(&target).await;
    seed_system_env_policy(su, "durable").await;
    reconcile_run_plane::reconcile_run_plane(ReconcileRunPlaneRequest {
        changes: AuthChanges::default(),
        system_database_url: system_url.to_string(),
        admin_database_url: target_url.clone(),
        org: CLI_ORG.to_string(),
        project: CLI_PROJECT.to_string(),
        tenant: TENANT.to_string(),
        env: CLI_ENV.to_string(),
        schema: SCHEMA.to_string(),
        dry_run: false,
    })
    .await
    .expect("reconcile-run-plane writes the tenant identity rows");
    let (administration_role, mut administration) =
        mint_administration_generation(&target, &target_url, &database).await;
    let configuration = rows::configuration(CLI_PROJECT);

    assert_eq!(
        rows::write(&mut administration, &configuration)
            .await
            .expect("the administration login writes the configuration rows"),
        6,
        "the first write writes one row of each table"
    );
    let (generation, guest) = mint_guest_generation(&target, &target_url, TENANT).await;
    assert_eq!(
        rows::read_back(&guest)
            .await
            .expect("the guest generation reads the configuration rows"),
        rows::expected(TENANT, CLI_PROJECT),
        "the guest generation reads each row as written, stamped by the caller"
    );
    let inserted: Vec<String> = rows::TABLES
        .iter()
        .map(|table| format!("{table} insert {} {}", rows::OPERATION, rows::CALLER))
        .collect();
    assert_eq!(
        rows::history(&target)
            .await
            .expect("read the history entries"),
        inserted,
        "each row write appends its history entry, with the caller and the operation"
    );

    assert_eq!(
        rows::write(&mut administration, &configuration)
            .await
            .expect("write the same rows again"),
        0,
        "the same rows again write nothing"
    );
    let mut changed = configuration;
    changed.settings[0].value = json!("dd.MM.yyyy");
    assert_eq!(
        rows::write(&mut administration, &changed)
            .await
            .expect("write a changed setting"),
        1,
        "a changed setting writes its one row"
    );
    let mut updated = inserted;
    updated.push(format!(
        "setting update {} {}",
        rows::OPERATION,
        rows::CALLER
    ));
    assert_eq!(
        rows::history(&target)
            .await
            .expect("read the history entries again"),
        updated,
        "a write that changes nothing appends no entry, and a change appends one"
    );
    assert_eq!(
        rows::write(&mut administration, &PlatformConfiguration::default())
            .await
            .expect("write no rows"),
        0,
        "no rows write none"
    );
    let setting: String = guest
        .query_one(
            "SELECT value::text FROM platform.setting WHERE key = 'date_format'",
            &[],
        )
        .await
        .expect("read the setting")
        .get(0);
    assert_eq!(
        setting, "\"dd.MM.yyyy\"",
        "the changed setting stays after a write of no rows"
    );
    assert_eq!(
        rows::read_back(&guest)
            .await
            .expect("read the configuration rows again")
            .len(),
        6,
        "a write of no rows removes none"
    );

    drop(guest);
    drop(administration);
    drop_generation_role(&target, &generation).await;
    drop_generation_role(&target, &administration_role).await;
    drop(target);
    drop_database(su, &database).await;
}

/// A separate entry, so the path can be run on its own. It holds the process
/// lock, as the other entries do, because they share the cluster-wide roles.
#[tokio::test]
async fn platform_configuration_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
    platform_configuration_leg(&su, &url).await;
}

//! The platform configuration rows on the admin-hierarchy path
//! (docs/architecture/data-access.md#actors, wamn-aqd0m.3).
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

const ENV_KEY: &str = "acme--billing--dev";
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
    let (generation, guest) = mint_guest_generation(&target, &target_url, ENV_KEY).await;
    assert_eq!(
        rows::read_back(&guest)
            .await
            .expect("the guest generation reads the configuration rows"),
        rows::expected(CLI_PROJECT),
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

/// Env apply's sequence step (docs/architecture/data-access.md#counters,
/// wamn-83eth.3): the first apply creates the `number_sequence` row of a
/// package default under the administration authority, stamped by
/// `wamn:provisioning`; a second apply after an edit of the pattern creates
/// nothing and keeps the edited pattern.
#[tokio::test]
async fn env_apply_creates_number_sequences_and_keeps_an_edit_live() {
    use wamn_control::environment::stage::{create_number_sequences, sequence_defaults};

    wamn_schema_generator::receipt::check_current(&wamn_fixture_package::package_root())
        .unwrap_or_else(|error| panic!("{error:#}"));
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
    let database = project_env_database_name(CLI_ORG, CLI_PROJECT, CLI_ENV, CLI_INSTANCE);
    recreate_database(&su, &database).await;
    let target_url = database_url(&url, &database);
    let target = connect(&target_url).await;
    reset(&target).await;
    seed_system_env_policy(&su, "durable").await;
    reconcile_run_plane::reconcile_run_plane(ReconcileRunPlaneRequest {
        changes: AuthChanges::default(),
        system_database_url: url.to_string(),
        admin_database_url: target_url.clone(),
        org: CLI_ORG.to_string(),
        project: CLI_PROJECT.to_string(),
        env: CLI_ENV.to_string(),
        schema: SCHEMA.to_string(),
        dry_run: false,
    })
    .await
    .expect("reconcile-run-plane writes the tenant identity rows");
    // The stable administration role and its surface, as env apply prepares
    // them for the control host.
    let (administration_role, administration) =
        mint_administration_generation(&target, &target_url, &database).await;
    drop(administration);

    let mut base =
        wamn_schema_generator::PackageManifest::from_slice(&wamn_fixture_package::manifest_bytes())
            .unwrap();
    base.package.id = "wamn_receiving".into();
    base.sequences = serde_json::from_value(
        serde_json::json!({"purchase_order": {"pattern": "PO-{seq:po,width=6}"}}),
    )
    .unwrap();
    let mut overlay = base.clone();
    overlay.package.id = "client_acme_receiving".into();
    let applications = serde_json::from_value(
        serde_json::json!({"receiving": {"packages": ["wamn_receiving", "client_acme_receiving"]}}),
    )
    .unwrap();
    let defaults = sequence_defaults(&applications, [&base, &overlay]).unwrap();
    assert_eq!(
        target
            .query_one("SELECT count(*) FROM platform.number_sequence", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let rows = async || -> Vec<String> {
        target
            .query(
                "SELECT application || ' ' || name || ' ' || pattern || ' ' || created_by::text \
                   FROM platform.number_sequence ORDER BY 1",
                &[],
            )
            .await
            .expect("read the number sequences")
            .iter()
            .map(|row| row.get(0))
            .collect()
    };
    let provisioning = wamn_project_state::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    assert_eq!(
        create_number_sequences(&target_url, &defaults)
            .await
            .expect("the first apply creates the sequence"),
        1
    );
    assert_eq!(
        rows().await,
        [format!(
            "receiving purchase_order PO-{{seq:po,width=6}} {provisioning}"
        )],
        "the row carries the package default, stamped by wamn:provisioning"
    );

    target
        .batch_execute(&format!(
            "BEGIN; SELECT set_config('app.user_id', '{}', true), \
               set_config('app.operation', '{}', true); \
             UPDATE platform.number_sequence SET pattern = 'P-{{seq:po,width=8}}'; COMMIT;",
            rows::CALLER,
            rows::OPERATION
        ))
        .await
        .expect("edit the pattern");
    assert_eq!(
        create_number_sequences(&target_url, &defaults)
            .await
            .expect("the second apply runs"),
        0,
        "a second apply creates nothing"
    );
    assert_eq!(
        rows().await,
        [format!(
            "receiving purchase_order P-{{seq:po,width=8}} {provisioning}"
        )],
        "the edited pattern stays"
    );
    let history: Vec<String> = target
        .query(
            "SELECT type || ' ' || operation FROM platform.number_sequence_history \
              ORDER BY position",
            &[],
        )
        .await
        .expect("read the history")
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        history,
        [
            "insert wamn:provisioning".to_owned(),
            format!("update {}", rows::OPERATION)
        ],
        "the create and the edit each log one entry; the second apply logs none"
    );

    drop_generation_role(&target, &administration_role).await;
    drop(target);
    drop_database(&su, &database).await;
}

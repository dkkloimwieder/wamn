//! Every restricted login of a project database is refused all it was not granted.

pub mod common;

use wamn_provision::WorkloadRoleFamily;
use wamn_test_infrastructure::database::{self, PASSWORD, Project, execute};

/// Log in as `login`, then check its catalog-derived refusals and the `contract` actions.
async fn login_refusals(
    project: &Project,
    login: &str,
    replication: bool,
    contract: &[String],
) -> anyhow::Result<usize> {
    let mut admin = project.admin().await?;
    common::assert_unelevated(&mut admin, login, replication).await?;
    let mut actor = common::log_in(&project.login_url(login)?).await?;
    common::assert_login_refusals(
        &mut admin,
        &project.postgres,
        login,
        PASSWORD,
        &mut actor,
        contract,
    )
    .await
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn migration_login_refusals() -> anyhow::Result<()> {
    let project = database::project().await?;
    let login = WorkloadRoleFamily::Migration.login_name();
    let mut actor = common::log_in(&project.login_url(login)?).await?;
    wamn_provision::migrations::MigrationDatabase::Project
        .require_login(&mut actor)
        .await?;
    let checked = login_refusals(&project, login, false, &[]).await?;
    common::assert_mapped_logins_are_platform_components(&mut project.admin().await?).await?;
    println!("{login}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn app_login_refusals() -> anyhow::Result<()> {
    use sqlx::Connection as _;

    let project = database::project().await?;
    let login = WorkloadRoleFamily::App.login_name();
    let mut admin = project.admin().await?;
    common::assert_no_grant_options(&mut admin, login).await?;
    let reached = common::mapped_logins_reached(&mut admin, login).await?;
    assert!(
        reached.is_empty(),
        "{login} reaches mapped platform logins: {reached:?}"
    );

    // Identity storage: no read, no login mapping, no change to a bound identity.
    let mut contract: Vec<String> = [
        "SELECT FROM platform.tx_identity",
        "SELECT FROM platform.login_principal",
        "INSERT INTO platform.login_principal (login, principal_id) \
         VALUES (current_user, gen_random_uuid())",
        "UPDATE platform.tx_identity SET role = role",
        "DELETE FROM platform.tx_identity",
        "TRUNCATE platform.tx_identity",
    ]
    .map(String::from)
    .to_vec();
    // History entries take no direct update or delete. The history tables are the
    // tables that the log trigger writes.
    let history: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT format('%I.%I', n.nspname, c.relname || '_history')
           FROM pg_catalog.pg_trigger t
           JOIN pg_catalog.pg_proc p ON p.oid = t.tgfoid
           JOIN pg_catalog.pg_namespace pn ON pn.oid = p.pronamespace
           JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
          WHERE pn.nspname = 'wamn_history' AND p.proname = 'log_row_change'",
    )
    .fetch_all(&mut admin)
    .await?;
    assert!(!history.is_empty(), "the project install logs no relation");
    for table in &history {
        contract.push(format!("UPDATE {table} SET operation = operation"));
        contract.push(format!("DELETE FROM {table}"));
    }
    let checked = login_refusals(&project, login, false, &contract).await?;

    let mut actor = common::log_in(&project.login_url(login)?).await?;
    // The history read utilities stay usable.
    sqlx::query("SELECT wamn_history.timestamptz_image(now())")
        .execute(&mut actor)
        .await?;

    // A bound identity is not replaced, and no identity binds another transaction.
    let principal = wamn_provision::PlatformComponent::Executor.principal_id();
    let mut transaction = actor.begin().await?;
    wamn_postgres::bind_transaction_identity(
        &mut transaction,
        &principal,
        "viewer",
        "fixture",
        None,
        None,
    )
    .await?;
    let replaced = wamn_postgres::bind_transaction_identity(
        &mut transaction,
        &principal,
        "admin",
        "fixture",
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(common::sqlstate(&replaced).as_deref(), Some("23505"));
    transaction.rollback().await?;
    let mut transaction = actor.begin().await?;
    let other = sqlx::query(
        "INSERT INTO platform.tx_identity (xid, principal_id, role, application) \
         VALUES ('1'::xid8, $1, 'viewer', 'fixture')",
    )
    .bind(sqlx::types::Uuid::from(principal))
    .execute(&mut *transaction)
    .await
    .unwrap_err();
    assert_eq!(common::sqlstate(&other).as_deref(), Some("42501"));
    transaction.rollback().await?;

    // Catalog rows stay hidden from a transaction without a bound identity.
    let mut seed = admin.begin().await?;
    wamn_postgres::bind_transaction_identity(
        &mut seed,
        &wamn_provision::PlatformComponent::ApplyPackage.principal_id(),
        "",
        "fixture",
        None,
        None,
    )
    .await?;
    execute(
        &mut seed,
        "INSERT INTO catalog.packages (package_id, package_version) VALUES ('probe', '1.0.0')",
    )
    .await?;
    seed.commit().await?;
    let count = "SELECT count(*) FROM catalog.packages";
    let hidden: i64 = sqlx::query_scalar(count).fetch_one(&mut actor).await?;
    assert_eq!(hidden, 0, "{login} reads catalog rows without an identity");
    let mut transaction = actor.begin().await?;
    wamn_postgres::bind_transaction_identity(
        &mut transaction,
        &principal,
        "viewer",
        "fixture",
        None,
        None,
    )
    .await?;
    let visible: i64 = sqlx::query_scalar(count)
        .fetch_one(&mut *transaction)
        .await?;
    assert_eq!(visible, 1);
    transaction.rollback().await?;
    println!("{login}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn query_login_refusals() -> anyhow::Result<()> {
    let project = database::project().await?;
    let login = WorkloadRoleFamily::Query.login_name();
    let mut admin = project.admin().await?;
    common::assert_no_grant_options(&mut admin, login).await?;
    let reached = common::mapped_logins_reached(&mut admin, login).await?;
    assert!(
        reached.is_empty(),
        "{login} reaches mapped platform logins: {reached:?}"
    );
    let writes = common::writes_outside(&mut admin, login, None).await?;
    assert!(
        writes.is_empty(),
        "{login} writes beyond its transaction identity: {writes:?}"
    );
    let checked = login_refusals(&project, login, false, &[]).await?;
    println!("{login}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn platform_login_refusals() -> anyhow::Result<()> {
    let project = database::project().await?;
    let login = WorkloadRoleFamily::Platform.login_name();
    common::assert_no_grant_options(&mut project.admin().await?, login).await?;
    let checked = login_refusals(&project, login, false, &[]).await?;
    println!("{login}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn cdc_login_refusals() -> anyhow::Result<()> {
    let project = database::project().await?;
    let login = wamn_role::CDC_LOGIN;
    common::assert_no_grant_options(&mut project.admin().await?, login).await?;
    let checked = login_refusals(&project, login, true, &[]).await?;
    println!("{login}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn public_executes_no_project_security_definer() -> anyhow::Result<()> {
    let project = database::project().await?;
    let checked = common::assert_public_executes_no_definer(&mut project.admin().await?).await?;
    println!("PUBLIC: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn project_extensions_are_the_declared_extensions() -> anyhow::Result<()> {
    let project = database::project().await?;
    common::assert_extensions(
        &project.postgres,
        &mut project.admin().await?,
        &project.extensions,
    )
    .await
}

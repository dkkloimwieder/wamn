//! Every restricted login of the system database is refused all it was not granted.

pub mod common;

use common::{PASSWORD, System};

/// Log in as `login`, then check its catalog-derived refusals.
async fn login_refusals(system: &System, login: &str, password: &str) -> anyhow::Result<usize> {
    let mut admin = system.admin().await?;
    common::assert_unelevated(&mut admin, login, false).await?;
    let mut actor = common::log_in(&system.login_url(login, password)?).await?;
    common::assert_login_refusals(&mut admin, &system.postgres, login, password, &mut actor, &[])
        .await
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn system_migration_login_refusals() -> anyhow::Result<()> {
    let system = common::system().await?;
    let login = system.declared_login(wamn_role::SYSTEM_ROLE)?.to_owned();
    let mut actor = common::log_in(&system.login_url(&login, PASSWORD)?).await?;
    wamn_provision::migrations::MigrationDatabase::System
        .require_login(&mut actor)
        .await?;
    let checked = login_refusals(&system, &login, PASSWORD).await?;
    common::assert_mapped_logins_are_platform_components(&mut system.admin().await?).await?;
    println!("{login}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn identity_issuer_login_refusals() -> anyhow::Result<()> {
    let system = common::system().await?;
    let login = system.declared_login(wamn_role::IDENTITY_ISSUER_ROLE)?.to_owned();
    let mut admin = system.admin().await?;
    common::assert_no_grant_options(&mut admin, &login).await?;
    let writes = common::writes_outside(&mut admin, &login, Some("identity")).await?;
    assert!(writes.is_empty(), "{login} writes outside identity storage: {writes:?}");
    let checked = login_refusals(&system, &login, PASSWORD).await?;
    println!("{login}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn control_login_refusals() -> anyhow::Result<()> {
    let system = common::system().await?;
    let org = system.org_login(wamn_role::CONTROL_ROLE)?;
    common::assert_no_grant_options(&mut system.admin().await?, org.login.as_str()).await?;
    let checked = login_refusals(&system, org.login.as_str(), &org.password).await?;
    println!("{}: {checked} refused actions", org.login);
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn apply_login_refusals() -> anyhow::Result<()> {
    let system = common::system().await?;
    let org = system.org_login(wamn_role::APPLY_ROLE)?;
    common::assert_no_grant_options(&mut system.admin().await?, org.login.as_str()).await?;
    let checked = login_refusals(&system, org.login.as_str(), &org.password).await?;
    println!("{}: {checked} refused actions", org.login);
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn pooler_auth_login_refusals() -> anyhow::Result<()> {
    let system = common::system().await?;
    let login = "wamn_pooler_auth";
    // The Pooler looks logins up in the postgres database.
    let mut admin = wamn_postgres::connect(system.postgres.admin_url()).await?;
    common::assert_unelevated(&mut admin, login, false).await?;
    common::assert_no_grant_options(&mut admin, login).await?;
    let mut actor =
        common::log_in(&system.postgres.database_url("postgres", login, PASSWORD)?).await?;
    let checked =
        common::assert_login_refusals(&mut admin, &system.postgres, login, PASSWORD, &mut actor, &[])
            .await?;

    // The lookup answers only for an org login of its own group.
    let lookup = "SELECT count(*) FROM wamn_pooler.lookup_login($1, $2)";
    let control = system.org_login(wamn_role::CONTROL_ROLE)?.login.as_str();
    let apply = system.org_login(wamn_role::APPLY_ROLE)?.login.as_str();
    let migration = system.declared_login(wamn_role::SYSTEM_ROLE)?;
    for (candidate, group, found) in [
        (control, wamn_role::CONTROL_ROLE, 1),
        (apply, wamn_role::APPLY_ROLE, 1),
        (apply, wamn_role::CONTROL_ROLE, 0),
        (migration, wamn_role::SYSTEM_ROLE, 0),
    ] {
        let rows: i64 = sqlx::query_scalar(lookup)
            .bind(candidate)
            .bind(group)
            .fetch_one(&mut actor)
            .await?;
        assert_eq!(rows, found, "lookup of {candidate} in {group}");
    }
    println!("{login}: {checked} refused actions");
    Ok(())
}

/// `wamn_system_reader` is a group with no deployed login, so the test enters it with SET ROLE.
#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn system_reader_group_through_set_role() -> anyhow::Result<()> {
    let system = common::system().await?;
    let group = wamn_role::SYSTEM_READER_ROLE;
    let mut admin = system.admin().await?;
    common::assert_unelevated(&mut admin, group, false).await?;
    common::assert_no_grant_options(&mut admin, group).await?;
    let actions = common::refused_actions(&mut admin, group).await?;
    let mut actor = system.admin().await?;
    common::execute(&mut actor, &format!("SET ROLE {group}")).await?;
    let checked = common::assert_refused(&mut actor, group, &actions).await?;
    println!("{group}: {checked} refused actions");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn public_executes_no_system_security_definer() -> anyhow::Result<()> {
    let system = common::system().await?;
    let checked = common::assert_public_executes_no_definer(&mut system.admin().await?).await?;
    println!("PUBLIC: {checked} refused actions");
    Ok(())
}

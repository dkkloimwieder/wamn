use std::cell::Cell;

use anyhow::Context as _;
use wamn_authentication::administration::ApplicationTarget;
use wamn_identity::identity::{create_user, org};
use wamn_platform_install::provision_org::{CONTROL_SECRET, create_org_logins};
use wamn_platform_install::provision_system::SYSTEM_DATABASE;
use wamn_test_infrastructure::identity as common;

#[tokio::test]
#[ignore = "requires: docker"]
async fn maintenance_copies_current_authority_past_an_unreachable_database() -> anyhow::Result<()> {
    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let mut cluster = wamn_postgres::connect(postgres.admin_url()).await?;
    // CloudNativePG creates the apply group before system installation.
    sqlx::raw_sql("CREATE ROLE wamn_apply NOLOGIN NOINHERIT")
        .execute(&mut cluster)
        .await?;
    let system = common::system(&postgres).await;
    let mut authority = wamn_postgres::connect(system.as_str()).await?;
    let org = "fill".parse()?;
    let project = "fixture".parse()?;
    let prod = "prod".parse()?;
    let control = create_org_logins(&mut authority, &org, SYSTEM_DATABASE)
        .await?
        .into_iter()
        .find(|login| login.secret == CONTROL_SECRET)
        .context("org control login")?;
    let control =
        postgres.database_url(SYSTEM_DATABASE, control.login.as_str(), &control.password)?;
    let control = wamn_postgres::pool(control.as_str(), 1).await?;
    let actor = wamn_identity_protocol::PlatformComponent::Provisioning.principal_id();
    let mut tx = common::transaction(&mut authority, &actor, "admin:project-authority").await?;
    sqlx::raw_sql(
        "INSERT INTO registry.projects (org,id) VALUES ('fill','fixture');
         INSERT INTO registry.environments (org,env) VALUES ('fill','dev'), ('fill','prod');
         INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix)
         VALUES ('fill','fixture','dev','secret-dev','aaaaaaa1'),
                ('fill','fixture','prod','secret-prod','aaaaaaa2');",
    )
    .execute(&mut *tx)
    .await?;
    let member = *create_user(
        &mut tx,
        "member@example.invalid",
        "member@example.invalid",
        "Member",
    )
    .await?
    .id();
    org::activate_org_membership(&mut tx, &member, &org).await?;
    org::grant_project_admin(&mut tx, &member, &org, &project).await?;
    tx.commit().await?;

    let database = common::project(&postgres, "fixture_prod").await;
    let mut target = wamn_postgres::connect(database.as_str()).await?;
    sqlx::raw_sql(
        "CREATE ROLE wamn_platform_login LOGIN IN ROLE wamn_platform PASSWORD 'test-only';
         GRANT CONNECT ON DATABASE fixture_prod TO wamn_platform_login;",
    )
    .execute(&mut target)
    .await?;
    let issuer = "https://identity.example.invalid";
    wamn_environment::environment_fill::fill(
        &mut authority,
        &mut target,
        issuer,
        ApplicationTarget {
            org: &org,
            project: &project,
            env: &prod,
        },
    )
    .await?;
    let mut tx = common::transaction(&mut target, &actor, "admin:authored-role").await?;
    let authored_role = "editor";
    wamn_authentication::administration::create_role(&mut tx, authored_role).await?;
    sqlx::query("INSERT INTO platform.user_role (user_id, role_name) VALUES ($1, $2)")
        .bind(uuid::Uuid::from(member))
        .bind(authored_role)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let mut tx = common::transaction(&mut authority, &actor, "admin:missed-revoke").await?;
    org::revoke_project_admin(&mut tx, &member, &org, &project).await?;
    tx.commit().await?;
    let mut tx = common::transaction(&mut authority, &actor, "admin:latest-grant").await?;
    org::grant_member(&mut tx, &member, &org, &project, &prod).await?;
    tx.commit().await?;

    // The pass sees dev first. This database does not exist on the live server.
    let dev_login = postgres.database_url("fixture_dev", wamn_role::PLATFORM_LOGIN, "test-only")?;
    let prod_login =
        postgres.database_url("fixture_prod", wamn_role::PLATFORM_LOGIN, "test-only")?;
    let attempted_dev = Cell::new(false);
    super::identities(&control, &org, issuer, |triple, _| {
        let url = match triple.env.as_str() {
            "dev" => {
                attempted_dev.set(true);
                &dev_login
            }
            "prod" => &prod_login,
            _ => panic!("unexpected fixture environment"),
        };
        std::future::ready((triple, url.as_str().parse().map_err(anyhow::Error::from)))
    })
    .await?;
    assert!(attempted_dev.get());

    let roles = sqlx::query_scalar::<_, String>(
        "SELECT r.role_name FROM platform.user_role r JOIN platform.\"user\" u ON u.id = r.user_id
         WHERE u.id = $1 AND u.status = 'active' ORDER BY r.role_name",
    )
    .bind(uuid::Uuid::from(member))
    .fetch_all(&mut target)
    .await?;
    assert_eq!(roles, [authored_role]);
    Ok(())
}

//! Identity provisioning fills the rows that application foreign keys reference.

use wamn_authentication::administration::ApplicationTarget;
use wamn_environment::environment_fill::fill;

use wamn_test_infrastructure::identity as common;

/// Seed the organization `fill`, its project `fixture` and the environment `dev`.
fn seed_org_project_env_sql() -> String {
    let actor = wamn_principal::PlatformComponent::Provisioning.principal_id();
    format!(
        "INSERT INTO platform.tx_identity(principal_id,role,application) VALUES('{actor}','','fixture'); \
         INSERT INTO registry.orgs(id) VALUES ('fill'); \
         INSERT INTO registry.projects(org,id) VALUES ('fill','fixture'); \
         WITH parent_environment AS (INSERT INTO registry.environments (org,env) VALUES ('fill','dev')) \
         INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('fill','fixture','dev','s1','aaaaaaa1');"
    )
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn provisioning_fills_and_removes_environment_member_rows() -> anyhow::Result<()> {
    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let system = common::system(&postgres).await;
    let mut setup_connection = wamn_postgres::connect(system.as_str()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(seed_org_project_env_sql()))
        .execute(&mut setup_connection)
        .await?;
    let mut authority_connection = wamn_postgres::connect(system.as_str()).await?;
    let mut authority = common::transaction(
        &mut authority_connection,
        &wamn_principal::PlatformComponent::Provisioning.principal_id(),
        "admin:environment-fill",
    )
    .await?;
    let member = *wamn_postgres::identity::create_user(
        &mut authority,
        "member@example.invalid",
        "member@example.invalid",
        "Member",
    )
    .await?
    .id();
    wamn_postgres::identity::org::activate_org_membership(
        &mut authority,
        &member,
        &"fill".parse().unwrap(),
    )
    .await?;
    wamn_postgres::identity::org::grant_member(
        &mut authority,
        &member,
        &"fill".parse().unwrap(),
        &"fixture".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await?;
    authority.commit().await?;

    let project = common::project(&postgres, "fixture_extra").await;
    let target = ApplicationTarget {
        org: &"fill".parse().unwrap(),
        project: &"fixture".parse().unwrap(),
        env: &"dev".parse().unwrap(),
    };
    let mut client = wamn_postgres::connect(project.as_str()).await?;
    fill(
        &mut authority_connection,
        &mut client,
        "https://identity.example.invalid",
        target,
    )
    .await?;
    let present: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT FROM platform.\"user\" WHERE id=$1::text::uuid)")
            .bind(member.to_string())
            .fetch_one(&mut client)
            .await?;
    assert!(present);
    let mut authority = common::transaction(
        &mut authority_connection,
        &wamn_principal::PlatformComponent::Provisioning.principal_id(),
        "admin:revoke-environment-member",
    )
    .await?;
    wamn_postgres::identity::org::revoke_member(
        &mut authority,
        &member,
        &"fill".parse().unwrap(),
        &"fixture".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await?;
    authority.commit().await?;
    fill(
        &mut authority_connection,
        &mut client,
        "https://identity.example.invalid",
        target,
    )
    .await?;
    let removed: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS(SELECT FROM platform.\"user\" WHERE id=$1::text::uuid)",
    )
    .bind(member.to_string())
    .fetch_one(&mut client)
    .await?;
    assert!(removed);
    Ok(())
}

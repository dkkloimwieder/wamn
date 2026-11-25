//! Identity provisioning fills the rows that application foreign keys reference.

use wamn_authentication::administration::{
    AdministrationRefusal, ApplicationTarget, GrantAuthority, GrantChange, application_users,
    mutate_grant,
};
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
async fn provisioning_disables_and_reactivates_environment_member_rows() -> anyhow::Result<()> {
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
    let member = *wamn_identity::identity::create_user(
        &mut authority,
        "member@example.invalid",
        "member@example.invalid",
        "Member",
    )
    .await?
    .id();
    wamn_identity::identity::org::activate_org_membership(
        &mut authority,
        &member,
        &"fill".parse().unwrap(),
    )
    .await?;
    wamn_identity::identity::org::grant_member(
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
    let member_id = sqlx::types::Uuid::from(member);
    let actor = wamn_principal::PlatformComponent::Provisioning.principal_id();
    let mut application =
        common::transaction(&mut client, &actor, "admin:application-grants").await?;
    wamn_authentication::administration::create_role(&mut application, "editor").await?;
    sqlx::query("INSERT INTO platform.user_role (user_id, role_name) VALUES ($1, 'editor')")
        .bind(member_id)
        .execute(&mut *application)
        .await?;
    let pat_id: sqlx::types::Uuid = sqlx::query_scalar(
        "INSERT INTO platform.pat (user_id, prefix, hash, label, expires_at)
         VALUES ($1, 'aabbccddeeff0011', 'sha256:' || repeat('0', 64), 'old human PAT', now() + interval '1 hour')
         RETURNING id",
    )
    .bind(member_id)
    .fetch_one(&mut *application)
    .await?;
    application.commit().await?;
    let mut authority = common::transaction(
        &mut authority_connection,
        &wamn_principal::PlatformComponent::Provisioning.principal_id(),
        "admin:revoke-environment-member",
    )
    .await?;
    wamn_identity::identity::org::revoke_member(
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
    let active: bool = sqlx::query_scalar("SELECT active FROM platform.\"user\" WHERE id = $1")
        .bind(member_id)
        .fetch_one(&mut client)
        .await?;
    assert!(!active);
    assert!(
        application_users(&mut client)
            .await?
            .iter()
            .all(|user| user.id != member)
    );
    let revoked: bool =
        sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM platform.pat WHERE id = $1")
            .bind(pat_id)
            .fetch_one(&mut client)
            .await?;
    assert!(revoked);
    let has_roles: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT FROM platform.user_role WHERE user_id = $1)")
            .bind(member_id)
            .fetch_one(&mut client)
            .await?;
    assert!(!has_roles);

    let mut tx = common::transaction(&mut client, &actor, "admin:grant-disabled-member").await?;
    let error = mutate_grant(
        &mut tx,
        &"fill--dev--fixture".parse()?,
        GrantAuthority::User(&actor),
        GrantChange::UserRole {
            user_id: &member,
            role: "editor",
            grant: true,
        },
    )
    .await
    .expect_err("a disabled user cannot receive an authored grant");
    assert!(matches!(
        error.refusal(),
        Some(AdministrationRefusal::UserNotFound { user_id }) if *user_id == member
    ));
    tx.rollback().await?;

    let mut authority =
        common::transaction(&mut authority_connection, &actor, "admin:re-add-member").await?;
    wamn_identity::identity::org::grant_member(
        &mut authority,
        &member,
        target.org,
        target.project,
        target.env,
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
    let (active, has_roles, pat_revoked): (bool, bool, bool) = sqlx::query_as(
        "SELECT active,
                EXISTS(SELECT FROM platform.user_role WHERE user_id = u.id),
                (SELECT revoked_at IS NOT NULL FROM platform.pat WHERE id = $2)
           FROM platform.\"user\" u WHERE id = $1",
    )
    .bind(member_id)
    .bind(pat_id)
    .fetch_one(&mut client)
    .await?;
    assert_eq!((active, has_roles, pat_revoked), (true, false, true));
    Ok(())
}

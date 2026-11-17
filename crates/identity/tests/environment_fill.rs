//! Identity provisioning fills the rows that application foreign keys reference.

use sqlx::Row as _;
use wamn_environment::{application::ApplicationTarget, environment::fill};

#[tokio::test]
#[ignore = "requires: postgres"]
async fn provisioning_fills_platform_identity_rows_from_the_platform_declaration()
-> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let mut case_server = wamn_test_postgres::start(&[]).unwrap();
    let system = url::Url::parse(
        case_server
            .create_database("fixture_1")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    wamn_provision::test_database::system(system.as_str()).await;
    {
        let batches: &[&str] = &[
            &wamn_provision::test_database::seed_org_project_env_sql(
                &"fill".parse().unwrap(),
                &"fixture".parse().unwrap(),
                &"dev".parse().unwrap(),
            ),
        ];
        let mut setup_connection = wamn_postgres::connect(system.as_str()).await.unwrap();
        for batch in batches {
            sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
                .execute(&mut setup_connection)
                .await?;
        }
    }
    let mut authority_connection = wamn_postgres::connect(system.as_str()).await?;
    let mut authority = wamn_test_postgres::transaction(
        &mut authority_connection,
        &wamn_provision::PlatformComponent::Provisioning.principal_id(),
        "admin:environment-fill",
    )
    .await?;
    let member = *wamn_identity::create_user(
        &mut authority,
        "member@example.invalid",
        "member@example.invalid",
        "Member",
    )
    .await?
    .id();
    wamn_identity::org::activate_org_membership(
        &mut authority,
        &member,
        &"fill".parse().unwrap(),
    )
    .await?;
    wamn_identity::org::grant_member(
        &mut authority,
        &member,
        &"fill".parse().unwrap(),
        &"fixture".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await?;
    authority.commit().await?;

    let project = url::Url::parse(
        case_server
            .create_database("fixture_extra")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    wamn_identity::test_database::project_platform(project.as_str()).await;
    let target = ApplicationTarget {
        org: &"fill".parse().unwrap(),
        project: &"fixture".parse().unwrap(),
        env: &"dev".parse().unwrap(),
    };
    fill(
        system.as_str(),
        project.as_str(),
        "https://identity.example.invalid",
        target,
    )
    .await?;
    let mut client = wamn_postgres::connect(project.as_str()).await?;
    for component in wamn_identity_protocol::PlatformComponent::ALL {
        let row = sqlx::query("SELECT type='platform' AND email=$2 AND created_by=$3 FROM platform.\"user\" WHERE id=$1").bind(component.principal_id()).bind(format!("{}@example.invalid",component.as_str())).bind(wamn_identity_protocol::PlatformComponent::Provisioning.principal_id()).fetch_one(&mut client).await?;
        assert!(row.get::<bool, _>(0));
    }
    let present: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT FROM platform.\"user\" WHERE id=$1::text::uuid)")
            .bind(member.to_string())
            .fetch_one(&mut client)
            .await?;
    assert!(present);
    let mut authority = wamn_test_postgres::transaction(
        &mut authority_connection,
        &wamn_provision::PlatformComponent::Provisioning.principal_id(),
        "admin:revoke-environment-member",
    )
    .await?;
    wamn_identity::org::revoke_member(
        &mut authority,
        &member,
        &"fill".parse().unwrap(),
        &"fixture".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await?;
    authority.commit().await?;
    fill(
        system.as_str(),
        project.as_str(),
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

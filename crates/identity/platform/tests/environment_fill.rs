//! Identity provisioning fills the rows that application foreign keys reference.

use sqlx::Row as _;
use wamn_platform_identity::{application::ApplicationTarget, environment::fill};

#[tokio::test]
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
    wamn_control_provision::test_database::system(system.as_str()).await;
    {
        let batches: &[&str] = &[
            &wamn_control_provision::test_database::seed_org_project_env_sql(
                "fill", "fixture", "dev",
            ),
            "SET app.user_id='770df186-ac15-579e-b46b-c297cae2011b'; UPDATE registry.meta SET platform_domain='example.invalid'",
        ];
        let mut setup_connection = wamn_postgres::connect(system.as_str()).await.unwrap();
        for batch in batches {
            sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
                .execute(&mut setup_connection)
                .await?;
        }
    }
    let mut authority = wamn_postgres::connect(system.as_str()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ),
    ))
    .execute(&mut authority)
    .await?;
    let member = *wamn_platform_identity::create_user(
        &mut authority,
        "member@example.invalid",
        "member@example.invalid",
        "Member",
    )
    .await?
    .id();
    wamn_platform_identity::org::activate_org_membership(&mut authority, &member, "fill").await?;
    wamn_platform_identity::org::grant_member(&mut authority, &member, "fill", "fixture", "dev")
        .await?;
    let service = *wamn_platform_identity::create_service(
        &mut authority,
        "automation",
        "Automation",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await?
    .id();
    wamn_platform_identity::assign_project_role(
        &mut authority,
        &service,
        "fill",
        "fixture",
        "project-admin",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await?;

    let project = url::Url::parse(
        case_server
            .create_database("fixture_extra")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    wamn_platform_identity::test_database::project_platform(project.as_str()).await;
    let target = ApplicationTarget {
        org: "fill",
        project: "fixture",
        env: "dev",
        project_env_key: "fill--dev--fixture",
    };
    fill(
        system.as_str(),
        project.as_str(),
        target,
        &wamn_broker::Publisher::default(),
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
    let service_row: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as(
            "SELECT created_at, updated_at FROM platform.\"user\" WHERE id=$1::text::uuid",
        )
        .bind(service.to_string())
        .fetch_one(&mut client)
        .await?;
    fill(
        system.as_str(),
        project.as_str(),
        target,
        &wamn_broker::Publisher::default(),
    )
    .await?;
    let preserved: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "SELECT created_at, updated_at FROM platform.\"user\" WHERE id=$1::text::uuid",
    )
    .bind(service.to_string())
    .fetch_one(&mut client)
    .await?;
    assert_eq!(preserved, service_row);
    let mut drops = wamn_broker::auth_changes::AuthDrops::default();
    wamn_platform_identity::org::revoke_member(
        &mut authority,
        &member,
        "fill",
        "fixture",
        "dev",
        &mut drops,
    )
    .await?;
    fill(
        system.as_str(),
        project.as_str(),
        target,
        &wamn_broker::Publisher::default(),
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

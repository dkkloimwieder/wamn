//! Bootstrap invitation refusals against a private PostgreSQL database.

use sqlx::{Connection as _, Row as _};
use wamn_broker::Publisher;
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};
use wamn_environment::invite::{InviteRequest, invite};
use wamn_identity_client::PatIssuerConfig;
use wamn_platform_identity::org::{MemberGrants, invite_member};

#[tokio::test]
async fn invite_refuses_an_org_with_an_active_admin_before_contacting_identity() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let mut transaction = client.begin().await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&bind_platform_principal_sql(PlatformComponent::Provisioning)).to_string(),
    ))
    .execute(&mut *transaction)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql("INSERT INTO registry.orgs(id, placement_type) VALUES ('acme', 'dedicated')")
        .execute(&mut *transaction)
        .await
        .map(drop)
        .unwrap();
    let mut admin = wamn_platform_identity::create_or_reuse_user(
        &mut transaction,
        "admin@example.test",
        "Admin",
    )
    .await
    .unwrap();
    invite_member(
        &mut transaction,
        &mut admin.principal_id,
        "acme",
        &MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        },
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    let request = InviteRequest {
        email: "next@example.test".into(),
        display_name: "Next".into(),
        org: "acme".into(),
        grants: MemberGrants {
            org_admin: true,
            ..MemberGrants::default()
        },
        system_database_url: database.url().into(),
        // Identity is deliberately unconfigured. The org guard must refuse first.
        identity: PatIssuerConfig::default(),
    };
    let error = invite(&request, &Publisher::default()).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "org acme already has an active org admin; use user.invite"
    );
    let row = sqlx::query("SELECT count(*) FROM identity.org_memberships WHERE org = 'acme'")
        .fetch_one(&mut client)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>(0), 1);

    // provision-org makes the owner the admin without a password, so the
    // owner's own invitation passes the guard and reaches identity.
    let owner = InviteRequest {
        email: " Admin@Example.test".into(),
        ..request
    };
    let error = invite(&owner, &Publisher::default()).await.unwrap_err();
    assert!(
        !error
            .to_string()
            .contains("already has an active org admin"),
        "{error:#}"
    );

    drop(client);
}

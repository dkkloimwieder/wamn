//! Membership ordering and application authority on private PostgreSQL.

use tokio_postgres::{Client, NoTls};
use wamn_broker::auth_changes::AuthDrops;
use wamn_control_provision::{PlatformComponent, platform_principals_sql, test_database};
use wamn_identity_protocol::PrincipalId;
use wamn_platform_identity::application::{ApplicationTarget, project_authority};
use wamn_platform_identity::org::{
    activate_org_membership, deactivate_org_membership, grant_project_admin, lock_membership,
};
use wamn_platform_identity::{assign_project_role, create_or_reuse_user, create_service};

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move { connection.await.expect("private database connection") });
    client.execute("SELECT set_config('app.user_id',$1,false), set_config('app.operation','wamn:provisioning',false)", &[&PlatformComponent::Provisioning.principal_id().to_string()]).await?;
    Ok(client)
}

async fn blocked(observer: &Client, waiting: i32, holder: i32) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let found: bool = observer
                .query_one("SELECT $2=ANY(pg_blocking_pids($1))", &[&waiting, &holder])
                .await?
                .get(0);
            if found {
                return anyhow::Ok(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inactive_membership_serializes_grants_and_refuses_sessions() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = test_database::system();
    let mut first = connect(database.url()).await?;
    let observer = connect(database.url()).await?;
    first
        .batch_execute(&test_database::seed_org_project_env_sql(
            "acme", "billing", "dev",
        ))
        .await?;
    let user = create_or_reuse_user(&first, "member@example.test", "Member")
        .await?
        .principal_id;
    activate_org_membership(&first, &user, "acme").await?;
    let holder: i32 = first
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    let mut second = connect(database.url()).await?;
    let waiting: i32 = second
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    let tx = first.transaction().await?;
    lock_membership(&tx, &user, "acme").await?;
    let target = user;
    let removal = tokio::spawn(async move {
        let tx = second.transaction().await?;
        deactivate_org_membership(&tx, &target, "acme", &mut AuthDrops::default()).await?;
        tx.commit().await?;
        anyhow::Ok(second)
    });
    blocked(&observer, waiting, holder).await?;
    grant_project_admin(&tx, &user, "acme", "billing").await?;
    tx.commit().await?;
    let mut second = removal.await??;
    let remaining: i64 = observer
        .query_one(
            "SELECT count(*) FROM identity.project_roles WHERE principal_id=$1::text::uuid",
            &[&user.to_string().as_str()],
        )
        .await?
        .get(0);
    assert_eq!(remaining, 0);

    activate_org_membership(&first, &user, "acme").await?;
    let tx = first.transaction().await?;
    deactivate_org_membership(&tx, &user, "acme", &mut AuthDrops::default()).await?;
    let target = user;
    let grant = tokio::spawn(async move {
        let tx = second.transaction().await?;
        let refusal = grant_project_admin(&tx, &target, "acme", "billing")
            .await
            .expect_err("inactive membership refuses after the lock wait");
        assert_eq!(
            refusal.refusal(),
            Some(wamn_platform_identity::IdentityRefusal::UserNotActive)
        );
        tx.rollback().await?;
        anyhow::Ok(second)
    });
    blocked(&observer, waiting, holder).await?;
    tx.commit().await?;
    let _second = grant.await??;

    // Retain inconsistent access rows to test membership independently of roles.
    first.execute("INSERT INTO identity.project_roles(principal_id,org,project,role) VALUES($1::text::uuid,'acme','billing','project-admin')", &[&user.to_string().as_str()]).await?;
    first.execute("INSERT INTO identity.project_env_memberships(principal_id,org,project,env) VALUES($1::text::uuid,'acme','billing','dev')", &[&user.to_string().as_str()]).await?;
    let issuer = "https://identity.example.test";
    let audience = "urn:wamn:control:acme";
    let tx = first.transaction().await?;
    let login = wamn_platform_identity::password_login::create_login(&tx, &user, issuer, audience)
        .await?
        .expect("active global user");
    tx.commit().await?;
    let claims: wamn_identity_protocol::token::SessionClaims = serde_json::from_value(
        serde_json::json!({
            "iss": issuer, "sub": user.to_string().as_str(), "org": "acme", "aud": audience,
            "roles": ["admin"], "iat": 1, "exp": 901, "jti": "membership-test", "authority": {"login": login.login.id}
        }),
    )?;
    assert!(!wamn_platform_identity::control::control_session_is_active(&first, &claims).await?);
    assert!(
        !wamn_platform_identity::session_token::session_is_active(
            &first, &claims, "billing", "dev"
        )
        .await?
    );
    assert!(
        !wamn_platform_identity::has_project_env_membership(
            &first, &user, "acme", "billing", "dev"
        )
        .await?
    );
    assert!(
        !wamn_platform_identity::control::is_project_admin(&first, &user, "acme", "billing", None)
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn service_admin_projects_without_user_membership_and_stale_admin_is_removed()
-> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let system_database = test_database::system();
    let target_database = wamn_platform_identity::test_database::project_platform();
    let mut system = connect(system_database.url()).await?;
    let mut target = connect(target_database.url()).await?;
    system
        .batch_execute(&test_database::seed_org_project_env_sql(
            "acme", "billing", "dev",
        ))
        .await?;
    system
        .batch_execute("UPDATE registry.meta SET platform_domain='example.test'")
        .await?;
    target
        .batch_execute(&platform_principals_sql("example.test")?)
        .await?;
    let service = create_service(
        &system,
        "operator-test",
        "Operator",
        &mut AuthDrops::default(),
    )
    .await?;
    let principal: PrincipalId = *service.id();
    let coordinate = ApplicationTarget {
        org: "acme",
        project: "billing",
        env: "dev",
        project_env_key: "acme--dev--billing",
    };
    for expected in [true, false] {
        let tx = system.transaction().await?;
        if expected {
            assign_project_role(
                &tx,
                &principal,
                "acme",
                "billing",
                "project-admin",
                &mut AuthDrops::default(),
            )
            .await?;
        } else {
            tx.execute(
                "DELETE FROM identity.project_roles WHERE principal_id=$1::text::uuid",
                &[&principal.to_string().as_str()],
            )
            .await?;
        }
        tx.commit().await?;
        let authority = system.transaction().await?;
        let application = target.transaction().await?;
        project_authority(
            &authority,
            &application,
            coordinate,
            &principal,
            &mut AuthDrops::default(),
        )
        .await?;
        application.commit().await?;
        authority.commit().await?;
        let admin: bool = target.query_one("SELECT EXISTS(SELECT 1 FROM platform.user_role WHERE user_id=$1::text::uuid AND role_name='admin')", &[&principal.to_string().as_str()]).await?.get(0);
        assert_eq!(admin, expected);
        let service_row: String = target
            .query_one(
                "SELECT type FROM platform.\"user\" WHERE id=$1::text::uuid",
                &[&principal.to_string().as_str()],
            )
            .await?
            .get(0);
        assert_eq!(service_row, "service");
    }
    let memberships: i64 = system
        .query_one(
            "SELECT count(*) FROM identity.org_memberships WHERE principal_id=$1::text::uuid",
            &[&principal.to_string().as_str()],
        )
        .await?
        .get(0);
    assert_eq!(memberships, 0);
    Ok(())
}

#[tokio::test]
async fn authored_mutations_refuse_direct_admin_and_escalation() -> anyhow::Result<()> {
    use wamn_platform_identity::application::{
        ApplicationUser, GrantAuthority, GrantChange, create_role, mutate_grant, write_user,
    };
    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    let mut client = connect(database.url()).await?;
    client
        .batch_execute(&platform_principals_sql("example.test")?)
        .await?;
    let actor: PrincipalId = "a5500000-0000-4000-8000-000000000001".parse()?;
    let target: PrincipalId = "a5500000-0000-4000-8000-000000000002".parse()?;
    let mut drops = AuthDrops::default();
    let tx = client.transaction().await?;
    for (principal, email) in [
        (&actor, "actor@example.test"),
        (&target, "target@example.test"),
    ] {
        write_user(
            &tx,
            "test",
            ApplicationUser {
                principal_id: principal,
                email,
                display_name: email,
            },
            &mut drops,
        )
        .await?;
    }
    for role in ["delegated", "reader"] {
        create_role(&tx, "test", role, &mut drops).await?;
    }
    mutate_grant(
        &tx,
        "test",
        GrantAuthority::Provisioning,
        GrantChange::UserRole {
            user_id: actor.to_string().as_str(),
            role: "delegated",
            grant: true,
        },
        &mut drops,
    )
    .await?;
    mutate_grant(
        &tx,
        "test",
        GrantAuthority::Provisioning,
        GrantChange::Permission {
            role: "delegated",
            reference: "example:item/read",
            grant: true,
        },
        &mut drops,
    )
    .await?;
    for grant in [true, false] {
        let error = mutate_grant(
            &tx,
            "test",
            GrantAuthority::User(&actor),
            GrantChange::UserRole {
                user_id: target.to_string().as_str(),
                role: "admin",
                grant,
            },
            &mut drops,
        )
        .await
        .expect_err("direct admin mutation refuses");
        assert!(matches!(
            error.refusal(),
            Some(wamn_platform_identity::application::AdministrationRefusal::AdminCovered { .. })
        ));
    }
    mutate_grant(
        &tx,
        "test",
        GrantAuthority::User(&actor),
        GrantChange::Permission {
            role: "reader",
            reference: "example:item/read",
            grant: true,
        },
        &mut drops,
    )
    .await?;
    for reference in ["example:item/write", "wamn-control:permission/grant"] {
        let error = mutate_grant(
            &tx,
            "test",
            GrantAuthority::User(&actor),
            GrantChange::Permission {
                role: "reader",
                reference,
                grant: true,
            },
            &mut drops,
        )
        .await
        .expect_err("unheld or fixed operation refuses");
        assert!(matches!(
            error.refusal(),
            Some(wamn_platform_identity::application::AdministrationRefusal::NotGrantable { .. })
        ));
    }
    tx.commit().await?;
    let permissions: Vec<String> = client
        .query(
            "SELECT permission FROM platform.permission WHERE role_name='reader'",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(permissions, ["example:item/read"]);
    Ok(())
}

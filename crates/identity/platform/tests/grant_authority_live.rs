//! Membership ordering and application authority on private PostgreSQL.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};

use wamn_broker::auth_changes::AuthDrops;
use wamn_control_provision::{PlatformComponent, platform_principals_sql, test_database};
use wamn_identity_protocol::PrincipalId;
use wamn_platform_identity::application::{ApplicationTarget, project_authority};
use wamn_platform_identity::org::{
    activate_org_membership, deactivate_org_membership, grant_project_admin, lock_membership,
};
use wamn_platform_identity::{assign_project_role, create_or_reuse_user, create_service};

async fn connect(url: &str) -> anyhow::Result<Client> {
    let mut client = wamn_postgres::connect(url).await?;

    sqlx::query("SELECT set_config('app.user_id',$1,false), set_config('app.operation','wamn:provisioning',false)").bind(&PlatformComponent::Provisioning.principal_id().to_string()).execute(&mut client).await.map(|result| result.rows_affected())?;
    Ok(client)
}

async fn blocked(observer: &mut Client, waiting: i32, holder: i32) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let found: bool = sqlx::query("SELECT $2=ANY(pg_blocking_pids($1))")
                .bind(&waiting)
                .bind(&holder)
                .fetch_one(&mut *observer)
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
    let mut observer = connect(database.url()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&test_database::seed_org_project_env_sql("acme", "billing", "dev")).to_string(),
    ))
    .execute(&mut first)
    .await
    .map(drop)?;
    let user = create_or_reuse_user(&mut first, "member@example.test", "Member")
        .await?
        .principal_id;
    activate_org_membership(&mut first, &user, "acme").await?;
    let holder: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut first)
        .await?
        .get(0);
    let mut second = connect(database.url()).await?;
    let waiting: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut second)
        .await?
        .get(0);
    let mut tx = first.begin().await?;
    lock_membership(&mut tx, &user, "acme").await?;
    let target = user;
    let removal = tokio::spawn(async move {
        let mut tx = second.begin().await?;
        deactivate_org_membership(&mut tx, &target, "acme", &mut AuthDrops::default()).await?;
        tx.commit().await?;
        anyhow::Ok(second)
    });
    blocked(&mut observer, waiting, holder).await?;
    grant_project_admin(&mut tx, &user, "acme", "billing").await?;
    tx.commit().await?;
    let mut second = removal.await??;
    let remaining: i64 = sqlx::query(
        "SELECT count(*) FROM identity.project_roles WHERE principal_id=$1::text::uuid",
    )
    .bind(&user.to_string().as_str())
    .fetch_one(&mut observer)
    .await?
    .get(0);
    assert_eq!(remaining, 0);

    activate_org_membership(&mut first, &user, "acme").await?;
    let mut tx = first.begin().await?;
    deactivate_org_membership(&mut tx, &user, "acme", &mut AuthDrops::default()).await?;
    let target = user;
    let grant = tokio::spawn(async move {
        let mut tx = second.begin().await?;
        let refusal = grant_project_admin(&mut tx, &target, "acme", "billing")
            .await
            .expect_err("inactive membership refuses after the lock wait");
        assert_eq!(
            refusal.refusal(),
            Some(wamn_platform_identity::IdentityRefusal::UserNotActive)
        );
        tx.rollback().await?;
        anyhow::Ok(second)
    });
    blocked(&mut observer, waiting, holder).await?;
    tx.commit().await?;
    let _second = grant.await??;

    // Retain inconsistent access rows to test membership independently of roles.
    sqlx::query("INSERT INTO identity.project_roles(principal_id,org,project,role) VALUES($1::text::uuid,'acme','billing','project-admin')").bind(&user.to_string().as_str()).execute(&mut first).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO identity.project_env_memberships(principal_id,org,project,env) VALUES($1::text::uuid,'acme','billing','dev')").bind(&user.to_string().as_str()).execute(&mut first).await.map(|result| result.rows_affected())?;
    let issuer = "https://identity.example.test";
    let audience = "urn:wamn:control:acme";
    let mut tx = first.begin().await?;
    let login =
        wamn_platform_identity::password_login::create_login(&mut tx, &user, issuer, audience)
            .await?
            .expect("active global user");
    tx.commit().await?;
    let claims: wamn_identity_protocol::token::SessionClaims = serde_json::from_value(
        serde_json::json!({
            "iss": issuer, "sub": user.to_string().as_str(), "org": "acme", "aud": audience,
            "roles": ["admin"], "iat": 1, "exp": 901, "jti": "membership-test", "authority": {"login": login.login.id}
        }),
    )?;
    assert!(
        !wamn_platform_identity::control::control_session_is_active(&mut first, &claims).await?
    );
    assert!(
        !wamn_platform_identity::session_token::session_is_active(
            &mut first, &claims, "billing", "dev"
        )
        .await?
    );
    assert!(
        !wamn_platform_identity::has_project_env_membership(
            &mut first, &user, "acme", "billing", "dev"
        )
        .await?
    );
    assert!(
        !wamn_platform_identity::control::is_project_admin(
            &mut first, &user, "acme", "billing", None
        )
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
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&test_database::seed_org_project_env_sql("acme", "billing", "dev")).to_string(),
    ))
    .execute(&mut system)
    .await
    .map(drop)?;
    sqlx::raw_sql("UPDATE registry.meta SET platform_domain='example.test'")
        .execute(&mut system)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&platform_principals_sql("example.test")?).to_string(),
    ))
    .execute(&mut target)
    .await
    .map(drop)?;
    let service = create_service(
        &mut system,
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
        let mut tx = system.begin().await?;
        if expected {
            assign_project_role(
                &mut tx,
                &principal,
                "acme",
                "billing",
                "project-admin",
                &mut AuthDrops::default(),
            )
            .await?;
        } else {
            sqlx::query("DELETE FROM identity.project_roles WHERE principal_id=$1::text::uuid")
                .bind(&principal.to_string().as_str())
                .execute(&mut *tx)
                .await
                .map(|result| result.rows_affected())?;
        }
        tx.commit().await?;
        let mut authority = system.begin().await?;
        let mut application = target.begin().await?;
        project_authority(
            &mut authority,
            &mut *application,
            coordinate,
            &principal,
            &mut AuthDrops::default(),
        )
        .await?;
        application.commit().await?;
        authority.commit().await?;
        let admin: bool = sqlx::query("SELECT EXISTS(SELECT 1 FROM platform.user_role WHERE user_id=$1::text::uuid AND role_name='admin')").bind(&principal.to_string().as_str()).fetch_one(&mut target).await?.get(0);
        assert_eq!(admin, expected);
        let service_row: String =
            sqlx::query("SELECT type FROM platform.\"user\" WHERE id=$1::text::uuid")
                .bind(&principal.to_string().as_str())
                .fetch_one(&mut target)
                .await?
                .get(0);
        assert_eq!(service_row, "service");
    }
    let memberships: i64 = sqlx::query(
        "SELECT count(*) FROM identity.org_memberships WHERE principal_id=$1::text::uuid",
    )
    .bind(&principal.to_string().as_str())
    .fetch_one(&mut system)
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
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&platform_principals_sql("example.test")?).to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)?;
    let actor: PrincipalId = "a5500000-0000-4000-8000-000000000001".parse()?;
    let target: PrincipalId = "a5500000-0000-4000-8000-000000000002".parse()?;
    let mut drops = AuthDrops::default();
    let mut tx = client.begin().await?;
    for (principal, email) in [
        (&actor, "actor@example.test"),
        (&target, "target@example.test"),
    ] {
        write_user(
            &mut tx,
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
        create_role(&mut tx, "test", role, &mut drops).await?;
    }
    mutate_grant(
        &mut tx,
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
        &mut tx,
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
            &mut tx,
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
        &mut tx,
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
            &mut tx,
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
    let permissions: Vec<String> =
        sqlx::query("SELECT permission FROM platform.permission WHERE role_name='reader'")
            .fetch_all(&mut client)
            .await?
            .into_iter()
            .map(|row| row.get(0))
            .collect();
    assert_eq!(permissions, ["example:item/read"]);
    Ok(())
}

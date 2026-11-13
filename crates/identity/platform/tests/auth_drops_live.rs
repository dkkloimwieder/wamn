//! The list of every writer of a fact that the host keeps in its auth memory
//! (docs/architecture/execution.md#auth-memory). Each writer runs once on
//! real PostgreSQL, and the test tests the drop it records. A new writer
//! takes an `AuthDrops`, so it does not compile until it joins this list.

use sqlx::{Connection as _, Row as _};
use std::time::Duration;
use wamn_broker::Publisher;

use sqlx::PgConnection as Client;
use wamn_broker::auth_changes::{AuthChange, AuthDrops};
use wamn_control_provision::{PlatformComponent, platform_principals_sql, test_database};
use wamn_identity_protocol::PrincipalId;
use wamn_platform_identity::application::{
    ApplicationUser, create_role, delete_role, remove_admin, remove_user, write_environment_status,
    write_service, write_user,
};
use wamn_platform_identity::org::{
    EnvironmentStatus, activate_org_membership, deactivate_org_membership, grant_member,
    grant_org_admin, grant_project_admin, revoke_member, revoke_org_admin, revoke_project_admin,
};
use wamn_platform_identity::password::{
    Password, enroll_password, issue_invitation, issue_reset, password_work, reset_password,
};
use wamn_platform_identity::password_login::{
    authenticate_renewal, create_login, revoke_all, revoke_login, rotate_login,
};
use wamn_platform_identity::{
    create_service, create_user, disable_principal, grant_project_env_membership, issue_pat,
    revoke_pat, revoke_project_env_membership,
};

const ISSUER: &str = "https://identity.example.invalid";
const AUDIENCE: &str = "acme:billing:dev:a1b2c3d4";

fn actor() -> PrincipalId {
    PlatformComponent::Provisioning
        .principal_id()
        .to_string()
        .parse()
        .unwrap()
}

async fn connect(url: &str, role: Option<&str>) -> Client {
    let mut client = wamn_postgres::connect(url).await.unwrap();

    if let Some(role) = role {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET ROLE {role}")))
            .execute(&mut client)
            .await
            .map(drop)
            .unwrap();
    }
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'wamn:provisioning', false)",
    )
    .bind(&actor().to_string().as_str())
    .execute(&mut client)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    client
}

async fn login(
    client: &mut Client,
    principal: &PrincipalId,
) -> wamn_platform_identity::password_login::Renewal {
    let mut tx = client.begin().await.unwrap();
    let issued = create_login(&mut tx, principal, ISSUER, AUDIENCE)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    issued
}

fn identity(principal: &PrincipalId) -> Vec<AuthChange> {
    vec![AuthChange::Identity(principal.to_string())]
}

fn changes(drops: &AuthDrops) -> Vec<AuthChange> {
    drops.changes().cloned().collect()
}

#[tokio::test]
async fn every_identity_writer_records_a_drop_of_its_principal() {
    let _lock = wamn_test_postgres::lock();
    let database = test_database::system();
    let mut client = connect(database.url(), Some("wamn_system")).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&test_database::seed_org_project_env_sql("acme", "billing", "dev")).to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    let alice = *create_user(
        &mut client,
        "alice@example.invalid",
        "alice@example.invalid",
        "Alice",
    )
    .await
    .unwrap()
    .id();

    // The org writers.
    activate_org_membership(&mut client, &alice, "acme")
        .await
        .unwrap();
    grant_member(&mut client, &alice, "acme", "billing", "dev")
        .await
        .unwrap();
    let mut drops = AuthDrops::default();
    revoke_member(&mut client, &alice, "acme", "billing", "dev", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), identity(&alice), "revoke_member");

    grant_project_admin(&mut client, &alice, "acme", "billing")
        .await
        .unwrap();
    let mut drops = AuthDrops::default();
    revoke_project_admin(&mut client, &alice, "acme", "billing", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), identity(&alice), "revoke_project_admin");

    grant_org_admin(&mut client, &alice, "acme").await.unwrap();
    let mut drops = AuthDrops::default();
    revoke_org_admin(&mut client, &alice, "acme", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), identity(&alice), "revoke_org_admin");

    let mut drops = AuthDrops::default();
    deactivate_org_membership(&mut client, &alice, "acme", &mut drops)
        .await
        .unwrap();
    assert_eq!(
        changes(&drops),
        identity(&alice),
        "deactivate_org_membership"
    );

    activate_org_membership(&mut client, &alice, "acme")
        .await
        .unwrap();
    grant_project_env_membership(&mut client, &alice, "acme", "billing", "dev")
        .await
        .unwrap();
    let mut drops = AuthDrops::default();
    revoke_project_env_membership(&mut client, &alice, "acme", "billing", "dev", &mut drops)
        .await
        .unwrap();
    assert_eq!(
        changes(&drops),
        identity(&alice),
        "revoke_project_env_membership"
    );

    // The PAT writer finds the principal from the token prefix.
    let service = create_service(&mut client, "api", "API", &mut AuthDrops::default())
        .await
        .unwrap();
    let issued = issue_pat(
        &mut client,
        service.id(),
        "laptop",
        Duration::from_secs(3600),
    )
    .await
    .unwrap();
    let mut drops = AuthDrops::default();
    revoke_pat(&mut client, issued.record().prefix(), &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), identity(service.id()), "revoke_pat");

    // The login writers find the principal from the renewal secret.
    let first = login(&mut client, &alice).await;
    let mut tx = client.begin().await.unwrap();
    let mut drops = AuthDrops::default();
    revoke_login(&mut tx, ISSUER, AUDIENCE, first.secret(), &mut drops)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(changes(&drops), identity(&alice), "revoke_login");

    // A replayed renewal secret revokes its login in both readers of it.
    let second = login(&mut client, &alice).await;
    let mut tx = client.begin().await.unwrap();
    let mut drops = AuthDrops::default();
    rotate_login(&mut tx, ISSUER, AUDIENCE, second.secret(), &mut drops)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(changes(&drops), Vec::new(), "a rotation revokes nothing");
    let mut tx = client.begin().await.unwrap();
    let mut drops = AuthDrops::default();
    assert!(
        rotate_login(&mut tx, ISSUER, AUDIENCE, second.secret(), &mut drops)
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    assert_eq!(changes(&drops), identity(&alice), "rotate_login replay");
    let mut tx = client.begin().await.unwrap();
    let mut drops = AuthDrops::default();
    assert!(
        authenticate_renewal(&mut tx, ISSUER, AUDIENCE, second.secret(), &mut drops)
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    assert_eq!(
        changes(&drops),
        identity(&alice),
        "authenticate_renewal replay"
    );

    let mut tx = client.begin().await.unwrap();
    let mut drops = AuthDrops::default();
    revoke_all(&mut tx, &alice, &mut drops).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(changes(&drops), identity(&alice), "revoke_all");

    // A reset revokes every login of the principal.
    let work = password_work();
    let password = || Password::new("my uncommon warehouse passphrase".to_owned()).unwrap();
    let invitation = issue_invitation(&mut client, &actor(), &alice)
        .await
        .unwrap();
    enroll_password(&mut client, &work, &alice, invitation.secret(), password())
        .await
        .unwrap();
    let reset = issue_reset(&mut client, &actor(), &alice, "alice@example.invalid")
        .await
        .unwrap();
    let mut drops = AuthDrops::default();
    reset_password(
        &mut client,
        &work,
        &alice,
        reset.secret(),
        password(),
        &mut drops,
    )
    .await
    .unwrap();
    assert_eq!(changes(&drops), identity(&alice), "reset_password");

    let mut drops = AuthDrops::default();
    disable_principal(&mut client, &alice, &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), identity(&alice), "disable_principal");
}

#[tokio::test]
async fn every_application_writer_records_a_drop_of_its_project_env_key_or_principal() {
    let database = wamn_platform_identity::test_database::project_platform();
    let mut client = connect(database.url(), None).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&platform_principals_sql("example.invalid").unwrap()).to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    // The writes stamp wamn:provisioning for the whole session.
    sqlx::query("SELECT set_config('app.operation', $1, false)")
        .bind(&PlatformComponent::Provisioning.principal_name())
        .execute(&mut client)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    let bob: PrincipalId = "0b0b0b0b-0000-4000-8000-000000000001".parse().unwrap();
    let robot: PrincipalId = "0b0b0b0b-0000-4000-8000-000000000002".parse().unwrap();
    let user = |id| ApplicationUser {
        principal_id: id,
        email: "bob@example.invalid",
        display_name: "Bob",
    };
    let principal = |id: &PrincipalId| {
        vec![AuthChange::Principal {
            project_env_key: "t1".to_owned(),
            principal: id.to_string(),
        }]
    };
    let project_env_key = vec![AuthChange::ProjectEnvKey("t1".to_owned())];

    let mut drops = AuthDrops::default();
    write_user(&mut client, "t1", user(&bob), &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), principal(&bob), "write_user");

    let mut drops = AuthDrops::default();
    remove_admin(&mut client, "t1", &bob, &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), principal(&bob), "remove_admin");

    let mut drops = AuthDrops::default();
    write_service(
        &mut client,
        "t1",
        ApplicationUser {
            principal_id: &robot,
            email: "robot@example.invalid",
            display_name: "Robot",
        },
        &mut drops,
    )
    .await
    .unwrap();
    assert_eq!(changes(&drops), principal(&robot), "write_service");

    let mut drops = AuthDrops::default();
    create_role(&mut client, "t1", "clerk", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), project_env_key, "create_role");

    let mut drops = AuthDrops::default();
    wamn_platform_identity::application::mutate_grant(
        &mut client,
        "t1",
        wamn_platform_identity::application::GrantAuthority::Provisioning,
        wamn_platform_identity::application::GrantChange::Permission {
            role: "clerk",
            reference: "acme:receipts/create",
            grant: true,
        },
        &mut drops,
    )
    .await
    .unwrap();
    assert_eq!(changes(&drops), project_env_key, "grant_permission");

    let mut drops = AuthDrops::default();
    wamn_platform_identity::application::mutate_grant(
        &mut client,
        "t1",
        wamn_platform_identity::application::GrantAuthority::Provisioning,
        wamn_platform_identity::application::GrantChange::Permission {
            role: "clerk",
            reference: "acme:receipts/create",
            grant: false,
        },
        &mut drops,
    )
    .await
    .unwrap();
    assert_eq!(changes(&drops), project_env_key, "revoke_permission");

    let mut drops = AuthDrops::default();
    wamn_platform_identity::application::mutate_grant(
        &mut client,
        "t1",
        wamn_platform_identity::application::GrantAuthority::Provisioning,
        wamn_platform_identity::application::GrantChange::UserRole {
            user_id: bob.to_string().as_str(),
            role: "clerk",
            grant: true,
        },
        &mut drops,
    )
    .await
    .unwrap();
    assert_eq!(changes(&drops), principal(&bob), "grant_user_role");

    let mut drops = AuthDrops::default();
    wamn_platform_identity::application::mutate_grant(
        &mut client,
        "t1",
        wamn_platform_identity::application::GrantAuthority::Provisioning,
        wamn_platform_identity::application::GrantChange::UserRole {
            user_id: bob.to_string().as_str(),
            role: "clerk",
            grant: false,
        },
        &mut drops,
    )
    .await
    .unwrap();
    assert_eq!(changes(&drops), principal(&bob), "revoke_user_role");

    let mut drops = AuthDrops::default();
    delete_role(&mut client, "t1", "clerk", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), project_env_key, "delete_role");

    let mut drops = AuthDrops::default();
    remove_user(&mut client, "t1", &bob, &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), principal(&bob), "remove_user");

    let mut drops = AuthDrops::default();
    write_environment_status(&mut client, "t1", EnvironmentStatus::Inactive, &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), project_env_key, "write_environment_status");
}

/// The broker must be private to this test run.
#[tokio::test]
#[ignore = "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY"]
async fn every_application_grant_write_deletes_exactly_its_auth_keys() {
    use wamn_broker::auth_changes::auth_bucket;
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY",
    );
    let broker = wamn_broker::nats::PlatformNats::from_env()
        .unwrap()
        .connect()
        .await
        .unwrap();
    wamn_broker::kv_memory::create_buckets(&broker)
        .await
        .unwrap();
    let bucket = auth_bucket(&broker).await.unwrap();
    let publisher = Publisher::new(broker);
    let database = wamn_platform_identity::test_database::project_platform();
    let mut client = connect(database.url(), None).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&platform_principals_sql("example.invalid").unwrap()).to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    let bob: PrincipalId = "0b0b0b0b-0000-4000-8000-000000000001".parse().unwrap();
    let robot: PrincipalId = "0b0b0b0b-0000-4000-8000-000000000002".parse().unwrap();
    let mut seed_drops = AuthDrops::default();
    for principal in [&bob, &robot] {
        write_user(
            &mut client,
            "t1",
            ApplicationUser {
                principal_id: principal,
                email: principal.to_string().as_str(),
                display_name: "Fixture",
            },
            &mut seed_drops,
        )
        .await
        .unwrap();
    }
    let keys = [
        "t1.0b0b0b0b-0000-4000-8000-000000000001",
        "t1.0b0b0b0b-0000-4000-8000-000000000002",
        "t1.environment",
        "t2.0b0b0b0b-0000-4000-8000-000000000001",
    ];
    for operation in [
        "create_role",
        "grant_permission",
        "revoke_permission",
        "grant_user_role",
        "revoke_user_role",
        "delete_role",
    ] {
        for key in keys {
            bucket.put(key, "cached".into()).await.unwrap();
        }
        let mut drops = AuthDrops::default();
        match operation {
            "create_role" => {
                create_role(&mut client, "t1", "clerk", &mut drops)
                    .await
                    .unwrap();
            }
            "grant_permission" => {
                wamn_platform_identity::application::mutate_grant(
                    &mut client,
                    "t1",
                    wamn_platform_identity::application::GrantAuthority::Provisioning,
                    wamn_platform_identity::application::GrantChange::Permission {
                        role: "clerk",
                        reference: "acme:receipts/create",
                        grant: true,
                    },
                    &mut drops,
                )
                .await
                .unwrap();
            }
            "revoke_permission" => {
                wamn_platform_identity::application::mutate_grant(
                    &mut client,
                    "t1",
                    wamn_platform_identity::application::GrantAuthority::Provisioning,
                    wamn_platform_identity::application::GrantChange::Permission {
                        role: "clerk",
                        reference: "acme:receipts/create",
                        grant: false,
                    },
                    &mut drops,
                )
                .await
                .unwrap();
            }
            "grant_user_role" => {
                wamn_platform_identity::application::mutate_grant(
                    &mut client,
                    "t1",
                    wamn_platform_identity::application::GrantAuthority::Provisioning,
                    wamn_platform_identity::application::GrantChange::UserRole {
                        user_id: bob.to_string().as_str(),
                        role: "clerk",
                        grant: true,
                    },
                    &mut drops,
                )
                .await
                .unwrap();
            }
            "revoke_user_role" => {
                wamn_platform_identity::application::mutate_grant(
                    &mut client,
                    "t1",
                    wamn_platform_identity::application::GrantAuthority::Provisioning,
                    wamn_platform_identity::application::GrantChange::UserRole {
                        user_id: bob.to_string().as_str(),
                        role: "clerk",
                        grant: false,
                    },
                    &mut drops,
                )
                .await
                .unwrap();
            }
            "delete_role" => {
                delete_role(&mut client, "t1", "clerk", &mut drops)
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        publisher.publish(drops).await;
        let principal_only = matches!(operation, "grant_user_role" | "revoke_user_role");
        for (index, key) in keys.into_iter().enumerate() {
            let deleted = index == 0 || (!principal_only && index < 3);
            assert_eq!(
                bucket.get(key).await.unwrap().is_none(),
                deleted,
                "{operation}: {key}"
            );
        }
    }
    for key in keys {
        bucket.delete(key).await.unwrap();
    }
}

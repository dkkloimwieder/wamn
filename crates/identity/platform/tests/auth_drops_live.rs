//! The list of every writer of a fact that the host keeps in its auth memory
//! (docs/architecture/execution.md#auth-memory). Each writer runs once on
//! real PostgreSQL, and the test checks the drop it records. A new writer
//! takes an `AuthDrops`, so it does not compile until it joins this list.

use std::time::Duration;

use tokio_postgres::Client;
use wamn_control_provision::{PlatformComponent, platform_principals_sql, test_database};
use wamn_platform_identity::application::{
    ApplicationUser, create_role, delete_role, grant_permission, grant_user_role, remove_admin,
    remove_user, revoke_permission, revoke_user_role, write_admin, write_environment_status,
    write_service, write_user,
};
use wamn_platform_identity::auth_changes::{AuthChange, AuthDrops};
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
    PrincipalId, create_user, disable_principal, grant_project_env_membership, issue_pat,
    revoke_pat, revoke_project_env_membership,
};

const ISSUER: &str = "https://identity.example.invalid";
const AUDIENCE: &str = "acme:billing:dev:a1b2c3d4";
const SEED_SQL: &str = "\
    INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES ('acme', 'pooled', 'wamn-pg'); \
    INSERT INTO registry.env_policies \
      (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) VALUES \
      ('acme', 'dev', '\"own\"', 0, 1, '1Gi', '1', '1Gi', 'postgres:18'); \
    INSERT INTO registry.projects (org, id) VALUES ('acme', 'billing'); \
    INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix, policy_name) VALUES \
      ('acme', 'billing', 'dev', 's1', 'aaaaaaa1', 'dev');";

fn actor() -> PrincipalId {
    PlatformComponent::Provisioning
        .principal_id()
        .to_string()
        .parse()
        .unwrap()
}

async fn connect(url: &str, role: Option<&str>) -> Client {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    if let Some(role) = role {
        client
            .batch_execute(&format!("SET ROLE {role}"))
            .await
            .unwrap();
    }
    client
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&actor().as_str()],
        )
        .await
        .unwrap();
    client
}

async fn login(
    client: &mut Client,
    principal: &PrincipalId,
) -> wamn_platform_identity::password_login::Renewal {
    let tx = client.transaction().await.unwrap();
    let issued = create_login(&tx, principal, ISSUER, AUDIENCE)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    issued
}

fn identity(principal: &PrincipalId) -> Vec<AuthChange> {
    vec![AuthChange::Identity(principal.as_str().to_owned())]
}

fn changes(drops: &AuthDrops) -> Vec<AuthChange> {
    drops.changes().cloned().collect()
}

#[tokio::test]
async fn every_identity_writer_records_a_drop_of_its_principal() {
    let _lock = wamn_test_postgres::lock();
    let database = test_database::system();
    let mut client = connect(database.url(), Some("wamn_system")).await;
    client.batch_execute(SEED_SQL).await.unwrap();
    let alice = create_user(
        &client,
        "alice@example.invalid",
        "alice@example.invalid",
        "Alice",
    )
    .await
    .unwrap()
    .id()
    .clone();

    // The org writers.
    activate_org_membership(&client, &alice, "acme")
        .await
        .unwrap();
    grant_member(&client, &alice, "acme", "billing", "dev")
        .await
        .unwrap();
    let mut drops = AuthDrops::default();
    revoke_member(&client, &alice, "acme", "billing", "dev", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), identity(&alice), "revoke_member");

    grant_project_admin(&client, &alice, "acme", "billing")
        .await
        .unwrap();
    let mut drops = AuthDrops::default();
    revoke_project_admin(&client, &alice, "acme", "billing", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), identity(&alice), "revoke_project_admin");

    grant_org_admin(&client, &alice, "acme").await.unwrap();
    let mut drops = AuthDrops::default();
    revoke_org_admin(&client, &alice, "acme", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), identity(&alice), "revoke_org_admin");

    let mut drops = AuthDrops::default();
    deactivate_org_membership(&client, &alice, "acme", &mut drops)
        .await
        .unwrap();
    assert_eq!(
        changes(&drops),
        identity(&alice),
        "deactivate_org_membership"
    );

    grant_project_env_membership(&client, &alice, "acme", "billing", "dev")
        .await
        .unwrap();
    let mut drops = AuthDrops::default();
    revoke_project_env_membership(&client, &alice, "acme", "billing", "dev", &mut drops)
        .await
        .unwrap();
    assert_eq!(
        changes(&drops),
        identity(&alice),
        "revoke_project_env_membership"
    );

    // The PAT writer finds the principal from the token prefix.
    let issued = issue_pat(&client, &alice, "laptop", Duration::from_secs(3600))
        .await
        .unwrap();
    let mut drops = AuthDrops::default();
    revoke_pat(&client, issued.record().prefix(), &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), identity(&alice), "revoke_pat");

    // The login writers find the principal from the renewal secret.
    let first = login(&mut client, &alice).await;
    let tx = client.transaction().await.unwrap();
    let mut drops = AuthDrops::default();
    revoke_login(&tx, ISSUER, AUDIENCE, first.secret(), &mut drops)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(changes(&drops), identity(&alice), "revoke_login");

    // A replayed renewal secret revokes its login in both readers of it.
    let second = login(&mut client, &alice).await;
    let tx = client.transaction().await.unwrap();
    let mut drops = AuthDrops::default();
    rotate_login(&tx, ISSUER, AUDIENCE, second.secret(), &mut drops)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(changes(&drops), Vec::new(), "a rotation revokes nothing");
    let tx = client.transaction().await.unwrap();
    let mut drops = AuthDrops::default();
    assert!(
        rotate_login(&tx, ISSUER, AUDIENCE, second.secret(), &mut drops)
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    assert_eq!(changes(&drops), identity(&alice), "rotate_login replay");
    let tx = client.transaction().await.unwrap();
    let mut drops = AuthDrops::default();
    assert!(
        authenticate_renewal(&tx, ISSUER, AUDIENCE, second.secret(), &mut drops)
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

    let tx = client.transaction().await.unwrap();
    let mut drops = AuthDrops::default();
    revoke_all(&tx, &alice, &mut drops).await.unwrap();
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
    disable_principal(&client, &alice, &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), identity(&alice), "disable_principal");
}

#[tokio::test]
async fn every_application_writer_records_a_drop_of_its_tenant_or_principal() {
    let database = wamn_project_state::test_database::tenant_platform();
    let client = connect(database.url(), None).await;
    client
        .batch_execute(&platform_principals_sql("t1", "example.invalid").unwrap())
        .await
        .unwrap();
    // The writes stamp wamn:provisioning for the whole session.
    client
        .execute(
            "SELECT set_config('app.operation', $1, false)",
            &[&PlatformComponent::Provisioning.principal_name()],
        )
        .await
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
            tenant: "t1".to_owned(),
            principal: id.as_str().to_owned(),
        }]
    };
    let tenant = vec![AuthChange::Tenant("t1".to_owned())];

    let mut drops = AuthDrops::default();
    write_user(&client, "t1", user(&bob), &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), principal(&bob), "write_user");

    let mut drops = AuthDrops::default();
    write_admin(&client, "t1", user(&bob), &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), principal(&bob), "write_admin");

    let mut drops = AuthDrops::default();
    remove_admin(&client, "t1", &bob, &mut drops).await.unwrap();
    assert_eq!(changes(&drops), principal(&bob), "remove_admin");

    let mut drops = AuthDrops::default();
    write_service(
        &client,
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
    create_role(&client, "t1", "clerk", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), tenant, "create_role");

    let mut drops = AuthDrops::default();
    grant_permission(&client, "t1", "clerk", "acme:receipts/create", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), tenant, "grant_permission");

    let mut drops = AuthDrops::default();
    revoke_permission(&client, "t1", "clerk", "acme:receipts/create", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), tenant, "revoke_permission");

    let mut drops = AuthDrops::default();
    grant_user_role(&client, "t1", bob.as_str(), "clerk", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), principal(&bob), "grant_user_role");

    let mut drops = AuthDrops::default();
    revoke_user_role(&client, "t1", bob.as_str(), "clerk", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), principal(&bob), "revoke_user_role");

    let mut drops = AuthDrops::default();
    delete_role(&client, "t1", "clerk", &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), tenant, "delete_role");

    let mut drops = AuthDrops::default();
    remove_user(&client, "t1", &bob, &mut drops).await.unwrap();
    assert_eq!(changes(&drops), principal(&bob), "remove_user");

    let mut drops = AuthDrops::default();
    write_environment_status(&client, "t1", EnvironmentStatus::Inactive, &mut drops)
        .await
        .unwrap();
    assert_eq!(changes(&drops), tenant, "write_environment_status");
}

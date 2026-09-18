//! Password enrollment uses the real system floor under its non-superuser owner.

use sha2::{Digest as _, Sha256};
use tokio_postgres::{Client, error::SqlState};
use wamn_control_provision::{PlatformComponent, test_database};
use wamn_platform_identity::password::{
    Password, PasswordErrorKind, authenticate_password, enroll_password, issue_invitation,
    password_work,
};
use wamn_platform_identity::{
    Principal, PrincipalId, authenticate_pat, create_human, create_service, disable_principal,
    issue_pat,
};

const PASSWORD: &str = "my uncommon warehouse passphrase";
fn password() -> Password {
    Password::new(PASSWORD.to_owned()).unwrap()
}
fn actor() -> PrincipalId {
    PlatformComponent::Provisioning
        .principal_id()
        .to_string()
        .parse()
        .unwrap()
}
async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        connection.await.unwrap();
    });
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    client
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&actor().as_str()],
        )
        .await
        .unwrap();
    client
}
async fn human(client: &Client, name: &str) -> Principal {
    create_human(
        client,
        &format!("{name}@example.invalid"),
        &format!("{name}@example.invalid"),
        name,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn enrollment_preserves_identity_and_pat_and_consumes_all_invitations() {
    let _lock = wamn_test_postgres::lock();
    let database = test_database::system();
    let mut client = connect(database.url()).await;
    let person = human(&client, "alice").await;
    let other = human(&client, "bob").await;
    let work = password_work();
    let pat = issue_pat(
        &client,
        person.id(),
        "existing",
        std::time::Duration::from_secs(3600),
    )
    .await
    .unwrap();
    client.batch_execute("INSERT INTO registry.orgs (id, placement_kind) VALUES ('password-test', 'dedicated');
        INSERT INTO registry.projects (org, id) VALUES ('password-test', 'receiving');
        INSERT INTO registry.env_policies (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image)
        VALUES ('password-test', 'dev', '\"own\"'::jsonb, 1, 1, '1Gi', '1', '1Gi', 'postgres:18');
        INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix)
        VALUES ('password-test', 'receiving', 'dev', 'password-test-secret', 'a1b2c3d4')").await.unwrap();
    wamn_platform_identity::grant_project_env_membership(
        &client,
        person.id(),
        "password-test",
        "receiving",
        "dev",
    )
    .await
    .unwrap();
    let membership: String = client.query_one("SELECT row_to_json(m)::text FROM identity.project_env_memberships m WHERE principal_id = $1::text::uuid", &[&person.id().as_str()]).await.unwrap().get(0);
    // Capture principal and authority rows as they exist before enrollment.
    let before: String = client
        .query_one(
            "SELECT row_to_json(p)::text FROM identity.principals p WHERE id = $1::text::uuid",
            &[&person.id().as_str()],
        )
        .await
        .unwrap()
        .get(0);
    let first = issue_invitation(&mut client, &actor(), person.id())
        .await
        .unwrap();
    let second = issue_invitation(&mut client, &actor(), person.id())
        .await
        .unwrap();
    assert_ne!(first.secret(), second.secret());
    assert!(!format!("{first:?}").contains(first.secret()));
    assert_eq!(
        enroll_password(&mut client, &work, other.id(), first.secret(), password())
            .await
            .unwrap_err()
            .kind(),
        PasswordErrorKind::Refused
    );
    let short = Password::new("short".into()).unwrap();
    assert_eq!(
        enroll_password(&mut client, &work, person.id(), first.secret(), short)
            .await
            .unwrap_err()
            .kind(),
        PasswordErrorKind::Policy
    );
    enroll_password(&mut client, &work, person.id(), first.secret(), password())
        .await
        .unwrap();
    for secret in [first.secret(), second.secret()] {
        assert_eq!(
            enroll_password(&mut client, &work, person.id(), secret, password())
                .await
                .unwrap_err()
                .kind(),
            PasswordErrorKind::Refused
        );
    }
    assert_eq!(
        issue_invitation(&mut client, &actor(), person.id())
            .await
            .unwrap_err()
            .kind(),
        PasswordErrorKind::Refused
    );
    let after: String = client
        .query_one(
            "SELECT row_to_json(p)::text FROM identity.principals p WHERE id = $1::text::uuid",
            &[&person.id().as_str()],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(before, after);
    assert_eq!(membership, client.query_one("SELECT row_to_json(m)::text FROM identity.project_env_memberships m WHERE principal_id = $1::text::uuid", &[&person.id().as_str()]).await.unwrap().get::<_, String>(0));
    let authenticated = authenticate_password(&client, &work, "alice@example.invalid", password())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(authenticated.principal().id(), person.id());
    assert_eq!(
        authenticate_pat(&client, pat.token())
            .await
            .unwrap()
            .unwrap()
            .principal()
            .id(),
        person.id()
    );
    assert!(
        authenticate_password(
            &client,
            &work,
            "alice@example.invalid",
            Password::new("incorrect".into()).unwrap()
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(
        authenticate_password(&client, &work, "unknown@example.invalid", password())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        authenticate_password(&client, &work, "bob@example.invalid", password())
            .await
            .unwrap()
            .is_none()
    );
    let row = client.query_one("SELECT password_hash, created_by::text FROM identity.password_credentials WHERE principal_id = $1::text::uuid", &[&person.id().as_str()]).await.unwrap();
    let hash: String = row.get(0);
    assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert!(!hash.contains(PASSWORD));
    assert_eq!(row.get::<_, &str>(1), person.id().as_str());
    let rows = client.query("SELECT token_hash, consumed_at IS NOT NULL, created_by::text FROM identity.password_tokens WHERE principal_id = $1::text::uuid", &[&person.id().as_str()]).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r.get::<_, bool>(1) && r.get::<_, &str>(2) == actor().as_str())
    );
    assert!(
        rows.iter()
            .any(|r| r.get::<_, Vec<u8>>(0) == Sha256::digest(first.secret().as_bytes()).to_vec())
    );
    disable_principal(&client, person.id()).await.unwrap();
    assert!(
        authenticate_password(&client, &work, "alice@example.invalid", password())
            .await
            .unwrap()
            .is_none()
    );
    client
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    assert_eq!(
        client
            .query("SELECT * FROM identity.password_credentials", &[])
            .await
            .unwrap_err()
            .code(),
        Some(&SqlState::INSUFFICIENT_PRIVILEGE)
    );
}

#[tokio::test]
async fn invitation_expiry_kind_purpose_and_transaction_rollback_refuse() {
    let _lock = wamn_test_postgres::lock();
    let database = test_database::system();
    let mut client = connect(database.url()).await;
    let person = human(&client, "expired").await;
    let work = password_work();
    let invitation = issue_invitation(&mut client, &actor(), person.id())
        .await
        .unwrap();
    client.execute("UPDATE identity.password_tokens SET expires_at = created_at + interval '1 microsecond' WHERE principal_id = $1::text::uuid", &[&person.id().as_str()]).await.unwrap();
    assert_eq!(
        enroll_password(
            &mut client,
            &work,
            person.id(),
            invitation.secret(),
            password()
        )
        .await
        .unwrap_err()
        .kind(),
        PasswordErrorKind::Refused
    );
    let service = create_service(&client, "password-service", "Service")
        .await
        .unwrap();
    assert_eq!(
        issue_invitation(&mut client, &actor(), service.id())
            .await
            .unwrap_err()
            .kind(),
        PasswordErrorKind::Refused
    );
    assert_eq!(
        issue_invitation(&mut client, &actor(), &actor())
            .await
            .unwrap_err()
            .kind(),
        PasswordErrorKind::Refused
    );
    let error = client.execute("INSERT INTO identity.password_tokens (token_hash, principal_id, purpose, expires_at) VALUES ($1, $2::text::uuid, 'unknown-purpose', clock_timestamp() + interval '1 hour')", &[&vec![1u8;32], &person.id().as_str()]).await.unwrap_err();
    assert_eq!(error.code(), Some(&SqlState::CHECK_VIOLATION));
    let error = client.execute("INSERT INTO identity.password_tokens (token_hash, principal_id, purpose, expires_at) VALUES ($1, $2::text::uuid, 'invitation', clock_timestamp() + interval '1 hour')", &[&vec![2u8;32], &service.id().as_str()]).await.unwrap_err();
    assert_eq!(error.code(), Some(&SqlState::FOREIGN_KEY_VIOLATION));
    let invitation = issue_invitation(&mut client, &actor(), person.id())
        .await
        .unwrap();
    client.batch_execute("CREATE FUNCTION identity.refuse_consumption() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test refusal'; END $$; CREATE TRIGGER test_refuse BEFORE UPDATE ON identity.password_tokens FOR EACH ROW EXECUTE FUNCTION identity.refuse_consumption()").await.unwrap();
    assert_eq!(
        enroll_password(
            &mut client,
            &work,
            person.id(),
            invitation.secret(),
            password()
        )
        .await
        .unwrap_err()
        .kind(),
        PasswordErrorKind::Infrastructure
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM identity.password_credentials", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    client
        .batch_execute("DROP TRIGGER test_refuse ON identity.password_tokens")
        .await
        .unwrap();
    enroll_password(
        &mut client,
        &work,
        person.id(),
        invitation.secret(),
        password(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_enrollment_creates_exactly_one_password() {
    let _lock = wamn_test_postgres::lock();
    let database = test_database::system();
    let mut left = connect(database.url()).await;
    let mut right = connect(database.url()).await;
    let person = human(&left, "concurrent").await;
    let first = issue_invitation(&mut left, &actor(), person.id())
        .await
        .unwrap();
    let second = issue_invitation(&mut right, &actor(), person.id())
        .await
        .unwrap();
    let work = password_work();
    let (a, b) = tokio::join!(
        enroll_password(&mut left, &work, person.id(), first.secret(), password()),
        enroll_password(
            &mut right,
            &work,
            person.id(),
            second.secret(),
            Password::new("another uncommon passphrase".into()).unwrap()
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        a.err().or(b.err()).unwrap().kind(),
        PasswordErrorKind::Refused
    );
    assert_eq!(
        left.query_one("SELECT count(*) FROM identity.password_credentials", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        left.query_one(
            "SELECT count(*) FROM identity.password_tokens WHERE consumed_at IS NULL",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}

#[tokio::test]
async fn reset_consumes_email_credentials_and_revokes_renewal_but_preserves_pat() {
    use wamn_platform_identity::{
        password::{issue_reset, reset_password},
        password_login,
    };
    let _lock = wamn_test_postgres::lock();
    let database = test_database::system();
    let mut client = connect(database.url()).await;
    let person = human(&client, "reset-person").await;
    let other = human(&client, "reset-other").await;
    let work = password_work();
    let invitation = issue_invitation(&mut client, &actor(), person.id())
        .await
        .unwrap();
    enroll_password(
        &mut client,
        &work,
        person.id(),
        invitation.secret(),
        password(),
    )
    .await
    .unwrap();
    let pat = issue_pat(
        &client,
        person.id(),
        "preserved",
        std::time::Duration::from_secs(3600),
    )
    .await
    .unwrap();
    let tx = client.transaction().await.unwrap();
    let renewal =
        password_login::create_login(&tx, person.id(), "https://reset.invalid", "receiving")
            .await
            .unwrap()
            .unwrap();
    tx.commit().await.unwrap();
    let first = issue_reset(&mut client, &actor(), person.id())
        .await
        .unwrap();
    let second = issue_reset(&mut client, &actor(), person.id())
        .await
        .unwrap();
    assert!(
        reset_password(&mut client, &work, other.id(), first.secret(), password())
            .await
            .is_err()
    );
    assert!(
        enroll_password(&mut client, &work, person.id(), first.secret(), password())
            .await
            .is_err()
    );
    let replacement = || Password::new("the new long replacement password".into()).unwrap();
    reset_password(
        &mut client,
        &work,
        person.id(),
        first.secret(),
        replacement(),
    )
    .await
    .unwrap();
    assert!(
        reset_password(
            &mut client,
            &work,
            person.id(),
            first.secret(),
            replacement()
        )
        .await
        .is_err()
    );
    assert!(
        reset_password(
            &mut client,
            &work,
            person.id(),
            second.secret(),
            replacement()
        )
        .await
        .is_err()
    );
    assert!(
        authenticate_password(&client, &work, "reset-person@example.invalid", password())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        authenticate_password(
            &client,
            &work,
            "reset-person@example.invalid",
            replacement()
        )
        .await
        .unwrap()
        .is_some()
    );
    let tx = client.transaction().await.unwrap();
    assert!(
        password_login::rotate_login(&tx, "https://reset.invalid", "receiving", renewal.secret())
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    assert!(
        authenticate_pat(&client, pat.token())
            .await
            .unwrap()
            .is_some()
    );
    let expired = issue_reset(&mut client, &actor(), person.id())
        .await
        .unwrap();
    client.batch_execute("UPDATE identity.password_tokens SET expires_at=created_at+interval '1 microsecond' WHERE purpose='reset'").await.unwrap();
    assert!(
        reset_password(
            &mut client,
            &work,
            person.id(),
            expired.secret(),
            replacement()
        )
        .await
        .is_err()
    );
}

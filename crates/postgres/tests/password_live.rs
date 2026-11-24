//! Password enrollment uses the real system base grants under its non-superuser owner.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use wamn_authentication::password::{Password, PasswordErrorType, RefusalCause, password_work};
use wamn_authentication::principal::Principal;
use wamn_identity_protocol::PrincipalId;
use wamn_postgres::identity::password::{authenticate_password, enroll_password, issue_invitation};
use wamn_postgres::identity::{create_user, disable_principal};
use wamn_provision::PlatformComponent;

use wamn_test_infrastructure::identity as common;

const PASSWORD: &str = "my uncommon warehouse passphrase";
fn password() -> Password {
    Password::new(PASSWORD.to_owned()).unwrap()
}
fn actor() -> PrincipalId {
    PlatformComponent::Provisioning.principal_id()
}
async fn connect(url: &str) -> Client {
    let mut client = wamn_postgres::connect(url).await.unwrap();

    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    client
}
async fn new_user(client: &mut Client, name: &str) -> Principal {
    create_user(
        client,
        &format!("{name}@example.invalid"),
        &format!("{name}@example.invalid"),
        name,
    )
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn enrollment_preserves_identity_and_pat_and_consumes_all_invitations() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = common::system(&postgres).await;
    let mut client = connect(database.as_str()).await;
    let user = new_user(&mut client, "alice").await;
    let other = new_user(&mut client, "bob").await;
    let work = password_work();
    sqlx::raw_sql("INSERT INTO registry.orgs (id) VALUES ('password-test');
        INSERT INTO registry.projects (org, id) VALUES ('password-test', 'widgets');
        WITH parent_environment AS (INSERT INTO registry.environments (org,env) VALUES ('password-test','dev') ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('password-test','widgets','dev','password-test-secret','a1b2c3d4')").execute(&mut client).await.map(drop).unwrap();
    wamn_postgres::identity::org::activate_org_membership(
        &mut client,
        user.id(),
        &"password-test".parse().unwrap(),
    )
    .await
    .unwrap();
    wamn_postgres::identity::grant_project_env_membership(
        &mut client,
        user.id(),
        &"password-test".parse().unwrap(),
        &"widgets".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await
    .unwrap();
    let membership: String = sqlx::query("SELECT row_to_json(m)::text FROM identity.project_env_memberships m WHERE principal_id = $1::text::uuid").bind(user.id().to_string().as_str()).fetch_one(&mut client).await.unwrap().get(0);
    // Capture principal and authority rows as they exist before enrollment.
    let before: String = sqlx::query(
        "SELECT row_to_json(p)::text FROM identity.principals p WHERE id = $1::text::uuid",
    )
    .bind(user.id().to_string().as_str())
    .fetch_one(&mut client)
    .await
    .unwrap()
    .get(0);
    let first = issue_invitation(&mut client, &actor(), user.id())
        .await
        .unwrap();
    let second = issue_invitation(&mut client, &actor(), user.id())
        .await
        .unwrap();
    assert_ne!(first.secret(), second.secret());
    assert!(!format!("{first:?}").contains(first.secret()));
    let expires: String = sqlx::query("SELECT to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI') FROM identity.password_tokens WHERE token_hash = $1").bind(wamn_digest::bytes_sha256(first.secret().as_bytes()).to_string()).fetch_one(&mut client).await.unwrap().get(0);
    assert_eq!(first.expires_at(), expires);
    assert_eq!(
        enroll_password(&mut client, &work, other.id(), first.secret(), password())
            .await
            .unwrap_err()
            .cause(),
        Some(RefusalCause::UnknownSecret)
    );
    let short = Password::new("short".into()).unwrap();
    assert_eq!(
        enroll_password(&mut client, &work, user.id(), first.secret(), short)
            .await
            .unwrap_err()
            .cause(),
        Some(RefusalCause::PasswordRule)
    );
    enroll_password(&mut client, &work, user.id(), first.secret(), password())
        .await
        .unwrap();
    for secret in [first.secret(), second.secret()] {
        assert_eq!(
            enroll_password(&mut client, &work, user.id(), secret, password())
                .await
                .unwrap_err()
                .cause(),
            Some(RefusalCause::Consumed)
        );
    }
    assert_eq!(
        issue_invitation(&mut client, &actor(), user.id())
            .await
            .unwrap_err()
            .cause(),
        Some(RefusalCause::AlreadyEnrolled)
    );
    let after: String = sqlx::query(
        "SELECT row_to_json(p)::text FROM identity.principals p WHERE id = $1::text::uuid",
    )
    .bind(user.id().to_string().as_str())
    .fetch_one(&mut client)
    .await
    .unwrap()
    .get(0);
    assert_eq!(before, after);
    assert_eq!(membership, sqlx::query("SELECT row_to_json(m)::text FROM identity.project_env_memberships m WHERE principal_id = $1::text::uuid").bind(user.id().to_string().as_str()).fetch_one(&mut client).await.unwrap().get::<String, _>(0));
    let authenticated =
        authenticate_password(&mut client, &work, "alice@example.invalid", password())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(authenticated.principal().id(), user.id());
    assert!(
        authenticate_password(
            &mut client,
            &work,
            "alice@example.invalid",
            Password::new("incorrect".into()).unwrap()
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(
        authenticate_password(&mut client, &work, "unknown@example.invalid", password())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        authenticate_password(&mut client, &work, "bob@example.invalid", password())
            .await
            .unwrap()
            .is_none()
    );
    let row = sqlx::query("SELECT password_hash, created_by::text FROM identity.password_credentials WHERE principal_id = $1::text::uuid").bind(user.id().to_string().as_str()).fetch_one(&mut client).await.unwrap();
    let hash: String = row.get(0);
    assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert!(!hash.contains(PASSWORD));
    assert_eq!(row.get::<&str, _>(1), user.id().to_string().as_str());
    let rows = sqlx::query("SELECT token_hash, consumed_at IS NOT NULL, created_by::text FROM identity.password_tokens WHERE principal_id = $1::text::uuid").bind(user.id().to_string().as_str()).fetch_all(&mut client).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r.get::<bool, _>(1) && r.get::<&str, _>(2) == actor().to_string().as_str())
    );
    assert!(rows.iter().any(
        |r| r.get::<&str, _>(0).parse::<wamn_digest::Digest>().unwrap()
            == wamn_digest::bytes_sha256(first.secret().as_bytes())
    ));
    disable_principal(&mut client, user.id()).await.unwrap();
    assert!(
        authenticate_password(&mut client, &work, "alice@example.invalid", password())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn invitation_expiry_type_purpose_and_transaction_rollback_refuse() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = common::system(&postgres).await;
    let mut client = connect(database.as_str()).await;
    let user = new_user(&mut client, "expired").await;
    let work = password_work();
    let invitation = issue_invitation(&mut client, &actor(), user.id())
        .await
        .unwrap();
    sqlx::query("UPDATE identity.password_tokens SET expires_at = created_at + interval '1 microsecond' WHERE principal_id = $1::text::uuid").bind(user.id().to_string().as_str()).execute(&mut client).await.map(|result| result.rows_affected()).unwrap();
    assert_eq!(
        enroll_password(
            &mut client,
            &work,
            user.id(),
            invitation.secret(),
            password()
        )
        .await
        .unwrap_err()
        .cause(),
        Some(RefusalCause::Expired)
    );
    assert_eq!(
        issue_invitation(&mut client, &actor(), &actor())
            .await
            .unwrap_err()
            .cause(),
        Some(RefusalCause::NoSuchAccount)
    );
    let error = sqlx::query(
        "INSERT INTO identity.password_tokens (token_hash, principal_id, purpose, expires_at) \
         VALUES ($1, $2, 'unknown-purpose', clock_timestamp() + interval '1 hour')",
    )
    .bind(wamn_digest::Digest::from([1u8; 32]).to_string())
    .bind(sqlx::types::Uuid::from(*user.id()))
    .execute(&mut client)
    .await
    .map(|result| result.rows_affected())
    .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23514")
    );
    let invitation = issue_invitation(&mut client, &actor(), user.id())
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION identity.refuse_consumption() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test refusal'; END $$; CREATE TRIGGER test_refuse BEFORE UPDATE ON identity.password_tokens FOR EACH ROW EXECUTE FUNCTION identity.refuse_consumption()").execute(&mut client).await.map(drop).unwrap();
    assert_eq!(
        enroll_password(
            &mut client,
            &work,
            user.id(),
            invitation.secret(),
            password()
        )
        .await
        .unwrap_err()
        .error_type(),
        PasswordErrorType::Infrastructure
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.password_credentials")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<i64, _>(0),
        0
    );
    sqlx::raw_sql("DROP TRIGGER test_refuse ON identity.password_tokens")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    enroll_password(
        &mut client,
        &work,
        user.id(),
        invitation.secret(),
        password(),
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn concurrent_enrollment_creates_exactly_one_password() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = common::system(&postgres).await;
    let mut left = connect(database.as_str()).await;
    let mut right = connect(database.as_str()).await;
    let user = new_user(&mut left, "concurrent").await;
    let first = issue_invitation(&mut left, &actor(), user.id())
        .await
        .unwrap();
    let second = issue_invitation(&mut right, &actor(), user.id())
        .await
        .unwrap();
    let work = password_work();
    let (a, b) = tokio::join!(
        enroll_password(&mut left, &work, user.id(), first.secret(), password()),
        enroll_password(
            &mut right,
            &work,
            user.id(),
            second.secret(),
            Password::new("another uncommon passphrase".into()).unwrap()
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        a.err().or(b.err()).unwrap().error_type(),
        PasswordErrorType::Refused
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.password_credentials")
            .fetch_one(&mut left)
            .await
            .unwrap()
            .get::<i64, _>(0),
        1
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.password_tokens WHERE consumed_at IS NULL")
            .fetch_one(&mut left)
            .await
            .unwrap()
            .get::<i64, _>(0),
        0
    );
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn reset_consumes_email_credentials_and_revokes_renewal_but_preserves_pat() {
    use wamn_postgres::identity::{
        password::{issue_reset, reset_password},
        password_login,
    };

    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let database = common::system(&postgres).await;
    let mut client = connect(database.as_str()).await;
    let user = new_user(&mut client, "reset-user").await;
    let other = new_user(&mut client, "reset-other").await;
    let work = password_work();
    let invitation = issue_invitation(&mut client, &actor(), user.id())
        .await
        .unwrap();
    enroll_password(
        &mut client,
        &work,
        user.id(),
        invitation.secret(),
        password(),
    )
    .await
    .unwrap();
    let mut tx = client.begin().await.unwrap();
    let renewal =
        password_login::create_login(&mut tx, user.id(), "https://reset.invalid", "widgets")
            .await
            .unwrap()
            .unwrap();
    tx.commit().await.unwrap();
    let first = issue_reset(
        &mut client,
        &actor(),
        user.id(),
        "reset-user@example.invalid",
    )
    .await
    .unwrap();
    let second = issue_reset(
        &mut client,
        &actor(),
        user.id(),
        "reset-user@example.invalid",
    )
    .await
    .unwrap();
    assert!(
        reset_password(&mut client, &work, other.id(), first.secret(), password())
            .await
            .is_err()
    );
    assert!(
        enroll_password(&mut client, &work, user.id(), first.secret(), password())
            .await
            .is_err()
    );
    let replacement = || Password::new("the new long replacement password".into()).unwrap();
    reset_password(&mut client, &work, user.id(), first.secret(), replacement())
        .await
        .unwrap();
    assert!(
        reset_password(&mut client, &work, user.id(), first.secret(), replacement())
            .await
            .is_err()
    );
    assert!(
        reset_password(
            &mut client,
            &work,
            user.id(),
            second.secret(),
            replacement()
        )
        .await
        .is_err()
    );
    assert!(
        authenticate_password(&mut client, &work, "reset-user@example.invalid", password())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        authenticate_password(
            &mut client,
            &work,
            "reset-user@example.invalid",
            replacement()
        )
        .await
        .unwrap()
        .is_some()
    );
    let mut tx = client.begin().await.unwrap();
    assert!(
        password_login::rotate_login(
            &mut tx,
            "https://reset.invalid",
            "widgets",
            renewal.secret()
        )
        .await
        .unwrap()
        .is_none()
    );
    tx.commit().await.unwrap();
    sqlx::query("UPDATE identity.principals SET email='replacement@example.invalid' WHERE id=$1::text::uuid").bind(user.id().to_string().as_str()).execute(&mut client).await.map(|result| result.rows_affected()).unwrap();
    assert!(
        issue_reset(
            &mut client,
            &actor(),
            user.id(),
            "reset-user@example.invalid"
        )
        .await
        .is_err()
    );
    let expired = issue_reset(
        &mut client,
        &actor(),
        user.id(),
        "replacement@example.invalid",
    )
    .await
    .unwrap();
    sqlx::raw_sql("UPDATE identity.password_tokens SET expires_at=created_at+interval '1 microsecond' WHERE purpose='reset'").execute(&mut client).await.map(drop).unwrap();
    assert!(
        reset_password(
            &mut client,
            &work,
            user.id(),
            expired.secret(),
            replacement()
        )
        .await
        .is_err()
    );
}

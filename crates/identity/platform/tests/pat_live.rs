//! Real-PostgreSQL test for the personal-access-token presenter.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _, Statement as _};
use std::time::Duration;
use wamn_broker::auth_changes::AuthDrops;

use wamn_control_provision::{PlatformComponent, SYSTEM_SCHEMA_SQL, bind_platform_principal_sql};
use wamn_identity_protocol::PAT_TOKEN_PREFIX;
use wamn_identity_protocol::PrincipalId;
use wamn_platform_identity::{
    IdentityErrorType, PrincipalType, authenticate_pat, create_service, disable_principal,
    issue_pat, list_pats, revoke_pat,
};

const TTL: Duration = Duration::from_secs(3600);

#[tokio::test]
async fn platform_pat_round_trip_on_postgres() {
    // The system schema creates the cluster-wide wamn_system and wamn_db_owner roles.
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let mut client = wamn_postgres::connect(&url)
        .await
        .expect("connect platform identity test database");

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DROP SCHEMA IF EXISTS identity CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
             THEN CREATE ROLE wamn_system; END IF; END $$;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("prepare empty platform schemas");
    sqlx::raw_sql(SYSTEM_SCHEMA_SQL)
        .execute(&mut client)
        .await
        .map(drop)
        .expect("apply the system schema composition");
    let provisioning = PlatformComponent::Provisioning.principal_id().to_string();

    // Issuance binds wamn:provisioning in its own transaction.
    let mut transaction = provisioning_transaction(&mut client).await;
    let user = create_service(
        &mut transaction,
        "author",
        "Widget Author",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await
    .expect("create service principal");
    let issued = issue_pat(&mut transaction, user.id(), " laptop ", TTL)
        .await
        .expect("issue service token from trusted context");
    transaction.commit().await.expect("commit the issuance");
    let issued_stamps = pat_stamps(&mut client, issued.record().prefix()).await;
    assert_eq!(issued_stamps.created_by, provisioning);
    assert_eq!(issued_stamps.updated_by, provisioning);
    assert_eq!(issued_stamps.created_at, issued_stamps.updated_at);
    let token = issued.token().to_owned();
    let prefix = issued.record().prefix().to_owned();
    assert!(token.starts_with(PAT_TOKEN_PREFIX));
    assert_eq!(issued.record().label(), "laptop");
    assert!(issued.record().revoked_at().is_none());
    assert!(issued.record().expires_at().ends_with('Z'));
    assert!(issued.record().expires_at() > issued.record().created_at());

    // Only a digest and non-secret lookup metadata persist.
    let stored: (String, String) = sqlx::query(
        "SELECT token_hash, encode(sha256(convert_to($1, 'UTF8')), 'hex') \
             FROM identity.pats WHERE token_prefix = $2",
    )
    .bind(&token)
    .bind(&prefix)
    .fetch_one(&mut client)
    .await
    .map(|row| (row.get(0), row.get(1)))
    .expect("read stored token row");
    assert_eq!(stored.0, stored.1);
    assert_ne!(stored.0, token);
    let leaked: i64 = sqlx::query(
        "SELECT count(*) FROM identity.pats \
             WHERE token_hash = $1 OR token_prefix = $1 OR label = $1",
    )
    .bind(&token)
    .fetch_one(&mut client)
    .await
    .expect("scan token columns for plaintext")
    .get(0);
    assert_eq!(leaked, 0);

    let authenticated = authenticate_pat(&mut client, &token)
        .await
        .expect("authenticate token")
        .expect("a valid token must authenticate");
    assert_eq!(authenticated.principal().id(), user.id());

    // A forged secret under a known lookup prefix is refused like any other.
    let forged = flip_last_hex_digit(&token);
    assert_ne!(forged, token);
    assert!(
        authenticate_pat(&mut client, &forged)
            .await
            .expect("reject forged token")
            .is_none()
    );
    let unknown = format!("{PAT_TOKEN_PREFIX}{}_{}", "f".repeat(16), "f".repeat(64));
    assert!(
        authenticate_pat(&mut client, &unknown)
            .await
            .expect("reject unknown prefix")
            .is_none()
    );
    for malformed in [
        "",
        "not-a-token",
        PAT_TOKEN_PREFIX,
        &token[PAT_TOKEN_PREFIX.len()..],
    ] {
        assert!(
            authenticate_pat(&mut client, malformed)
                .await
                .expect("reject malformed token")
                .is_none(),
            "accepted malformed token {malformed:?}"
        );
    }

    // A revocation with no bound actor refuses.
    let unbound =
        sqlx::query("UPDATE identity.pats SET revoked_at = now() WHERE token_prefix = $1")
            .bind(&issued.record().prefix())
            .execute(&mut client)
            .await
            .map(|result| result.rows_affected())
            .expect_err("a revocation with no bound actor must refuse");
    assert_eq!(
        unbound
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("55000")
    );
    assert_eq!(
        unbound
            .as_database_error()
            .map(sqlx::error::DatabaseError::message),
        Some("actor-required")
    );
    assert_eq!(
        revoke_pat(
            &mut client,
            issued.record().prefix(),
            &mut AuthDrops::default()
        )
        .await
        .expect_err("the library revocation also needs a bound actor")
        .error_type(),
        IdentityErrorType::Database
    );

    // Revocation is a one-way stamp and repeating it changes nothing. It
    // binds wamn:provisioning in a later transaction and keeps the created pair.
    let mut transaction = provisioning_transaction(&mut client).await;
    let revocable = issue_pat(&mut transaction, user.id(), "revocable", TTL)
        .await
        .expect("issue revocable token");
    transaction.commit().await.expect("commit the issuance");
    let before = pat_stamps(&mut client, revocable.record().prefix()).await;
    let mut transaction = provisioning_transaction(&mut client).await;
    let revoked = revoke_pat(
        &mut transaction,
        revocable.record().prefix(),
        &mut AuthDrops::default(),
    )
    .await
    .expect("revoke token");
    transaction.commit().await.expect("commit the revocation");
    assert!(revoked.revoked_at().is_some());
    let after = pat_stamps(&mut client, revocable.record().prefix()).await;
    assert_eq!(after.created_at, before.created_at);
    assert_eq!(after.created_by, provisioning);
    assert_eq!(after.updated_by, provisioning);
    assert!(
        after.updated_after_created,
        "the revocation moves the updated time"
    );

    // The rest of the fixture is platform setup, so it writes as wamn:provisioning.
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(&provisioning)
        .execute(&mut client)
        .await
        .map(|result| result.rows_affected())
        .expect("bind wamn:provisioning for the fixture session");
    platform_principal_cannot_hold_a_token(&mut client, &provisioning).await;
    token_rows_refuse_malformed_values(&mut client, user.id(), &prefix).await;

    // An elapsed expiry refuses without any revocation. The stamp trigger keeps
    // created_at, so the fixture moves expires_at to just after it.
    let expired = issue_pat(&mut client, user.id(), "expiring", TTL)
        .await
        .expect("issue expiring token");
    sqlx::query(
        "UPDATE identity.pats \
             SET expires_at = created_at + interval '1 microsecond' \
             WHERE token_prefix = $1",
    )
    .bind(&expired.record().prefix())
    .execute(&mut client)
    .await
    .map(|result| result.rows_affected())
    .expect("expire the expiring token");
    assert!(
        authenticate_pat(&mut client, expired.token())
            .await
            .expect("reject expired token")
            .is_none()
    );
    assert!(
        authenticate_pat(&mut client, revocable.token())
            .await
            .expect("reject revoked token")
            .is_none()
    );
    sqlx::query(
        "UPDATE identity.pats SET revoked_at = now() - interval '1 hour' \
             WHERE token_prefix = $1",
    )
    .bind(&revocable.record().prefix())
    .execute(&mut client)
    .await
    .map(|result| result.rows_affected())
    .expect("backdate the revocation");
    let backdated = revoke_pat(
        &mut client,
        revocable.record().prefix(),
        &mut AuthDrops::default(),
    )
    .await
    .expect("re-revoke token");
    assert_ne!(backdated.revoked_at(), revoked.revoked_at());
    assert_eq!(
        revoke_pat(
            &mut client,
            revocable.record().prefix(),
            &mut AuthDrops::default()
        )
        .await
        .expect("revoke token again")
        .revoked_at(),
        backdated.revoked_at()
    );
    assert_eq!(
        revoke_pat(&mut client, &"0".repeat(16), &mut AuthDrops::default())
            .await
            .expect_err("unknown prefix must not revoke")
            .error_type(),
        IdentityErrorType::NotFound
    );

    // Service principals get tokens through the same trusted-context path.
    let service = create_service(
        &mut client,
        "agent-ci",
        "CI Agent",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await
    .expect("create service principal");
    let service_token = issue_pat(&mut client, service.id(), "ci", TTL)
        .await
        .expect("issue service token");
    let service_authenticated = authenticate_pat(&mut client, service_token.token())
        .await
        .expect("authenticate service token")
        .expect("a valid service token must authenticate");
    assert_eq!(
        service_authenticated.principal().principal_type(),
        PrincipalType::Service
    );

    // Listing returns the stored metadata, newest first, and no token material.
    let listed = list_pats(&mut client, user.id())
        .await
        .expect("list user tokens");
    assert_eq!(
        listed
            .iter()
            .map(wamn_platform_identity::PatRecord::label)
            .collect::<Vec<_>>(),
        ["expiring", "revocable", "laptop"]
    );
    assert!(listed.iter().all(|pat| pat.prefix().len() == 16));
    assert!(!format!("{listed:?}").contains(&token));

    // Disabling the principal refuses live tokens and further issuance.
    disable_principal(&mut client, user.id(), &mut AuthDrops::default())
        .await
        .expect("disable user");
    assert!(
        authenticate_pat(&mut client, &token)
            .await
            .expect("reject token of a disabled principal")
            .is_none()
    );
    assert_eq!(
        issue_pat(&mut client, user.id(), "after-disable", TTL)
            .await
            .expect_err("disabled principals must not gain tokens")
            .error_type(),
        IdentityErrorType::NotFound
    );

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DROP SCHEMA identity CASCADE; \
             DROP SCHEMA provisioning CASCADE; \
             DROP SCHEMA registry CASCADE;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("remove platform identity test schemas");
    drop(client);
}

/// Forge a token that keeps its lookup prefix but carries a different secret.
/// Begin a transaction that binds wamn:provisioning, as the identity issuer and
/// wamn-ctl do.
async fn provisioning_transaction(client: &mut Client) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut transaction = client
        .begin()
        .await
        .expect("begin a provisioning transaction");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&bind_platform_principal_sql(PlatformComponent::Provisioning)).to_string(),
    ))
    .execute(&mut *transaction)
    .await
    .map(drop)
    .expect("bind wamn:provisioning");
    transaction
}

struct PatStamps {
    created_at: String,
    created_by: String,
    updated_at: String,
    updated_by: String,
    updated_after_created: bool,
}

async fn pat_stamps(client: &mut Client, prefix: &str) -> PatStamps {
    let row = sqlx::query(
        "SELECT created_at::text, created_by::text, updated_at::text, updated_by::text, \
                    updated_at > created_at \
             FROM identity.pats WHERE token_prefix = $1",
    )
    .bind(&prefix)
    .fetch_one(&mut *client)
    .await
    .expect("read token stamps");
    PatStamps {
        created_at: row.get(0),
        created_by: row.get(1),
        updated_at: row.get(2),
        updated_by: row.get(3),
        updated_after_created: row.get(4),
    }
}

/// The pats foreign key carries the principal type, so no token names the
/// platform principal, through the library or through direct SQL.
async fn platform_principal_cannot_hold_a_token(client: &mut Client, provisioning: &str) {
    let principal: PrincipalId = provisioning
        .parse()
        .expect("a derived id is a principal id");
    assert_eq!(
        issue_pat(client, &principal, "platform", TTL)
            .await
            .expect_err("a platform principal must not gain a token")
            .error_type(),
        IdentityErrorType::NotFound
    );
    for (type_, code) in [
        ("platform", "23514"),
        ("user", "23514"),
        ("service", "23503"),
    ] {
        let error = sqlx::query(
            "INSERT INTO identity.pats \
                   (principal_id, principal_type, token_prefix, token_hash, label, expires_at) \
                 VALUES ($1::text::uuid, $2, $3, $4, 'platform', now() + interval '1 hour')",
        )
        .bind(&provisioning)
        .bind(&type_)
        .bind(&"e".repeat(16))
        .bind(&"e".repeat(64))
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .expect_err("direct SQL must not bind a token to the platform principal");
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some(code),
            "{type_}"
        );
    }
}

/// Direct SQL cannot store a token row that the presenter would never write:
/// a reused lookup prefix, a digest or prefix outside lowercase hex of its
/// length, or an expiry at its creation. A principal that holds a token cannot
/// be deleted.
async fn token_rows_refuse_malformed_values(
    client: &mut Client,
    principal: &PrincipalId,
    issued_prefix: &str,
) {
    let (prefix, hash) = ("d".repeat(16), "d".repeat(64));
    for (case, token_prefix, token_hash, expiry, code) in [
        (
            "reused prefix",
            issued_prefix.to_owned(),
            hash.clone(),
            "now() + interval '1 hour'",
            "23505",
        ),
        (
            "uppercase prefix",
            "D".repeat(16),
            hash.clone(),
            "now() + interval '1 hour'",
            "23514",
        ),
        (
            "short prefix",
            "d".repeat(15),
            hash.clone(),
            "now() + interval '1 hour'",
            "23514",
        ),
        (
            "uppercase hash",
            prefix.clone(),
            "D".repeat(64),
            "now() + interval '1 hour'",
            "23514",
        ),
        (
            "short hash",
            prefix.clone(),
            "d".repeat(63),
            "now() + interval '1 hour'",
            "23514",
        ),
        (
            "expiry at creation",
            prefix.clone(),
            hash.clone(),
            "now()",
            "23514",
        ),
    ] {
        let error = sqlx::query(sqlx::AssertSqlSafe(
            (&format!(
                "INSERT INTO identity.pats \
                       (principal_id, principal_type, token_prefix, token_hash, label, expires_at) \
                     VALUES ($1::text::uuid, 'service', $2, $3, 'malformed', {expiry})"
            ))
                .to_string(),
        ))
        .bind(&principal.to_string().as_str())
        .bind(&token_prefix)
        .bind(&token_hash)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .expect_err("a malformed token row must refuse");
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some(code),
            "{case}"
        );
    }
    let error = sqlx::query("DELETE FROM identity.principals WHERE id = $1::text::uuid")
        .bind(&principal.to_string().as_str())
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .expect_err("a principal that holds a token must not be deleted");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23001")
    );
}

fn flip_last_hex_digit(token: &str) -> String {
    let (head, last) = token.split_at(token.len() - 1);
    let replacement = if last == "a" { 'b' } else { 'a' };
    format!("{head}{replacement}")
}

//! Real-PostgreSQL test for the personal-access-token presenter.

use std::time::Duration;

use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, Transaction};
use wamn_control_provision::{PlatformComponent, SYSTEM_SCHEMA_SQL, bind_platform_principal_sql};
use wamn_platform_identity::{
    IdentityErrorKind, PAT_TOKEN_PREFIX, PrincipalId, PrincipalKind, authenticate_pat,
    create_human, create_service, disable_principal, issue_pat, list_pats, revoke_pat,
};

const TTL: Duration = Duration::from_secs(3600);

#[tokio::test]
async fn platform_pat_round_trip_on_postgres() {
    // The system schema creates the cluster-wide wamn_system and wamn_db_owner roles.
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

    let (mut client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .expect("connect platform identity test database");
    let connection_task = tokio::spawn(async move {
        connection
            .await
            .expect("drive platform identity test database");
    });

    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS identity CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
             THEN CREATE ROLE wamn_system; END IF; END $$;",
        )
        .await
        .expect("prepare empty platform schemas");
    client
        .batch_execute(SYSTEM_SCHEMA_SQL)
        .await
        .expect("apply the system schema composition");
    let provisioning = PlatformComponent::Provisioning.principal_id().to_string();

    // Issuance binds wamn:provisioning in its own transaction.
    let transaction = provisioning_transaction(&mut client).await;
    let human = create_human(
        &transaction,
        "author@example.com",
        "author@example.com",
        "Receiving Author",
    )
    .await
    .expect("create human principal");
    let issued = issue_pat(&transaction, human.id(), " laptop ", TTL)
        .await
        .expect("issue human token from trusted context");
    transaction.commit().await.expect("commit the issuance");
    let issued_stamps = pat_stamps(&client, issued.record().prefix()).await;
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
    let stored: (String, String) = client
        .query_one(
            "SELECT token_hash, encode(sha256(convert_to($1, 'UTF8')), 'hex') \
             FROM identity.pats WHERE token_prefix = $2",
            &[&token, &prefix],
        )
        .await
        .map(|row| (row.get(0), row.get(1)))
        .expect("read stored token row");
    assert_eq!(stored.0, stored.1);
    assert_ne!(stored.0, token);
    let leaked: i64 = client
        .query_one(
            "SELECT count(*) FROM identity.pats \
             WHERE token_hash = $1 OR token_prefix = $1 OR label = $1",
            &[&token],
        )
        .await
        .expect("scan token columns for plaintext")
        .get(0);
    assert_eq!(leaked, 0);

    let authenticated = authenticate_pat(&client, &token)
        .await
        .expect("authenticate token")
        .expect("a valid token must authenticate");
    assert_eq!(authenticated.principal().id(), human.id());

    // A forged secret under a known lookup prefix is refused like any other.
    let forged = flip_last_hex_digit(&token);
    assert_ne!(forged, token);
    assert!(
        authenticate_pat(&client, &forged)
            .await
            .expect("reject forged token")
            .is_none()
    );
    let unknown = format!("{PAT_TOKEN_PREFIX}{}_{}", "f".repeat(16), "f".repeat(64));
    assert!(
        authenticate_pat(&client, &unknown)
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
            authenticate_pat(&client, malformed)
                .await
                .expect("reject malformed token")
                .is_none(),
            "accepted malformed token {malformed:?}"
        );
    }

    // A revocation with no bound actor refuses.
    let unbound = client
        .execute(
            "UPDATE identity.pats SET revoked_at = now() WHERE token_prefix = $1",
            &[&issued.record().prefix()],
        )
        .await
        .expect_err("a revocation with no bound actor must refuse");
    assert_eq!(
        unbound.code(),
        Some(&SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
    );
    assert_eq!(
        unbound
            .as_db_error()
            .map(tokio_postgres::error::DbError::message),
        Some("actor-required")
    );
    assert_eq!(
        revoke_pat(&client, issued.record().prefix())
            .await
            .expect_err("the library revocation also needs a bound actor")
            .kind(),
        IdentityErrorKind::Database
    );

    // Revocation is a one-way stamp and repeating it changes nothing. It
    // binds wamn:provisioning in a later transaction and keeps the created pair.
    let transaction = provisioning_transaction(&mut client).await;
    let revocable = issue_pat(&transaction, human.id(), "revocable", TTL)
        .await
        .expect("issue revocable token");
    transaction.commit().await.expect("commit the issuance");
    let before = pat_stamps(&client, revocable.record().prefix()).await;
    let transaction = provisioning_transaction(&mut client).await;
    let revoked = revoke_pat(&transaction, revocable.record().prefix())
        .await
        .expect("revoke token");
    transaction.commit().await.expect("commit the revocation");
    assert!(revoked.revoked_at().is_some());
    let after = pat_stamps(&client, revocable.record().prefix()).await;
    assert_eq!(after.created_at, before.created_at);
    assert_eq!(after.created_by, provisioning);
    assert_eq!(after.updated_by, provisioning);
    assert!(
        after.updated_after_created,
        "the revocation moves the updated time"
    );

    // The rest of the fixture is platform setup, so it writes as wamn:provisioning.
    client
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&provisioning],
        )
        .await
        .expect("bind wamn:provisioning for the fixture session");
    platform_principal_cannot_hold_a_token(&client, &provisioning).await;

    // An elapsed expiry refuses without any revocation. The stamp trigger keeps
    // created_at, so the fixture moves expires_at to just after it.
    let expired = issue_pat(&client, human.id(), "expiring", TTL)
        .await
        .expect("issue expiring token");
    client
        .execute(
            "UPDATE identity.pats \
             SET expires_at = created_at + interval '1 microsecond' \
             WHERE token_prefix = $1",
            &[&expired.record().prefix()],
        )
        .await
        .expect("expire the expiring token");
    assert!(
        authenticate_pat(&client, expired.token())
            .await
            .expect("reject expired token")
            .is_none()
    );
    assert!(
        authenticate_pat(&client, revocable.token())
            .await
            .expect("reject revoked token")
            .is_none()
    );
    client
        .execute(
            "UPDATE identity.pats SET revoked_at = now() - interval '1 hour' \
             WHERE token_prefix = $1",
            &[&revocable.record().prefix()],
        )
        .await
        .expect("backdate the revocation");
    let backdated = revoke_pat(&client, revocable.record().prefix())
        .await
        .expect("re-revoke token");
    assert_ne!(backdated.revoked_at(), revoked.revoked_at());
    assert_eq!(
        revoke_pat(&client, revocable.record().prefix())
            .await
            .expect("revoke token again")
            .revoked_at(),
        backdated.revoked_at()
    );
    assert_eq!(
        revoke_pat(&client, &"0".repeat(16))
            .await
            .expect_err("unknown prefix must not revoke")
            .kind(),
        IdentityErrorKind::NotFound
    );

    // Service principals get tokens through the same trusted-context path.
    let service = create_service(&client, "agent-ci", "CI Agent")
        .await
        .expect("create service principal");
    let service_token = issue_pat(&client, service.id(), "ci", TTL)
        .await
        .expect("issue service token");
    let service_authenticated = authenticate_pat(&client, service_token.token())
        .await
        .expect("authenticate service token")
        .expect("a valid service token must authenticate");
    assert_eq!(
        service_authenticated.principal().kind(),
        PrincipalKind::Service
    );

    // Listing returns the stored metadata, newest first, and no token material.
    let listed = list_pats(&client, human.id())
        .await
        .expect("list human tokens");
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
    disable_principal(&client, human.id())
        .await
        .expect("disable human");
    assert!(
        authenticate_pat(&client, &token)
            .await
            .expect("reject token of a disabled principal")
            .is_none()
    );
    assert_eq!(
        issue_pat(&client, human.id(), "after-disable", TTL)
            .await
            .expect_err("disabled principals must not gain tokens")
            .kind(),
        IdentityErrorKind::NotFound
    );

    client
        .batch_execute(
            "DROP SCHEMA identity CASCADE; \
             DROP SCHEMA provisioning CASCADE; \
             DROP SCHEMA registry CASCADE;",
        )
        .await
        .expect("remove platform identity test schemas");
    drop(client);
    connection_task
        .await
        .expect("join database connection task");
}

/// Forge a token that keeps its lookup prefix but carries a different secret.
/// Begin a transaction that binds wamn:provisioning, as the identity issuer and
/// wamn-ctl do.
async fn provisioning_transaction(client: &mut Client) -> Transaction<'_> {
    let transaction = client
        .transaction()
        .await
        .expect("begin a provisioning transaction");
    transaction
        .batch_execute(&bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        ))
        .await
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

async fn pat_stamps(client: &Client, prefix: &str) -> PatStamps {
    let row = client
        .query_one(
            "SELECT created_at::text, created_by::text, updated_at::text, updated_by::text, \
                    updated_at > created_at \
             FROM identity.pats WHERE token_prefix = $1",
            &[&prefix],
        )
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

/// The pats foreign key carries the principal kind, so no token names the
/// platform principal, through the library or through direct SQL.
async fn platform_principal_cannot_hold_a_token(client: &Client, provisioning: &str) {
    let principal: PrincipalId = provisioning
        .parse()
        .expect("a derived id is a principal id");
    assert_eq!(
        issue_pat(client, &principal, "platform", TTL)
            .await
            .expect_err("a platform principal must not gain a token")
            .kind(),
        IdentityErrorKind::Database
    );
    for (kind, code) in [
        ("platform", SqlState::CHECK_VIOLATION),
        ("human", SqlState::FOREIGN_KEY_VIOLATION),
        ("service", SqlState::FOREIGN_KEY_VIOLATION),
    ] {
        let error = client
            .execute(
                "INSERT INTO identity.pats \
                   (principal_id, principal_kind, token_prefix, token_hash, label, expires_at) \
                 VALUES ($1::text::uuid, $2, $3, $4, 'platform', now() + interval '1 hour')",
                &[&provisioning, &kind, &"e".repeat(16), &"e".repeat(64)],
            )
            .await
            .expect_err("direct SQL must not bind a token to the platform principal");
        assert_eq!(error.code(), Some(&code), "{kind}");
    }
}

fn flip_last_hex_digit(token: &str) -> String {
    let (head, last) = token.split_at(token.len() - 1);
    let replacement = if last == "a" { 'b' } else { 'a' };
    format!("{head}{replacement}")
}

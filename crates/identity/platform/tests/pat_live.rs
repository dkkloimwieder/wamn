//! Optional real-PostgreSQL proof for the personal-access-token presenter.

use std::time::Duration;

use wamn_platform_identity::{
    IdentityErrorKind, PAT_TOKEN_PREFIX, PrincipalKind, authenticate_pat, create_human,
    create_service, disable_principal, issue_pat, list_pats, login_local, revoke_pat,
};

const SYSTEM_SCHEMA: &str = include_str!("../../../../deploy/sql/system-schema.sql");
const SECRET: &[u8] = b"correct horse battery staple";
const TTL: Duration = Duration::from_secs(3600);

#[tokio::test]
async fn platform_pat_round_trip_on_postgres() {
    let Ok(url) = std::env::var("WAMN_PLATFORM_IDENTITY_PG_URL") else {
        eprintln!(
            "skipping platform_pat_round_trip_on_postgres \
             (set WAMN_PLATFORM_IDENTITY_PG_URL to run)"
        );
        return;
    };

    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
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
        .batch_execute(SYSTEM_SCHEMA)
        .await
        .expect("apply deploy/sql/system-schema.sql");

    let human = create_human(&client, "author@example.com", "Receiving Author", SECRET)
        .await
        .expect("create human principal");

    // The headless login flow mints exactly one token and only on a valid secret.
    assert!(
        login_local(
            &client,
            "author@example.com",
            b"wrong secret",
            "laptop",
            TTL
        )
        .await
        .expect("reject invalid secret")
        .is_none()
    );
    let issued = login_local(&client, "author@example.com", SECRET, " laptop ", TTL)
        .await
        .expect("login human")
        .expect("valid human secret must mint a token");
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

    // An elapsed expiry refuses without any revocation.
    let expired = issue_pat(&client, human.id(), "expiring", TTL)
        .await
        .expect("issue expiring token");
    client
        .execute(
            "UPDATE identity.pats \
             SET created_at = now() - interval '2 hours', \
                 expires_at = now() - interval '1 hour' \
             WHERE token_prefix = $1",
            &[&expired.record().prefix()],
        )
        .await
        .expect("age the expiring token");
    assert!(
        authenticate_pat(&client, expired.token())
            .await
            .expect("reject expired token")
            .is_none()
    );

    // Revocation is a one-way stamp and repeating it changes nothing.
    let revocable = issue_pat(&client, human.id(), "revocable", TTL)
        .await
        .expect("issue revocable token");
    let revoked = revoke_pat(&client, revocable.record().prefix())
        .await
        .expect("revoke token");
    assert!(revoked.revoked_at().is_some());
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
    // `expiring` sorts last because its issuance was aged two hours above.
    let listed = list_pats(&client, human.id())
        .await
        .expect("list human tokens");
    assert_eq!(
        listed.iter().map(|pat| pat.label()).collect::<Vec<_>>(),
        ["revocable", "laptop", "expiring"]
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
fn flip_last_hex_digit(token: &str) -> String {
    let (head, last) = token.split_at(token.len() - 1);
    let replacement = if last == "a" { 'b' } else { 'a' };
    format!("{head}{replacement}")
}

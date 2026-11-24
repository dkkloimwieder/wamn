//! The issuer stamps each recorded admission attempt.

use wamn_authentication::password_requests::PasswordStore as _;
use wamn_identity_protocol::PlatformComponent;
use wamn_postgres::identity::issuer::Issuer;
use wamn_test_infrastructure::containers::start_postgres;

#[tokio::test]
#[ignore = "requires: docker"]
async fn admission_stamps_every_bucket_as_the_platform_principal() {
    let postgres = start_postgres().await.unwrap();
    let database = wamn_test_infrastructure::identity::system(&postgres).await;
    let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();
    let batches: &[&[(&str, i32)]] = &[
        &[("global", 10)],
        &[("source:test", 5), ("account:test", 5)],
    ];
    for &buckets in batches {
        for _ in 0..2 {
            assert!(
                Issuer(&mut client)
                    .admit("https://history.invalid", buckets)
                    .await
                    .unwrap()
            );
        }
    }
    let actor = PlatformComponent::IdentityService.principal_id();
    wamn_test_infrastructure::record_history::assert_stamp(
        &mut client,
        "identity.password_attempts",
        "true",
        &actor,
        &actor,
    )
    .await;
    let stored: std::collections::BTreeSet<_> =
        sqlx::query_scalar::<_, String>("SELECT bucket FROM identity.password_attempts")
            .fetch_all(&mut client)
            .await
            .unwrap()
            .into_iter()
            .collect();
    assert_eq!(
        stored,
        batches
            .iter()
            .flat_map(|buckets| buckets.iter())
            .map(|(name, _)| (*name).to_owned())
            .collect()
    );
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn password_exchange_requires_current_org_membership() {
    use std::io::Write as _;

    use sqlx::Connection as _;
    use wamn_authentication::password::{Password, password_work};
    use wamn_authentication::session_exchange::{self, Exchange, ExchangeFailure};
    use wamn_postgres::identity::{create_user, org, password, session_keys};
    use wamn_test_infrastructure::identity as fixture;

    let postgres = start_postgres().await.unwrap();
    let database = fixture::system(&postgres).await;
    let mut admin = wamn_postgres::connect(database.as_str()).await.unwrap();
    let owner = PlatformComponent::Provisioning.principal_id();
    let org = "session-test".parse().unwrap();
    let email = "alice@example.invalid";
    let passphrase = || Password::new("warehouse session fixture password".to_owned()).unwrap();
    let mut setup = fixture::transaction(&mut admin, &owner, "wamn:provisioning")
        .await
        .unwrap();
    sqlx::query("INSERT INTO registry.orgs (id) VALUES ($1)")
        .bind(wamn_registry::OrgId::as_str(&org))
        .execute(&mut *setup)
        .await
        .unwrap();
    let principal = create_user(&mut setup, email, email, "Alice")
        .await
        .unwrap();
    setup.commit().await.unwrap();

    let work = password_work();
    let invitation = password::issue_invitation(&mut admin, &owner, principal.id())
        .await
        .unwrap();
    password::enroll_password(
        &mut admin,
        &work,
        principal.id(),
        invitation.secret(),
        passphrase(),
    )
    .await
    .unwrap();

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_platform_install::identity_role::grant_identity_issuer_surface_sql(),
    ))
    .execute(&mut admin)
    .await
    .unwrap();
    let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();
    sqlx::raw_sql("SET ROLE wamn_identity_issuer")
        .execute(&mut client)
        .await
        .unwrap();
    let mut key = tempfile::NamedTempFile::new().unwrap();
    key.write_all(&[7; 32]).unwrap();
    let envelope = wamn_key::from_file(key.path()).unwrap();
    let issuer = "https://session.example.invalid";
    let audience = wamn_registry::name::org_audience(&org);
    session_keys::initialize_session_key(&mut client, issuer, &envelope)
        .await
        .unwrap();
    let exchange = || Exchange {
        issuer,
        audience: &audience,
        started_at: session_exchange::unix_seconds().unwrap(),
        envelope: &envelope,
    };

    let result = session_exchange::login(
        &mut Issuer(&mut client),
        exchange(),
        &work,
        email,
        passphrase(),
    )
    .await;
    assert!(matches!(result, Err(ExchangeFailure::Unauthorized)));
    client.ping().await.unwrap();
    let persisted = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM identity.password_logins WHERE principal_id = $1)",
    )
    .bind(uuid::Uuid::from(*principal.id()))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert!(!persisted);

    let mut setup = fixture::transaction(&mut admin, &owner, "wamn:provisioning")
        .await
        .unwrap();
    org::activate_org_membership(&mut setup, principal.id(), &org)
        .await
        .unwrap();
    setup.commit().await.unwrap();

    let session = session_exchange::login(
        &mut Issuer(&mut client),
        exchange(),
        &work,
        email,
        passphrase(),
    )
    .await
    .unwrap();
    assert_eq!(session.token.claims().sub, *principal.id());
    assert_eq!(session.token.claims().aud, audience);
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn concurrent_issuer_connections_share_the_account_limit() {
    let postgres = start_postgres().await.unwrap();
    let database = wamn_test_infrastructure::identity::system(&postgres).await;
    let mut admin = wamn_postgres::connect(database.as_str()).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_platform_install::identity_role::grant_identity_issuer_surface_sql(),
    ))
    .execute(&mut admin)
    .await
    .unwrap();

    let limit = 5;
    let mut jobs = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let url = database.to_string();
        jobs.spawn(async move {
            let mut connection = wamn_postgres::connect(&url).await.unwrap();
            sqlx::raw_sql("SET ROLE wamn_identity_issuer")
                .execute(&mut connection)
                .await
                .unwrap();
            Issuer(&mut connection)
                .admit("https://fixture.invalid", &[("race-account", limit)])
                .await
                .unwrap()
        });
    }
    let mut allowed = 0;
    while let Some(result) = jobs.join_next().await {
        allowed += i32::from(result.unwrap());
    }
    assert_eq!(allowed, limit);
}

//! The issuer stamps each recorded admission attempt.

use wamn_identity::authentication_event::{ClientAddress, Event, EventType, Outcome};
use wamn_identity::identity::issuer::{Admission, Issuer};
use wamn_identity_protocol::PlatformComponent;
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
            assert_eq!(
                Issuer(&mut client)
                    .admit("https://history.invalid", buckets)
                    .await
                    .unwrap(),
                Admission::Allowed,
            );
        }
    }
    let actor = PlatformComponent::IdentityService.principal_id();
    wamn_test_infrastructure::record_history::assert_stamp(
        &mut client,
        "identity.password_attempt",
        "true",
        &actor,
        &actor,
    )
    .await;
    let stored: std::collections::BTreeSet<_> =
        sqlx::query_scalar::<_, String>("SELECT bucket FROM identity.password_attempt")
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
    use wamn_identity::identity::{create_user, org, password, session_keys};
    use wamn_identity::password::{Password, password_work};
    use wamn_identity::session_exchange::{self, Exchange, ExchangeFailure};
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
    sqlx::query("INSERT INTO registry.org (id) VALUES ($1)")
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

    let mut event = Event::new(
        EventType::PasswordSignIn,
        ClientAddress::Socket("192.0.2.10".parse().unwrap()),
    );
    event.attempted_email(email);
    event.org = Some(org.clone());

    let result = session_exchange::login(
        &mut Issuer(&mut client),
        exchange(),
        &work,
        email,
        passphrase(),
        Some(&mut event),
    )
    .await;
    assert!(matches!(result, Err(ExchangeFailure::Unauthorized)));
    assert_eq!(event.outcome, Some(Outcome::MembershipMissing));
    client.ping().await.unwrap();
    let persisted = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM identity.password_login WHERE principal_id = $1)",
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

    sqlx::raw_sql("ALTER POLICY issuer_append ON identity.authentication_event WITH CHECK (false)")
        .execute(&mut admin)
        .await
        .unwrap();
    let result = session_exchange::login(
        &mut Issuer(&mut client),
        exchange(),
        &work,
        email,
        passphrase(),
        Some(&mut event),
    )
    .await;
    assert!(matches!(result, Err(ExchangeFailure::Unavailable)));
    client.ping().await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT NOT EXISTS (SELECT FROM identity.password_login)")
            .fetch_one(&mut admin)
            .await
            .unwrap()
    );
    sqlx::raw_sql("ALTER POLICY issuer_append ON identity.authentication_event WITH CHECK (true)")
        .execute(&mut admin)
        .await
        .unwrap();
    let session = session_exchange::login(
        &mut Issuer(&mut client),
        exchange(),
        &work,
        email,
        passphrase(),
        Some(&mut event),
    )
    .await
    .unwrap();
    assert_eq!(session.token.claims().sub, *principal.id());
    assert_eq!(session.token.claims().aud, audience);
    let (outcome, principal_id, login_id, jti): (String, uuid::Uuid, String, String) =
        sqlx::query_as(
            "SELECT outcome, principal_id, login_id::text, session_jti
             FROM identity.authentication_event WHERE event_type = 'password_sign_in'",
        )
        .fetch_one(&mut admin)
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Succeeded.code());
    assert_eq!(principal_id, uuid::Uuid::from(*principal.id()));
    assert_eq!(login_id, session.renewal.login.id);
    assert_eq!(jti, session.token.claims().jti);
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
    let mut first_denial = 0;
    while let Some(result) = jobs.join_next().await {
        match result.unwrap() {
            Admission::Allowed => allowed += 1,
            Admission::Denied(Some(Outcome::AccountRateLimited)) => first_denial += 1,
            Admission::Denied(None) => (),
            Admission::Denied(Some(other)) => panic!("unexpected admission decision: {other:?}"),
        }
    }
    assert_eq!(allowed, limit);
    assert_eq!(first_denial, 1);
    let mut tx = wamn_test_infrastructure::identity::transaction(
        &mut admin,
        &PlatformComponent::Provisioning.principal_id(),
        "wamn:provisioning",
    )
    .await
    .unwrap();
    sqlx::raw_sql("UPDATE identity.password_attempt SET started_at = clock_timestamp() - interval '60 seconds' WHERE bucket = 'race-account'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    for _ in 0..limit {
        assert_eq!(
            Issuer(&mut admin)
                .admit("https://fixture.invalid", &[("race-account", limit)])
                .await
                .unwrap(),
            Admission::Allowed
        );
    }
    assert_eq!(
        Issuer(&mut admin)
            .admit("https://fixture.invalid", &[("race-account", limit)])
            .await
            .unwrap(),
        Admission::Denied(Some(Outcome::AccountRateLimited)),
    );
}

//! Optional real-PostgreSQL proof for the platform identity core.

use tokio_postgres::error::SqlState;
use wamn_platform_identity::{
    IdentityErrorKind, PrincipalKind, PrincipalStatus, assign_project_role, authenticate_local,
    create_human, create_service, disable_principal, project_roles, resolve_principal,
    resolve_subject,
};

const SYSTEM_SCHEMA: &str = include_str!("../../../../deploy/sql/system-schema.sql");

#[tokio::test]
async fn platform_identity_round_trip_on_postgres() {
    let Ok(url) = std::env::var("WAMN_PLATFORM_IDENTITY_PG_URL") else {
        eprintln!(
            "skipping platform_identity_round_trip_on_postgres \
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
    client
        .batch_execute(
            "INSERT INTO registry.orgs (id, placement_kind, pool_cluster) \
             VALUES ('acme', 'pooled', 'wamn-pg'); \
             INSERT INTO registry.projects (org, id) VALUES ('acme', 'receiving');",
        )
        .await
        .expect("seed registered project");

    let human = create_human(
        &client,
        "Author@Example.com",
        "Receiving Author",
        b"correct horse battery staple",
    )
    .await
    .expect("create human principal");
    assert_eq!(human.kind(), PrincipalKind::Human);
    assert_eq!(human.subject(), "author@example.com");
    assert_eq!(human.status(), PrincipalStatus::Active);

    let duplicate = create_human(
        &client,
        "author@example.com",
        "Duplicate",
        b"different secret",
    )
    .await
    .expect_err("duplicate human identity must fail");
    assert_eq!(duplicate.kind(), IdentityErrorKind::Conflict);

    let service = create_service(&client, "agent-ci", "CI Agent")
        .await
        .expect("create service principal");
    assert_eq!(service.kind(), PrincipalKind::Service);
    assert!(
        resolve_subject(&client, PrincipalKind::Service, "agent-ci")
            .await
            .expect("resolve service")
            .is_some()
    );

    let authenticated = authenticate_local(
        &client,
        "author@example.com",
        b"correct horse battery staple",
    )
    .await
    .expect("authenticate human")
    .expect("valid human secret must authenticate");
    assert_eq!(authenticated.principal().id(), human.id());
    assert!(
        authenticate_local(&client, "author@example.com", b"wrong secret")
            .await
            .expect("reject invalid secret")
            .is_none()
    );
    assert!(
        authenticate_local(&client, "missing@example.com", b"wrong secret")
            .await
            .expect("reject unknown human")
            .is_none()
    );

    assign_project_role(&client, human.id(), "acme", "receiving", "project-author")
        .await
        .expect("assign author role");
    assign_project_role(&client, human.id(), "acme", "receiving", "project-promoter")
        .await
        .expect("assign promoter role");
    let roles = project_roles(&client, human.id(), "acme", "receiving")
        .await
        .expect("read project roles");
    assert_eq!(
        roles.iter().map(|role| role.as_str()).collect::<Vec<_>>(),
        ["project-author", "project-promoter"]
    );

    let hash: String = client
        .query_one(
            "SELECT password_hash FROM identity.local_credentials \
             WHERE principal_id=$1::text::uuid",
            &[&human.id().as_str()],
        )
        .await
        .expect("read stored credential hash")
        .get(0);
    assert!(hash.starts_with("$argon2id$v=19$"));
    assert!(!hash.contains("correct horse battery staple"));
    let service_local_secret = client
        .execute(
            "INSERT INTO identity.local_credentials (principal_id, password_hash) \
             VALUES ($1::text::uuid, $2)",
            &[&service.id().as_str(), &hash],
        )
        .await
        .expect_err("service principals must not admit local credentials");
    assert_eq!(
        service_local_secret.code(),
        Some(&SqlState::FOREIGN_KEY_VIOLATION)
    );
    let service_credentials: i64 = client
        .query_one(
            "SELECT count(*) FROM identity.local_credentials \
             WHERE principal_id=$1::text::uuid",
            &[&service.id().as_str()],
        )
        .await
        .expect("count service local credentials")
        .get(0);
    assert_eq!(service_credentials, 0);

    let disabled = disable_principal(&client, human.id())
        .await
        .expect("disable human");
    assert_eq!(disabled.status(), PrincipalStatus::Disabled);
    assert!(
        authenticate_local(
            &client,
            "author@example.com",
            b"correct horse battery staple",
        )
        .await
        .expect("reject disabled human")
        .is_none()
    );
    assert_eq!(
        resolve_principal(&client, human.id())
            .await
            .expect("resolve disabled human")
            .expect("disabled human remains stored")
            .status(),
        PrincipalStatus::Disabled
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

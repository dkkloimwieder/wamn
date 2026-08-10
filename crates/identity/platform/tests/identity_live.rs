//! Optional real-PostgreSQL proof for the platform identity core.

use wamn_platform_identity::{
    IdentityErrorKind, PrincipalKind, PrincipalStatus, assign_project_role, create_human,
    create_service, disable_principal, project_roles, resolve_principal, resolve_subject,
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

    let human = create_human(&client, "Author@Example.com", "Receiving Author")
        .await
        .expect("create human principal");
    assert_eq!(human.kind(), PrincipalKind::Human);
    assert_eq!(human.subject(), "author@example.com");
    assert_eq!(human.status(), PrincipalStatus::Active);

    let duplicate = create_human(&client, "author@example.com", "Duplicate")
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

    let disabled = disable_principal(&client, human.id())
        .await
        .expect("disable human");
    assert_eq!(disabled.status(), PrincipalStatus::Disabled);
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

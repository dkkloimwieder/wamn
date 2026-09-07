//! Optional real-PostgreSQL proof for the platform identity core.

use wamn_platform_identity::{
    IdentityErrorKind, PreparedIdentityReads, Principal, PrincipalKind, PrincipalStatus,
    assign_project_role, create_human, create_service, disable_principal,
    grant_project_env_membership, has_project_env_membership, project_roles, resolve_principal,
    resolve_subject, revoke_project_env_membership,
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

    project_environment_membership_round_trip(&client, &human, &service).await;

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

async fn project_environment_membership_round_trip(
    client: &tokio_postgres::Client,
    human: &Principal,
    service: &Principal,
) {
    client
        .batch_execute(
            "INSERT INTO registry.orgs (id, placement_kind, pool_cluster) \
             VALUES ('other', 'pooled', 'wamn-pg'); \
             INSERT INTO registry.projects (org, id) \
             VALUES ('acme', 'inventory'), ('other', 'receiving'); \
             INSERT INTO registry.env_policies \
               (org, name, recovery_domain, promotion_rank, instances, \
                storage, cpu, memory, image) \
             SELECT id, env, '\"own\"'::jsonb, 1, 1, '1Gi', '1', '1Gi', 'postgres:17' \
             FROM registry.orgs CROSS JOIN (VALUES ('dev'), ('prod')) AS envs(env); \
             INSERT INTO registry.project_envs \
               (org, project, env, secret_name, instance_suffix) \
             SELECT p.org, p.id, e.name, 'membership-test-secret', 'a1b2c3d4' \
             FROM registry.projects p JOIN registry.env_policies e ON p.org = e.org;",
        )
        .await
        .expect("seed distinct registered project-environments");
    let reads = PreparedIdentityReads::prepare(client)
        .await
        .expect("prepare identity queries");
    let other_human = create_human(client, "other@example.com", "Other Human")
        .await
        .expect("create another human");
    assert!(
        !has_project_env_membership(client, human.id(), "acme", "receiving", "dev")
            .await
            .expect("read membership before grant"),
        "project management roles cannot imply environment membership"
    );

    for _ in 0..2 {
        grant_project_env_membership(client, human.id(), "acme", "receiving", "dev")
            .await
            .expect("grant human membership idempotently");
    }
    let stored = client
        .query_one(
            "SELECT principal_id::text, count(*) OVER () \
             FROM identity.project_env_memberships",
            &[],
        )
        .await
        .expect("read the stored grant");
    assert_eq!(stored.get::<_, String>(0), human.id().as_str());
    assert_eq!(stored.get::<_, i64>(1), 1);
    assert!(
        has_project_env_membership(client, human.id(), "acme", "receiving", "dev")
            .await
            .expect("unprepared query observes new grant")
    );
    assert!(
        reads
            .has_project_env_membership(client, human.id(), "acme", "receiving", "dev")
            .await
            .expect("prepared query observes new grant")
    );
    let other_memberships = [
        (&other_human, "acme", "receiving", "dev"),
        (human, "other", "receiving", "dev"),
        (human, "acme", "inventory", "dev"),
        (human, "acme", "receiving", "prod"),
    ];
    for (principal, org, project, env) in other_memberships {
        assert!(
            !has_project_env_membership(client, principal.id(), org, project, env)
                .await
                .expect("read another principal or environment")
        );
        assert!(
            !reads
                .has_project_env_membership(client, principal.id(), org, project, env)
                .await
                .expect("prepared query isolates the exact grant")
        );
    }
    assert_eq!(
        grant_project_env_membership(client, service.id(), "acme", "receiving", "dev")
            .await
            .expect_err("service cannot hold a human membership")
            .kind(),
        IdentityErrorKind::NotFound
    );
    let service_insert = client
        .execute(
            "INSERT INTO identity.project_env_memberships \
             (principal_id, principal_kind, org, project, env) \
             VALUES ($1::text::uuid, 'service', 'acme', 'receiving', 'dev')",
            &[&service.id().as_str()],
        )
        .await
        .expect_err("direct SQL cannot bypass the human-only constraint");
    assert_eq!(
        service_insert.code(),
        Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
    );
    assert_eq!(
        grant_project_env_membership(client, human.id(), "acme", "receiving", "missing")
            .await
            .expect_err("unregistered environment cannot receive a membership")
            .kind(),
        IdentityErrorKind::NotFound
    );

    for (principal, org, project, env) in other_memberships {
        grant_project_env_membership(client, principal.id(), org, project, env)
            .await
            .expect("grant independent memberships before revocation");
    }
    assert!(
        revoke_project_env_membership(client, human.id(), "acme", "receiving", "dev")
            .await
            .expect("revoke membership")
    );
    assert!(
        !reads
            .has_project_env_membership(client, human.id(), "acme", "receiving", "dev")
            .await
            .expect("prepared query observes revocation immediately")
    );
    assert!(
        !revoke_project_env_membership(client, human.id(), "acme", "receiving", "dev")
            .await
            .expect("repeat revocation harmlessly")
    );
    for (principal, org, project, env) in other_memberships {
        assert!(
            reads
                .has_project_env_membership(client, principal.id(), org, project, env)
                .await
                .expect("revocation preserves every other principal and environment")
        );
    }

    grant_project_env_membership(client, human.id(), "acme", "receiving", "dev")
        .await
        .expect("grant before environment replacement");
    client
        .batch_execute(
            "DELETE FROM registry.project_envs \
             WHERE org = 'acme' AND project = 'receiving' AND env = 'dev'; \
             INSERT INTO registry.project_envs \
               (org, project, env, secret_name, instance_suffix) \
             VALUES ('acme', 'receiving', 'dev', 'replacement-secret', 'e5f6g7h8');",
        )
        .await
        .expect("replace the provisioned environment");
    assert!(
        !reads
            .has_project_env_membership(client, human.id(), "acme", "receiving", "dev")
            .await
            .expect("replacement environment must not inherit the deleted grant")
    );

    grant_project_env_membership(client, other_human.id(), "acme", "receiving", "dev")
        .await
        .expect("grant another human before deletion");
    client
        .execute(
            "DELETE FROM identity.principals WHERE id = $1::text::uuid",
            &[&other_human.id().as_str()],
        )
        .await
        .expect("delete a principal with no PAT audit records");
    assert!(
        !has_project_env_membership(client, other_human.id(), "acme", "receiving", "dev")
            .await
            .expect("principal deletion removes its membership")
    );
}

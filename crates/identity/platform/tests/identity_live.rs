//! Optional real-PostgreSQL test for the platform identity core.

use std::time::Duration;

use tokio_postgres::error::SqlState;
use wamn_control_provision::{PlatformComponent, SYSTEM_SCHEMA_SQL};
use wamn_platform_identity::{
    IdentityErrorKind, PreparedIdentityReads, Principal, PrincipalId, PrincipalKind,
    PrincipalStatus, assign_project_role, create_human, create_service, disable_principal,
    grant_project_env_membership, has_project_env_membership, issue_pat, project_roles,
    resolve_principal, resolve_subject, revoke_project_env_membership,
};

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
        .batch_execute(SYSTEM_SCHEMA_SQL)
        .await
        .expect("apply the system schema composition");
    client
        .batch_execute(
            "INSERT INTO registry.orgs (id, placement_kind, pool_cluster) \
             VALUES ('acme', 'pooled', 'wamn-pg'); \
             INSERT INTO registry.projects (org, id) VALUES ('acme', 'receiving');",
        )
        .await
        .expect("seed registered project");
    let provisioning = provisioning_principal_is_seeded(&client).await;
    unbound_identity_writes_refuse(&client).await;
    // The fixture is platform setup, so it writes as wamn:provisioning.
    client
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&provisioning.as_str()],
        )
        .await
        .expect("bind wamn:provisioning for the fixture session");
    platform_principal_check_refuses_other_rows(&client).await;

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
    let stamps = client
        .query_one(
            "SELECT count(*) FROM identity.principals p \
             JOIN identity.project_roles r ON r.principal_id = p.id \
             WHERE p.id = $1::text::uuid \
               AND p.created_by = $2::text::uuid AND p.updated_by = $2::text::uuid \
               AND r.created_by = $2::text::uuid AND r.updated_by = $2::text::uuid",
            &[&human.id().as_str(), &provisioning.as_str()],
        )
        .await
        .expect("read principal and role stamps");
    assert_eq!(
        stamps.get::<_, i64>(0),
        2,
        "the principal and both roles stamp wamn:provisioning"
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
        .expect("prepare the identity query");
    let human_token = issue_pat(
        client,
        human.id(),
        "route member",
        Duration::from_secs(3600),
    )
    .await
    .expect("issue the human route PAT");
    let service_token = issue_pat(
        client,
        service.id(),
        "route service",
        Duration::from_secs(3600),
    )
    .await
    .expect("issue the service route PAT");
    for token in [human_token.token(), service_token.token()] {
        assert!(
            route_principal_id(&reads, client, token, "acme", "receiving", "dev")
                .await
                .is_none(),
            "a valid PAT without its kind's authority passed"
        );
    }
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
    let stamped: bool = client
        .query_one(
            "SELECT created_by = updated_by AND created_by = current_setting('app.user_id')::uuid \
             FROM identity.project_env_memberships",
            &[],
        )
        .await
        .expect("read the grant stamps")
        .get(0);
    assert!(
        stamped,
        "the grant stamps the bound wamn:provisioning actor"
    );
    assert!(
        has_project_env_membership(client, human.id(), "acme", "receiving", "dev")
            .await
            .expect("unprepared query observes new grant")
    );
    assert_eq!(
        route_principal_id(
            &reads,
            client,
            human_token.token(),
            "acme",
            "receiving",
            "dev"
        )
        .await,
        Some(human.id().as_str().to_owned()),
        "human membership must not require a project management role"
    );
    assign_project_role(client, service.id(), "acme", "receiving", "route-caller")
        .await
        .expect("assign the service route role");
    assert_eq!(
        route_principal_id(
            &reads,
            client,
            service_token.token(),
            "acme",
            "receiving",
            "dev"
        )
        .await,
        Some(service.id().as_str().to_owned()),
        "a service needs its role, not a human membership"
    );
    for (org, project, env) in [
        ("other", "receiving", "dev"),
        ("acme", "inventory", "dev"),
        ("acme", "receiving", "prod"),
    ] {
        assert!(
            route_principal_id(&reads, client, human_token.token(), org, project, env)
                .await
                .is_none(),
            "the prepared PAT query accepted another environment"
        );
    }
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
        route_principal_id(
            &reads,
            client,
            human_token.token(),
            "acme",
            "receiving",
            "dev"
        )
        .await
        .is_none(),
        "the prepared PAT query must observe membership revocation"
    );
    assert!(
        !revoke_project_env_membership(client, human.id(), "acme", "receiving", "dev")
            .await
            .expect("repeat revocation harmlessly")
    );
    for (principal, org, project, env) in other_memberships {
        assert!(
            has_project_env_membership(client, principal.id(), org, project, env)
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
        route_principal_id(
            &reads,
            client,
            human_token.token(),
            "acme",
            "receiving",
            "dev"
        )
        .await
        .is_none(),
        "replacement environment must not inherit the deleted grant"
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

/// The system schema seeds the wamn:provisioning row with the derived id, and
/// the row stamps itself.
async fn provisioning_principal_is_seeded(client: &tokio_postgres::Client) -> PrincipalId {
    let component = PlatformComponent::Provisioning;
    let id: PrincipalId = component
        .principal_id()
        .to_string()
        .parse()
        .expect("a derived id is a principal id");
    let row = client
        .query_one(
            "SELECT subject, display_name, created_by::text, updated_by::text, \
                    (SELECT count(*) FROM identity.principals) \
             FROM identity.principals WHERE id = $1::text::uuid",
            &[&id.as_str()],
        )
        .await
        .expect("read the seeded wamn:provisioning row");
    assert_eq!(row.get::<_, String>(0), component.principal_name());
    assert_eq!(row.get::<_, String>(1), component.principal_name());
    assert_eq!(row.get::<_, String>(2), id.as_str());
    assert_eq!(row.get::<_, String>(3), id.as_str());
    assert_eq!(
        row.get::<_, i64>(4),
        1,
        "only the wamn:provisioning row is seeded"
    );
    let principal = resolve_principal(client, &id)
        .await
        .expect("resolve the platform principal")
        .expect("the platform principal is stored");
    assert_eq!(principal.kind(), PrincipalKind::Platform);
    id
}

/// A write to an identity authority relation with no bound actor raises
/// SQLSTATE 55000 with the message actor-required. The stamp trigger runs
/// before every constraint, so the rows need no valid references.
async fn unbound_identity_writes_refuse(client: &tokio_postgres::Client) {
    const PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";
    for statement in [
        "INSERT INTO identity.principals (kind, subject, display_name) \
         VALUES ('human', 'unbound', 'Unbound')"
            .to_owned(),
        format!(
            "INSERT INTO identity.project_roles (principal_id, org, project, role) \
             VALUES ('{PRINCIPAL}', 'acme', 'receiving', 'unbound')"
        ),
        format!(
            "INSERT INTO identity.project_env_memberships (principal_id, org, project, env) \
             VALUES ('{PRINCIPAL}', 'acme', 'receiving', 'dev')"
        ),
        format!(
            "INSERT INTO identity.pats \
               (principal_id, principal_kind, token_prefix, token_hash, label, expires_at) \
             VALUES ('{PRINCIPAL}', 'human', '{}', '{}', 'unbound', now() + interval '1 hour')",
            "0".repeat(16),
            "0".repeat(64),
        ),
        "UPDATE identity.principals SET display_name = 'Unbound'".to_owned(),
    ] {
        let error = client
            .execute(statement.as_str(), &[])
            .await
            .expect_err("a write with no bound actor must refuse");
        assert_eq!(
            error.code(),
            Some(&SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE),
            "{statement}"
        );
        assert_eq!(
            error
                .as_db_error()
                .map(tokio_postgres::error::DbError::message),
            Some("actor-required"),
            "{statement}"
        );
    }
}

/// `principals_platform_principal_check` admits only the pinned
/// wamn:provisioning row for kind platform, and the other kinds refuse a
/// `wamn:` name.
async fn platform_principal_check_refuses_other_rows(client: &tokio_postgres::Client) {
    const OTHER: &str = "00000000-0000-4000-8000-0000000000f2";
    let provisioning = PlatformComponent::Provisioning;
    let executor = PlatformComponent::Executor;
    for (kind, id, subject, display_name) in [
        (
            "platform",
            OTHER.to_owned(),
            provisioning.principal_name(),
            provisioning.principal_name(),
        ),
        (
            "platform",
            executor.principal_id().to_string(),
            provisioning.principal_name(),
            provisioning.principal_name(),
        ),
        (
            "platform",
            executor.principal_id().to_string(),
            executor.principal_name(),
            executor.principal_name(),
        ),
        (
            "platform",
            provisioning.principal_id().to_string(),
            provisioning.principal_name(),
            "Provisioning",
        ),
        ("human", OTHER.to_owned(), "person", "wamn:person"),
        ("service", OTHER.to_owned(), "station", "wamn:station"),
        (
            "human",
            OTHER.to_owned(),
            provisioning.principal_name(),
            "Person",
        ),
    ] {
        let error = client
            .execute(
                "INSERT INTO identity.principals (id, kind, subject, display_name) \
                 VALUES ($1::text::uuid, $2, $3, $4)",
                &[&id, &kind, &subject, &display_name],
            )
            .await
            .expect_err("the principal CHECKs must refuse this row");
        assert_eq!(
            error.code(),
            Some(&SqlState::CHECK_VIOLATION),
            "{kind} {id} {subject} {display_name}"
        );
    }
}

async fn route_principal_id(
    reads: &PreparedIdentityReads,
    client: &tokio_postgres::Client,
    token: &str,
    org: &str,
    project: &str,
    env: &str,
) -> Option<String> {
    reads
        .authenticate_route_pat(client, token, org, project, env, "route-caller")
        .await
        .expect("read route PAT and scope in one statement")
        .map(|authenticated| authenticated.principal().id().as_str().to_owned())
}

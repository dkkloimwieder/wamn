//! Real-PostgreSQL test for the platform identity core.

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
    // The system schema creates the cluster-wide wamn_system and wamn_db_owner roles.
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();

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
             VALUES ('demo', 'pooled', 'wamn-pg'); \
             INSERT INTO registry.projects (org, id) VALUES ('demo', 'widgets');",
        )
        .await
        .expect("seed registered project");
    identity_relations_carry_stamps(&client).await;
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

    let human = create_human(
        &client,
        "Author@Example.com",
        "author@example.com",
        "Widget Author",
    )
    .await
    .expect("create human principal");
    assert_eq!(human.kind(), PrincipalKind::Human);
    assert_eq!(human.subject(), "author@example.com");
    assert_eq!(human.status(), PrincipalStatus::Active);

    let duplicate = create_human(
        &client,
        "author@example.com",
        "author@example.com",
        "Duplicate",
    )
    .await
    .expect_err("duplicate human identity must fail");
    assert_eq!(duplicate.kind(), IdentityErrorKind::Conflict);

    one_address_admits_one_human(&client).await;

    let service = create_service(&client, "agent-ci", "CI Agent")
        .await
        .expect("create service principal");
    assert_eq!(service.kind(), PrincipalKind::Service);
    let duplicate_service = create_service(&client, "agent-ci", "Second CI Agent")
        .await
        .expect_err("one subject admits one service");
    assert_eq!(duplicate_service.kind(), IdentityErrorKind::Conflict);
    assert!(
        resolve_subject(&client, PrincipalKind::Service, "agent-ci")
            .await
            .expect("resolve service")
            .is_some()
    );

    assign_project_role(&client, human.id(), "demo", "widgets", "project-author")
        .await
        .expect("assign author role");
    assign_project_role(&client, human.id(), "demo", "widgets", "project-promoter")
        .await
        .expect("assign promoter role");
    let roles = project_roles(&client, human.id(), "demo", "widgets")
        .await
        .expect("read project roles");
    assert_eq!(
        roles
            .iter()
            .map(wamn_platform_identity::ProjectRole::as_str)
            .collect::<Vec<_>>(),
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
    project_delete_removes_its_roles_and_memberships(&client, &human).await;

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

/// Two spellings of one address are one address (`wamn-0h0g.9.18`).
/// `checked_email` folds the case in full, and `UNIQUE (email)` then
/// refuses the second human. The two humans carry different subjects, so only
/// the email constraint can refuse the second one.
async fn one_address_admits_one_human(client: &tokio_postgres::Client) {
    let first = create_human(
        client,
        "case-fold-first",
        "Case.Fold@Example.Invalid",
        "Case Fold First",
    )
    .await
    .expect("create the first human of the address");
    let stored: String = client
        .query_one(
            "SELECT email FROM identity.principals WHERE id = $1::text::uuid",
            &[&first.id().as_str()],
        )
        .await
        .expect("read the stored email")
        .get(0);
    assert_eq!(
        stored, "case.fold@example.invalid",
        "the stored address is folded in full"
    );
    let second = create_human(
        client,
        "case-fold-second",
        "case.fold@example.invalid",
        "Case Fold Second",
    )
    .await
    .expect_err("one address admits one human");
    assert_eq!(second.kind(), IdentityErrorKind::Conflict);
}

/// A project delete takes the project's roles and environment memberships with
/// it, and leaves every other project's rows in place.
async fn project_delete_removes_its_roles_and_memberships(
    client: &tokio_postgres::Client,
    human: &Principal,
) {
    assign_project_role(client, human.id(), "demo", "inventory", "project-author")
        .await
        .expect("assign a role on the project to delete");
    grant_project_env_membership(client, human.id(), "demo", "inventory", "dev")
        .await
        .expect("grant a membership on the project to delete");
    let rows = |project: &'static str| async move {
        client
            .query_one(
                "SELECT (SELECT count(*) FROM identity.project_roles \
                         WHERE org = 'demo' AND project = $1), \
                        (SELECT count(*) FROM identity.project_env_memberships \
                         WHERE org = 'demo' AND project = $1)",
                &[&project],
            )
            .await
            .map(|row| (row.get::<_, i64>(0), row.get::<_, i64>(1)))
            .expect("count project roles and memberships")
    };
    let widgets = rows("widgets").await;
    assert_eq!(rows("inventory").await, (1, 1));
    client
        .execute(
            "DELETE FROM registry.projects WHERE org = 'demo' AND id = 'inventory'",
            &[],
        )
        .await
        .expect("delete the project");
    assert_eq!(rows("inventory").await, (0, 0));
    assert_eq!(rows("widgets").await, widgets);
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
             VALUES ('demo', 'inventory'), ('other', 'widgets'); \
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
            route_principal_id(&reads, client, token, "demo", "widgets", "dev")
                .await
                .is_none(),
            "a valid PAT without its kind's authority passed"
        );
    }
    let other_human = create_human(
        client,
        "other@example.com",
        "other@example.com",
        "Other Human",
    )
    .await
    .expect("create another human");
    assert!(
        !has_project_env_membership(client, human.id(), "demo", "widgets", "dev")
            .await
            .expect("read membership before grant"),
        "project management roles cannot imply environment membership"
    );

    for _ in 0..2 {
        grant_project_env_membership(client, human.id(), "demo", "widgets", "dev")
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
        has_project_env_membership(client, human.id(), "demo", "widgets", "dev")
            .await
            .expect("unprepared query observes new grant")
    );
    assert_eq!(
        route_principal_id(
            &reads,
            client,
            human_token.token(),
            "demo",
            "widgets",
            "dev"
        )
        .await,
        Some(human.id().as_str().to_owned()),
        "human membership must not require a project management role"
    );
    assign_project_role(client, service.id(), "demo", "widgets", "route-caller")
        .await
        .expect("assign the service route role");
    assert_eq!(
        route_principal_id(
            &reads,
            client,
            service_token.token(),
            "demo",
            "widgets",
            "dev"
        )
        .await,
        Some(service.id().as_str().to_owned()),
        "a service needs its role, not a human membership"
    );
    for (org, project, env) in [
        ("other", "widgets", "dev"),
        ("demo", "inventory", "dev"),
        ("demo", "widgets", "prod"),
    ] {
        assert!(
            route_principal_id(&reads, client, human_token.token(), org, project, env)
                .await
                .is_none(),
            "the prepared PAT query accepted another environment"
        );
    }
    let other_memberships = [
        (&other_human, "demo", "widgets", "dev"),
        (human, "other", "widgets", "dev"),
        (human, "demo", "inventory", "dev"),
        (human, "demo", "widgets", "prod"),
    ];
    for (principal, org, project, env) in other_memberships {
        assert!(
            !has_project_env_membership(client, principal.id(), org, project, env)
                .await
                .expect("read another principal or environment")
        );
    }
    assert_eq!(
        grant_project_env_membership(client, service.id(), "demo", "widgets", "dev")
            .await
            .expect_err("service cannot hold a human membership")
            .kind(),
        IdentityErrorKind::NotFound
    );
    let service_insert = client
        .execute(
            "INSERT INTO identity.project_env_memberships \
             (principal_id, principal_kind, org, project, env) \
             VALUES ($1::text::uuid, 'service', 'demo', 'widgets', 'dev')",
            &[&service.id().as_str()],
        )
        .await
        .expect_err("direct SQL cannot bypass the human-only constraint");
    assert_eq!(
        service_insert.code(),
        Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
    );
    assert_eq!(
        grant_project_env_membership(client, human.id(), "demo", "widgets", "missing")
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
        revoke_project_env_membership(client, human.id(), "demo", "widgets", "dev")
            .await
            .expect("revoke membership")
    );
    assert!(
        route_principal_id(
            &reads,
            client,
            human_token.token(),
            "demo",
            "widgets",
            "dev"
        )
        .await
        .is_none(),
        "the prepared PAT query must observe membership revocation"
    );
    assert!(
        !revoke_project_env_membership(client, human.id(), "demo", "widgets", "dev")
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

    grant_project_env_membership(client, human.id(), "demo", "widgets", "dev")
        .await
        .expect("grant before environment replacement");
    client
        .batch_execute(
            "DELETE FROM registry.project_envs \
             WHERE org = 'demo' AND project = 'widgets' AND env = 'dev'; \
             INSERT INTO registry.project_envs \
               (org, project, env, secret_name, instance_suffix) \
             VALUES ('demo', 'widgets', 'dev', 'replacement-secret', 'e5f6g7h8');",
        )
        .await
        .expect("replace the provisioned environment");
    assert!(
        route_principal_id(
            &reads,
            client,
            human_token.token(),
            "demo",
            "widgets",
            "dev"
        )
        .await
        .is_none(),
        "replacement environment must not inherit the deleted grant"
    );

    grant_project_env_membership(client, other_human.id(), "demo", "widgets", "dev")
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
        !has_project_env_membership(client, other_human.id(), "demo", "widgets", "dev")
            .await
            .expect("principal deletion removes its membership")
    );
}

/// Each identity authority relation carries the four stamp columns as NOT NULL
/// with no default, and one static stamp trigger.
async fn identity_relations_carry_stamps(client: &tokio_postgres::Client) {
    const RELATIONS: [&str; 8] = [
        "password_credentials",
        "password_logins",
        "password_tokens",
        "pats",
        "principals",
        "project_env_memberships",
        "project_roles",
        "renewal_credentials",
    ];
    let columns = client
        .query(
            "SELECT table_name::text, column_name::text, data_type::text, is_nullable::text, \
                    column_default IS NULL \
             FROM information_schema.columns \
             WHERE table_schema = 'identity' \
               AND column_name IN ('created_at', 'created_by', 'updated_at', 'updated_by') \
             ORDER BY table_name, column_name",
            &[],
        )
        .await
        .expect("read the identity stamp columns")
        .iter()
        .map(|row| {
            format!(
                "{}.{} {} nullable={} no_default={}",
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, String>(3),
                row.get::<_, bool>(4),
            )
        })
        .collect::<Vec<_>>();
    let expected_columns = RELATIONS
        .iter()
        .flat_map(|relation| {
            [
                ("created_at", "timestamp with time zone"),
                ("created_by", "uuid"),
                ("updated_at", "timestamp with time zone"),
                ("updated_by", "uuid"),
            ]
            .map(|(column, data_type)| {
                format!("{relation}.{column} {data_type} nullable=NO no_default=true")
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(columns, expected_columns);

    let triggers = client
        .query(
            "SELECT pg_catalog.pg_get_triggerdef(t.oid) \
             FROM pg_catalog.pg_trigger t \
             JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'identity' AND NOT t.tgisinternal \
               AND t.tgname = 'wamn_record_history_stamp' \
             ORDER BY c.relname, t.tgname",
            &[],
        )
        .await
        .expect("read the identity triggers")
        .iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    let expected_triggers = RELATIONS
        .map(|relation| {
            format!(
                "CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON identity.{relation} \
                 FOR EACH ROW EXECUTE FUNCTION \
                 wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by')"
            )
        })
        .to_vec();
    assert_eq!(triggers, expected_triggers);
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
             VALUES ('{PRINCIPAL}', 'demo', 'widgets', 'unbound')"
        ),
        format!(
            "INSERT INTO identity.project_env_memberships (principal_id, org, project, env) \
             VALUES ('{PRINCIPAL}', 'demo', 'widgets', 'dev')"
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
///
/// The last two rows pin `principals_email_check` instead (`wamn-0h0g.9.18`):
/// a human needs an email, and no other kind carries one. Every other row here
/// gets a valid email, so it still fails on the platform check alone.
async fn platform_principal_check_refuses_other_rows(client: &tokio_postgres::Client) {
    const OTHER: &str = "00000000-0000-4000-8000-0000000000f2";
    const ADDRESS: Option<&str> = Some("refused@example.invalid");
    let provisioning = PlatformComponent::Provisioning;
    let executor = PlatformComponent::Executor;
    for (kind, id, subject, email, display_name) in [
        (
            "platform",
            OTHER.to_owned(),
            provisioning.principal_name(),
            None,
            provisioning.principal_name(),
        ),
        (
            "platform",
            executor.principal_id().to_string(),
            provisioning.principal_name(),
            None,
            provisioning.principal_name(),
        ),
        (
            "platform",
            executor.principal_id().to_string(),
            executor.principal_name(),
            None,
            executor.principal_name(),
        ),
        (
            "platform",
            provisioning.principal_id().to_string(),
            provisioning.principal_name(),
            None,
            "Provisioning",
        ),
        ("human", OTHER.to_owned(), "person", ADDRESS, "wamn:person"),
        ("service", OTHER.to_owned(), "station", None, "wamn:station"),
        (
            "human",
            OTHER.to_owned(),
            provisioning.principal_name(),
            ADDRESS,
            "Person",
        ),
        ("human", OTHER.to_owned(), "mailless", None, "Mailless"),
        ("service", OTHER.to_owned(), "mailed", ADDRESS, "Mailed"),
    ] {
        let error = client
            .execute(
                "INSERT INTO identity.principals (id, kind, subject, email, display_name) \
                 VALUES ($1::text::uuid, $2, $3, $4, $5)",
                &[&id, &kind, &subject, &email, &display_name],
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

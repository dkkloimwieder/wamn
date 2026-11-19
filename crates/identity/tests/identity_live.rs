//! Real-PostgreSQL test for the platform identity core.

use sqlx::Row as _;

use wamn_provision::PlatformComponent;
use wamn_identity_protocol::PrincipalId;
use wamn_identity::{
    IdentityErrorType, Principal, PrincipalStatus, PrincipalType, assign_project_role,
    create_user, disable_principal, grant_project_env_membership,
    has_project_env_membership, project_roles, resolve_principal,
    revoke_project_env_membership,
};

#[sqlx::test(migrations = false)]
async fn platform_identity_round_trip_on_postgres(pool: sqlx::PgPool) {
    // The system schema creates the cluster-wide wamn_system role.
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let url = test_database.as_str().to_owned();

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
    wamn_provision::test_database::system_schema(&url).await;
    let provisioning = provisioning_principal_is_seeded(&mut client).await;
    unbound_identity_writes_refuse(&mut client).await;
    // The fixture is platform setup, so it writes as wamn:provisioning.
    sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES(current_user,$1)")
        .bind(PlatformComponent::Provisioning.principal_id())
        .execute(&mut client)
        .await
        .expect("map the native fixture login");

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("INSERT INTO registry.orgs (id) \
             VALUES ('demo'); \
             INSERT INTO registry.projects (org, id) VALUES ('demo', 'widgets');")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("seed registered project");
    platform_principal_constraint_refuses_other_rows(&mut client).await;

    let user = create_user(
        &mut client,
        "Author@Example.com",
        "author@example.com",
        "Widget Author",
    )
    .await
    .expect("create user principal");
    wamn_identity::org::activate_org_membership(
        &mut client,
        user.id(),
        &"demo".parse().unwrap(),
    )
    .await
    .expect("active org membership fixture");
    assert_eq!(user.principal_type(), PrincipalType::User);
    assert_eq!(user.subject(), "author@example.com");
    assert_eq!(user.status(), PrincipalStatus::Active);

    let duplicate = create_user(
        &mut client,
        "author@example.com",
        "author@example.com",
        "Duplicate",
    )
    .await
    .expect_err("duplicate user identity must fail");
    assert_eq!(duplicate.error_type(), IdentityErrorType::Conflict);

    one_address_admits_one_user(&mut client).await;

    assign_project_role(
        &mut client,
        user.id(),
        &"demo".parse().unwrap(),
        &"widgets".parse().unwrap(),
        "project-author",
    )
    .await
    .expect("assign author role");
    assign_project_role(
        &mut client,
        user.id(),
        &"demo".parse().unwrap(),
        &"widgets".parse().unwrap(),
        "project-promoter",
    )
    .await
    .expect("assign promoter role");
    let roles = project_roles(
        &mut client,
        user.id(),
        &"demo".parse().unwrap(),
        &"widgets".parse().unwrap(),
    )
    .await
    .expect("read project roles");
    assert_eq!(
        roles
            .iter()
            .map(wamn_identity_protocol::RoleName::as_str)
            .collect::<Vec<_>>(),
        ["project-author", "project-promoter"]
    );
    let stamps = sqlx::query(
        "SELECT count(*) FROM identity.principals p \
             JOIN identity.project_roles r ON r.principal_id = p.id \
             WHERE p.id = $1::text::uuid \
               AND p.created_by = $2::text::uuid AND p.updated_by = $2::text::uuid \
               AND r.created_by = $2::text::uuid AND r.updated_by = $2::text::uuid",
    )
    .bind(&user.id().to_string().as_str())
    .bind(&provisioning.to_string().as_str())
    .fetch_one(&mut client)
    .await
    .expect("read principal and role stamps");
    assert_eq!(
        stamps.get::<i64, _>(0),
        2,
        "the principal and both roles stamp wamn:provisioning"
    );

    project_environment_membership_round_trip(&mut client, &user).await;
    project_delete_removes_its_roles_and_memberships(&mut client, &user).await;

    let disabled = disable_principal(&mut client, user.id())
        .await
        .expect("disable user");
    assert_eq!(disabled.status(), PrincipalStatus::Disabled);
    assert_eq!(
        resolve_principal(&mut client, user.id())
            .await
            .expect("resolve disabled user")
            .expect("disabled user remains stored")
            .status(),
        PrincipalStatus::Disabled
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

/// Two spellings of one address are one address (`wamn-0h0g.9.18`).
/// `require_email` folds the case in full, and `UNIQUE (email)` then
/// refuses the second user. The two users carry different subjects, so only
/// the email constraint can refuse the second one.
async fn one_address_admits_one_user(client: &mut sqlx::PgConnection) {
    let first = create_user(
        client,
        "case-fold-first",
        "Case.Fold@Example.Invalid",
        "Case Fold First",
    )
    .await
    .expect("create the first user of the address");
    let stored: String =
        sqlx::query("SELECT email FROM identity.principals WHERE id = $1::text::uuid")
            .bind(&first.id().to_string().as_str())
            .fetch_one(&mut *client)
            .await
            .expect("read the stored email")
            .get(0);
    assert_eq!(
        stored, "case.fold@example.invalid",
        "the stored address is folded in full"
    );
    let second = create_user(
        client,
        "case-fold-second",
        "case.fold@example.invalid",
        "Case Fold Second",
    )
    .await
    .expect_err("one address admits one user");
    assert_eq!(second.error_type(), IdentityErrorType::Conflict);
}

/// A project delete takes the project's roles and environment memberships with
/// it, and leaves every other project's rows in place.
async fn project_delete_removes_its_roles_and_memberships(
    client: &mut sqlx::PgConnection,
    user: &Principal,
) {
    assign_project_role(
        client,
        user.id(),
        &"demo".parse().unwrap(),
        &"inventory".parse().unwrap(),
        "project-author",
    )
    .await
    .expect("assign a role on the project to delete");
    grant_project_env_membership(
        client,
        user.id(),
        &"demo".parse().unwrap(),
        &"inventory".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await
    .expect("grant a membership on the project to delete");
    async fn rows(
        client: &mut sqlx::PgConnection,
        project: &wamn_registry::ProjectId,
    ) -> (i64, i64) {
        sqlx::query(
            "SELECT (SELECT count(*) FROM identity.project_roles \
                         WHERE org = 'demo' AND project = $1), \
                        (SELECT count(*) FROM identity.project_env_memberships \
                         WHERE org = 'demo' AND project = $1)",
        )
        .bind(&project)
        .fetch_one(&mut *client)
        .await
        .map(|row| (row.get::<i64, _>(0), row.get::<i64, _>(1)))
        .expect("count project roles and memberships")
    }
    let widgets = rows(client, &"widgets".parse().unwrap()).await;
    assert_eq!(rows(client, &"inventory".parse().unwrap()).await, (1, 1));
    sqlx::query("DELETE FROM registry.projects WHERE org = 'demo' AND id = 'inventory'")
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .expect("delete the project");
    assert_eq!(rows(client, &"inventory".parse().unwrap()).await, (0, 0));
    assert_eq!(rows(client, &"widgets".parse().unwrap()).await, widgets);
}

async fn project_environment_membership_round_trip(
    client: &mut sqlx::PgConnection,
    user: &Principal,
) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("INSERT INTO registry.orgs (id) \
             VALUES ('other'); \
             INSERT INTO registry.projects (org, id) \
             VALUES ('demo', 'inventory'), ('other', 'widgets'); \
             INSERT INTO registry.environments (org, env) \
             SELECT id, env \
             FROM registry.orgs CROSS JOIN (VALUES ('dev'), ('prod')) AS envs(env); \
             INSERT INTO registry.environment_projects \
               (org, env, project, secret_name, instance_suffix) \
             SELECT p.org, e.env, p.id, 'membership-test-secret', 'a1b2c3d4' \
             FROM registry.projects p JOIN registry.environments e ON p.org = e.org;")
            .to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)
    .expect("seed distinct registered project-environments");
    let other_user = create_user(
        client,
        "other@example.com",
        "other@example.com",
        "Other User",
    )
    .await
    .expect("create another user");
    for (principal, org) in [(user, "demo"), (user, "other"), (&other_user, "demo")] {
        wamn_identity::org::activate_org_membership(
            client,
            principal.id(),
            &org.parse().unwrap(),
        )
        .await
        .expect("active org membership fixture");
    }
    assert!(
        !has_project_env_membership(
            client,
            user.id(),
            &"demo".parse().unwrap(),
            &"widgets".parse().unwrap(),
            &"dev".parse().unwrap()
        )
        .await
        .expect("read membership before grant"),
        "project management roles cannot imply environment membership"
    );

    for _ in 0..2 {
        grant_project_env_membership(
            client,
            user.id(),
            &"demo".parse().unwrap(),
            &"widgets".parse().unwrap(),
            &"dev".parse().unwrap(),
        )
        .await
        .expect("grant user membership idempotently");
    }
    let stored = sqlx::query(
        "SELECT principal_id::text, count(*) OVER () \
             FROM identity.project_env_memberships",
    )
    .fetch_one(&mut *client)
    .await
    .expect("read the stored grant");
    assert_eq!(stored.get::<String, _>(0), user.id().to_string().as_str());
    assert_eq!(stored.get::<i64, _>(1), 1);
    let stamped: bool = sqlx::query(
        "SELECT created_by = updated_by AND created_by = (SELECT wamn_authority.current_principal_id()) \
             FROM identity.project_env_memberships",
    )
    .fetch_one(&mut *client)
    .await
    .expect("read the grant stamps")
    .get(0);
    assert!(
        stamped,
        "the grant stamps the bound wamn:provisioning actor"
    );
    assert!(
        has_project_env_membership(
            client,
            user.id(),
            &"demo".parse().unwrap(),
            &"widgets".parse().unwrap(),
            &"dev".parse().unwrap()
        )
        .await
        .expect("unprepared query observes new grant")
    );

    for (org, project, env) in [
        ("other", "widgets", "dev"),
        ("demo", "inventory", "dev"),
        ("demo", "widgets", "prod"),
    ] {
        assert!(
            !has_project_env_membership(
                client,
                user.id(),
                &org.parse().unwrap(),
                &project.parse().unwrap(),
                &env.parse().unwrap()
            )
            .await
            .unwrap(),
            "the prepared PAT query accepted another environment"
        );
    }
    let other_memberships = [
        (&other_user, "demo", "widgets", "dev"),
        (user, "other", "widgets", "dev"),
        (user, "demo", "inventory", "dev"),
        (user, "demo", "widgets", "prod"),
    ];
    for (principal, org, project, env) in other_memberships {
        assert!(
            !has_project_env_membership(
                client,
                principal.id(),
                &org.parse().unwrap(),
                &project.parse().unwrap(),
                &env.parse().unwrap()
            )
            .await
            .expect("read another principal or environment")
        );
    }
    assert_eq!(
        grant_project_env_membership(
            client,
            user.id(),
            &"demo".parse().unwrap(),
            &"widgets".parse().unwrap(),
            &"missing".parse().unwrap()
        )
        .await
        .expect_err("unregistered environment cannot receive a membership")
        .error_type(),
        IdentityErrorType::NotFound
    );

    for (principal, org, project, env) in other_memberships {
        grant_project_env_membership(
            client,
            principal.id(),
            &org.parse().unwrap(),
            &project.parse().unwrap(),
            &env.parse().unwrap(),
        )
        .await
        .expect("grant independent memberships before revocation");
    }
    assert!(
        revoke_project_env_membership(
            client,
            user.id(),
            &"demo".parse().unwrap(),
            &"widgets".parse().unwrap(),
            &"dev".parse().unwrap()
        )
        .await
        .expect("revoke membership")
    );
    assert!(
        !has_project_env_membership(
            client,
            user.id(),
            &"demo".parse().unwrap(),
            &"widgets".parse().unwrap(),
            &"dev".parse().unwrap()
        )
        .await
        .unwrap(),
        "the prepared PAT query must observe membership revocation"
    );
    assert!(
        !revoke_project_env_membership(
            client,
            user.id(),
            &"demo".parse().unwrap(),
            &"widgets".parse().unwrap(),
            &"dev".parse().unwrap()
        )
        .await
        .expect("repeat revocation harmlessly")
    );
    for (principal, org, project, env) in other_memberships {
        assert!(
            has_project_env_membership(
                client,
                principal.id(),
                &org.parse().unwrap(),
                &project.parse().unwrap(),
                &env.parse().unwrap()
            )
            .await
            .expect("revocation preserves every other principal and environment")
        );
    }

    grant_project_env_membership(
        client,
        user.id(),
        &"demo".parse().unwrap(),
        &"widgets".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await
    .expect("grant before environment replacement");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DELETE FROM registry.environment_projects \
             WHERE org = 'demo' AND project = 'widgets' AND env = 'dev'; \
             INSERT INTO registry.environment_projects \
               (org, env, project, secret_name, instance_suffix) \
             VALUES ('demo', 'dev', 'widgets', 'replacement-secret', 'e5f6g7h8');")
            .to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)
    .expect("replace the provisioned environment");
    assert!(
        !has_project_env_membership(
            client,
            user.id(),
            &"demo".parse().unwrap(),
            &"widgets".parse().unwrap(),
            &"dev".parse().unwrap()
        )
        .await
        .unwrap(),
        "replacement environment must not inherit the deleted grant"
    );

    grant_project_env_membership(
        client,
        other_user.id(),
        &"demo".parse().unwrap(),
        &"widgets".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await
    .expect("grant another user before deletion");
    sqlx::query("DELETE FROM identity.principals WHERE id = $1::text::uuid")
        .bind(&other_user.id().to_string().as_str())
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .expect("delete a principal with no PAT audit records");
    assert!(
        !has_project_env_membership(
            client,
            other_user.id(),
            &"demo".parse().unwrap(),
            &"widgets".parse().unwrap(),
            &"dev".parse().unwrap()
        )
        .await
        .expect("principal deletion removes its membership")
    );
}

/// The system schema seeds the wamn:provisioning row with the derived id, and
/// the row stamps itself.
async fn provisioning_principal_is_seeded(client: &mut sqlx::PgConnection) -> PrincipalId {
    let component = PlatformComponent::Provisioning;
    let id: PrincipalId = component.principal_id();
    let row = sqlx::query(
        "SELECT subject, display_name, created_by::text, updated_by::text, \
                    (SELECT count(*) FROM identity.principals) \
             FROM identity.principals WHERE id = $1::text::uuid",
    )
    .bind(&id.to_string().as_str())
    .fetch_one(&mut *client)
    .await
    .expect("read the seeded wamn:provisioning row");
    assert_eq!(row.get::<String, _>(0), component.principal_name());
    assert_eq!(row.get::<String, _>(1), component.principal_name());
    assert_eq!(row.get::<String, _>(2), id.to_string().as_str());
    assert_eq!(row.get::<String, _>(3), id.to_string().as_str());
    assert_eq!(
        row.get::<i64, _>(4),
        1,
        "only the wamn:provisioning row is seeded"
    );
    let principal = resolve_principal(client, &id)
        .await
        .expect("resolve the platform principal")
        .expect("the platform principal is stored");
    assert_eq!(principal.principal_type(), PrincipalType::Platform);
    id
}

/// A write to an identity authority relation with no bound actor raises
/// SQLSTATE 55000 with the message actor-required. The stamp trigger runs
/// before every constraint, so the rows need no valid references.
async fn unbound_identity_writes_refuse(client: &mut sqlx::PgConnection) {
    const PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";
    for statement in [
        "INSERT INTO identity.principals (type, subject, display_name) \
         VALUES ('user', 'unbound', 'Unbound')"
            .to_owned(),
        format!(
            "INSERT INTO identity.project_roles (principal_id, org, project, role) \
             VALUES ('{PRINCIPAL}', 'demo', 'widgets', 'unbound')"
        ),
        format!(
            "INSERT INTO identity.project_env_memberships (principal_id, org, project, env) \
             VALUES ('{PRINCIPAL}', 'demo', 'widgets', 'dev')"
        ),
        "UPDATE identity.principals SET display_name = 'Unbound'".to_owned(),
    ] {
        let error = sqlx::query(sqlx::AssertSqlSafe((statement.as_str()).to_string()))
            .execute(&mut *client)
            .await
            .map(|result| result.rows_affected())
            .expect_err("a write with no bound actor must refuse");
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("55000"),
            "{statement}"
        );
    }
}

/// `principals_platform_principal_check` admits only the pinned
/// wamn:provisioning row for type platform, and the other types refuse a
/// `wamn:` name.
///
/// The last row checks that a user needs an email.
async fn platform_principal_constraint_refuses_other_rows(client: &mut sqlx::PgConnection) {
    const OTHER: &str = "00000000-0000-4000-8000-0000000000f2";
    const ADDRESS: Option<&str> = Some("refused@example.invalid");
    let provisioning = PlatformComponent::Provisioning;
    let executor = PlatformComponent::Executor;
    for (type_, id, subject, email, display_name) in [
        (
            "platform",
            OTHER.parse::<PrincipalId>().unwrap(),
            provisioning.principal_name().as_str(),
            None,
            provisioning.principal_name().as_str(),
        ),
        (
            "platform",
            executor.principal_id(),
            provisioning.principal_name().as_str(),
            None,
            provisioning.principal_name().as_str(),
        ),
        (
            "platform",
            executor.principal_id(),
            executor.principal_name().as_str(),
            None,
            executor.principal_name().as_str(),
        ),
        (
            "platform",
            provisioning.principal_id(),
            provisioning.principal_name().as_str(),
            None,
            "Provisioning",
        ),
        (
            "user",
            OTHER.parse::<PrincipalId>().unwrap(),
            "user",
            ADDRESS,
            "wamn:user",
        ),
        (
            "user",
            OTHER.parse::<PrincipalId>().unwrap(),
            provisioning.principal_name().as_str(),
            ADDRESS,
            "User",
        ),
        (
            "user",
            OTHER.parse::<PrincipalId>().unwrap(),
            "mailless",
            None,
            "Mailless",
        ),
    ] {
        let error = sqlx::query(
            "INSERT INTO identity.principals (id, type, subject, email, display_name) \
                 VALUES ($1::text::uuid, $2, $3, $4, $5)",
        )
        .bind(&id)
        .bind(&type_)
        .bind(&subject)
        .bind(&email)
        .bind(&display_name)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .expect_err("the principal CHECKs must refuse this row");
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("23514"),
            "{type_} {id} {subject} {display_name}"
        );
    }
}

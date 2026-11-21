//! Shared installation and catalog rules for login interface tests.

use postgres_protocol::escape::escape_identifier;
use sqlx::Connection as _;
use wamn_provision::migrations::MigrationDatabase;
use wamn_provision::workload_role::WorkloadRoleIdentityType;
use wamn_provision::{
    PlatformComponent, WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login,
};

#[derive(Debug)]
pub struct Fixture {
    pub admin: sqlx::PgConnection,
    pub url: String,
    pub login: wamn_identity_protocol::LoginName,
}

pub async fn execute(connection: &mut sqlx::PgConnection, statement: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(connection)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

/// Install the schemas of `family` on the database of `url` and create its login.
pub async fn prepare(url: url::Url, family: WorkloadRoleFamily) -> Fixture {
    let database = url.path().trim_start_matches('/');
    let mut admin = wamn_postgres::connect(url.as_str()).await.unwrap();
    match family.identity_type() {
        WorkloadRoleIdentityType::Control | WorkloadRoleIdentityType::Org => {
            super::database::system(url.as_str()).await;
        }
        _ => {
            // As in production, the migration login owns the project database
            // and installs its schemas.
            execute(&mut admin, sql::ensure_project_roles_sql()).await;
            super::database::platform_migration_input(url.as_str(), MigrationDatabase::Project)
                .await;
            let owner = MigrationDatabase::Project.login(database);
            execute(
                &mut admin,
                &format!("SET ROLE {}", escape_identifier(owner.as_str())),
            )
            .await;
            for batch in [
                wamn_provision::CATALOG_SCHEMA_SQL,
                include_str!("../../../../deploy/sql/run-state.sql"),
                include_str!("../../../../deploy/sql/run-queue.sql"),
                wamn_provision::APP_SCHEMA_SQL,
                "RESET ROLE",
            ] {
                execute(&mut admin, batch).await;
            }
        }
    }
    let target = match family.identity_type() {
        WorkloadRoleIdentityType::ProjectEnvKey => WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &"t1--dev--project".parse().unwrap(),
            database,
        },
        WorkloadRoleIdentityType::ProjectEnvironment => WorkloadRoleIdentity::ProjectEnvironment {
            org: &"acme".parse().unwrap(),
            project: &"billing".parse().unwrap(),
            environment: &"dev".parse().unwrap(),
            database,
        },
        WorkloadRoleIdentityType::Control => WorkloadRoleIdentity::Control {
            org: &"acme".parse().unwrap(),
            project: &"billing".parse().unwrap(),
            environment: &"dev".parse().unwrap(),
            database,
        },
        WorkloadRoleIdentityType::Org => WorkloadRoleIdentity::Org {
            org: &"acme".parse().unwrap(),
            database,
        },
    };
    let login = workload_login(family, target).unwrap();
    execute(
        &mut admin,
        &super::database::login_sql(family, database, &login, "grants-test"),
    )
    .await;
    if family == WorkloadRoleFamily::Migration {
        execute(
            &mut admin,
            &wamn_provision::migration::grants_sql(&login),
        )
        .await;
    }
    let mut url = url;
    url.set_username(login.as_str()).unwrap();
    url.set_password(Some("grants-test")).unwrap();
    Fixture {
        admin,
        url: url.into(),
        login,
    }
}

pub async fn refuses(url: &str, statement: &str) {
    let mut connection = wamn_postgres::connect(url).await.unwrap();
    let result = sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(&mut connection)
        .await;
    match result {
        Ok(result) => panic!(
            "login {} accepted {statement}; rows affected: {}",
            url::Url::parse(url).unwrap().username(),
            result.rows_affected()
        ),
        Err(error) => assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("42501"),
            "{statement}: {error}"
        ),
    }
}

pub async fn rules(fixture: &mut Fixture, family: WorkloadRoleFamily) {
    let mut login = wamn_postgres::connect(&fixture.url).await.unwrap();
    let restricted: bool = sqlx::query_scalar(
        "SELECT rolcanlogin AND NOT (rolsuper OR rolbypassrls OR rolcreatedb OR rolcreaterole OR rolreplication)
         AND NOT EXISTS (SELECT FROM pg_roles r WHERE (r.rolsuper OR r.rolbypassrls OR r.rolcreaterole)
                         AND pg_has_role(current_user,r.oid,'MEMBER'))
         FROM pg_roles WHERE rolname=current_user",
    ).fetch_one(&mut login).await.unwrap();
    assert!(
        restricted,
        "{} can bypass its database privileges",
        fixture.login
    );
    for attribute in ["SUPERUSER", "BYPASSRLS"] {
        refuses(
            &fixture.url,
            &format!(
                "ALTER ROLE {} {attribute}",
                escape_identifier(fixture.login.as_str())
            ),
        )
        .await;
    }
    // Check effective access, including PUBLIC, inherited roles and column grants.
    let exposed: Vec<String> = sqlx::query_scalar(
        "SELECT c.oid::regclass::text FROM pg_class c
         WHERE c.oid IN ('platform.tx_identity'::regclass,'platform.login_principal'::regclass)
           AND (has_table_privilege($1,c.oid,'SELECT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER')
                OR has_any_column_privilege($1,c.oid,'SELECT,UPDATE,REFERENCES')
                OR (c.oid <> 'platform.tx_identity'::regclass AND
                    (has_table_privilege($1,c.oid,'INSERT') OR has_any_column_privilege($1,c.oid,'INSERT'))))",
    ).bind(&fixture.login).fetch_all(&mut fixture.admin).await.unwrap();
    assert!(
        exposed.is_empty(),
        "{} exposes caller identity storage: {exposed:?}",
        fixture.login
    );
    for statement in [
        "SELECT * FROM platform.tx_identity",
        "DELETE FROM platform.tx_identity",
        "SELECT * FROM platform.login_principal",
        "DELETE FROM platform.login_principal",
    ] {
        refuses(&fixture.url, statement).await;
    }
    let writable: Vec<String> = sqlx::query_scalar(
        "SELECT nspname::text FROM pg_namespace
         WHERE nspname NOT LIKE 'pg_%' AND nspname <> 'information_schema'
           AND NOT ($2 AND nspname='wamn_history')
           AND has_schema_privilege($1,oid,'CREATE')",
    )
    .bind(&fixture.login)
    .bind(family == WorkloadRoleFamily::Migration)
    .fetch_all(&mut fixture.admin)
    .await
    .unwrap();
    assert!(
        writable.is_empty(),
        "{} can create objects in {writable:?}",
        fixture.login
    );

    let grantable: Vec<String> = sqlx::query_scalar(
        "WITH privileges AS (
           SELECT c.oid::regclass::text AS object,a.* FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) a
           UNION ALL SELECT c.attrelid::regclass::text,a.* FROM pg_attribute c CROSS JOIN LATERAL aclexplode(c.attacl) a
           UNION ALL SELECT n.nspname::text,a.* FROM pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) a
           UNION ALL SELECT p.oid::regprocedure::text,a.* FROM pg_proc p CROSS JOIN LATERAL aclexplode(p.proacl) a
           UNION ALL SELECT d.datname::text,a.* FROM pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) a)
         SELECT object FROM privileges WHERE is_grantable
           AND CASE WHEN grantee=0 THEN true ELSE pg_has_role($1,grantee,'USAGE') END",
    ).bind(&fixture.login).fetch_all(&mut fixture.admin).await.unwrap();
    assert!(
        grantable.is_empty(),
        "{} can pass on privileges: {grantable:?}",
        fixture.login
    );
}

pub async fn family(url: url::Url, family: WorkloadRoleFamily) -> Fixture {
    let mut fixture = prepare(url, family).await;
    rules(&mut fixture, family).await;
    // A write rule covers every installed relation and every column grant.
    if matches!(
        family,
        WorkloadRoleFamily::Query
    ) {
        let writes: Vec<String> = sqlx::query_scalar(
            "SELECT c.oid::regclass::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE n.nspname NOT LIKE 'pg_%' AND n.nspname<>'information_schema'
               AND c.relkind IN ('r','p','v','m','f')
               AND (has_table_privilege($1,c.oid,'UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER')
                    OR has_any_column_privilege($1,c.oid,'UPDATE,REFERENCES')
                    OR (c.oid<>'platform.tx_identity'::regclass AND
                       (has_table_privilege($1,c.oid,'INSERT') OR has_any_column_privilege($1,c.oid,'INSERT'))))",
        ).bind(&fixture.login).fetch_all(&mut fixture.admin).await.unwrap();
        assert!(
            writes.is_empty(),
            "{} holds writes beyond caller identity: {writes:?}",
            fixture.login
        );
    }
    if family == WorkloadRoleFamily::App || family == WorkloadRoleFamily::Query {
        // Positive control: identity INSERT works before a forbidden application write.
        let mut connection = wamn_postgres::connect(&fixture.url).await.unwrap();
        let mut transaction = connection.begin().await.unwrap();
        sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application) VALUES($1,'viewer','fixture')")
            .bind(PlatformComponent::Executor.principal_id()).execute(&mut *transaction).await.unwrap();
        transaction.rollback().await.unwrap();
    }
    let statement = match family {
        WorkloadRoleFamily::Query => {
            "INSERT INTO catalog.packages(package_id,package_version) VALUES('forged','1.0.0')"
        }
        WorkloadRoleFamily::App | WorkloadRoleFamily::Migration | WorkloadRoleFamily::Platform => {
            "UPDATE platform.login_principal SET principal_id=principal_id"
        }
    };
    refuses(&fixture.url, statement).await;
    // The family cannot gain the owner's role through SET ROLE.
    refuses(
        &fixture.url,
        &format!(
            "SET ROLE {}",
            escape_identifier(
                MigrationDatabase::Project.login(
                    url::Url::parse(&fixture.url)
                        .unwrap()
                        .path()
                        .trim_start_matches('/')
                )
                .as_str()
            )
        ),
    )
    .await;
    fixture
}

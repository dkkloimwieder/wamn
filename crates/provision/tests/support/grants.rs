//! Shared installation and catalog rules for login interface tests.

use postgres_protocol::escape::escape_identifier;
use sqlx::Connection as _;
use wamn_control_provision::workload_role::WorkloadRoleIdentityType;
use wamn_control_provision::{
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

pub async fn prepare(pool: &sqlx::PgPool, family: WorkloadRoleFamily) -> Fixture {
    let url = wamn_test_postgres::url(pool);
    let database = url.path().trim_start_matches('/');
    let mut admin = wamn_postgres::connect(url.as_str()).await.unwrap();
    match family.identity_type() {
        WorkloadRoleIdentityType::Control | WorkloadRoleIdentityType::Org => {
            wamn_control_provision::test_database::system(url.as_str()).await;
        }
        _ => {
            for batch in [
                &sql::ensure_app_acl_role_sql(),
                sql::ensure_db_owner_role_sql(),
                wamn_catalog::CATALOG_SCHEMA_SQL,
                include_str!("../../../../deploy/sql/run-state.sql"),
                include_str!("../../../../deploy/sql/run-queue.sql"),
                wamn_control_provision::APP_SCHEMA_SQL,
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
            org: "acme",
            project: "billing",
            environment: &"dev".parse().unwrap(),
            database,
        },
        WorkloadRoleIdentityType::Control => WorkloadRoleIdentity::Control {
            org: "acme",
            project: "billing",
            environment: &"dev".parse().unwrap(),
            database,
        },
        WorkloadRoleIdentityType::Org => WorkloadRoleIdentity::Org {
            org: "acme",
            database,
        },
    };
    let login = workload_login(family, target).unwrap();
    execute(
        &mut admin,
        &wamn_control_provision::test_database::login_sql(family, database, &login, "grants-test"),
    )
    .await;
    if family == WorkloadRoleFamily::Migration {
        execute(
            &mut admin,
            &wamn_control_provision::migration::grants_sql(&login),
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
    if matches!(
        family,
        WorkloadRoleFamily::IdentityReader | WorkloadRoleFamily::ConnectionReader
    ) {
        let sequences: Vec<String> = sqlx::query_scalar(
            "SELECT c.oid::regclass::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE c.relkind='S' AND n.nspname NOT LIKE 'pg_%'
               AND CASE WHEN c.relkind='S' THEN has_sequence_privilege($1,c.oid,'USAGE,SELECT,UPDATE') ELSE false END",
        ).bind(&fixture.login).fetch_all(&mut fixture.admin).await.unwrap();
        assert!(
            sequences.is_empty(),
            "{} reaches sequences: {sequences:?}",
            fixture.login
        );
    }
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

pub async fn family(
    pool: &sqlx::PgPool,
    family: WorkloadRoleFamily,
) -> (Fixture, wamn_test_postgres::ProcessLock) {
    let lock = wamn_test_postgres::lock();
    let mut fixture = prepare(pool, family).await;
    rules(&mut fixture, family).await;
    // A write rule covers every installed relation and every column grant.
    if matches!(
        family,
        WorkloadRoleFamily::Query
            | WorkloadRoleFamily::IdentityReader
            | WorkloadRoleFamily::HttpAdmitter
            | WorkloadRoleFamily::SessionRoleReader
            | WorkloadRoleFamily::ServiceReader
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
        // Seed both projects through the installed product schemas.
        execute(&mut fixture.admin, &format!("BEGIN; {} INSERT INTO catalog.packages(project_env_key,package_id,package_version) VALUES('t1--dev--project','fixture','1.0.0'),('t2--dev--project','fixture','1.0.0'); COMMIT", wamn_control_provision::bind_platform_principal_sql(PlatformComponent::Provisioning))).await;
        let mut transaction = connection.begin().await.unwrap();
        sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application) VALUES($1,'viewer','fixture')")
            .bind(PlatformComponent::Executor.principal_id()).execute(&mut *transaction).await.unwrap();
        let (own, foreign): (bool,bool) = sqlx::query_as("SELECT EXISTS(SELECT FROM catalog.packages WHERE project_env_key='t1--dev--project'), EXISTS(SELECT FROM catalog.packages WHERE project_env_key<>'t1--dev--project')")
            .fetch_one(&mut *transaction).await.unwrap();
        assert!(
            own && !foreign,
            "{} reads another project's rows: own={own}, foreign={foreign}",
            fixture.login
        );
        transaction.rollback().await.unwrap();
    }
    let statement = match family {
        WorkloadRoleFamily::Query => {
            "INSERT INTO catalog.packages(project_env_key,package_id,package_version) VALUES('t1--dev--project','forged','1.0.0')"
        }
        WorkloadRoleFamily::IdentityReader => "UPDATE identity.principals SET status='active'",
        WorkloadRoleFamily::ConnectionReader => "SELECT * FROM registry.connection",
        WorkloadRoleFamily::ServiceReader => "SELECT * FROM catalog.packages",
        WorkloadRoleFamily::Retention => "UPDATE wamn_run.runs SET package_id=package_id",
        WorkloadRoleFamily::AuditRetention => "DELETE FROM catalog.packages",
        WorkloadRoleFamily::ExecutorPlatform => "UPDATE wamn_run.runs SET input_json='{}'",
        WorkloadRoleFamily::HttpAdmitter | WorkloadRoleFamily::SessionRoleReader => {
            "UPDATE platform.permission SET permission=permission"
        }
        WorkloadRoleFamily::Administration | WorkloadRoleFamily::Control => {
            "DELETE FROM catalog.connection_binding"
        }
        WorkloadRoleFamily::App | WorkloadRoleFamily::Migration => {
            "UPDATE platform.login_principal SET principal_id=principal_id"
        }
    };
    refuses(&fixture.url, statement).await;
    // The family cannot gain the owner's role through SET ROLE.
    refuses(
        &fixture.url,
        &format!("SET ROLE {}", escape_identifier("wamn_db_owner")),
    )
    .await;
    (fixture, lock)
}

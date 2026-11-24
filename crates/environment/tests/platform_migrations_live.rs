//! Live test of `provision-system` against a disposable PostgreSQL server.
//!
//! Each test starts its own server, because the verb changes cluster-wide
//! roles and the PUBLIC CONNECT base grants of every database.

use wamn_platform_install::provision_system::initialize_system;

/// Create the system migration login of the database `name` as CNPG would, and return its URL.
async fn system_migration_login(
    postgres: &wamn_test_infrastructure::containers::Postgres,
    name: &str,
) -> url::Url {
    use postgres_protocol::escape::escape_identifier;
    let login = wamn_postgres::migration_scope::MigrationDatabase::System.login(name);
    let role = escape_identifier(login.as_str());
    // CNPG grants this membership with SET permission, which ownership transfers require.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS \
         PASSWORD 'platform-migration-fixture'; \
         GRANT {} TO {role} WITH INHERIT TRUE, SET TRUE; GRANT CONNECT ON DATABASE {} TO {role}",
        escape_identifier(wamn_role::SYSTEM_ROLE),
        escape_identifier(name),
    )))
    .execute(&mut wamn_postgres::connect(postgres.admin_url()).await.unwrap())
    .await
    .unwrap();
    postgres
        .database_url(name, login.as_str(), "platform-migration-fixture")
        .unwrap()
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn platform_initialization_applies_pending_migrations_on_upgrade() {
    use sqlx::SqlSafeStr as _;
    use sqlx::migrate::{Migration, MigrationType};
    use wamn_postgres::environment_schema;
    use wamn_postgres::migration_scope::MigrationDatabase;
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    postgres.create_system_role().await.unwrap();
    let url = postgres
        .create_database("system")
        .await
        .unwrap()
        .to_string();
    let mut client = wamn_postgres::connect(&url).await.unwrap();
    let source = |version, sql: &str| {
        Migration::new(
            version,
            "probe".into(),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(sql.to_owned()).into_sql_str(),
            false,
        )
    };
    let first = source(
        10,
        "CREATE TABLE registry.migration_probe(value int); INSERT INTO registry.migration_probe VALUES (1)",
    );
    let second = source(30, "INSERT INTO registry.migration_probe VALUES (2)");
    let migration_input = system_migration_login(&postgres, "system").await;
    assert!(
        environment_schema::apply_reported(
            &mut client,
            environment_schema::MigrationTarget::System,
            std::slice::from_ref(&first)
        )
        .await
        .is_err()
    );
    initialize_system(
        &mut client,
        migration_input.as_str(),
        std::slice::from_ref(&first),
    )
    .await
    .unwrap();
    let sources = [first, second];
    initialize_system(&mut client, migration_input.as_str(), &sources)
        .await
        .unwrap();
    let observed = wamn_environment::platform_migrations::observe(
        &mut client,
        MigrationDatabase::System,
        &sources,
    )
    .await
    .unwrap();
    assert!(
        observed.pending.is_empty() && observed.missing.is_empty() && observed.warnings.is_empty()
    );
    let values: Vec<i32> =
        sqlx::query_scalar("SELECT value FROM registry.migration_probe ORDER BY value")
            .fetch_all(&mut client)
            .await
            .unwrap();
    assert_eq!(values, [1, 2]);
    let owns: bool = sqlx::query_scalar("SELECT relowner=(SELECT oid FROM pg_roles WHERE rolname='wamn_system') FROM pg_class WHERE oid='registry._sqlx_migrations'::regclass").fetch_one(&mut client).await.unwrap();
    assert!(owns);
}

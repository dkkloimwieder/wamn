//! PostgreSQL contracts of SQLx package migrations and the restricted login.

use sqlx::SqlSafeStr as _;
use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};
use wamn_control_provision::migration;
use wamn_environment::migrations;

fn source(version: i64, sql: &str) -> Migration {
    Migration::new(
        version,
        "change".into(),
        MigrationType::Simple,
        sqlx::AssertSqlSafe(sql.to_owned()).into_sql_str(),
        sql.starts_with("-- no-transaction"),
    )
}
fn migrator(sources: &[Migration]) -> Migrator {
    Migrator::with_migrations(sources.to_vec())
}

#[sqlx::test(migrations = false)]
async fn sqlx_commits_files_with_their_history_and_refuses_checksum_drift(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let mut admin = wamn_postgres::connect(database.as_str()).await?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut admin)
        .await?;
    let component = wamn_control_provision::PlatformComponent::ApplyPackage;
    sqlx::query("SELECT set_config('app.user_id',$1,false), set_config('app.operation',$2,false)")
        .bind(component.principal_id().to_string())
        .bind(component.principal_name())
        .execute(&mut admin)
        .await?;
    let table = migration::history_table("fixture--dev--one", "orders");
    migration::prepare_history(&mut admin, &table, "postgres").await?;
    let first = source(
        10,
        "CREATE TABLE public.migration_effect(id int PRIMARY KEY)",
    );
    let failing = source(
        30,
        "INSERT INTO public.migration_effect VALUES (1); SELECT 1/0",
    );
    assert!(
        migrations::run(
            &mut admin,
            migrator(&[first.clone(), failing]),
            table.clone()
        )
        .await
        .is_err()
    );
    let rows: Vec<i32> = sqlx::query_scalar("SELECT id FROM public.migration_effect")
        .fetch_all(&mut admin)
        .await?;
    assert!(rows.is_empty());
    let last = source(30, "INSERT INTO public.migration_effect VALUES (1)");
    let sources = [first, last];
    migrations::run(&mut admin, migrator(&sources), table.clone()).await?;
    use sqlx::migrate::Migrate as _;
    let applied = admin.list_applied_migrations(&table).await?;
    for expected in migrator(&sources).iter() {
        assert!(applied.iter().any(
            |actual| actual.version == expected.version && actual.checksum == expected.checksum
        ));
    }
    let stamps: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT bool_and(created_by=$1 AND updated_by=$1) FROM {table}"
    )))
    .bind(component.principal_id())
    .fetch_one(&mut admin)
    .await?;
    assert!(stamps);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM public.migration_effect")
        .fetch_one(&mut admin)
        .await?;
    assert_eq!(count, 1);
    assert_eq!(
        migrations::run(&mut admin, migrator(&sources), table.clone()).await?,
        0
    );
    let changed = [source(10, "SELECT 1"), sources[1].clone()];
    let error = migrations::run(&mut admin, migrator(&changed), table.clone())
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<MigrateError>(),
        Some(MigrateError::VersionMismatch(_))
    ));
    let error = migrations::run(&mut admin, migrator(&sources[..1]), table.clone())
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<MigrateError>(),
        Some(MigrateError::VersionMissing(_))
    ));
    // An unavailable history write rolls the migration's data back with it.
    let extra = source(50, "INSERT INTO public.migration_effect VALUES (2)");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE FUNCTION public.refuse_record() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'unavailable'; END $$; CREATE TRIGGER refuse_record BEFORE INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION public.refuse_record()")))
        .execute(&mut admin).await?;
    let mut with_extra = sources.to_vec();
    with_extra.push(extra);
    assert!(
        migrations::run(&mut admin, migrator(&with_extra), table.clone())
            .await
            .is_err()
    );
    let rows: Vec<i32> = sqlx::query_scalar("SELECT id FROM public.migration_effect ORDER BY id")
        .fetch_all(&mut admin)
        .await?;
    assert_eq!(rows, [1]);
    // SQLx retries a nontransactional file whose effects committed before its history write failed.
    let mut nontransactional = sources.to_vec();
    nontransactional.push(source(50, "-- no-transaction\nCREATE INDEX CONCURRENTLY IF NOT EXISTS migration_effect_idx ON public.migration_effect(id)"));
    assert!(
        migrations::run(&mut admin, migrator(&nontransactional), table.clone())
            .await
            .is_err()
    );
    let indexed: bool = sqlx::query_scalar(
        "SELECT indisvalid FROM pg_index WHERE indexrelid='public.migration_effect_idx'::regclass",
    )
    .fetch_one(&mut admin)
    .await?;
    assert!(indexed);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP TRIGGER refuse_record ON {table}"
    )))
    .execute(&mut admin)
    .await?;
    migrations::run(&mut admin, migrator(&nontransactional), table.clone()).await?;
    for sql in [
        format!("DELETE FROM {table}"),
        format!("UPDATE {table} SET checksum='changed'::bytea"),
    ] {
        assert!(
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(&mut admin)
                .await
                .is_err()
        );
    }
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn migration_login_cannot_escape_or_change_privileges_even_through_dynamic_sql(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let mut admin = wamn_postgres::connect(database.as_str()).await?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut admin)
        .await?;
    sqlx::raw_sql("CREATE SCHEMA platform")
        .execute(&mut admin)
        .await?;
    sqlx::raw_sql(include_str!(
        "../../../../deploy/sql/migration-privileges.sql"
    ))
    .execute(&mut admin)
    .await?;
    let database_name = wamn_postgres::identity(database.as_str())?.1;
    let login = migration::login("fixture--dev--one", &database_name)?;
    let role = postgres_protocol::escape::escape_identifier(&login);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE ROLE {role} LOGIN PASSWORD 'migration-test' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; CREATE SCHEMA application AUTHORIZATION {role}; GRANT CONNECT ON DATABASE {} TO {role}", postgres_protocol::escape::escape_identifier(&database_name))))
        .execute(&mut admin).await?;
    let options = wamn_postgres::connect_options(database.as_str())
        .await?
        .username(&login)
        .password("migration-test");
    let mut executor = wamn_postgres::connect(options).await?;
    migration::require_login(&mut executor, "fixture--dev--one").await?;
    assert!(
        migration::require_login(&mut admin, "fixture--dev--one")
            .await
            .is_err()
    );
    sqlx::raw_sql("RESET ROLE; CREATE TABLE application.item(id int); CREATE FUNCTION application.echo(value int) RETURNS int LANGUAGE sql AS 'SELECT value'")
        .execute(&mut executor).await?;
    for sql in [
        "GRANT SELECT ON application.item TO PUBLIC",
        "REVOKE SELECT ON application.item FROM PUBLIC",
        "ALTER DEFAULT PRIVILEGES GRANT SELECT ON TABLES TO PUBLIC",
        "DO $$ BEGIN EXECUTE 'GRANT SELECT ON application.item TO PUBLIC'; END $$",
        "SET ROLE wamn_db_owner",
        "SET SESSION AUTHORIZATION postgres",
    ] {
        let error = sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
            .execute(&mut executor)
            .await
            .unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("42501")
        );
    }
    let public_reads: bool = sqlx::query_scalar("SELECT EXISTS(SELECT FROM pg_class c, LATERAL aclexplode(c.relacl) acl WHERE c.oid='application.item'::regclass AND acl.grantee=0 AND acl.privilege_type='SELECT')").fetch_one(&mut admin).await?;
    assert!(!public_reads);
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn package_apply_refuses_rebinding_an_existing_relation(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    use wamn_environment::apply_package::{self, ApplyPackageRequest, PackageRoot};
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_platform_identity::test_database::project_platform(database.as_str()).await;
    let migration_input = wamn_control_provision::test_database::package_migration_input(
        database.as_str(),
        "mapping",
        "fixture",
        "dev",
    )
    .await?;
    let migration_input = migration::configuration(&migration_input, "mapping", "fixture", "dev")?;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let write_package =
        |name: &str, entity: &str, version: &str, initial: &str| -> anyhow::Result<PackageRoot> {
            let root = scratch.path().join(name);
            std::fs::create_dir_all(root.join("generated"))?;
            std::fs::create_dir_all(root.join("migrations/notes"))?;
            std::fs::write(root.join("migrations/README.md"), "migration notes")?;
            let manifest = serde_json::json!({"package":{"id":name,"version":version},
            "required_platform_policy_contract":{"id":"fixture_access","state":"satisfied"},
            "models":{(entity):{"schema":"fixture_app","table":"item","owner":name,
                "audit_log":{"columns":[],"retention":"none"},"operations":{}}},
            "connections":{},"components":{}});
            std::fs::write(
                root.join("generated/wamn.json"),
                serde_json::to_vec(&manifest)?,
            )?;
            std::fs::write(root.join("migrations/0001_initial.up.sql"), initial)?;
            std::fs::write(
                root.join("migrations/0001_initial.down.sql"),
                "DROP TABLE fixture_app.item",
            )?;
            Ok(PackageRoot {
                path: root,
                artifact_digest: Some(format!("sha256:{}", "a".repeat(64))),
            })
        };
    let request = |package| ApplyPackageRequest {
        component_source: None,
        package,
        changes: wamn_broker::Publisher::default(),
        database_url: database.as_str().to_owned(),
        migration_database_url: migration_input.clone(),
        org: "mapping".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    };
    const INITIAL: &str = "CREATE TABLE fixture_app.item(id int PRIMARY KEY)";
    apply_package::apply_package_schema(request(write_package("first", "item", "1.0.0", INITIAL)?))
        .await?;
    let mut admin = wamn_postgres::connect(database.as_str()).await?;
    let original: sqlx::postgres::types::Oid =
        sqlx::query_scalar("SELECT 'fixture_app.item'::regclass::oid")
            .fetch_one(&mut admin)
            .await?;
    for package in [
        write_package("second", "item", "1.0.0", "SELECT 1")?,
        write_package("first", "replacement", "2.0.0", INITIAL)?,
    ] {
        assert!(
            apply_package::apply_package_schema(request(package))
                .await
                .is_err()
        );
        let (oid, original_mapping): (sqlx::postgres::types::Oid,bool) = sqlx::query_as(
            "SELECT relation_oid::oid, package_id='first' AND entity_id='item' FROM fixture_app.wamn_entities WHERE relation_oid='fixture_app.item'::regclass")
            .fetch_one(&mut admin).await?;
        assert_eq!(oid, original);
        assert!(original_mapping);
    }
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn platform_migrations_refuse_administrators_and_use_scope_credentials(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    use wamn_control_provision::migrations::MigrationDatabase;
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut administrator = wamn_postgres::connect(database.as_str()).await?;

    for scope in [MigrationDatabase::System, MigrationDatabase::Project] {
        assert!(scope.require_login(&mut administrator).await.is_err());
        let input = wamn_control_provision::test_database::platform_migration_input(
            database.as_str(),
            scope,
        )
        .await?;
        // An explicit input retains its password and native CA configuration and authenticates directly.
        let input = scope.configuration(&input)?;
        let mut connection = wamn_postgres::connect(&input).await?;
        scope.require_login(&mut connection).await?;
        let owner_stays_closed: bool =
            sqlx::query_scalar("SELECT NOT rolcanlogin FROM pg_roles WHERE rolname=$1")
                .bind(scope.owner_role())
                .fetch_one(&mut administrator)
                .await?;
        assert!(owner_stays_closed);
        let declaration = scope.managed_role(database.path().trim_start_matches('/'));
        assert!(
            declaration.login
                && !declaration.superuser
                && !declaration.createdb
                && !declaration.createrole
                && !declaration.bypassrls
        );
    }
    Ok(())
}

#[tokio::test]
async fn reset_preserves_platform_schemas_and_cdc_and_reapplies_package_sql() -> anyhow::Result<()>
{
    use wamn_environment::{apply_package, environment::sql::reset::reset_database};
    let _lock = wamn_test_postgres::lock();
    let mut case_server = wamn_test_postgres::start(&[]).unwrap();
    let system = url::Url::parse(
        case_server
            .create_database("fixture_1")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    wamn_control_provision::test_database::system(system.as_str()).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::test_database::seed_org_project_env_sql("reset", "fixture", "dev"),
    ))
    .execute(&mut wamn_postgres::connect(system.as_str()).await.unwrap())
    .await?;
    let triple = wamn_registry::Triple::new("reset", "dev", "fixture");

    let database = url::Url::parse(
        case_server
            .create_database("fixture_extra")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::set_database_owner_sql(
            database.path().trim_start_matches('/'),
        ),
    ))
    .execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
    .await?;
    wamn_platform_install::environment::initialize_database(database.as_str()).await?;
    assert!(
        wamn_platform_install::environment::initialize_database(database.as_str())
            .await
            .is_err()
    );
    let mut administrator = wamn_postgres::connect(database.as_str()).await?;
    let baseline: bool = sqlx::query_scalar("SELECT to_regclass('platform.\"user\"') IS NOT NULL")
        .fetch_one(&mut administrator)
        .await?;
    assert!(baseline);

    let migration_input = wamn_control_provision::test_database::package_migration_input(
        database.as_str(),
        "reset",
        "fixture",
        "dev",
    )
    .await?;
    let owner_input = wamn_control_provision::test_database::platform_migration_input(
        database.as_str(),
        wamn_control_provision::migrations::MigrationDatabase::Project,
    )
    .await?;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let root = scratch.path().join("package");
    wamn_publish::test_support::copy_schema_fixture(&root);
    let directory = apply_package::read_package_directory(&root)?;
    let metadata = wamn_catalog::package_schema::package_metadata(&directory)?;
    let apply = || apply_package::ApplyPackageRequest {
        package: wamn_publish::package_artifact::fixture_package_root(&root).unwrap(),
        component_source: None,
        changes: wamn_broker::Publisher::default(),
        database_url: database.as_str().to_owned(),
        migration_database_url: migration_input.clone(),
        org: "reset".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    };
    apply_package::apply_package_schema(apply()).await?;
    let mut admin = wamn_postgres::connect(database.as_str()).await?;
    let before: sqlx::postgres::types::Oid =
        sqlx::query_scalar("SELECT oid FROM pg_database WHERE datname=current_database()")
            .fetch_one(&mut admin)
            .await?;
    let schemas = metadata
        .models
        .iter()
        .map(|model| postgres_protocol::escape::escape_identifier(&model.schema))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE PUBLICATION reset_publication FOR TABLES IN SCHEMA {schemas}"
    )))
    .execute(&mut admin)
    .await?;
    sqlx::query("INSERT INTO platform.write_log(operation,idempotency_key,request,result) VALUES ('fixture.create','before-reset',$1,'committed')")
        .bind(&b"request"[..]).execute(&mut admin).await?;
    sqlx::raw_sql("SET app.user_id='770df186-ac15-579e-b46b-c297cae2011b'; UPDATE registry.environments SET disposable=false WHERE org='reset' AND env='dev'").execute(&mut wamn_postgres::connect(system.as_str()).await.unwrap()).await?;
    assert!(
        reset_database(system.as_str(), &owner_input, &migration_input, &triple)
            .await
            .is_err()
    );
    sqlx::raw_sql("SET app.user_id='770df186-ac15-579e-b46b-c297cae2011b'; UPDATE registry.environments SET disposable=true WHERE org='reset' AND env='dev'").execute(&mut wamn_postgres::connect(system.as_str()).await.unwrap()).await?;
    reset_database(system.as_str(), &owner_input, &migration_input, &triple).await?;
    let after: sqlx::postgres::types::Oid =
        sqlx::query_scalar("SELECT oid FROM pg_database WHERE datname=current_database()")
            .fetch_one(&mut admin)
            .await?;
    assert_eq!(before, after);
    let retained: bool = sqlx::query_scalar("SELECT to_regclass('platform.\"user\"') IS NOT NULL AND EXISTS(SELECT FROM pg_publication_namespace WHERE pnpubid=(SELECT oid FROM pg_publication WHERE pubname='reset_publication'))").fetch_one(&mut admin).await?;
    assert!(retained);
    for model in &metadata.models {
        let absent: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NULL")
            .bind(format!(
                "{}.{}",
                postgres_protocol::escape::escape_identifier(&model.schema),
                postgres_protocol::escape::escape_identifier(&model.table)
            ))
            .fetch_one(&mut admin)
            .await?;
        assert!(absent);
    }
    let empty: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT FROM catalog.packages) AND NOT EXISTS(SELECT FROM catalog.workflows)").fetch_one(&mut admin).await?;
    assert!(empty);
    let claim_removed: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT FROM platform.write_log WHERE operation='fixture.create' AND idempotency_key='before-reset')")
        .fetch_one(&mut admin).await?;
    assert!(claim_removed);
    let outcome = apply_package::apply_package_schema(apply()).await?;
    assert!(outcome.migrations_applied > 0);
    let mut executor = wamn_postgres::connect(&migration_input).await?;
    migration::require_login(&mut executor, "reset--dev--fixture").await?;
    Ok(())
}

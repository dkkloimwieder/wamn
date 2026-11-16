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
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::ensure_app_acl_role_sql(),
    ))
    .execute(&mut admin)
    .await?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut admin)
        .await?;
    let component = wamn_control_provision::PlatformComponent::ApplyPackage;
    sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES(current_user,$1::text::uuid) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
        .bind(component.principal_id())
        .execute(&mut admin)
        .await?;
    let table = migration::history_table(
        &"fixture--dev--one".parse().unwrap(),
        &"orders".parse().unwrap(),
    );
    migration::prepare_history(&mut admin, &table, &"postgres".parse().unwrap()).await?;
    let first = source(
        10,
        "CREATE TABLE public.migration_effect(id int PRIMARY KEY, created_at timestamptz NOT NULL, created_by uuid NOT NULL, updated_at timestamptz NOT NULL, updated_by uuid NOT NULL);
        CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON public.migration_effect FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row('created_at','created_by','updated_at','updated_by');
        SELECT wamn_history.create_history_table('public','migration_effect',false);
        CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE ON public.migration_effect FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');",
    );
    let failing = source(
        30,
        "INSERT INTO public.migration_effect(id) VALUES (1); SELECT 1/0",
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
    let last = source(30, "INSERT INTO public.migration_effect(id) VALUES (1)");
    let sources = [
        first,
        last,
        source(40, "UPDATE public.migration_effect SET id=2 WHERE id=1"),
        source(
            50,
            "-- no-transaction\nUPDATE public.migration_effect SET id=1 WHERE id=2",
        ),
    ];
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
    let actors: bool =
        sqlx::query_scalar("SELECT bool_and(changed_by=$1) FROM public.migration_effect_history")
            .bind(component.principal_id())
            .fetch_one(&mut admin)
            .await?;
    assert!(actors);
    let principal: wamn_identity_protocol::PrincipalId =
        sqlx::query_scalar("SELECT updated_by FROM public.migration_effect WHERE id=1")
            .fetch_one(&mut admin)
            .await?;
    assert_eq!(principal, component.principal_id());
    sqlx::raw_sql(
        "GRANT INSERT ON public.migration_effect TO wamn_app; SET ROLE wamn_app;
        DO $$ BEGIN
            BEGIN
                INSERT INTO public.migration_effect(id) VALUES(99);
                RAISE EXCEPTION 'application write succeeded without its transaction identity';
            EXCEPTION WHEN object_not_in_prerequisite_state THEN NULL;
            END;
        END $$; RESET ROLE;",
    )
    .execute(&mut admin)
    .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM public.migration_effect")
        .fetch_one(&mut admin)
        .await?;
    assert_eq!(count, 1);
    assert_eq!(
        migrations::run(&mut admin, migrator(&sources), table.clone()).await?,
        0
    );
    let mut changed = sources.clone();
    changed[0] = source(10, "SELECT 1");
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
    let extra = source(60, "INSERT INTO public.migration_effect(id) VALUES (2)");
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
    nontransactional.push(source(60, "-- no-transaction\nCREATE INDEX CONCURRENTLY IF NOT EXISTS migration_effect_idx ON public.migration_effect(id)"));
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
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::ensure_app_acl_role_sql(),
    ))
    .execute(&mut admin)
    .await?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut admin)
        .await?;
    sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS platform")
        .execute(&mut admin)
        .await?;
    sqlx::raw_sql(include_str!("../../../deploy/sql/migration-privileges.sql"))
        .execute(&mut admin)
        .await?;
    let database_name = wamn_postgres::identity(database.as_str())?.1;
    let login = migration::login(&"fixture--dev--one".parse().unwrap(), &database_name)?;
    let role = postgres_protocol::escape::escape_identifier(login.as_str());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE ROLE {role} LOGIN PASSWORD 'migration-test' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; CREATE SCHEMA application AUTHORIZATION {role}; GRANT CONNECT ON DATABASE {} TO {role}", postgres_protocol::escape::escape_identifier(&database_name))))
        .execute(&mut admin).await?;
    let options = wamn_postgres::connect_options(database.as_str())
        .await?
        .username(login.as_str())
        .password("migration-test");
    let mut executor = wamn_postgres::connect(options).await?;
    migration::require_login(&mut executor, &"fixture--dev--one".parse().unwrap()).await?;
    assert!(
        migration::require_login(&mut admin, &"fixture--dev--one".parse().unwrap())
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
        &"mapping".parse().unwrap(),
        &"fixture".parse().unwrap(),
        &"dev".parse().unwrap(),
    )
    .await?;
    let migration_input = migration::configuration(
        &migration_input,
        &"mapping".parse().unwrap(),
        &"fixture".parse().unwrap(),
        &"dev".parse().unwrap(),
    )?;
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
                artifact_digest: Some(
                    (format!("sha256:{}", "a".repeat(64)))
                        .parse::<wamn_digest::Digest>()
                        .unwrap(),
                ),
            })
        };
    let request = |package| ApplyPackageRequest {
        component_source: None,
        package,
        changes: wamn_broker::Publisher::default(),
        database_url: database.as_str().to_owned(),
        migration_database_url: migration_input.clone(),
        org: "mapping".parse().unwrap(),
        project: "fixture".parse().unwrap(),
        environment: "dev".parse().unwrap(),
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

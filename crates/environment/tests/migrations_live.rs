//! PostgreSQL contracts of SQLx package migrations and the restricted login.

use sqlx::SqlSafeStr as _;
use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};
use wamn_environment::project_migration;
use wamn_postgres::migrations;

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

#[tokio::test]
#[ignore = "requires: docker"]
async fn sqlx_commits_files_with_their_history_and_refuses_checksum_drift() -> anyhow::Result<()> {
    use sqlx::migrate::Migrate as _;

    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let database = postgres.create_database("project").await?;
    let mut admin = wamn_postgres::connect(database.as_str()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_environment::sql::ensure_app_acl_role_sql(),
    ))
    .execute(&mut admin)
    .await?;
    sqlx::raw_sql(wamn_environment::project_setup::CATALOG_SCHEMA_SQL)
        .execute(&mut admin)
        .await?;
    let component = wamn_principal::PlatformComponent::ApplyPackage;
    sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES(current_user,$1::text::uuid) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
        .bind(uuid::Uuid::from(component.principal_id()))
        .execute(&mut admin)
        .await?;
    let table = project_migration::history_table(
        &"fixture--dev--one".parse().unwrap(),
        &"orders".parse().unwrap(),
    );
    migrations::prepare_history(&mut admin, &table, &"postgres".parse().unwrap()).await?;
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
    let applied = admin.list_applied_migrations(&table).await?;
    for expected in migrator(&sources).iter() {
        assert!(applied.iter().any(
            |actual| actual.version == expected.version && actual.checksum == expected.checksum
        ));
    }
    let stamps: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT bool_and(created_by=$1 AND updated_by=$1) FROM {table}"
    )))
    .bind(uuid::Uuid::from(component.principal_id()))
    .fetch_one(&mut admin)
    .await?;
    assert!(stamps);
    let actors: bool =
        sqlx::query_scalar("SELECT bool_and(changed_by=$1) FROM public.migration_effect_history")
            .bind(uuid::Uuid::from(component.principal_id()))
            .fetch_one(&mut admin)
            .await?;
    assert!(actors);
    let principal: uuid::Uuid =
        sqlx::query_scalar("SELECT updated_by FROM public.migration_effect WHERE id=1")
            .fetch_one(&mut admin)
            .await?;
    assert_eq!(principal, uuid::Uuid::from(component.principal_id()));
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

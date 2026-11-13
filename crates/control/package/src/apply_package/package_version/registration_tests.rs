use super::{installed_environment_fingerprint, load_applied_package};
use crate::apply_package::{ApplyPackageRequest, apply_package, apply_package_schema};
use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use std::path::Path;

const PROJECT_ENV_KEY: &str = "schema-records--dev--fixture";

fn request(root: &Path, database_url: &str) -> ApplyPackageRequest {
    ApplyPackageRequest {
        component_source: None,
        package: crate::package_artifact::fixture_package_root(root).unwrap(),
        changes: wamn_broker::Publisher::default(),
        database_url: database_url.into(),
        org: "schema-records".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    }
}

async fn state(client: &mut Client) -> wamn_schema_control::AppliedPackage {
    let mut tx = client.begin().await.unwrap();
    let state = load_applied_package(&mut tx, PROJECT_ENV_KEY, "wamn_inventory")
        .await
        .unwrap()
        .unwrap();
    tx.rollback().await.unwrap();
    state
}

#[tokio::test]
async fn explicit_sql_advances_across_versions_without_selecting_code() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    database.execute(&[
        include_str!("../../../../../../deploy/sql/app-schema-baseline.sql"),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ]).unwrap();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    crate::schema_baseline::record_project(&mut client)
        .await
        .unwrap();
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let old = scratch.path().join("old");
    let next = scratch.path().join("next");
    crate::test_support::copy_schema_fixture(&old);
    crate::test_support::copy_schema_fixture(&next);
    apply_package(request(&old, database.url())).await.unwrap();
    let baseline = state(&mut client).await;
    assert_eq!(
        baseline.baseline_fingerprint,
        baseline.physical_schema_fingerprint
    );
    let environment = installed_environment_fingerprint(&mut client, PROJECT_ENV_KEY)
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO inventory.dock (dock_code) VALUES ('retained')")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();

    // The cache remains byte-identical during schema work. It selects no code.
    let mut cache_tx = client.begin().await.unwrap();
    crate::apply_package::bind_apply_package_principal(&mut cache_tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO catalog.releases (project_env_key, manifest_digest, canonical_bytes) \
         SELECT $1, 'sha256:' || encode(sha256('{}'::bytea), 'hex'), '{}'::bytea",
    )
    .bind(&PROJECT_ENV_KEY)
    .execute(&mut *cache_tx)
    .await
    .map(|result| result.rows_affected())
    .unwrap();
    cache_tx.commit().await.unwrap();
    let cache_before: String = sqlx::query("SELECT xmin::text FROM catalog.releases")
        .fetch_one(&mut client)
        .await
        .unwrap()
        .get(0);
    let manifest_path = next.join("generated/wamn.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["package"]["version"] = "2.0.0".into();
    // No predecessor code version is needed to continue the applied SQL prefix.
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(
        !apply_package(request(&next, database.url()))
            .await
            .unwrap()
            .changed
    );
    assert_eq!(state(&mut client).await, baseline);

    assert!(
        !apply_package_schema(request(&next, database.url()), None)
            .await
            .unwrap()
            .changed
    );
    assert_eq!(state(&mut client).await, baseline);

    manifest["package"]["version"] = "3.0.0".into();
    manifest["models"]["dock"]["audit_log"]["retention"] = "unlimited".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(
        next.join("migrations/0002_dock_note.sql"),
        "ALTER TABLE inventory.dock ADD COLUMN note text; UPDATE inventory.dock SET dock_code = dock_code || '-migrated';",
    )
    .unwrap();
    let result = apply_package_schema(request(&next, database.url()), None)
        .await
        .unwrap();
    assert_eq!(result.migrations_applied, 1);
    let installed = state(&mut client).await;
    assert_eq!(
        installed.baseline_fingerprint,
        baseline.baseline_fingerprint
    );
    assert_ne!(
        installed.physical_schema_fingerprint,
        baseline.physical_schema_fingerprint
    );
    assert_eq!(installed.coordinate.package_version(), "3.0.0");
    assert_eq!(
        installed.artifact_digest,
        request(&next, database.url()).package.artifact_digest
    );
    assert_ne!(
        installed_environment_fingerprint(&mut client, PROJECT_ENV_KEY)
            .await
            .unwrap(),
        environment
    );
    assert!(
        sqlx::query("SELECT to_regclass('inventory.dock_history') IS NOT NULL")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<bool, _>(0)
    );
    assert_eq!(
        sqlx::query("SELECT dock_code FROM inventory.dock")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<String, _>(0),
        "retained-migrated"
    );
    assert!(
        sqlx::query(
            "SELECT dock.xmin = migration.xmin FROM inventory.dock AS dock \
         CROSS JOIN catalog.package_migrations AS migration \
         WHERE migration.relative_path = 'migrations/0002_dock_note.sql'"
        )
        .fetch_one(&mut client)
        .await
        .unwrap()
        .get::<bool, _>(0)
    );
    assert_eq!(
        sqlx::query("SELECT xmin::text FROM catalog.releases")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<String, _>(0),
        cache_before
    );
    let record_before: String = sqlx::query("SELECT xmin::text FROM catalog.packages")
        .fetch_one(&mut client)
        .await
        .unwrap()
        .get(0);
    assert!(
        !apply_package(request(&old, database.url()))
            .await
            .unwrap()
            .changed
    );
    assert_eq!(state(&mut client).await, installed);
    assert_eq!(
        sqlx::query("SELECT xmin::text FROM catalog.packages")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<String, _>(0),
        record_before
    );
    assert!(
        !apply_package_schema(request(&next, database.url()), None)
            .await
            .unwrap()
            .changed
    );
    assert_eq!(
        sqlx::query("SELECT dock_code FROM inventory.dock")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<String, _>(0),
        "retained-migrated"
    );
    // A retry must preserve the recorded paths, checksums and outcomes as well as the data.
    assert_eq!(state(&mut client).await, installed);
    let data_path = next.join("migrations/0002_dock_note.sql");
    let data_sql = std::fs::read_to_string(&data_path).unwrap();
    std::fs::write(
        &data_path,
        format!("{data_sql}\n-- changed committed file\n"),
    )
    .unwrap();
    assert!(
        apply_package_schema(request(&next, database.url()), None)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query("SELECT dock_code FROM inventory.dock")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<String, _>(0),
        "retained-migrated"
    );
    std::fs::write(data_path, data_sql).unwrap();

    // A failing statement rolls back its file and record together.
    manifest["package"]["version"] = "4.0.0".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(
        next.join("migrations/0003_candidate.sql"),
        "UPDATE inventory.dock SET dock_code = dock_code || '-failed'; ALTER TABLE inventory.dock ADD COLUMN failed text; ALTER TABLE inventory.dock ADD COLUMN failed text;",
    ).unwrap();
    assert!(
        apply_package_schema(request(&next, database.url()), None)
            .await
            .is_err()
    );
    assert_eq!(state(&mut client).await, installed);
    assert_eq!(
        sqlx::query("SELECT dock_code FROM inventory.dock")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<String, _>(0),
        "retained-migrated"
    );
    assert!(!sqlx::query("SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema='inventory' AND table_name='dock' AND column_name='failed')").fetch_one(&mut client).await.unwrap().get::<bool, _>(0));

    // Reconstruct the retained SQL source, even after older code was selected.
    std::fs::remove_file(next.join("migrations/0003_candidate.sql")).unwrap();
    manifest["package"]["version"] = "3.0.0".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let fresh = wamn_platform_identity::test_database::project_platform();
    fresh.execute(&["DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;"]).unwrap();
    apply_package(request(&next, fresh.url())).await.unwrap();
    let mut fresh_client = wamn_postgres::connect(fresh.url()).await.unwrap();

    let replacement = state(&mut fresh_client).await;
    assert_eq!(
        replacement.baseline_fingerprint,
        installed.physical_schema_fingerprint
    );
    assert_eq!(
        replacement.physical_schema_fingerprint,
        installed.physical_schema_fingerprint
    );
    assert_eq!(replacement.migrations, installed.migrations);
    assert_eq!(replacement.artifact_digest, installed.artifact_digest);
    assert_eq!(state(&mut client).await, installed);
}

#[tokio::test]
async fn nontransactional_files_keep_committed_work_and_require_explicit_resolution() {
    use crate::apply_package::{MigrationResolution, read_package_migrations};
    use wamn_schema_control::MigrationOutcome;

    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    database.execute(&[
        include_str!("../../../../../../deploy/sql/app-schema-baseline.sql"),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ]).unwrap();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();

    crate::schema_baseline::record_project(&mut client)
        .await
        .unwrap();
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let root = scratch.path().join("candidate");
    crate::test_support::copy_schema_fixture(&root);
    let manifest_path = root.join("generated/wamn.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    std::fs::write(root.join("migrations/0002_dock_index.sql"),
        "-- wamn:no-transaction\nCREATE INDEX CONCURRENTLY dock_code_idx ON inventory.dock (dock_code);").unwrap();
    std::fs::write(
        root.join("migrations/0003_note.sql"),
        "ALTER TABLE inventory.dock ADD COLUMN note text;",
    )
    .unwrap();
    std::fs::write(
        root.join("migrations/0004_failure.sql"),
        "ALTER TABLE inventory.dock ADD COLUMN note text;",
    )
    .unwrap();

    assert!(
        apply_package_schema(request(&root, database.url()), None)
            .await
            .is_err()
    );
    let committed = read_package_migrations(&mut client, PROJECT_ENV_KEY, "wamn_inventory")
        .await
        .unwrap();
    let directory = crate::apply_package::read_package_directory(&root).unwrap();
    let complete = wamn_schema_control::plan_package_migrations(&directory, None, &[]).unwrap();
    assert_eq!(
        committed
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect::<Vec<_>>(),
        complete
            .pending
            .iter()
            .take_while(|file| file.ordinal <= 3)
            .map(|file| file.relative_path.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        committed
            .iter()
            .all(|file| file.outcome == MigrationOutcome::Applied)
    );
    assert!(
        !sqlx::query("SELECT EXISTS (SELECT FROM catalog.packages)")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<bool, _>(0)
    );
    assert!(sqlx::query("SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema='inventory' AND table_name='dock' AND column_name='note')").fetch_one(&mut client).await.unwrap().get::<bool, _>(0));
    let first_index: i64 = sqlx::query("SELECT 'inventory.dock_code_idx'::regclass::oid::bigint")
        .fetch_one(&mut client)
        .await
        .unwrap()
        .get(0);
    std::fs::remove_file(root.join("migrations/0004_failure.sql")).unwrap();
    let result = apply_package_schema(request(&root, database.url()), None)
        .await
        .unwrap();
    assert_eq!(result.migrations_applied, 0);
    assert_eq!(
        sqlx::query("SELECT 'inventory.dock_code_idx'::regclass::oid::bigint")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<i64, _>(0),
        first_index
    );
    let installed = state(&mut client).await;
    assert!(
        !apply_package_schema(request(&root, database.url()), None)
            .await
            .unwrap()
            .changed
    );

    manifest["package"]["version"] = "2.0.0".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let uncertain_path = "migrations/0004_note_index.sql";
    let uncertain_sql =
        "-- wamn:no-transaction\nCREATE INDEX CONCURRENTLY dock_note_idx ON inventory.dock (note);";
    std::fs::write(root.join(uncertain_path), uncertain_sql).unwrap();
    // Fail the real completion write after PostgreSQL finishes the index.
    sqlx::raw_sql("CREATE FUNCTION public.fail_migration_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'completion unavailable'; END $$; CREATE TRIGGER fail_migration_completion BEFORE UPDATE ON catalog.package_migrations FOR EACH ROW EXECUTE FUNCTION public.fail_migration_completion()").execute(&mut client).await.map(drop).unwrap();
    assert!(
        apply_package_schema(request(&root, database.url()), None)
            .await
            .is_err()
    );
    sqlx::raw_sql("DROP TRIGGER fail_migration_completion ON catalog.package_migrations; DROP FUNCTION public.fail_migration_completion()").execute(&mut client).await.map(drop).unwrap();
    assert!(
        installed_environment_fingerprint(&mut client, PROJECT_ENV_KEY)
            .await
            .unwrap()
            .is_none()
    );
    let initialization: String =
        sqlx::query("SELECT installed_at::text FROM platform.schema_baseline WHERE singleton")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get(0);
    let source = request(&root, database.url()).package;
    assert!(
        crate::apply_package::adopt_package_schemas(
            database.url(),
            PROJECT_ENV_KEY,
            &initialization,
            &[&source],
            None
        )
        .await
        .is_err()
    );
    let uncertain = state(&mut client).await;
    assert_eq!(
        uncertain.physical_schema_fingerprint,
        installed.physical_schema_fingerprint
    );
    assert_eq!(
        uncertain.migrations.last().unwrap().outcome,
        MigrationOutcome::Started
    );
    let row = sqlx::query("SELECT 'inventory.dock_note_idx'::regclass::oid::bigint, applied_at IS NULL FROM catalog.package_migrations WHERE relative_path=$1").bind(&uncertain_path).fetch_one(&mut client).await.unwrap();
    let completed_index: i64 = row.get(0);
    assert!(row.get::<bool, _>(1));
    assert!(
        apply_package_schema(request(&root, database.url()), None)
            .await
            .is_err()
    );
    assert_eq!(state(&mut client).await, uncertain);
    std::fs::write(root.join(uncertain_path), format!("{uncertain_sql}\n")).unwrap();
    assert!(
        apply_package_schema(
            request(&root, database.url()),
            Some(
                &format!("wamn_inventory:{uncertain_path}=applied")
                    .parse()
                    .unwrap()
            )
        )
        .await
        .is_err()
    );
    std::fs::write(root.join(uncertain_path), uncertain_sql).unwrap();
    let applied: MigrationResolution = format!("wamn_inventory:{uncertain_path}=applied")
        .parse()
        .unwrap();
    let result = apply_package_schema(request(&root, database.url()), Some(&applied))
        .await
        .unwrap();
    assert_eq!(result.migrations_applied, 0);
    assert_eq!(
        sqlx::query("SELECT 'inventory.dock_note_idx'::regclass::oid::bigint")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<i64, _>(0),
        completed_index
    );
    assert_eq!(
        state(&mut client).await.migrations.last().unwrap().outcome,
        MigrationOutcome::Applied
    );
    assert!(
        !apply_package_schema(request(&root, database.url()), Some(&applied))
            .await
            .unwrap()
            .changed
    );

    manifest["package"]["version"] = "3.0.0".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let retry_path = "migrations/0005_repaired_index.sql";
    std::fs::write(root.join(retry_path), "-- wamn:no-transaction\nCREATE INDEX CONCURRENTLY dock_repaired_idx ON inventory.dock (repaired);").unwrap();
    assert!(
        apply_package_schema(request(&root, database.url()), None)
            .await
            .is_err()
    );
    assert_eq!(
        state(&mut client).await.migrations.last().unwrap().outcome,
        MigrationOutcome::Started
    );
    // The developer repairs the database, then explicitly permits another attempt.
    sqlx::raw_sql("ALTER TABLE inventory.dock ADD COLUMN repaired text")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    assert!(
        apply_package_schema(request(&root, database.url()), None)
            .await
            .is_err()
    );
    let retry: MigrationResolution = format!("wamn_inventory:{retry_path}=retry")
        .parse()
        .unwrap();
    assert_eq!(
        apply_package_schema(request(&root, database.url()), Some(&retry))
            .await
            .unwrap()
            .migrations_applied,
        1
    );
    assert!(sqlx::query("SELECT indisvalid FROM pg_index WHERE indexrelid='inventory.dock_repaired_idx'::regclass").fetch_one(&mut client).await.unwrap().get::<bool, _>(0));
    assert!(
        !apply_package_schema(request(&root, database.url()), Some(&retry))
            .await
            .unwrap()
            .changed
    );
}

#[tokio::test]
async fn adoption_preserves_execution_records_and_never_executes_asserted_files() {
    use crate::apply_package::adopt_package_schemas;
    use wamn_schema_control::MigrationOutcome;
    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    database.execute(&[
        include_str!("../../../../../../deploy/sql/app-schema-baseline.sql"),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ]).unwrap();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();
    crate::schema_baseline::record_project(&mut client)
        .await
        .unwrap();
    let initialization: String =
        sqlx::query("SELECT installed_at::text FROM platform.schema_baseline WHERE singleton")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get(0);
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let root = scratch.path().join("application");
    crate::test_support::copy_schema_fixture(&root);
    apply_package(request(&root, database.url())).await.unwrap();
    sqlx::raw_sql("INSERT INTO inventory.dock (dock_code) VALUES ('retained')")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let baseline = state(&mut client).await;
    let manifest = wamn_compiler::manifest::PackageManifest::from_slice(
        &std::fs::read(root.join("generated/wamn.json")).unwrap(),
    )
    .unwrap();
    let mut tx = client.begin().await.unwrap();
    let mut columns = std::collections::BTreeMap::new();
    for (id, model) in &manifest.models {
        columns.insert(
            id.to_owned(),
            crate::apply_package::record_history::read_relation_columns(
                &mut tx,
                &model.schema,
                &model.table,
            )
            .await
            .unwrap(),
        );
    }
    tx.rollback().await.unwrap();
    let source = || {
        let directory = crate::apply_package::read_package_directory(&root).unwrap();
        let manifest =
            wamn_compiler::manifest::PackageManifest::from_slice(&directory.manifest_bytes)
                .unwrap();
        let statements = wamn_schema_control::physical_schema::render(
            &(&manifest).into(),
            &directory.migrations,
            &columns,
        )
        .unwrap();
        std::fs::write(
            root.join("generated/physical-schema.json"),
            serde_json::to_vec(&serde_json::json!({"columns": columns, "statements": statements}))
                .unwrap(),
        )
        .unwrap();
        request(&root, database.url()).package
    };
    let selected = source();
    let matched = adopt_package_schemas(
        database.url(),
        PROJECT_ENV_KEY,
        &initialization,
        &[&selected],
        None,
    )
    .await
    .unwrap();
    assert!(matched.values().all(Vec::is_empty));
    assert_eq!(state(&mut client).await, baseline);

    // The user accepts a pending file as a starting point. Adoption records
    // that assertion without executing its DDL.
    std::fs::write(
        root.join("migrations/0002_asserted.sql"),
        "ALTER TABLE inventory.dock ADD COLUMN asserted text;",
    )
    .unwrap();
    let selected = source();
    adopt_package_schemas(
        database.url(),
        PROJECT_ENV_KEY,
        &initialization,
        &[&selected],
        None,
    )
    .await
    .unwrap();
    let adopted = state(&mut client).await;
    assert_eq!(adopted.baseline_fingerprint, baseline.baseline_fingerprint);
    assert_eq!(
        adopted.migrations[..baseline.migrations.len()],
        baseline.migrations
    );
    assert_eq!(
        adopted.migrations.last().unwrap().outcome,
        MigrationOutcome::Adopted
    );
    assert!(
        !sqlx::query("SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema='inventory' AND table_name='dock' AND column_name='asserted')").fetch_one(&mut client)
            .await
            .unwrap()
            .get::<bool, _>(0)
    );
    assert!(
        sqlx::query(
            "SELECT applied_at IS NULL FROM catalog.package_migrations WHERE outcome='adopted'"
        )
        .fetch_one(&mut client)
        .await
        .unwrap()
        .get::<bool, _>(0)
    );

    let mut next_manifest = manifest.clone();
    next_manifest.package.version = "2.0.0".into();
    std::fs::write(
        root.join("generated/wamn.json"),
        serde_json::to_vec(&next_manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("migrations/0003_next.sql"),
        "ALTER TABLE inventory.dock ADD COLUMN next text;",
    )
    .unwrap();
    assert_eq!(
        apply_package_schema(request(&root, database.url()), None)
            .await
            .unwrap()
            .migrations_applied,
        1
    );
    assert!(
        !apply_package_schema(request(&root, database.url()), None)
            .await
            .unwrap()
            .changed
    );
    let executed = state(&mut client).await;
    assert_eq!(
        executed.migrations.last().unwrap().outcome,
        MigrationOutcome::Applied
    );
    assert!(
        sqlx::query("SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema='inventory' AND table_name='dock' AND column_name='next')").fetch_one(&mut client)
            .await
            .unwrap()
            .get::<bool, _>(0)
    );

    // Manual repair changes an accepted input. Ordinary apply refuses its drift.
    let initial = root.join(&baseline.migrations[0].relative_path);
    let mut sql = std::fs::read(initial.clone()).unwrap();
    sql.extend_from_slice(b"\n-- accepted manual repair\n");
    std::fs::write(initial, sql).unwrap();
    assert!(
        apply_package_schema(request(&root, database.url()), None)
            .await
            .is_err()
    );
    assert_eq!(state(&mut client).await, executed);
    let selected = source();
    // The same database name cannot authorize an assertion after recreation.
    assert!(
        adopt_package_schemas(
            database.url(),
            PROJECT_ENV_KEY,
            "old-initialization",
            &[&selected],
            None
        )
        .await
        .is_err()
    );
    assert_eq!(state(&mut client).await, executed);
    adopt_package_schemas(
        database.url(),
        PROJECT_ENV_KEY,
        &initialization,
        &[&selected],
        None,
    )
    .await
    .unwrap();
    let repaired = state(&mut client).await;
    assert_eq!(repaired.migrations[0].outcome, MigrationOutcome::Adopted);
    assert_eq!(repaired.migrations[1..], executed.migrations[1..]);
    assert!(
        adopt_package_schemas(
            database.url(),
            PROJECT_ENV_KEY,
            &initialization,
            &[&selected],
            None
        )
        .await
        .unwrap()
        .values()
        .all(Vec::is_empty)
    );
    assert_eq!(state(&mut client).await, repaired);
    assert!(
        sqlx::query("SELECT EXISTS (SELECT FROM inventory.dock WHERE dock_code='retained')")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<bool, _>(0)
    );
    next_manifest.package.version = "3.0.0".into();
    std::fs::write(
        root.join("generated/wamn.json"),
        serde_json::to_vec(&next_manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(root.join("migrations/0004_uncertain.sql"),
        "-- wamn:no-transaction\nCREATE INDEX CONCURRENTLY uncertain_idx ON inventory.dock (absent);").unwrap();
    assert!(
        apply_package_schema(request(&root, database.url()), None)
            .await
            .is_err()
    );
    let uncertain = state(&mut client).await;
    assert_eq!(
        uncertain.migrations.last().unwrap().outcome,
        MigrationOutcome::Started
    );
    let selected = source();
    assert!(
        adopt_package_schemas(
            database.url(),
            PROJECT_ENV_KEY,
            &initialization,
            &[&selected],
            None
        )
        .await
        .is_err()
    );
    assert_eq!(state(&mut client).await, uncertain);
}

#[tokio::test]
async fn adoption_establishes_a_missing_baseline_without_running_ddl() {
    use crate::apply_package::adopt_package_schemas;
    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    database
        .execute(&[include_str!(
            "../../../../../../deploy/sql/app-schema-baseline.sql"
        )])
        .unwrap();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();
    crate::schema_baseline::record_project(&mut client)
        .await
        .unwrap();
    let initialization: String =
        sqlx::query("SELECT installed_at::text FROM platform.schema_baseline WHERE singleton")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get(0);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/component_package");
    let source = crate::package_artifact::fixture_package_root(&root).unwrap();
    let result = adopt_package_schemas(
        database.url(),
        PROJECT_ENV_KEY,
        &initialization,
        &[&source],
        None,
    )
    .await
    .unwrap();
    let mut tx = client.begin().await.unwrap();
    let adopted = load_applied_package(&mut tx, PROJECT_ENV_KEY, "source_fixture")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        adopted.baseline_fingerprint,
        adopted.physical_schema_fingerprint
    );
    assert!(
        adopted
            .migrations
            .iter()
            .all(|file| file.outcome == wamn_schema_control::MigrationOutcome::Adopted)
    );
    assert_eq!(
        result["source_fixture"],
        adopted
            .migrations
            .iter()
            .map(|file| file.relative_path.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        sqlx::query("SELECT to_regclass('inventory.item') IS NULL")
            .fetch_one(&mut *tx)
            .await
            .unwrap()
            .get::<bool, _>(0)
    );
}

#[tokio::test]
async fn resolution_selects_one_package_with_a_shared_migration_path() {
    use crate::apply_package::{read_package_migrations, resolution_package};
    use wamn_schema_control::MigrationOutcome;
    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    database.execute(&[
        include_str!("../../../../../../deploy/sql/app-schema-baseline.sql"),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ]).unwrap();
    let mut client = wamn_postgres::connect(database.url()).await.unwrap();
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let path = "migrations/0002_index.sql";
    for name in ["alpha", "beta"] {
        let root = scratch.path().join(name);
        crate::test_support::copy_schema_fixture(&root);
        for file in ["generated/wamn.json", "migrations/0001_initial.sql"] {
            let bytes = std::fs::read_to_string(root.join(file))
                .unwrap()
                .replace("inventory", name);
            std::fs::write(root.join(file), bytes).unwrap();
        }
        std::fs::write(root.join(path), format!("-- wamn:no-transaction\nCREATE INDEX CONCURRENTLY missing_idx ON {name}.dock (missing);")).unwrap();
        assert!(
            apply_package_schema(request(&root, database.url()), None)
                .await
                .is_err()
        );
    }
    let alpha = read_package_migrations(&mut client, PROJECT_ENV_KEY, "wamn_alpha")
        .await
        .unwrap();
    let beta = read_package_migrations(&mut client, PROJECT_ENV_KEY, "wamn_beta")
        .await
        .unwrap();
    assert_eq!(alpha.last().unwrap().outcome, MigrationOutcome::Started);
    assert_eq!(beta.last().unwrap().outcome, MigrationOutcome::Started);
    let resolution = format!("wamn_beta:{path}=retry").parse().unwrap();
    let selected = resolution_package(
        Some(&resolution),
        [
            ("wamn_alpha", alpha.as_slice()),
            ("wamn_beta", beta.as_slice()),
        ],
    )
    .unwrap();
    assert_eq!(selected, Some("wamn_beta"));
    assert!(
        apply_package_schema(
            request(&scratch.path().join("alpha"), database.url()),
            Some(&resolution)
        )
        .await
        .is_err()
    );
    sqlx::raw_sql("ALTER TABLE beta.dock ADD COLUMN missing text")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    assert_eq!(
        apply_package_schema(
            request(&scratch.path().join("beta"), database.url()),
            Some(&resolution)
        )
        .await
        .unwrap()
        .migrations_applied,
        1
    );
    assert_eq!(
        read_package_migrations(&mut client, PROJECT_ENV_KEY, "wamn_alpha")
            .await
            .unwrap(),
        alpha
    );
    let after = read_package_migrations(&mut client, PROJECT_ENV_KEY, "wamn_beta")
        .await
        .unwrap();
    assert_eq!(after.last().unwrap().outcome, MigrationOutcome::Applied);
    assert_eq!(after.last().unwrap().sha256, beta.last().unwrap().sha256);
}

#[tokio::test]
async fn lost_commit_response_retries_without_repeating_sql() -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::{Duration, timeout};

    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    database.execute(&[
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ])?;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let root = scratch.path().join("lost-result");
    crate::test_support::copy_schema_fixture(&root);
    apply_package(request(&root, database.url())).await?;
    let manifest = root.join("generated/wamn.json");
    std::fs::write(
        &manifest,
        std::fs::read_to_string(&manifest)?.replace("1.0.0", "2.0.0"),
    )?;
    let mut client = wamn_postgres::connect(database.url()).await?;
    sqlx::raw_sql("INSERT INTO inventory.dock (dock_code) VALUES ('retained')")
        .execute(&mut client)
        .await
        .map(drop)?;
    let path = "migrations/0002_note.sql";
    std::fs::write(
        root.join(path),
        "ALTER TABLE inventory.dock ADD COLUMN note text;",
    )?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut proxy_url = url::Url::parse(database.url())?;
    let endpoint = proxy_url.socket_addrs(|| None)?[0];
    proxy_url
        .set_port(Some(listener.local_addr()?.port()))
        .unwrap();
    let proxy = tokio::spawn(async move {
        let (mut caller, _) = listener.accept().await?;
        let mut server = TcpStream::connect(endpoint).await?;
        let (mut caller_read, mut caller_write) = caller.split();
        let (mut server_read, mut server_write) = server.split();
        let forward = tokio::io::copy(&mut caller_read, &mut server_write);
        let lose_commit = async {
            loop {
                // PostgreSQL sends a tag and a length before each response body.
                let tag = server_read.read_u8().await?;
                let length = server_read.read_u32().await?;
                let mut body = vec![0; usize::try_from(length - 4)?];
                server_read.read_exact(&mut body).await?;
                if tag == b'C' && body == b"COMMIT\0" {
                    return Ok::<_, anyhow::Error>(());
                }
                caller_write.write_u8(tag).await?;
                caller_write.write_u32(length).await?;
                caller_write.write_all(&body).await?;
            }
        };
        tokio::select! {
            result = lose_commit => result,
            result = forward => anyhow::bail!("caller closed before COMMIT: {result:?}"),
        }
    });
    timeout(
        Duration::from_secs(15),
        apply_package_schema(request(&root, proxy_url.as_str()), None),
    )
    .await?
    .expect_err("the proxy discards the committed response");
    timeout(Duration::from_secs(2), proxy).await???;
    let before = sqlx::query("SELECT sha256, xmin::text FROM catalog.package_migrations WHERE project_env_key=$1 AND relative_path=$2").bind(&PROJECT_ENV_KEY).bind(&path).fetch_one(&mut client)
        .await?;
    let result = apply_package_schema(request(&root, database.url()), None).await?;
    assert_eq!(result.migrations_applied, 0);
    let after = sqlx::query("SELECT sha256, xmin::text FROM catalog.package_migrations WHERE project_env_key=$1 AND relative_path=$2").bind(&PROJECT_ENV_KEY).bind(&path).fetch_one(&mut client)
        .await?;
    assert_eq!(before.get::<String, _>(0), after.get::<String, _>(0));
    assert_eq!(before.get::<String, _>(1), after.get::<String, _>(1));
    assert_eq!(
        sqlx::query("SELECT dock_code FROM inventory.dock WHERE note IS NULL")
            .fetch_one(&mut client)
            .await?
            .get::<String, _>(0),
        "retained"
    );
    Ok(())
}

#[tokio::test]
async fn resolution_waits_for_a_running_nontransactional_statement() -> anyhow::Result<()> {
    use crate::apply_package::read_package_migrations;
    use tokio::time::{Duration, timeout};
    use wamn_schema_control::MigrationOutcome;

    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    database.execute(&[
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ])?;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let root = scratch.path().join("running-sql");
    crate::test_support::copy_schema_fixture(&root);
    apply_package(request(&root, database.url())).await?;
    let manifest = root.join("generated/wamn.json");
    std::fs::write(
        &manifest,
        std::fs::read_to_string(&manifest)?.replace("1.0.0", "2.0.0"),
    )?;
    let path = "migrations/0002_index.sql";
    std::fs::write(
        root.join(path),
        "-- wamn:no-transaction\nCREATE INDEX CONCURRENTLY dock_code_idx ON inventory.dock (dock_code);",
    )?;
    let mut observer = wamn_postgres::connect(database.url()).await?;
    let mut blocker_owner = wamn_postgres::connect(database.url()).await?;
    let mut blocker = blocker_owner.begin().await?;
    sqlx::raw_sql("LOCK TABLE inventory.dock IN SHARE MODE")
        .execute(&mut *blocker)
        .await
        .map(drop)?;
    let mut worker_url = url::Url::parse(database.url())?;
    worker_url
        .query_pairs_mut()
        .append_pair("application_name", "running-schema-worker");
    let work = request(&root, worker_url.as_str());
    let worker = tokio::spawn(async move { apply_package_schema(work, None).await });
    timeout(Duration::from_secs(15), async {
        loop {
            assert!(!worker.is_finished());
            if sqlx::query("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE application_name='running-schema-worker' AND wait_event_type='Lock')").fetch_one(&mut observer).await?.get::<bool, _>(0) {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    let started = read_package_migrations(&mut observer, PROJECT_ENV_KEY, "wamn_inventory").await?;
    assert_eq!(started.last().unwrap().outcome, MigrationOutcome::Started);
    worker_url
        .query_pairs_mut()
        .clear()
        .append_pair("application_name", "schema-resolution-worker");
    let work = request(&root, worker_url.as_str());
    let choice = format!("wamn_inventory:{path}=applied").parse()?;
    let mut resolution =
        tokio::spawn(async move { apply_package_schema(work, Some(&choice)).await });
    // Keep the writer blocked while the resolver attempts acquisition and backs off.
    assert!(
        timeout(Duration::from_millis(300), &mut resolution)
            .await
            .is_err()
    );
    timeout(Duration::from_secs(15), async {
        loop {
            assert!(!resolution.is_finished());
            if sqlx::query("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE application_name='schema-resolution-worker' AND state='idle' AND backend_xmin IS NULL AND backend_xid IS NULL)").fetch_one(&mut observer).await?.get::<bool, _>(0) {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    assert_eq!(
        read_package_migrations(&mut observer, PROJECT_ENV_KEY, "wamn_inventory").await?,
        started
    );
    blocker.commit().await?;
    assert_eq!(
        timeout(Duration::from_secs(15), worker)
            .await???
            .migrations_applied,
        1
    );
    assert_eq!(
        timeout(Duration::from_secs(15), resolution)
            .await???
            .migrations_applied,
        0
    );
    let completed =
        read_package_migrations(&mut observer, PROJECT_ENV_KEY, "wamn_inventory").await?;
    assert_eq!(
        completed.last().unwrap().sha256,
        started.last().unwrap().sha256
    );
    assert_eq!(completed.last().unwrap().outcome, MigrationOutcome::Applied);
    assert!(
        sqlx::query(
            "SELECT indisvalid FROM pg_index WHERE indexrelid='inventory.dock_code_idx'::regclass"
        )
        .fetch_one(&mut observer)
        .await?
        .get::<bool, _>(0)
    );
    Ok(())
}

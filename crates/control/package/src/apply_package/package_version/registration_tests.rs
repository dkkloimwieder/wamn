use super::{installed_environment_fingerprint, load_applied_package};
use crate::apply_package::{ApplyPackageRequest, apply_package, apply_package_schema};
use std::path::Path;
use tokio_postgres::{Client, NoTls};

const ENV_KEY: &str = "schema-records--fixture--dev";

fn request(root: &Path, database_url: &str) -> ApplyPackageRequest {
    ApplyPackageRequest {
        component_source: None,
        package: crate::package_artifact::fixture_package_root(root).unwrap(),
        changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        database_url: database_url.into(),
        org: "schema-records".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    }
}

async fn state(client: &mut Client) -> wamn_schema_control::AppliedPackage {
    let tx = client.transaction().await.unwrap();
    let state = load_applied_package(&tx, ENV_KEY, "wamn_inventory")
        .await
        .unwrap()
        .unwrap();
    tx.rollback().await.unwrap();
    state
}

#[tokio::test]
async fn explicit_sql_advances_across_versions_without_selecting_code() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
    database.execute(&[
        include_str!("../../../../../../deploy/sql/app-schema-baseline.sql"),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ]).unwrap();
    let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    crate::schema_baseline::record_project(&client)
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
    let environment = installed_environment_fingerprint(&client, ENV_KEY)
        .await
        .unwrap();
    client
        .batch_execute("INSERT INTO inventory.dock (dock_code) VALUES ('retained')")
        .await
        .unwrap();

    // The cache remains byte-identical during schema work. It selects no code.
    let cache_tx = client.transaction().await.unwrap();
    crate::apply_package::bind_apply_package_principal(&cache_tx)
        .await
        .unwrap();
    cache_tx
        .execute(
            "INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) \
         SELECT $1, 'sha256:' || encode(sha256('{}'::bytea), 'hex'), '{}'::bytea",
            &[&ENV_KEY],
        )
        .await
        .unwrap();
    cache_tx.commit().await.unwrap();
    let cache_before: String = client
        .query_one("SELECT xmin::text FROM catalog.releases", &[])
        .await
        .unwrap()
        .get(0);
    let manifest_path = next.join("wamn.json");
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
        "ALTER TABLE inventory.dock ADD COLUMN note text;",
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
        installed_environment_fingerprint(&client, ENV_KEY)
            .await
            .unwrap(),
        environment
    );
    assert!(
        client
            .query_one(
                "SELECT to_regclass('inventory.dock_history') IS NOT NULL",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(
        client
            .query_one("SELECT dock_code FROM inventory.dock", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "retained"
    );
    assert_eq!(
        client
            .query_one("SELECT xmin::text FROM catalog.releases", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        cache_before
    );
    let record_before: String = client
        .query_one("SELECT xmin::text FROM catalog.packages", &[])
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
        client
            .query_one("SELECT xmin::text FROM catalog.packages", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        record_before
    );
    assert!(
        !apply_package_schema(request(&next, database.url()), None)
            .await
            .unwrap()
            .changed
    );

    // A later failing statement rolls back DDL, checksums, and current state.
    manifest["package"]["version"] = "4.0.0".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(
        next.join("migrations/0003_candidate.sql"),
        "ALTER TABLE inventory.dock ADD COLUMN failed text;",
    )
    .unwrap();
    std::fs::write(
        next.join("migrations/0004_failure.sql"),
        "ALTER TABLE inventory.dock ADD COLUMN failed text;",
    )
    .unwrap();
    assert!(
        apply_package_schema(request(&next, database.url()), None)
            .await
            .is_err()
    );
    assert_eq!(state(&mut client).await, installed);
    assert!(!client.query_one("SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema='inventory' AND table_name='dock' AND column_name='failed')", &[]).await.unwrap().get::<_, bool>(0));

    // Reconstruct the retained SQL source, even after older code was selected.
    std::fs::remove_file(next.join("migrations/0003_candidate.sql")).unwrap();
    std::fs::remove_file(next.join("migrations/0004_failure.sql")).unwrap();
    manifest["package"]["version"] = "3.0.0".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let fresh = wamn_project_state::test_database::tenant_platform();
    fresh.execute(&["DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;"]).unwrap();
    apply_package(request(&next, fresh.url())).await.unwrap();
    let (mut fresh_client, fresh_connection) =
        tokio_postgres::connect(fresh.url(), NoTls).await.unwrap();
    tokio::spawn(fresh_connection);
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
    let database = wamn_project_state::test_database::tenant_platform();
    database.execute(&[
        include_str!("../../../../../../deploy/sql/app-schema-baseline.sql"),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ]).unwrap();
    let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let root = scratch.path().join("candidate");
    crate::test_support::copy_schema_fixture(&root);
    let manifest_path = root.join("wamn.json");
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
    let committed = read_package_migrations(&client, ENV_KEY, "wamn_inventory")
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
            .take_while(|file| file.ordinal <= 2)
            .map(|file| file.relative_path.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        committed
            .iter()
            .all(|file| file.outcome == MigrationOutcome::Applied)
    );
    assert!(
        !client
            .query_one("SELECT EXISTS (SELECT FROM catalog.packages)", &[])
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert!(!client.query_one("SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema='inventory' AND table_name='dock' AND column_name='note')", &[]).await.unwrap().get::<_, bool>(0));
    let first_index: u32 = client
        .query_one("SELECT 'inventory.dock_code_idx'::regclass::oid", &[])
        .await
        .unwrap()
        .get(0);
    std::fs::remove_file(root.join("migrations/0004_failure.sql")).unwrap();
    let result = apply_package_schema(request(&root, database.url()), None)
        .await
        .unwrap();
    assert_eq!(result.migrations_applied, 1);
    assert_eq!(
        client
            .query_one("SELECT 'inventory.dock_code_idx'::regclass::oid", &[])
            .await
            .unwrap()
            .get::<_, u32>(0),
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
    client.batch_execute("CREATE FUNCTION public.fail_migration_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'completion unavailable'; END $$; CREATE TRIGGER fail_migration_completion BEFORE UPDATE ON catalog.package_migrations FOR EACH ROW EXECUTE FUNCTION public.fail_migration_completion()").await.unwrap();
    assert!(
        apply_package_schema(request(&root, database.url()), None)
            .await
            .is_err()
    );
    client.batch_execute("DROP TRIGGER fail_migration_completion ON catalog.package_migrations; DROP FUNCTION public.fail_migration_completion()").await.unwrap();
    let uncertain = state(&mut client).await;
    assert_eq!(
        uncertain.physical_schema_fingerprint,
        installed.physical_schema_fingerprint
    );
    assert_eq!(
        uncertain.migrations.last().unwrap().outcome,
        MigrationOutcome::Started
    );
    let row = client.query_one("SELECT 'inventory.dock_note_idx'::regclass::oid, applied_at IS NULL FROM catalog.package_migrations WHERE relative_path=$1", &[&uncertain_path]).await.unwrap();
    let completed_index: u32 = row.get(0);
    assert!(row.get::<_, bool>(1));
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
            Some(&format!("{uncertain_path}=applied").parse().unwrap())
        )
        .await
        .is_err()
    );
    std::fs::write(root.join(uncertain_path), uncertain_sql).unwrap();
    let applied: MigrationResolution = format!("{uncertain_path}=applied").parse().unwrap();
    let result = apply_package_schema(request(&root, database.url()), Some(&applied))
        .await
        .unwrap();
    assert_eq!(result.migrations_applied, 0);
    assert_eq!(
        client
            .query_one("SELECT 'inventory.dock_note_idx'::regclass::oid", &[])
            .await
            .unwrap()
            .get::<_, u32>(0),
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
    client
        .batch_execute("ALTER TABLE inventory.dock ADD COLUMN repaired text")
        .await
        .unwrap();
    assert!(
        apply_package_schema(request(&root, database.url()), None)
            .await
            .is_err()
    );
    let retry: MigrationResolution = format!("{retry_path}=retry").parse().unwrap();
    assert_eq!(
        apply_package_schema(request(&root, database.url()), Some(&retry))
            .await
            .unwrap()
            .migrations_applied,
        1
    );
    assert!(client.query_one("SELECT indisvalid FROM pg_index WHERE indexrelid='inventory.dock_repaired_idx'::regclass", &[]).await.unwrap().get::<_, bool>(0));
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
    let database = wamn_project_state::test_database::tenant_platform();
    database.execute(&[
        include_str!("../../../../../../deploy/sql/app-schema-baseline.sql"),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ]).unwrap();
    let mut client = crate::database::connect(database.url()).await.unwrap();
    crate::schema_baseline::record_project(&client)
        .await
        .unwrap();
    let initialization: String = client
        .query_one(
            "SELECT installed_at::text FROM platform.schema_baseline WHERE singleton",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let root = scratch.path().join("application");
    crate::test_support::copy_schema_fixture(&root);
    apply_package(request(&root, database.url())).await.unwrap();
    client
        .batch_execute("INSERT INTO inventory.dock (dock_code) VALUES ('retained')")
        .await
        .unwrap();
    let baseline = state(&mut client).await;
    let manifest = wamn_catalog::manifest::PackageManifest::from_slice(
        &std::fs::read(root.join("wamn.json")).unwrap(),
    )
    .unwrap();
    let tx = client.transaction().await.unwrap();
    let mut columns = std::collections::BTreeMap::new();
    for (id, model) in &manifest.models {
        columns.insert(
            id.to_owned(),
            crate::apply_package::record_history::read_relation_columns(
                &tx,
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
            wamn_catalog::manifest::PackageManifest::from_slice(&directory.manifest_bytes).unwrap();
        let statements = wamn_schema_control::physical_schema::render(
            &manifest,
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
    let matched =
        adopt_package_schemas(database.url(), ENV_KEY, &initialization, &[&selected], None)
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
    adopt_package_schemas(database.url(), ENV_KEY, &initialization, &[&selected], None)
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
        !client
            .query_one(
                "SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema='inventory' AND table_name='dock' AND column_name='asserted')",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert!(
        client
            .query_one(
                "SELECT applied_at IS NULL FROM catalog.package_migrations WHERE outcome='adopted'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );

    let mut next_manifest = manifest.clone();
    next_manifest.package.version = "2.0.0".into();
    std::fs::write(
        root.join("wamn.json"),
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
        client
            .query_one(
                "SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema='inventory' AND table_name='dock' AND column_name='next')",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
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
            ENV_KEY,
            "old-initialization",
            &[&selected],
            None
        )
        .await
        .is_err()
    );
    assert_eq!(state(&mut client).await, executed);
    adopt_package_schemas(database.url(), ENV_KEY, &initialization, &[&selected], None)
        .await
        .unwrap();
    let repaired = state(&mut client).await;
    assert_eq!(repaired.migrations[0].outcome, MigrationOutcome::Adopted);
    assert_eq!(repaired.migrations[1..], executed.migrations[1..]);
    assert!(
        adopt_package_schemas(database.url(), ENV_KEY, &initialization, &[&selected], None)
            .await
            .unwrap()
            .values()
            .all(Vec::is_empty)
    );
    assert_eq!(state(&mut client).await, repaired);
    assert!(
        client
            .query_one(
                "SELECT EXISTS (SELECT FROM inventory.dock WHERE dock_code='retained')",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    next_manifest.package.version = "3.0.0".into();
    std::fs::write(
        root.join("wamn.json"),
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
        adopt_package_schemas(database.url(), ENV_KEY, &initialization, &[&selected], None)
            .await
            .is_err()
    );
    assert_eq!(state(&mut client).await, uncertain);
}

#[tokio::test]
async fn adoption_establishes_a_missing_baseline_without_running_ddl() {
    use crate::apply_package::adopt_package_schemas;
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
    database
        .execute(&[include_str!(
            "../../../../../../deploy/sql/app-schema-baseline.sql"
        )])
        .unwrap();
    let mut client = crate::database::connect(database.url()).await.unwrap();
    crate::schema_baseline::record_project(&client)
        .await
        .unwrap();
    let initialization: String = client
        .query_one(
            "SELECT installed_at::text FROM platform.schema_baseline WHERE singleton",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/component_package");
    let source = crate::package_artifact::fixture_package_root(&root).unwrap();
    let result = adopt_package_schemas(database.url(), ENV_KEY, &initialization, &[&source], None)
        .await
        .unwrap();
    let tx = client.transaction().await.unwrap();
    let adopted = load_applied_package(&tx, ENV_KEY, "source_fixture")
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
        tx.query_one("SELECT to_regclass('inventory.item') IS NULL", &[])
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
}

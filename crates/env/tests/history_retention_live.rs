//! Retention contracts through the owning library interface.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wamn_env::apply_package::{self, ApplyOutcome, ApplyPackageRequest};
use wamn_release::package_source::PackageRoot;

const SCHEMA: &str = "record_retention";
const PACKAGE_ID: &str = "record_retention_fixture";
/// The test principal that the fixture writes bind.
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f2";

/// The fixture relations and the retention of each.
const RELATIONS: [(&str, &str); 3] = [
    ("shipment", "P30D"),
    ("parcel", "P7D"),
    ("ledger", "unlimited"),
];

async fn connect(url: &str) -> Client {
    wamn_postgres::connect(url)
        .await
        .expect("connect to disposable PostgreSQL")
}

/// Install the catalog and the application minimum on a clean database.
async fn provision(admin: &mut Client) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (wamn_env::project_setup::ensure_project_roles_sql()).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .expect("ensure the package-owner role");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $grant$ BEGIN \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO CURRENT_USER', current_database()); \
             END $grant$;")
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .expect("grant the package-owner role its database authority");
}

async fn drop_fixture_schemas(admin: &mut Client) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS platform CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE;"
    )))
    .execute(&mut *admin)
    .await
    .map(drop)
    .expect("drop the fixture schemas");
}

fn fixture_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()))
}

/// Write the fixture package: three logged relations, one migration.
fn write_package(root: &Path) {
    let mut models = RELATIONS
        .iter()
        .map(|(table, retention)| {
            (
                (*table).to_owned(),
                serde_json::json!({
                    "schema": SCHEMA,
                    "table": table,
                    "owner": PACKAGE_ID,
                    "audit_log": {"columns": [], "retention": retention},
                    "operations": {}
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    // The manifest groups one operation into its one component.
    models["shipment"]["operations"] = serde_json::json!({
        "get": {
            "permission": "shipment.get",

            "result": "one"
        }
    });
    let manifest = serde_json::json!({
        "package": {"id": PACKAGE_ID, "version": "1.0.0"},
        "required_platform_policy_contract": {
            "id": "record_retention_access",
            "state": "unsatisfied"
        },
        "models": models,
        "connections": {},
        "components": {"data": {}}
    });
    let migration = RELATIONS
        .iter()
        .map(|(table, retention)| {
            format!(
                "CREATE TABLE {SCHEMA}.{table} (\n    \
                     id uuid CONSTRAINT {table}_id_pkey PRIMARY KEY,\n    \
                     note text NOT NULL\n); \
                 SELECT wamn_history.create_history_table('{SCHEMA}', '{table}', false); \
                 CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE \
                     ON {SCHEMA}.{table} FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('{retention}');"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("migrations")).expect("create the fixture package");
    std::fs::create_dir_all(root.join("generated")).expect("create the generated package output");
    std::fs::write(
        root.join("generated/wamn.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialize the fixture manifest"),
    )
    .expect("write the fixture manifest");
    std::fs::write(root.join("migrations/0001_initial.sql"), migration)
        .expect("write the fixture migration");
}

/// Apply the fixture schema through the package library.
///
/// The `apply-package` verb takes only a pushed artifact, so the fixture tree
/// stands in for one under a digest of its own bytes.
async fn apply(url: &str, migration: &str, package: &Path) -> anyhow::Result<ApplyOutcome> {
    let package = PackageRoot {
        path: package.to_path_buf(),
        artifact_digest: Some(
            wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(package)?,
        ),
    };
    let directory = wamn_release::package_source::read_package_root(&package)?;
    let migration_root = wamn_release::package_source::migration_root(&package.path);
    apply_package::apply_package_from_directory(
        ApplyPackageRequest {
            migration_database_url: migration.to_owned(),
            package,
            database_url: url.to_owned(),
            org: "record-retention-t".parse().unwrap(),
            project: "fixture".parse().unwrap(),
            env: "dev".parse().unwrap(),
        },
        directory,
        &migration_root,
    )
    .await
}

/// One fixture write: a statement in its own transaction as the fixture
/// principal, with every new entry of the history table moved `age_days` into
/// the past. Returns the positions of the new entries.
async fn seed(admin: &mut Client, table: &str, statement: &str, age_days: i32) -> Vec<i64> {
    let history = format!("{SCHEMA}.{table}_history");
    let last: i64 = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT coalesce(max(position), 0) FROM {history}"
    )))
    .fetch_one(&mut *admin)
    .await
    .expect("read the last history position")
    .get(0);
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        format!(
            "BEGIN; \
             INSERT INTO platform.tx_identity(principal_id,role,app,operation) VALUES('{FIXTURE_PRINCIPAL}'::text::uuid,'','fixture','admin:seed-retention-fixture'); \
             {statement}; \
             COMMIT;"
        ),
    ))
    .execute(&mut *admin)
    .await.map_or_else(|error| panic!("write {statement}: {error}"), drop);
    let mut positions = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {history} SET changed_at = now() - make_interval(days => $1) \
                  WHERE position > $2 RETURNING position"
    )))
    .bind(age_days)
    .bind(last)
    .fetch_all(&mut *admin)
    .await
    .expect("move the new entries into the past")
    .iter()
    .map(|row| row.get(0))
    .collect::<Vec<i64>>();
    positions.sort_unstable();
    assert_eq!(
        positions.len(),
        1,
        "the fixture write {statement} wrote {} entries",
        positions.len()
    );
    positions
}

fn row_id(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

/// The positions of each history table, in position order.
async fn positions(admin: &mut Client) -> BTreeMap<String, Vec<i64>> {
    let mut observed = BTreeMap::new();
    for (table, _) in RELATIONS {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT position FROM {SCHEMA}.{table}_history ORDER BY position"
        )))
        .fetch_all(&mut *admin)
        .await
        .expect("read a history table");
        observed.insert(
            table.to_owned(),
            rows.iter().map(|row| row.get(0)).collect::<Vec<i64>>(),
        );
    }
    observed
}

async fn wait_for_blocker(observer: &mut Client, session_name: &str, blocker: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let waiting: bool = sqlx::query(
                "SELECT EXISTS (SELECT FROM pg_catalog.pg_stat_activity \
                      WHERE application_name = $1 AND $2 = ANY(pg_blocking_pids(pid)))",
            )
            .bind(session_name)
            .bind(blocker)
            .fetch_one(&mut *observer)
            .await
            .expect("read the blocking process")
            .get(0);
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the expected process blocks the queued operation");
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn history_table_creation_waits_for_a_running_prune() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    let migration = postgres.create_project_database("project").await.unwrap();
    wamn_env::platform_migrations::install_project(&mut connect(migration.as_str()).await)
        .await
        .unwrap();
    let mut database = url::Url::parse(postgres.admin_url()).unwrap();
    database.set_path("project");
    let url = database.to_string();
    let mut admin = connect(&url).await;
    provision(&mut admin).await;
    let package = fixture_root("prune-record-history-lock");
    write_package(&package);
    apply(&url, migration.as_str(), &package)
        .await
        .expect("apply the fixture package");
    let expired = seed(
        &mut admin,
        "parcel",
        &format!(
            "INSERT INTO {SCHEMA}.parcel VALUES ('{}', 'expired')",
            row_id(1)
        ),
        10,
    )
    .await;
    assert_eq!(positions(&mut admin).await["parcel"], expired);

    let mut holder = connect(&url).await;
    let holder_pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut holder)
        .await
        .unwrap()
        .get(0);
    let mut held = holder.begin().await.expect("hold the history table");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "LOCK TABLE {SCHEMA}.parcel_history IN ACCESS EXCLUSIVE MODE"
    )))
    .execute(&mut *held)
    .await
    .map(drop)
    .expect("prevent pruning from completing");

    let mut pruning = connect(&url).await;
    let prune_session = "history-lock-prune";
    sqlx::query("SELECT set_config('application_name', $1, false)")
        .bind(prune_session)
        .execute(&mut pruning)
        .await
        .map(drop)
        .unwrap();
    let prune =
        tokio::spawn(async move { wamn_env::prune_record_history::prune(&mut pruning).await });
    wait_for_blocker(&mut admin, prune_session, holder_pid).await;

    let mut applying_url = database;
    let mut applying_migration = migration;
    for input in [&mut applying_url, &mut applying_migration] {
        input
            .query_pairs_mut()
            .append_pair("application_name", "history_lock_apply");
    }
    let applying_package = package.clone();
    let applying = tokio::spawn(async move {
        apply(
            applying_url.as_str(),
            applying_migration.as_str(),
            &applying_package,
        )
        .await
    });
    // Package application polls its session lock, and reads the holders after each refusal.
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT FROM pg_catalog.pg_stat_activity \
                  WHERE application_name = 'history_lock_apply' \
                  AND query LIKE 'SELECT pid FROM pg_locks %')",
        )
        .fetch_one(&mut admin)
        .await
        .expect("read the package application session")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("package application waits for retention");
    held.commit().await.expect("allow pruning to finish");
    let (pruned, applied) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(prune, applying)
    })
    .await
    .expect("pruning and history creation complete in lock order");
    pruned
        .expect("join pruning")
        .expect("prune expired history");
    let applied = applied
        .expect("join apply")
        .expect("repeat the exact package application");
    assert_eq!(applied.migrations_applied, 0);
    assert!(positions(&mut admin).await["parcel"].is_empty());

    drop_fixture_schemas(&mut admin).await;
    std::fs::remove_dir_all(package).expect("remove the fixture package");
}

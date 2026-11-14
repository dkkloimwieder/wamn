//! Live test of the `prune-record-history` verb of `wamn-ctl-ops`.
//!
//! It runs on the PostgreSQL server of its test process and holds the process
//! lock of that server, because it creates roles.
//!
//! Each case applies a fixture package through the apply-package library. The package logs three relations: `shipment` keeps its entries for
//! 30 days, `parcel` for 7 days, and `ledger` without limit. The case mints a
//! real `wamn_audit_retention` credential generation, writes history, moves
//! entry times into the past, and runs the real verb through `wamn-ctl-ops`.
//!
//! The first case tests the retention half of level-2 spec tests 1, 1a, 6, 15, and 20:
//!
//! 1. The verb removes expired entries and nothing else, with two relations of
//!    different retention values. It removes a prefix of the history of a row,
//!    never an interior entry, and writes no marker.
//! 2. A live row keeps its retained diffs after its insert expires. A deleted
//!    row whose images expired keeps no entry.
//! 3. The verb refuses a login outside the audit retention family and a project
//!    that the credential was not minted for. The case reads the post-state.
//! 4. The role reads no history image column and holds no `wamn_platform` edge.
//!    It holds pruning grants on finite history and successful write claims.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wamn_control_provision::{
    AUDIT_RETENTION_ROLE, WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login,
};
use wamn_package_control::apply_package::{self, ApplyOutcome, ApplyPackageRequest};
use wamn_test_infrastructure::ctl_process;
use wamn_test_infrastructure::locked_database;

const SCHEMA: &str = "record_retention";
const PACKAGE_ID: &str = "record_retention_fixture";
const PROJECT_ENV_KEY: &str = "record-retention-t--dev--fixture";
const OTHER_PROJECT_ENV_KEY: &str = "record-retention-other";
const GENERATION_PASSWORD: &str = "record-retention-test-generation";
/// The test principal that the fixture writes bind.
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f2";

/// The fixture relations and the retention of each.
const RELATIONS: [(&str, &str); 3] = [
    ("shipment", "P30D"),
    ("parcel", "P7D"),
    ("ledger", "unlimited"),
];

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to disposable PostgreSQL");

    client
}

async fn current_database(admin: &mut Client) -> String {
    sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut *admin)
        .await
        .expect("read the target database name")
        .get(0)
}

/// Install the catalog and the application minimum on a clean database.
async fn provision(admin: &mut Client) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .expect("ensure the package-owner role");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $grant$ BEGIN \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); \
             END $grant$;")
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .expect("grant the package-owner role its database authority");
}

async fn drop_fixture_schemas(admin: &mut Client) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS platform CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE;"
        ))
            .to_string(),
    ))
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
        "components": {"data": {"connections": ["postgres"]}}
    });
    let migration = RELATIONS
        .iter()
        .map(|(table, _)| {
            format!(
                "CREATE TABLE {SCHEMA}.{table} (\n    \
                     id uuid CONSTRAINT {table}_id_pkey PRIMARY KEY,\n    \
                     note text NOT NULL\n);"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("migrations")).expect("create the fixture package");
    std::fs::write(
        root.join("wamn.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialize the fixture manifest"),
    )
    .expect("write the fixture manifest");
    std::fs::write(root.join("migrations/0001_initial.sql"), migration)
        .expect("write the fixture migration");
    wamn_test_infrastructure::fixture_digest::write_build_record(root);
}

/// Apply the fixture schema through the package library.
///
/// The `apply-package` verb takes only a pushed artifact, so the fixture tree
/// stands in for one under a digest of its own bytes.
async fn apply(url: &str, package: &Path) -> anyhow::Result<ApplyOutcome> {
    apply_package::apply_package_schema(
        ApplyPackageRequest {
            migration_database_url: wamn_control_provision::test_database::package_migration_input(
                &url,
                &"record-retention-t",
                &"fixture",
                &"dev",
            )
            .await
            .unwrap(),
            component_source: apply_package::component_source_from_env(None)?,
            changes: wamn_broker::Publisher::default(),
            package: wamn_package_control::apply_package::PackageRoot {
                path: package.to_path_buf(),
                artifact_digest: Some(
                    wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(package)?,
                ),
            },
            database_url: url.to_owned(),
            org: "record-retention-t".into(),
            project: "fixture".into(),
            environment: "dev".into(),
        },
        None,
    )
    .await
}

fn generation(family: WorkloadRoleFamily, project_env_key: &str, database: &str) -> String {
    workload_login(
        family,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key,
            database,
        },
    )
    .unwrap_or_else(|error| panic!("derive the {} generation identity: {error}", family.label()))
}

async fn drop_generation_role(admin: &mut Client, role: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DO $record_retention$ BEGIN \
               IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $record_retention$;"
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .expect("drop the generation role");
}

/// Mint one generation through the production prepare builder.
async fn mint(admin: &mut Client, family: WorkloadRoleFamily, database: &str, role: &str) {
    drop_generation_role(admin, role).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            family,
            database,
            role,
            GENERATION_PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .expect("prepare the generation");
    let clean: bool = sqlx::query(
        "SELECT NOT (rolsuper OR rolbypassrls) AND rolcanlogin AND rolinherit \
               FROM pg_catalog.pg_roles WHERE rolname = $1",
    )
    .bind(&role)
    .fetch_one(&mut *admin)
    .await
    .expect("read the generation attributes")
    .get(0);
    assert!(
        clean,
        "the {role} generation is superuser, BYPASSRLS, or cannot log in"
    );
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
        (&format!(
            "BEGIN; \
             SELECT set_config('app.user_id', '{FIXTURE_PRINCIPAL}', true), \
                    set_config('app.operation', 'admin:seed-retention-fixture', true); \
             {statement}; \
             COMMIT;"
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .unwrap_or_else(|error| panic!("write {statement}: {error}"));
    let mut positions = sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "UPDATE {history} SET changed_at = now() - make_interval(days => $1) \
                  WHERE position > $2 RETURNING position"
        ))
            .to_string(),
    ))
    .bind(&age_days)
    .bind(&last)
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
        let rows = sqlx::query(sqlx::AssertSqlSafe(
            (&format!("SELECT position FROM {SCHEMA}.{table}_history ORDER BY position"))
                .to_string(),
        ))
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

fn prune_argv<'a>(url: &'a str, project_env_key: &'a str) -> [&'a str; 9] {
    [
        "prune-record-history",
        "--database-url",
        url,
        "--org",
        project_env_key.split("--").next().expect("fixture org"),
        "--project",
        "fixture",
        "--environment",
        "dev",
    ]
}

/// Run the verb and require the identity refusal.
async fn assert_refused(url: &str, project_env_key: &str, label: &str) {
    ctl_process::require_ops_success(prune_argv(url, project_env_key))
        .await
        .expect_err(label);
}

/// Re-point a connection URL at another role, keeping host, port, and database.
fn replace_role(url: &str, role: &str, password: &str) -> String {
    let mut input = url::Url::parse(url).expect("parse fixture input");
    input.set_username(role).unwrap();
    input.set_password(Some(password)).unwrap();
    input.into()
}

/// The server's answer about the grants and the reach of the role (spec test 20).
async fn assert_authority(admin: &mut Client, credential_url: &str) {
    let grants = sqlx::query(sqlx::AssertSqlSafe(
        (sql::role_database_grants_sql()).to_string(),
    ))
    .bind(&AUDIT_RETENTION_ROLE)
    .fetch_all(&mut *admin)
    .await
    .expect("read the audit retention grants")
    .iter()
    .map(|row| {
        format!(
            "{} {}.{} {}",
            row.get::<String, _>("object_type"),
            row.get::<String, _>("schema_name"),
            row.get::<String, _>("object_name"),
            row.get::<String, _>("privilege_type"),
        )
    })
    .collect::<Vec<_>>();
    let mut expected = vec![
        "column platform.write_log.created_at SELECT".to_owned(),
        "column platform.write_log.result SELECT".to_owned(),
    ];
    for history in ["parcel_history", "shipment_history"] {
        for column in ["changed_at", "position", "row_key", "transaction_id"] {
            expected.push(format!("column {SCHEMA}.{history}.{column} SELECT"));
        }
    }
    expected.push("relation platform.write_log DELETE".to_owned());
    for history in ["parcel_history", "shipment_history"] {
        expected.push(format!("relation {SCHEMA}.{history} DELETE"));
    }
    for privilege in ["INSERT", "SELECT", "UPDATE"] {
        expected.push(format!(
            "relation wamn_history.pruning_watermarks {privilege}"
        ));
    }
    expected.push("schema platform.platform USAGE".to_owned());
    expected.push(format!("schema {SCHEMA}.{SCHEMA} USAGE"));
    expected.push("schema wamn_history.wamn_history USAGE".to_owned());
    assert_eq!(grants, expected, "the audit retention grants");
    let edges: i64 = sqlx::query(
        "SELECT count(*) FROM pg_catalog.pg_auth_members AS edge \
               JOIN pg_catalog.pg_roles AS member ON member.oid = edge.member \
              WHERE member.rolname = $1",
    )
    .bind(&AUDIT_RETENTION_ROLE)
    .fetch_one(&mut *admin)
    .await
    .expect("read the audit retention memberships")
    .get(0);
    assert_eq!(edges, 0, "the audit retention role holds a membership");

    let mut credential = connect(credential_url).await;
    for statement in [
        format!("SELECT before FROM {SCHEMA}.shipment_history LIMIT 1"),
        format!("SELECT count(*) FROM {SCHEMA}.ledger_history"),
        format!("DELETE FROM {SCHEMA}.ledger_history"),
        format!("SELECT count(*) FROM {SCHEMA}.shipment"),
    ] {
        let refused = sqlx::query(sqlx::AssertSqlSafe((&statement).to_string()))
            .fetch_all(&mut credential)
            .await
            .err()
            .and_then(|error| {
                error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .map(|code| code.into_owned())
            })
            .is_some_and(|code| code == "42501");
        assert!(refused, "the generation was not refused: {statement}");
    }
}

#[tokio::test]
async fn prune_removes_only_the_expired_prefix_of_each_row() {
    let url = locked_database::database(wamn_platform_identity::test_database::project_platform);
    let mut admin = connect(&url).await;
    let mut database = current_database(&mut admin).await;
    provision(&mut admin).await;
    let package = fixture_root("prune-record-history-live");
    write_package(&package);
    apply(&url, &package)
        .await
        .expect("apply the fixture package");

    // Spec tests 1a and 6. Each step is one write with the age of its entry,
    // and the expected result keeps each entry whose flag is true.
    let shipment = row_id;
    let steps: [(&str, String, i32, bool); 14] = [
        // A long-lived row: its insert and first update expire, today's update stays.
        (
            "shipment",
            format!(
                "INSERT INTO {SCHEMA}.shipment VALUES ('{}', 'a')",
                shipment(1)
            ),
            40,
            false,
        ),
        (
            "shipment",
            format!(
                "UPDATE {SCHEMA}.shipment SET note = 'b' WHERE id = '{}'",
                shipment(1)
            ),
            35,
            false,
        ),
        (
            "shipment",
            format!(
                "UPDATE {SCHEMA}.shipment SET note = 'c' WHERE id = '{}'",
                shipment(1)
            ),
            0,
            true,
        ),
        // A deleted row whose images expired keeps no entry.
        (
            "shipment",
            format!(
                "INSERT INTO {SCHEMA}.shipment VALUES ('{}', 'a')",
                shipment(2)
            ),
            40,
            false,
        ),
        (
            "shipment",
            format!("DELETE FROM {SCHEMA}.shipment WHERE id = '{}'", shipment(2)),
            35,
            false,
        ),
        // A recent row stays whole.
        (
            "shipment",
            format!(
                "INSERT INTO {SCHEMA}.shipment VALUES ('{}', 'a')",
                shipment(3)
            ),
            10,
            true,
        ),
        // An old entry after a recent entry of the same row is interior, so it stays.
        (
            "shipment",
            format!(
                "INSERT INTO {SCHEMA}.shipment VALUES ('{}', 'a')",
                shipment(4)
            ),
            40,
            false,
        ),
        (
            "shipment",
            format!(
                "UPDATE {SCHEMA}.shipment SET note = 'b' WHERE id = '{}'",
                shipment(4)
            ),
            1,
            true,
        ),
        (
            "shipment",
            format!(
                "UPDATE {SCHEMA}.shipment SET note = 'c' WHERE id = '{}'",
                shipment(4)
            ),
            40,
            true,
        ),
        // The seven-day relation removes what the thirty-day relation keeps.
        (
            "parcel",
            format!("INSERT INTO {SCHEMA}.parcel VALUES ('{}', 'a')", row_id(11)),
            10,
            false,
        ),
        (
            "parcel",
            format!(
                "UPDATE {SCHEMA}.parcel SET note = 'b' WHERE id = '{}'",
                row_id(11)
            ),
            8,
            false,
        ),
        (
            "parcel",
            format!(
                "UPDATE {SCHEMA}.parcel SET note = 'c' WHERE id = '{}'",
                row_id(11)
            ),
            1,
            true,
        ),
        (
            "parcel",
            format!("INSERT INTO {SCHEMA}.parcel VALUES ('{}', 'a')", row_id(12)),
            10,
            false,
        ),
        // The unlimited relation keeps everything.
        (
            "ledger",
            format!("INSERT INTO {SCHEMA}.ledger VALUES ('{}', 'a')", row_id(21)),
            400,
            true,
        ),
    ];
    let mut kept: BTreeMap<String, Vec<i64>> = RELATIONS
        .iter()
        .map(|(table, _)| ((*table).to_owned(), Vec::new()))
        .collect();
    for (table, statement, age, keep) in &steps {
        let written = seed(&mut admin, table, statement, *age).await;
        if *keep {
            kept.entry((*table).to_owned()).or_default().extend(written);
        }
    }
    let seeded = positions(&mut admin).await;

    let credential_role = generation(
        WorkloadRoleFamily::AuditRetention,
        PROJECT_ENV_KEY,
        &mut database,
    );
    let run_retention_role = generation(
        WorkloadRoleFamily::Retention,
        PROJECT_ENV_KEY,
        &mut database,
    );
    mint(
        &mut admin,
        WorkloadRoleFamily::AuditRetention,
        &mut database,
        &credential_role,
    )
    .await;
    mint(
        &mut admin,
        WorkloadRoleFamily::Retention,
        &mut database,
        &run_retention_role,
    )
    .await;
    let credential_url = replace_role(&url, &credential_role, GENERATION_PASSWORD);
    let run_retention_url = replace_role(&url, &run_retention_role, GENERATION_PASSWORD);

    // Spec test 15, against the populated history.
    assert_refused(
        &credential_url,
        OTHER_PROJECT_ENV_KEY,
        "foreign environment coordinate",
    )
    .await;
    assert_refused(
        &run_retention_url,
        PROJECT_ENV_KEY,
        "run retention generation",
    )
    .await;
    assert_refused(&url, PROJECT_ENV_KEY, "superuser login").await;
    assert_eq!(
        positions(&mut admin).await,
        seeded,
        "the refusals left the history untouched"
    );

    assert_authority(&mut admin, &credential_url).await;

    ctl_process::require_ops_success(prune_argv(&credential_url, PROJECT_ENV_KEY))
        .await
        .expect("prune record history through wamn-ctl-ops");

    assert_eq!(positions(&mut admin).await, kept, "the retained positions");
    // Spec tests 1 and 1a: the oldest retained entry of the live row is an
    // update, so its history is incomplete. The recent row keeps its insert.
    async fn oldest(admin: &mut Client, id: String) -> Option<String> {
        sqlx::query(sqlx::AssertSqlSafe(
            (&format!(
                "SELECT type FROM {SCHEMA}.shipment_history \
                          WHERE row_key = jsonb_build_object('id', $1::text::uuid) \
                          ORDER BY position LIMIT 1"
            ))
                .to_string(),
        ))
        .bind(&id)
        .fetch_optional(&mut *admin)
        .await
        .expect("read the oldest retained entry")
        .map(|row| row.get::<String, _>(0))
    }
    assert_eq!(
        oldest(&mut admin, shipment(1)).await.as_deref(),
        Some("update"),
        "the live row keeps an incomplete history"
    );
    assert_eq!(
        oldest(&mut admin, shipment(2)).await,
        None,
        "the deleted row keeps no entry"
    );
    assert_eq!(
        oldest(&mut admin, shipment(3)).await.as_deref(),
        Some("insert"),
        "the recent row keeps its insert"
    );
    let markers: i64 = sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "SELECT (SELECT count(*) FROM {SCHEMA}.shipment_history \
                          WHERE operation <> 'admin:seed-retention-fixture') \
                      + (SELECT count(*) FROM {SCHEMA}.parcel_history \
                          WHERE operation <> 'admin:seed-retention-fixture') \
                      + (SELECT count(*) FROM {SCHEMA}.ledger_history \
                          WHERE operation <> 'admin:seed-retention-fixture')"
        ))
            .to_string(),
    ))
    .fetch_one(&mut admin)
    .await
    .expect("read entries that the verb wrote")
    .get(0);
    assert_eq!(markers, 0, "the verb wrote no marker");

    drop_generation_role(&mut admin, &credential_role).await;
    drop_generation_role(&mut admin, &run_retention_role).await;
    drop_fixture_schemas(&mut admin).await;
    std::fs::remove_dir_all(package).expect("remove the fixture package");
}

async fn wait_for_blocker(observer: &mut Client, application: &str, blocker: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let waiting: bool = sqlx::query(
                "SELECT EXISTS (SELECT FROM pg_catalog.pg_stat_activity \
                      WHERE application_name = $1 AND $2 = ANY(pg_blocking_pids(pid)))",
            )
            .bind(&application)
            .bind(&blocker)
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
async fn history_table_creation_waits_for_a_running_prune() {
    let url = locked_database::database(wamn_platform_identity::test_database::project_platform);
    let mut admin = connect(&url).await;
    provision(&mut admin).await;
    let package = fixture_root("prune-record-history-lock");
    write_package(&package);
    apply(&url, &package)
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
    sqlx::raw_sql("SET application_name = 'history_lock_prune'")
        .execute(&mut pruning)
        .await
        .map(drop)
        .unwrap();
    let prune_pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut pruning)
        .await
        .unwrap()
        .get(0);
    let prune = tokio::spawn(async move {
        wamn_environment_control::prune_record_history::prune(&mut pruning).await
    });
    wait_for_blocker(&mut admin, "history_lock_prune", holder_pid).await;

    let mut applying_url = url::Url::parse(&url).expect("parse the database URL");
    applying_url
        .query_pairs_mut()
        .append_pair("application_name", "history_lock_apply");
    let applying_package = package.clone();
    let applying =
        tokio::spawn(async move { apply(applying_url.as_str(), &applying_package).await });
    wait_for_blocker(&mut admin, "history_lock_apply", prune_pid).await;
    let waiting_query: String = sqlx::query(
        "SELECT query FROM pg_catalog.pg_stat_activity \
              WHERE application_name = 'history_lock_apply' AND wait_event = 'advisory'",
    )
    .fetch_one(&mut admin)
    .await
    .expect("apply waits on the retention advisory lock")
    .get(0);
    assert_eq!(
        waiting_query,
        wamn_control_provision::audit_retention::AUDIT_RETENTION_LOCK_SQL
    );

    held.commit().await.expect("allow pruning to finish");
    let (pruned, applied) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(prune, applying)
    })
    .await
    .expect("pruning and history creation complete in lock order");
    let pruned = pruned
        .expect("join pruning")
        .expect("prune expired history");
    assert!(
        pruned
            .iter()
            .any(|table| table.history == "parcel_history" && table.removed == 1)
    );
    let applied = applied
        .expect("join apply")
        .expect("repeat the exact package application");
    assert_eq!(applied.migrations_applied, 0);
    assert!(positions(&mut admin).await["parcel"].is_empty());

    drop_fixture_schemas(&mut admin).await;
    std::fs::remove_dir_all(package).expect("remove the fixture package");
}

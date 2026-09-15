//! Live test of the `prune-record-history` verb of `wamn-ctl-ops`.
//!
//! Run it through `wamn-test-postgres` with `WAMN_CTL_PG_URL`. It needs its own
//! PostgreSQL 18 server, because it creates roles and replaces the catalog
//! schemas.
//!
//! Each case applies a fixture package through the real `wamn-ctl apply-package`
//! process. The package logs three relations: `shipment` keeps its entries for
//! 30 days, `pallet` for 7 days, and `ledger` without limit. The case mints a
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
//! 3. The verb refuses a login outside the audit retention family and a tenant
//!    that the credential was not minted for. The case reads the post-state.
//! 4. The role holds its grants only on the P<n>D history tables, reads no image
//!    column, and holds no `wamn_platform` edge.
//!
//! The second case tests the keep window under the audit retention lock: a
//! retention change in apply-package waits for a running prune, so the prune
//! deletes with the retention that it read.

mod support;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use tokio_postgres::{Client, NoTls};
use wamn_control_provision::audit_retention::AUDIT_RETENTION_LOCK_SQL;
use wamn_control_provision::{
    AUDIT_RETENTION_ROLE, CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, compose_url,
    sql, workload_generation_role,
};
use wamn_test_infrastructure::ctl_process;

const APP_SCHEMA: &str = include_str!("../../../deploy/sql/app-schema.sql");
const SCHEMA: &str = "record_retention";
const PACKAGE_ID: &str = "record_retention_fixture";
const TENANT: &str = "record-retention-t";
const OTHER_TENANT: &str = "record-retention-other";
const GENERATION_PASSWORD: &str = "record-retention-test-generation";
/// The test principal that the fixture writes bind.
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f2";

/// The fixture relations and the retention of each.
const RELATIONS: [(&str, &str); 3] = [
    ("shipment", "P30D"),
    ("pallet", "P7D"),
    ("ledger", "unlimited"),
];

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn current_database(admin: &Client) -> String {
    admin
        .query_one("SELECT current_database()::text", &[])
        .await
        .expect("read the target database name")
        .get(0)
}

/// Install the catalog and the application floor on a clean database.
async fn provision(admin: &Client) {
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE; \
             DO $roles$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN; \
               END IF; \
             END $roles$;"
        ))
        .await
        .expect("reset the fixture schemas");
    admin
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .expect("ensure the package-owner role");
    admin
        .batch_execute(
            "DO $grant$ BEGIN \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); \
             END $grant$;",
        )
        .await
        .expect("grant the package-owner role its database authority");
    admin
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .expect("install the catalog schema");
    admin
        .batch_execute(APP_SCHEMA)
        .await
        .expect("install the application authorization floor");
}

async fn drop_fixture_schemas(admin: &Client) {
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE;"
        ))
        .await
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
            "error_details": {
                "invalid_input": {"required": ["field"]},
                "not_found": {"required": ["field", "id"]},
                "retry": {},
                "timeout": {},
                "permission_denied": {"required": ["operation"]},
                "internal_error": {}
            },
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
        "connections": {"postgres": {"interface": "wamn:postgres@0.1.0"}},
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
}

/// Rewrite the fixture manifest as the next version, with a new retention for one relation.
fn upgrade_retention(root: &Path, table: &str, retention: &str) {
    let path = root.join("wamn.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read the fixture manifest"))
            .expect("parse the fixture manifest");
    manifest["package"]["version"] = serde_json::json!("1.0.1");
    manifest["package"]["predecessor_version"] = serde_json::json!("1.0.0");
    manifest["models"][table]["audit_log"]["retention"] = serde_json::json!(retention);
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&manifest).expect("serialize the upgraded manifest"),
    )
    .expect("write the upgraded manifest");
}

/// Apply the fixture package through the real `wamn-ctl apply-package` process.
async fn apply(url: &str, package: &Path) -> anyhow::Result<Output> {
    ctl_process::run_checked([
        OsStr::new("apply-package"),
        OsStr::new("--package"),
        package.as_os_str(),
        OsStr::new("--database-url"),
        OsStr::new(url),
        OsStr::new("--tenant"),
        OsStr::new(TENANT),
    ])
    .await
}

fn generation(family: WorkloadRoleFamily, tenant: &str, database: &str) -> String {
    workload_generation_role(
        family,
        WorkloadRoleScope::Tenant { tenant, database },
        CredentialGeneration::A,
    )
    .unwrap_or_else(|error| panic!("derive the {} generation identity: {error}", family.label()))
}

async fn drop_generation_role(admin: &Client, role: &str) {
    admin
        .batch_execute(&format!(
            "DO $record_retention$ BEGIN \
               IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $record_retention$;"
        ))
        .await
        .expect("drop the generation role");
}

/// Mint one generation through the production prepare builder.
async fn mint(admin: &Client, family: WorkloadRoleFamily, database: &str, role: &str) {
    drop_generation_role(admin, role).await;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            family,
            database,
            role,
            GENERATION_PASSWORD,
            "2100-01-01T00:00:00Z",
        ))
        .await
        .expect("prepare the generation");
    let clean: bool = admin
        .query_one(
            "SELECT NOT (rolsuper OR rolbypassrls) AND rolcanlogin AND rolinherit \
               FROM pg_catalog.pg_roles WHERE rolname = $1",
            &[&role],
        )
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
async fn seed(admin: &Client, table: &str, statement: &str, age_days: i32) -> Vec<i64> {
    let history = format!("{SCHEMA}.{table}_history");
    let last: i64 = admin
        .query_one(
            &format!("SELECT coalesce(max(position), 0) FROM {history}"),
            &[],
        )
        .await
        .expect("read the last history position")
        .get(0);
    admin
        .batch_execute(&format!(
            "BEGIN; \
             SELECT set_config('app.user_id', '{FIXTURE_PRINCIPAL}', true), \
                    set_config('app.operation', 'admin:seed-retention-fixture', true); \
             {statement}; \
             COMMIT;"
        ))
        .await
        .unwrap_or_else(|error| panic!("write {statement}: {error}"));
    let mut positions = admin
        .query(
            &format!(
                "UPDATE {history} SET changed_at = now() - make_interval(days => $1) \
                  WHERE position > $2 RETURNING position"
            ),
            &[&age_days, &last],
        )
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
async fn positions(admin: &Client) -> BTreeMap<String, Vec<i64>> {
    let mut observed = BTreeMap::new();
    for (table, _) in RELATIONS {
        let rows = admin
            .query(
                &format!("SELECT position FROM {SCHEMA}.{table}_history ORDER BY position"),
                &[],
            )
            .await
            .expect("read a history table");
        observed.insert(
            table.to_owned(),
            rows.iter().map(|row| row.get(0)).collect::<Vec<i64>>(),
        );
    }
    observed
}

fn prune_argv<'a>(url: &'a str, tenant: &'a str) -> [&'a str; 5] {
    [
        "prune-record-history",
        "--database-url",
        url,
        "--tenant",
        tenant,
    ]
}

/// Run the verb and require the identity refusal.
async fn assert_refused(url: &str, tenant: &str, label: &str) {
    let error = ctl_process::run_ops_checked(prune_argv(url, tenant))
        .await
        .expect_err(label);
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("refusing to prune record history"),
        "{label}: the failure was not the refusal: {rendered}"
    );
}

/// Re-point a connection URL at another role, keeping host, port, and database.
fn replace_role(url: &str, role: &str, password: &str) -> String {
    let config: tokio_postgres::Config = url.parse().expect("parse the connection URL");
    let host = match config.get_hosts() {
        [tokio_postgres::config::Host::Tcp(host)] => host.clone(),
        hosts => panic!("the record history retention test needs one TCP host, got {hosts:?}"),
    };
    let port = *config
        .get_ports()
        .first()
        .expect("the connection URL names a port");
    let database = config
        .get_dbname()
        .expect("the connection URL names a database");
    compose_url(role, password, &host, port, database)
}

/// The server's answer about the grants and the reach of the role (spec test 20).
async fn assert_authority(admin: &Client, credential_url: &str) {
    let grants = admin
        .query(sql::role_database_grants_sql(), &[&AUDIT_RETENTION_ROLE])
        .await
        .expect("read the audit retention grants")
        .iter()
        .map(|row| {
            format!(
                "{} {}.{} {}",
                row.get::<_, String>("object_kind"),
                row.get::<_, String>("schema_name"),
                row.get::<_, String>("object_name"),
                row.get::<_, String>("privilege_type"),
            )
        })
        .collect::<Vec<_>>();
    let mut expected = Vec::new();
    for history in ["pallet_history", "shipment_history"] {
        for column in ["changed_at", "position", "row_key"] {
            expected.push(format!("column {SCHEMA}.{history}.{column} SELECT"));
        }
    }
    for history in ["pallet_history", "shipment_history"] {
        expected.push(format!("relation {SCHEMA}.{history} DELETE"));
    }
    expected.push(format!("schema {SCHEMA}.{SCHEMA} USAGE"));
    assert_eq!(grants, expected, "the audit retention grants");
    let edges: i64 = admin
        .query_one(
            "SELECT count(*) FROM pg_catalog.pg_auth_members AS edge \
               JOIN pg_catalog.pg_roles AS member ON member.oid = edge.member \
              WHERE member.rolname = $1",
            &[&AUDIT_RETENTION_ROLE],
        )
        .await
        .expect("read the audit retention memberships")
        .get(0);
    assert_eq!(edges, 0, "the audit retention role holds a membership");

    let credential = connect(credential_url).await;
    for statement in [
        format!("SELECT before FROM {SCHEMA}.shipment_history LIMIT 1"),
        format!("SELECT count(*) FROM {SCHEMA}.ledger_history"),
        format!("DELETE FROM {SCHEMA}.ledger_history"),
        format!("SELECT count(*) FROM {SCHEMA}.shipment"),
    ] {
        let refused = credential
            .query(&statement, &[])
            .await
            .err()
            .and_then(|error| error.code().cloned())
            .is_some_and(|code| code == tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE);
        assert!(refused, "the generation was not refused: {statement}");
    }
}

#[tokio::test]
async fn prune_removes_only_the_expired_prefix_of_each_row() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let admin = connect(&url).await;
    let database = current_database(&admin).await;
    provision(&admin).await;
    let package = fixture_root("prune-record-history-live");
    write_package(&package);
    apply(&url, &package)
        .await
        .expect("apply the fixture package through wamn-ctl");

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
            "pallet",
            format!("INSERT INTO {SCHEMA}.pallet VALUES ('{}', 'a')", row_id(11)),
            10,
            false,
        ),
        (
            "pallet",
            format!(
                "UPDATE {SCHEMA}.pallet SET note = 'b' WHERE id = '{}'",
                row_id(11)
            ),
            8,
            false,
        ),
        (
            "pallet",
            format!(
                "UPDATE {SCHEMA}.pallet SET note = 'c' WHERE id = '{}'",
                row_id(11)
            ),
            1,
            true,
        ),
        (
            "pallet",
            format!("INSERT INTO {SCHEMA}.pallet VALUES ('{}', 'a')", row_id(12)),
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
        let written = seed(&admin, table, statement, *age).await;
        if *keep {
            kept.entry((*table).to_owned()).or_default().extend(written);
        }
    }
    let seeded = positions(&admin).await;

    let credential_role = generation(WorkloadRoleFamily::AuditRetention, TENANT, &database);
    let run_retention_role = generation(WorkloadRoleFamily::Retention, TENANT, &database);
    mint(
        &admin,
        WorkloadRoleFamily::AuditRetention,
        &database,
        &credential_role,
    )
    .await;
    mint(
        &admin,
        WorkloadRoleFamily::Retention,
        &database,
        &run_retention_role,
    )
    .await;
    let credential_url = replace_role(&url, &credential_role, GENERATION_PASSWORD);
    let run_retention_url = replace_role(&url, &run_retention_role, GENERATION_PASSWORD);

    // Spec test 15, against the populated history.
    assert_refused(&credential_url, OTHER_TENANT, "foreign --tenant").await;
    assert_refused(&run_retention_url, TENANT, "run retention generation").await;
    assert_refused(&url, TENANT, "superuser login").await;
    assert_eq!(
        positions(&admin).await,
        seeded,
        "the refusals left the history untouched"
    );

    assert_authority(&admin, &credential_url).await;

    let pruned = ctl_process::run_ops_checked(prune_argv(&credential_url, TENANT))
        .await
        .expect("prune record history through wamn-ctl-ops");
    let stdout = String::from_utf8(pruned.stdout).expect("prune output is UTF-8");
    print!("{stdout}");
    for line in [
        format!(
            "removed 5 entries from {SCHEMA}.shipment_history (retention P30D, tenant {TENANT})"
        ),
        format!("removed 3 entries from {SCHEMA}.pallet_history (retention P7D, tenant {TENANT})"),
        format!("pruned 2 history table(s) for tenant {TENANT}"),
    ] {
        assert!(stdout.contains(&line), "the verb did not report: {line}");
    }

    assert_eq!(positions(&admin).await, kept, "the retained positions");
    // Spec tests 1 and 1a: the oldest retained entry of the live row is an
    // update, so its history is incomplete. The recent row keeps its insert.
    let oldest = |id: String| {
        let admin = &admin;
        async move {
            admin
                .query_opt(
                    &format!(
                        "SELECT kind FROM {SCHEMA}.shipment_history \
                          WHERE row_key = jsonb_build_object('id', $1::text::uuid) \
                          ORDER BY position LIMIT 1"
                    ),
                    &[&id],
                )
                .await
                .expect("read the oldest retained entry")
                .map(|row| row.get::<_, String>(0))
        }
    };
    assert_eq!(
        oldest(shipment(1)).await.as_deref(),
        Some("update"),
        "the live row keeps an incomplete history"
    );
    assert_eq!(
        oldest(shipment(2)).await,
        None,
        "the deleted row keeps no entry"
    );
    assert_eq!(
        oldest(shipment(3)).await.as_deref(),
        Some("insert"),
        "the recent row keeps its insert"
    );
    let markers: i64 = admin
        .query_one(
            &format!(
                "SELECT (SELECT count(*) FROM {SCHEMA}.shipment_history \
                          WHERE operation <> 'admin:seed-retention-fixture') \
                      + (SELECT count(*) FROM {SCHEMA}.pallet_history \
                          WHERE operation <> 'admin:seed-retention-fixture') \
                      + (SELECT count(*) FROM {SCHEMA}.ledger_history \
                          WHERE operation <> 'admin:seed-retention-fixture')"
            ),
            &[],
        )
        .await
        .expect("read entries that the verb wrote")
        .get(0);
    assert_eq!(markers, 0, "the verb wrote no marker");

    drop_generation_role(&admin, &credential_role).await;
    drop_generation_role(&admin, &run_retention_role).await;
    drop_fixture_schemas(&admin).await;
    std::fs::remove_dir_all(package).expect("remove the fixture package");
}

/// Wait until the audit retention lock has the expected waiters: the verb,
/// logged in as `credential_role`, and apply-package, logged in as the admin.
async fn await_lock_waiters(
    observer: &Client,
    credential_role: &str,
    expected: (i64, i64),
    expectation: &str,
) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let waiting = observer
                .query_one(
                    "SELECT count(*) FILTER (WHERE usename = $1), \
                            count(*) FILTER (WHERE usename = current_user) \
                       FROM pg_stat_activity \
                      WHERE datname = current_database() \
                        AND wait_event_type = 'Lock' AND wait_event = 'advisory' \
                        AND query LIKE '%wamn.audit-retention%'",
                    &[&credential_role],
                )
                .await
                .expect("observe the audit retention lock waiters");
            if (waiting.get::<_, i64>(0), waiting.get::<_, i64>(1)) == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(expectation);
}

/// The keep window holds while a retention change waits on the audit retention lock.
///
/// A blocker holds the lock. The verb queues on it first, and apply-package
/// queues second with a `pallet` retention change from P7D to P1D. PostgreSQL
/// grants queued lock requests in queue order, so the transaction of `pallet`,
/// the first relation that the verb visits, reads and deletes with P7D.
/// apply-package changes the trigger only after that transaction commits.
#[tokio::test]
async fn a_retention_change_waits_for_a_running_prune() {
    let url =
        support::LockedUrl::required("WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database");
    let admin = connect(&url).await;
    let database = current_database(&admin).await;
    provision(&admin).await;
    let package = fixture_root("prune-record-history-lock-live");
    write_package(&package);
    apply(&url, &package)
        .await
        .expect("apply the fixture package through wamn-ctl");

    // The insert expires under P7D. The 3-day update stays under P7D and
    // expires under P1D.
    let mut kept = Vec::new();
    for (statement, age, keep) in [
        (
            format!("INSERT INTO {SCHEMA}.pallet VALUES ('{}', 'a')", row_id(11)),
            10,
            false,
        ),
        (
            format!(
                "UPDATE {SCHEMA}.pallet SET note = 'b' WHERE id = '{}'",
                row_id(11)
            ),
            3,
            true,
        ),
        (
            format!(
                "UPDATE {SCHEMA}.pallet SET note = 'c' WHERE id = '{}'",
                row_id(11)
            ),
            0,
            true,
        ),
    ] {
        let written = seed(&admin, "pallet", &statement, age).await;
        if keep {
            kept.extend(written);
        }
    }
    let credential_role = generation(WorkloadRoleFamily::AuditRetention, TENANT, &database);
    mint(
        &admin,
        WorkloadRoleFamily::AuditRetention,
        &database,
        &credential_role,
    )
    .await;
    let credential_url = replace_role(&url, &credential_role, GENERATION_PASSWORD);
    upgrade_retention(&package, "pallet", "P1D");

    let mut blocker = connect(&url).await;
    let blocker_tx = blocker
        .transaction()
        .await
        .expect("begin the audit retention lock blocker");
    blocker_tx
        .query_one(AUDIT_RETENTION_LOCK_SQL, &[])
        .await
        .expect("hold the audit retention lock");

    let prune_task = tokio::spawn(async move {
        ctl_process::run_ops_checked(prune_argv(&credential_url, TENANT)).await
    });
    await_lock_waiters(
        &admin,
        &credential_role,
        (1, 0),
        "the verb must wait on the audit retention lock",
    )
    .await;
    let apply_url = url.to_string();
    let apply_package = package.clone();
    let apply_task = tokio::spawn(async move { apply(&apply_url, &apply_package).await });
    await_lock_waiters(
        &admin,
        &credential_role,
        (1, 1),
        "apply-package must wait on the audit retention lock behind the verb",
    )
    .await;

    blocker_tx
        .commit()
        .await
        .expect("release the audit retention lock blocker");
    let pruned = prune_task
        .await
        .expect("join the prune")
        .expect("prune record history through wamn-ctl-ops");
    apply_task
        .await
        .expect("join apply-package")
        .expect("apply the retention change through wamn-ctl");

    let stdout = String::from_utf8(pruned.stdout).expect("prune output is UTF-8");
    print!("{stdout}");
    let line =
        format!("removed 1 entries from {SCHEMA}.pallet_history (retention P7D, tenant {TENANT})");
    assert!(stdout.contains(&line), "the verb did not report: {line}");
    assert_eq!(
        positions(&admin).await,
        BTreeMap::from([
            ("ledger".to_owned(), Vec::new()),
            ("pallet".to_owned(), kept),
            ("shipment".to_owned(), Vec::new()),
        ]),
        "the verb deleted with the P7D keep window"
    );
    let trigger: String = admin
        .query_one(
            &format!(
                "SELECT pg_catalog.pg_get_triggerdef(oid) FROM pg_catalog.pg_trigger \
                  WHERE tgrelid = '{SCHEMA}.pallet'::regclass \
                    AND tgname = 'wamn_record_history_log'"
            ),
            &[],
        )
        .await
        .expect("read the pallet log trigger")
        .get(0);
    assert_eq!(
        trigger,
        format!(
            "CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE \
             ON {SCHEMA}.pallet FOR EACH ROW \
             EXECUTE FUNCTION wamn_history.log_row_change('P1D')"
        ),
        "apply-package committed the P1D retention after the prune"
    );

    drop_generation_role(&admin, &credential_role).await;
    drop_fixture_schemas(&admin).await;
    std::fs::remove_dir_all(package).expect("remove the fixture package");
}

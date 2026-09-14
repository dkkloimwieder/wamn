//! The `record-history-retention` subcommand: the live `prune-record-history` gate.
//!
//! The gate applies a fixture package through the real `wamn-ctl apply-package`
//! process. The package logs three relations: `shipment` keeps its entries for
//! 30 days, `pallet` for 7 days, and `ledger` without limit. The gate mints a
//! real `wamn_audit_retention` credential generation, writes history, moves
//! entry times into the past, and runs the real verb through `wamn-ctl-ops`.
//!
//! It tests the retention half of level-2 spec tests 1, 1a, 6, 15, and 20:
//!
//! 1. The verb removes expired entries and nothing else, with two relations of
//!    different retention values. It removes a prefix of the history of a row,
//!    never an interior entry, and writes no marker.
//! 2. A live row keeps its retained diffs after its insert expires. A deleted
//!    row whose images expired keeps no entry.
//! 3. The verb refuses a login outside the audit retention family and a tenant
//!    that the credential was not minted for. The gate reads the post-state.
//! 4. The role holds its grants only on the P<n>D history tables, reads no image
//!    column, and holds no `wamn_platform` edge.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::{
    AUDIT_RETENTION_ROLE, CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, compose_url,
    sql, workload_generation_role,
};

use crate::ctl_process;

const SCHEMA: &str = "record_retention";
const PACKAGE_ID: &str = "record_retention_fixture";
const TENANT: &str = "record-retention-t";
const OTHER_TENANT: &str = "record-retention-other";
const GENERATION_PASSWORD: &str = "record-retention-gate-generation";
/// The test principal that the fixture writes bind.
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f2";

/// The fixture relations and the retention of each.
const RELATIONS: [(&str, &str); 3] = [
    ("shipment", "P30D"),
    ("pallet", "P7D"),
    ("ledger", "unlimited"),
];

#[derive(Debug, Args)]
pub struct RecordHistoryRetentionArgs {
    /// Superuser URL of a disposable database. The gate installs the catalog,
    /// applies the fixture package, and mints the credential generations.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,
}

async fn connect(url: &str, purpose: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| format!("{purpose} connect"))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

/// Install the catalog and the application floor on a clean database.
async fn provision(admin: &Client) -> anyhow::Result<()> {
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
        .context("reset the fixture schemas")?;
    admin
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .context("ensure the package-owner role")?;
    admin
        .batch_execute(
            "DO $grant$ BEGIN \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); \
             END $grant$;",
        )
        .await
        .context("grant the package-owner role its database authority")?;
    admin
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .context("install the catalog schema")?;
    admin
        .batch_execute(include_str!("../../../deploy/sql/app-schema.sql"))
        .await
        .context("install the application authorization floor")
}

/// Write the fixture package: three logged relations, one migration.
fn write_package(root: &Path) -> anyhow::Result<()> {
    let models = RELATIONS
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
    let mut models = models;
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
    std::fs::create_dir_all(root.join("migrations"))?;
    std::fs::write(
        root.join("wamn.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::write(root.join("migrations/0001_initial.sql"), migration)?;
    Ok(())
}

fn generation(family: WorkloadRoleFamily, tenant: &str, database: &str) -> anyhow::Result<String> {
    workload_generation_role(
        family,
        WorkloadRoleScope::Tenant { tenant, database },
        CredentialGeneration::A,
    )
    .with_context(|| format!("derive the {} generation identity", family.label()))
}

async fn drop_generation_role(admin: &Client, role: &str) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
            "DO $record_retention_gate$ BEGIN \
               IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $record_retention_gate$;"
        ))
        .await
        .with_context(|| format!("drop generation role {role}"))
}

/// Mint one generation through the production prepare builder.
async fn mint(
    admin: &Client,
    family: WorkloadRoleFamily,
    database: &str,
    role: &str,
) -> anyhow::Result<()> {
    drop_generation_role(admin, role).await?;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            family,
            database,
            role,
            GENERATION_PASSWORD,
            "2100-01-01T00:00:00Z",
        ))
        .await
        .with_context(|| format!("prepare the {} generation", family.label()))?;
    let clean: bool = admin
        .query_one(
            "SELECT NOT (rolsuper OR rolbypassrls) AND rolcanlogin AND rolinherit \
               FROM pg_catalog.pg_roles WHERE rolname = $1",
            &[&role],
        )
        .await
        .context("read the generation attributes")?
        .get(0);
    if !clean {
        bail!("the {role} generation is superuser, BYPASSRLS, or cannot log in");
    }
    Ok(())
}

/// One fixture write: a statement in its own transaction as the fixture
/// principal, with every new entry of the history table moved `age_days` into
/// the past. Returns the positions of the new entries.
async fn seed(
    admin: &Client,
    table: &str,
    statement: &str,
    age_days: i32,
) -> anyhow::Result<Vec<i64>> {
    let history = format!("{SCHEMA}.{table}_history");
    let last: i64 = admin
        .query_one(
            &format!("SELECT coalesce(max(position), 0) FROM {history}"),
            &[],
        )
        .await
        .context("read the last history position")?
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
        .with_context(|| format!("write {statement}"))?;
    admin
        .query(
            &format!(
                "UPDATE {history} SET changed_at = now() - make_interval(days => $1) \
                  WHERE position > $2 RETURNING position"
            ),
            &[&age_days, &last],
        )
        .await
        .context("move the new entries into the past")
        .map(|rows| {
            let mut positions = rows.iter().map(|row| row.get(0)).collect::<Vec<i64>>();
            positions.sort_unstable();
            positions
        })
}

fn row_id(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

/// The positions of each history table, in position order.
async fn positions(admin: &Client) -> anyhow::Result<BTreeMap<String, Vec<i64>>> {
    let mut observed = BTreeMap::new();
    for (table, _) in RELATIONS {
        let rows = admin
            .query(
                &format!("SELECT position FROM {SCHEMA}.{table}_history ORDER BY position"),
                &[],
            )
            .await
            .with_context(|| format!("read {table}_history"))?;
        observed.insert(
            table.to_owned(),
            rows.iter().map(|row| row.get(0)).collect::<Vec<i64>>(),
        );
    }
    Ok(observed)
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
async fn expect_refusal(url: &str, tenant: &str, label: &str) -> bool {
    match ctl_process::run_ops_checked(prune_argv(url, tenant)).await {
        Ok(output) => {
            println!(
                "  {label}: NOT REFUSED. stdout: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            false
        }
        Err(error) => {
            let rendered = format!("{error:#}");
            let refused = rendered.contains("refusing to prune record history");
            println!("  {label}: refused={refused}");
            if !refused {
                println!("  {label}: the failure was not the refusal: {rendered}");
            }
            refused
        }
    }
}

/// Re-point a connection URL at another role, keeping host, port, and database.
fn replace_role(url: &str, role: &str, password: &str) -> anyhow::Result<String> {
    let config: tokio_postgres::Config = url.parse().context("parse the connection URL")?;
    let host = match config.get_hosts() {
        [tokio_postgres::config::Host::Tcp(host)] => host.clone(),
        hosts => bail!("the record history retention gate needs one TCP host, got {hosts:?}"),
    };
    let port = *config
        .get_ports()
        .first()
        .context("the connection URL names a port")?;
    let database = config
        .get_dbname()
        .context("the connection URL names a database")?;
    Ok(compose_url(role, password, &host, port, database))
}

/// The server's answer about the grants and the reach of the role (spec test 20).
async fn authority_arm(admin: &Client, credential_url: &str) -> anyhow::Result<bool> {
    let mut ok = true;
    let grants = admin
        .query(sql::role_database_grants_sql(), &[&AUDIT_RETENTION_ROLE])
        .await
        .context("read the audit retention grants")?
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
    if grants != expected {
        println!("  authority: grants {grants:?}, expected {expected:?}");
        ok = false;
    }
    let edges: i64 = admin
        .query_one(
            "SELECT count(*) FROM pg_catalog.pg_auth_members AS edge \
               JOIN pg_catalog.pg_roles AS member ON member.oid = edge.member \
              WHERE member.rolname = $1",
            &[&AUDIT_RETENTION_ROLE],
        )
        .await
        .context("read the audit retention memberships")?
        .get(0);
    if edges != 0 {
        println!("  authority: the audit retention role holds {edges} membership(s)");
        ok = false;
    }

    let credential = connect(credential_url, "audit retention generation").await?;
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
        if !refused {
            println!("  authority: the generation was not refused: {statement}");
            ok = false;
        }
    }
    println!("  authority arm: {ok}");
    Ok(ok)
}

async fn gate(admin: &Client, admin_url: &str, database: &str) -> anyhow::Result<bool> {
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
        let written = seed(admin, table, statement, *age).await?;
        if written.len() != 1 {
            bail!(
                "the fixture write {statement} wrote {} entries",
                written.len()
            );
        }
        if *keep {
            kept.entry((*table).to_owned()).or_default().extend(written);
        }
    }
    let seeded = positions(admin).await?;

    let credential_role = generation(WorkloadRoleFamily::AuditRetention, TENANT, database)?;
    let run_retention_role = generation(WorkloadRoleFamily::Retention, TENANT, database)?;
    mint(
        admin,
        WorkloadRoleFamily::AuditRetention,
        database,
        &credential_role,
    )
    .await?;
    mint(
        admin,
        WorkloadRoleFamily::Retention,
        database,
        &run_retention_role,
    )
    .await?;
    let credential_url = replace_role(admin_url, &credential_role, GENERATION_PASSWORD)?;
    let run_retention_url = replace_role(admin_url, &run_retention_role, GENERATION_PASSWORD)?;

    let result = async {
        // Spec test 15, against the populated history.
        let foreign_tenant_refused =
            expect_refusal(&credential_url, OTHER_TENANT, "foreign --tenant").await;
        let run_retention_refused =
            expect_refusal(&run_retention_url, TENANT, "run retention generation").await;
        let superuser_refused = expect_refusal(admin_url, TENANT, "superuser login").await;
        let untouched = positions(admin).await? == seeded;
        println!("  refusals left the history untouched: {untouched}");

        let authority_ok = authority_arm(admin, &credential_url).await?;

        let pruned = ctl_process::run_ops_checked(prune_argv(&credential_url, TENANT))
            .await
            .context("prune record history through wamn-ctl-ops")?;
        let stdout = String::from_utf8(pruned.stdout).context("prune output is UTF-8")?;
        print!("{stdout}");
        let reported = stdout.contains(&format!(
            "removed 5 entries from {SCHEMA}.shipment_history (retention P30D, tenant {TENANT})"
        )) && stdout.contains(&format!(
            "removed 3 entries from {SCHEMA}.pallet_history (retention P7D, tenant {TENANT})"
        )) && stdout
            .contains(&format!("pruned 2 history table(s) for tenant {TENANT}"));

        let observed = positions(admin).await?;
        let exact = observed == kept;
        if !exact {
            println!("  retained positions {observed:?}, expected {kept:?}");
        }
        // Spec tests 1 and 1a: the oldest retained entry of the live row is an
        // update, so its history is incomplete. The recent row keeps its insert.
        let oldest = |id: String| async move {
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
                .context("read the oldest retained entry")
                .map(|row| row.map(|row| row.get::<_, String>(0)))
        };
        let live_row_incomplete = oldest(shipment(1)).await?.as_deref() == Some("update");
        let deleted_row_gone = oldest(shipment(2)).await?.is_none();
        let recent_row_complete = oldest(shipment(3)).await?.as_deref() == Some("insert");
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
            .context("read entries that the verb wrote")?
            .get(0);
        let no_marker = markers == 0;

        let pass = foreign_tenant_refused
            && run_retention_refused
            && superuser_refused
            && untouched
            && authority_ok
            && reported
            && exact
            && live_row_incomplete
            && deleted_row_gone
            && recent_row_complete
            && no_marker;
        println!(
            "  foreign_tenant_refused={foreign_tenant_refused} \
             run_retention_refused={run_retention_refused} superuser_refused={superuser_refused} \
             untouched={untouched} authority_ok={authority_ok} reported={reported} exact={exact}"
        );
        println!(
            "  live_row_incomplete={live_row_incomplete} deleted_row_gone={deleted_row_gone} \
             recent_row_complete={recent_row_complete} no_marker={no_marker}"
        );
        Ok::<bool, anyhow::Error>(pass)
    }
    .await;
    let _ = drop_generation_role(admin, &credential_role).await;
    let _ = drop_generation_role(admin, &run_retention_role).await;
    result
}

pub async fn run(args: RecordHistoryRetentionArgs) -> anyhow::Result<()> {
    let admin_url = args.admin_database_url.context(
        "record-history-retention needs a superuser url: pass --admin-database-url / \
         WAMN_PG_ADMIN_URL",
    )?;
    println!("# wamn-gates record-history-retention (schema {SCHEMA}, tenant {TENANT})");
    let admin = connect(&admin_url, "admin").await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await
        .context("read the target database name")?
        .get(0);
    provision(&admin).await?;
    let package: PathBuf =
        std::env::temp_dir().join(format!("record-history-retention-{}", std::process::id()));
    write_package(&package).context("write the fixture package")?;
    let applied = ctl_process::run_checked([
        OsStr::new("apply-package"),
        OsStr::new("--package"),
        package.as_os_str(),
        OsStr::new("--database-url"),
        OsStr::new(&admin_url),
        OsStr::new("--tenant"),
        OsStr::new(TENANT),
    ])
    .await
    .context("apply the fixture package through wamn-ctl");

    let outcome = match applied {
        Ok(_) => gate(&admin, &admin_url, &database).await,
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_dir_all(&package);
    let _ = admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE;"
        ))
        .await;
    let pass = outcome?;
    println!("\nrecord-history-retention complete, overall PASS: {pass}");
    if !pass {
        bail!("the prune-record-history retention gate failed");
    }
    Ok(())
}

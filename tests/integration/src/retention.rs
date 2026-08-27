//! The `retention` subcommand: the live `prune-run-history` gate.
//!
//! Pure host-side (no wasm guest): it applies the REAL `deploy/sql/run-state.sql`
//! and `deploy/sql/run-queue.sql` into throwaway ephemeral schemas, mints the
//! REAL `wamn_run_retention` credential generation the production verb requires,
//! seeds aged run history for TWO tenants, then drives the REAL
//! `prune-run-history` verb through the `wamn-ctl-ops` process — the same
//! `wamn_run_state::sql::prune_terminal_runs_sql` builder production uses.
//!
//! It proves four things, and the last three are the ones `wamn-0h0g.12.69`
//! exists for:
//!
//! 1. **POSITIVE.** Only OLD TERMINAL runs of the credential's own tenant are
//!    removed, and each pruned run's `run_queue` row cascades away with it —
//!    under a role holding NO privilege on `run_queue` at all.
//! 2. **NEGATIVE-NEGATIVE.** Asked to prune a tenant the mounted credential was
//!    not minted for, the verb REFUSES. It does not print
//!    `pruned 0 terminal run(s)` and exit 0, and the other tenant's rows are
//!    still there afterwards — the POST-STATE is read back, because an assertion
//!    that cannot tell REFUSED from MATCHED-NOTHING proves nothing here.
//! 3. **NOT THE SHARED LOGIN.** The same verb pointed at `wamn_app` refuses too,
//!    so the cutover cannot be silently reverted by editing a Secret.
//! 4. **THE PLATFORM ARM BUYS IT ALMOST NOTHING.** `wamn_run_retention` is a
//!    `wamn_platform` member — it has to be, or FORCE RLS default-denies it to
//!    zero rows — and that arm is `USING (true)`. What confines it is the GRANT
//!    SET, so this gate reads back from the server that the role cannot read a
//!    run payload column, cannot touch `run_queue`, and holds nothing outside
//!    `runs`. The residual it CANNOT close is asserted honestly rather than
//!    papered over: see [`platform_membership_arm`].
//!
//! Extracted from the retired `capturebench` harness (wamn-x1gy). Retention is a
//! live ops verb and this is its only watcher, so the phase survives its harness
//! — under a name that says what it proves.

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, compose_url, sql,
    workload_generation_role,
};

use crate::ctl_process;

const SCHEMA: &str = "wamn_retention";
const CATALOG_SCHEMA: &str = "wamn_retention_catalog";
const TENANT: &str = "retention-t";
/// A SECOND tenant with history of its own. Without it the cross-tenant arm
/// could not distinguish a refusal from a match of nothing.
const OTHER_TENANT: &str = "retention-other";
const CATALOG_ID: &str = "retention-fixture";
const RETENTION_DAYS: &str = "30";
const GENERATION_PASSWORD: &str = "retention-gate-generation";

#[derive(Debug, Args)]
pub struct RetentionArgs {
    /// LEGACY AND IGNORED. The gate mints its own scoped `wamn_run_retention`
    /// credential generation from `--admin-database-url`, because a generation's
    /// password is created WITH it and cannot be handed in from outside.
    /// Retained so existing invocations still parse.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: applies/drops the ephemeral run-plane schemas and mints
    /// the retention credential generation.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Ephemeral schemas: the REAL run-state.sql + run-queue.sql, schema-rewritten
// (no stand-in DDL, so the `runs` shape and its ACL can never drift from the
// schema of record). Only the `catalog.releases` foreign key reaches outside the
// run plane, so that is the only fixture relation this gate stands up.
// ---------------------------------------------------------------------------

/// The DOT-ANCHORED rewrite `wamn_schema_control::run_plane::rewrite_schema`
/// performs, restated here because this gate has no dependency on that crate.
///
/// A naive `replace("wamn_run", SCHEMA)` is WRONG, and is the shape this gate
/// used to carry: `wamn_run_retention` — the stable ACL role `run-state.sql` now
/// creates and grants to — has `wamn_run` as a prefix, so the naive form
/// rewrites the ROLE NAME into `wamn_retention_retention` and the gate then
/// measures the ACL of a role that exists nowhere else.
fn rewrite_schema(ddl: &str) -> String {
    ddl.replace(
        "SET search_path = pg_catalog, wamn_run, pg_temp",
        &format!("SET search_path = pg_catalog, {SCHEMA}, pg_temp"),
    )
    .replace("wamn_run.", &format!("{SCHEMA}."))
    .replace(
        "SCHEMA IF NOT EXISTS wamn_run",
        &format!("SCHEMA IF NOT EXISTS {SCHEMA}"),
    )
    .replace("SCHEMA wamn_run", &format!("SCHEMA {SCHEMA}"))
    .replace("catalog.releases", &format!("{CATALOG_SCHEMA}.releases"))
}

fn run_state_ddl() -> String {
    rewrite_schema(include_str!("../../../deploy/sql/run-state.sql"))
}

fn run_queue_ddl() -> String {
    rewrite_schema(include_str!("../../../deploy/sql/run-queue.sql"))
}

async fn connect(
    url: &str,
    purpose: &str,
) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| format!("{purpose} connect"))?;
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok((client, handle))
}

async fn admin_exec(admin_url: &str, sql: &str) -> anyhow::Result<()> {
    let (client, handle) = connect(admin_url, "admin").await?;
    // `.context`, not `anyhow!("{e}")`: tokio_postgres::Error's Display is only
    // its kind ("db error"); the server's message — e.g. `role
    // "wamn_scenario_author" does not exist` — hangs off `source()`. Formatting
    // the error away collapsed every provisioning failure to "admin exec: db
    // error"; chaining keeps the cause in the `Caused by:` report.
    let r = client.batch_execute(sql).await.context("admin exec");
    drop(client);
    let _ = handle.await;
    r
}

async fn admin_scalar<T>(admin_url: &str, statement: &str) -> anyhow::Result<T>
where
    T: for<'a> tokio_postgres::types::FromSql<'a>,
{
    let (client, handle) = connect(admin_url, "admin").await?;
    let value = client
        .query_one(statement, &[])
        .await
        .context("admin query")
        .map(|row| row.get::<_, T>(0));
    drop(client);
    let _ = handle.await;
    value
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS {CATALOG_SCHEMA} CASCADE; \
             CREATE SCHEMA {CATALOG_SCHEMA}; \
             CREATE TABLE {CATALOG_SCHEMA}.releases ( \
               tenant_id text NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL, \
               PRIMARY KEY (tenant_id, catalog_id, catalog_version) \
             ); \
             INSERT INTO {CATALOG_SCHEMA}.releases \
             VALUES ('{TENANT}','{CATALOG_ID}',1), ('{OTHER_TENANT}','{CATALOG_ID}',1);"
        ),
    )
    .await?;
    admin_exec(admin_url, &run_state_ddl()).await?;
    admin_exec(admin_url, &run_queue_ddl()).await?;
    admin_exec(
        admin_url,
        &format!(
            "INSERT INTO {SCHEMA}.environment_policies \
               (tenant_id, expected_environment, durability_class) \
             VALUES ('{TENANT}', 'test', 'standard'), \
                    ('{OTHER_TENANT}', 'test', 'standard');"
        ),
    )
    .await
}

async fn teardown(admin_url: &str, generation_role: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS {CATALOG_SCHEMA} CASCADE;"
        ),
    )
    .await?;
    drop_generation_role(admin_url, generation_role).await
}

/// `DROP OWNED BY` then `DROP ROLE`, INSIDE an existence check.
///
/// Roles are CLUSTER-global, so a leftover healthy generation from an earlier
/// run would sit happily inside every `IF NOT EXISTS` this gate issues and mask
/// a mutated builder. `DROP OWNED BY` reaches only the CURRENT database, so a
/// role that owns something in another database refuses to drop and names that
/// database in the DETAIL — which is the loud failure a dirty cluster deserves,
/// not something to swallow.
async fn drop_generation_role(admin_url: &str, role: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!(
            "DO $retention_gate$ BEGIN \
               IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $retention_gate$;"
        ),
    )
    .await
}

/// Mint the exact credential generation the production verb demands, through
/// the PRODUCTION builders — `prepare_workload_generation_sql` for the login and
/// `platform_group_membership_sql` for the floor edge — so a mutated builder
/// fails here instead of being transcribed around.
async fn mint_retention_credential(
    admin_url: &str,
    database: &str,
    role: &str,
) -> anyhow::Result<()> {
    drop_generation_role(admin_url, role).await?;
    admin_exec(
        admin_url,
        &sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::Retention,
            database,
            role,
            GENERATION_PASSWORD,
            "2100-01-01T00:00:00Z",
        ),
    )
    .await
    .context("prepare the retention credential generation")?;
    admin_exec(
        admin_url,
        &sql::platform_group_membership_sql(WorkloadRoleFamily::Retention),
    )
    .await
    .context("converge the retention platform-group edge")
}

/// A superuser fixture masks RLS. Every arm below runs as this generation, so
/// the gate first proves the role it measures is neither superuser nor
/// bypassing — and that it inherits, since the whole floor chain is per-edge.
async fn assert_not_super_or_bypassing(admin_url: &str, role: &str) -> anyhow::Result<()> {
    let clean: bool = admin_scalar(
        admin_url,
        &format!(
            "SELECT NOT (rolsuper OR rolbypassrls) AND rolcanlogin AND rolinherit \
               FROM pg_catalog.pg_roles WHERE rolname = '{role}'"
        ),
    )
    .await
    .context("read the retention generation's attributes")?;
    if !clean {
        bail!(
            "the retention generation {role} is superuser, BYPASSRLS, cannot log in, or does not \
             inherit — every arm below would prove nothing"
        );
    }
    Ok(())
}

/// Insert a `runs` row whose `created_at` is `now()` shifted back `age_days`
/// days, plus its queue row, so the gate can seed aged history and watch the
/// cascade. Fixture-only superuser seed: production run admission is available
/// only through the private native run-state adapter.
async fn seed_run(
    admin_url: &str,
    tenant: &str,
    run_id: &str,
    status: &str,
    age_days: i64,
) -> anyhow::Result<()> {
    let (client, handle) = connect(admin_url, "admin seed-run").await?;
    let result = async {
        client
            .execute(
                &format!(
                    "INSERT INTO {SCHEMA}.runs ( \
                       tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, \
                       environment, status, input_json, created_at \
                     ) VALUES ($1, $2, 'f', 1, '{CATALOG_ID}', 1, 'test', $3, \
                               jsonb_build_object('payload', $1::text), \
                               now() - ($4::bigint * interval '1 day'))"
                ),
                &[&tenant, &run_id, &status, &age_days],
            )
            .await
            .context("seed run")?;
        client
            .execute(
                &format!("INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ($1, $2)"),
                &[&tenant, &run_id],
            )
            .await
            .context("seed queue row")?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = handle.await;
    result
}

async fn row_exists(
    admin_url: &str,
    relation: &str,
    tenant: &str,
    run_id: &str,
) -> anyhow::Result<bool> {
    let (client, handle) = connect(admin_url, "admin post-state").await?;
    let r = client
        .query_one(
            &format!(
                "SELECT count(*) FROM {SCHEMA}.{relation} WHERE tenant_id = $1 AND run_id = $2"
            ),
            &[&tenant, &run_id],
        )
        .await
        .context("read post-state")
        .map(|row| row.get::<_, i64>(0) == 1);
    drop(client);
    let _ = handle.await;
    r
}

fn prune_argv<'a>(url: &'a str, tenant: &'a str) -> [&'a str; 9] {
    [
        "prune-run-history",
        "--database-url",
        url,
        "--schema",
        SCHEMA,
        "--tenant",
        tenant,
        "--retention-days",
        RETENTION_DAYS,
    ]
}

/// Drive the verb and REQUIRE it to fail with the refusal — not with any other
/// error, and above all not with a success line reporting zero pruned.
async fn expect_refusal(url: &str, tenant: &str, label: &str) -> anyhow::Result<bool> {
    match ctl_process::run_ops_checked(prune_argv(url, tenant)).await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            println!("  {label}: NOT REFUSED — the verb succeeded. stdout: {stdout}");
            Ok(false)
        }
        Err(error) => {
            let rendered = format!("{error:#}");
            let refused = rendered.contains("refusing to prune");
            let silently_zero = rendered.contains("pruned 0 terminal run(s)");
            println!("  {label}: refused={refused} reported_zero_success={silently_zero}");
            if !refused {
                println!("  {label}: the failure was NOT the refusal: {rendered}");
            }
            Ok(refused && !silently_zero)
        }
    }
}

/// What the shared `wamn_platform` arm does and does not buy the retention role,
/// read back FROM THE SERVER under the generation itself.
///
/// The membership is not optional and this gate does not pretend otherwise:
/// `wamn_run.runs` FORCEs RLS, its tenant arm is `TO wamn_app`, PostgreSQL
/// default-denies when no policy matches the connected role, and the one arm a
/// platform-grain family matches is `TO wamn_platform USING (true)`. Revoke the
/// edge and retention reads and deletes zero rows in silence — measured.
///
/// So the confinement is grant-shaped, and these are the grants doing it: the
/// three `WHERE`-clause columns are readable, every other column is not,
/// `run_queue` is not, and nothing outside `runs` is. The one thing this arm
/// asserts as PRESENT rather than absent is the residual — under `USING (true)`
/// the role can still read those three columns for another tenant, and
/// PostgreSQL privileges are relation- and column-shaped rather than row-shaped,
/// so no grant closes it. `prune-run-history`'s identity refusal is what closes
/// it for the verb; closing it for a raw session is not this bead's.
async fn platform_membership_arm(credential_url: &str) -> anyhow::Result<bool> {
    let (client, handle) = connect(credential_url, "retention generation").await?;

    let mut ok = true;
    let mut check = |label: &str, actual: bool, expected: bool| {
        if actual == expected {
            println!("  platform-arm {label}: {actual}");
        } else {
            println!("  platform-arm {label}: expected {expected}, got {actual}");
            ok = false;
        }
    };

    // The three WHERE-clause columns are readable; a payload column is not.
    let three_columns = client
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE status = 'completed'"),
            &[],
        )
        .await
        .is_ok();
    check("reads the three WHERE columns", three_columns, true);

    let payload = client
        .query_one(
            &format!("SELECT input_json FROM {SCHEMA}.runs LIMIT 1"),
            &[],
        )
        .await
        .is_ok();
    check("reads a run payload column", payload, false);

    let star = client
        .query_one(&format!("SELECT * FROM {SCHEMA}.runs LIMIT 1"), &[])
        .await
        .is_ok();
    check("reads SELECT *", star, false);

    let queue = client
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.run_queue"), &[])
        .await
        .is_ok();
    check("reads run_queue directly", queue, false);

    // The independent terminal-only trigger still refuses a raw non-terminal
    // DELETE under this role — the grant bounds WHO, the trigger bounds WHAT,
    // and neither was absorbed into the other.
    let refused_nonterminal = client
        .execute(
            &format!("DELETE FROM {SCHEMA}.runs WHERE tenant_id = $1 AND status = 'running'"),
            &[&TENANT],
        )
        .await
        .err()
        .is_some_and(|error| format!("{error:?}").contains("run-delete-nonterminal"));
    check(
        "trigger refuses a raw non-terminal DELETE",
        refused_nonterminal,
        true,
    );

    // Cluster-wide, the stable ACL role holds nothing outside `runs`. Asked of
    // the SERVER's catalogs, not of the statement that created the grants.
    let stray: i64 = client
        .query_one(
            "SELECT count(*) FROM pg_catalog.pg_class c \
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
               CROSS JOIN unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE', \
                                       'REFERENCES','TRIGGER']) p \
              WHERE c.relkind IN ('r','p') \
                AND n.nspname NOT IN ('pg_catalog','information_schema') \
                AND c.relname <> 'runs' \
                AND pg_catalog.has_table_privilege('wamn_run_retention', c.oid, p)",
            &[],
        )
        .await
        .context("sweep the stable retention role for stray table privileges")?
        .get(0);
    check("holds zero privileges outside runs", stray == 0, true);

    // THE RESIDUAL, asserted as present rather than wished away: under
    // `USING (true)` the credential still sees the other tenant's three columns.
    let cross_tenant: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE tenant_id = $1"),
            &[&OTHER_TENANT],
        )
        .await
        .context("read the other tenant's rows under the platform arm")?
        .get(0);
    check(
        "RESIDUAL: the platform arm still exposes another tenant's three columns",
        cross_tenant > 0,
        true,
    );

    drop(client);
    let _ = handle.await;
    Ok(ok)
}

async fn retention_gate(admin_url: &str, credential_url: &str) -> anyhow::Result<bool> {
    println!(
        "\n## retention — the real prune-run-history verb, as its own scoped credential, prunes \
         old TERMINAL runs of ITS OWN tenant and refuses everything else"
    );

    // Seed: an old completed run, a recent completed run, and an OLD but RUNNING
    // run (the terminal-only guard) — plus a SECOND tenant's old completed run,
    // which nothing in this gate is allowed to remove.
    seed_run(admin_url, TENANT, "old-done", "completed", 40).await?;
    seed_run(admin_url, TENANT, "recent-done", "completed", 1).await?;
    seed_run(admin_url, TENANT, "old-running", "running", 40).await?;
    seed_run(admin_url, OTHER_TENANT, "foreign-done", "completed", 40).await?;

    // The refusal arms run FIRST, against a POPULATED store: a refusal measured
    // on an empty table could not be told from a match of nothing.
    let cross_tenant_refused =
        expect_refusal(credential_url, OTHER_TENANT, "cross-tenant --tenant").await?;
    let unknown_tenant_refused =
        expect_refusal(credential_url, "no-such-tenant", "unknown --tenant").await?;
    let foreign_survived_refusal =
        row_exists(admin_url, "runs", OTHER_TENANT, "foreign-done").await?;

    // The shared login the cutover retired is refused outright.
    let app_url = replace_role(credential_url, APP_ROLE, APP_ROLE)?;
    let shared_login_refused = expect_refusal(&app_url, TENANT, "wamn_app credential").await?;

    let platform_arm_ok = platform_membership_arm(credential_url).await?;

    // The positive path, last, because it is the only arm that mutates.
    let prune = ctl_process::run_ops_checked(prune_argv(credential_url, TENANT))
        .await
        .context("prune through wamn-ctl-ops")?;
    let prune_stdout = String::from_utf8(prune.stdout).context("prune output is UTF-8")?;
    let reported_one = prune_stdout.contains("pruned 1 terminal run(s)");

    let old_gone = !row_exists(admin_url, "runs", TENANT, "old-done").await?;
    let old_queue_cascaded = !row_exists(admin_url, "run_queue", TENANT, "old-done").await?;
    let recent_kept = row_exists(admin_url, "runs", TENANT, "recent-done").await?;
    let running_kept = row_exists(admin_url, "runs", TENANT, "old-running").await?;
    let foreign_kept = row_exists(admin_url, "runs", OTHER_TENANT, "foreign-done").await?;
    let foreign_queue_kept =
        row_exists(admin_url, "run_queue", OTHER_TENANT, "foreign-done").await?;

    let pass = reported_one
        && old_gone
        && old_queue_cascaded
        && recent_kept
        && running_kept
        && foreign_kept
        && foreign_queue_kept
        && cross_tenant_refused
        && unknown_tenant_refused
        && foreign_survived_refusal
        && shared_login_refused
        && platform_arm_ok;
    println!(
        "  reported_one={reported_one} old_gone={old_gone} \
         old_queue_cascaded={old_queue_cascaded} recent_kept={recent_kept} \
         running_kept={running_kept} foreign_kept={foreign_kept} \
         foreign_queue_kept={foreign_queue_kept}"
    );
    println!(
        "  cross_tenant_refused={cross_tenant_refused} \
         unknown_tenant_refused={unknown_tenant_refused} \
         foreign_survived_refusal={foreign_survived_refusal} \
         shared_login_refused={shared_login_refused} platform_arm_ok={platform_arm_ok}"
    );
    println!(
        "PASS(retention: own-tenant terminal pruned with its queue row, everything else refused): \
         {pass}"
    );
    Ok(pass)
}

/// Re-point a connection URL at a different role, keeping host/port/database.
///
/// Parsed through `tokio_postgres::Config`, the same parser the connection
/// itself uses, so a URL this gate can compose is a URL the driver accepts.
fn replace_role(url: &str, role: &str, password: &str) -> anyhow::Result<String> {
    let config: tokio_postgres::Config = url.parse().context("parse the connection URL")?;
    let host = match config.get_hosts() {
        [tokio_postgres::config::Host::Tcp(host)] => host.clone(),
        hosts => bail!("the retention gate needs a single TCP host, got {hosts:?}"),
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

pub async fn run(args: RetentionArgs) -> anyhow::Result<()> {
    if args.database_url.is_some() {
        println!(
            "note: --database-url is ignored; the gate mints its own retention credential \
             generation (wamn-0h0g.12.69)"
        );
    }
    let admin_url = args.admin_database_url.clone().context(
        "retention needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;

    println!("# wamn-gates retention (schema {SCHEMA}, tenant {TENANT})");
    provision(&admin_url)
        .await
        .context("provision ephemeral run-plane schemas")?;

    let database: String = admin_scalar(&admin_url, "SELECT current_database()::text")
        .await
        .context("read the target database name")?;
    // DERIVED, never typed: this is the same derivation the verb recomputes to
    // decide whether to refuse, so a drift in either end fails here.
    let generation_role = workload_generation_role(
        WorkloadRoleFamily::Retention,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database: &database,
        },
        CredentialGeneration::A,
    )
    .context("derive the retention generation identity")?;
    println!("retention credential generation: {generation_role}");

    mint_retention_credential(&admin_url, &database, &generation_role).await?;
    assert_not_super_or_bypassing(&admin_url, &generation_role).await?;
    let credential_url = replace_role(&admin_url, &generation_role, GENERATION_PASSWORD)?;

    let outcome = retention_gate(&admin_url, &credential_url).await;

    let _ = teardown(&admin_url, &generation_role).await;
    let pass = outcome?;

    println!("\nretention complete — overall PASS: {pass}");
    if !pass {
        bail!("the prune-run-history retention gate failed");
    }
    Ok(())
}

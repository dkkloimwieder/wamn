//! impactproof — the 11.8 schema-change impact-analysis gate (wamn-wvb).
//!
//! The in-cluster gate for the dependency graph a staged migration touches. It
//! runs the WHOLE analysis against a throwaway Postgres (`WAMN_PG_ADMIN_URL`
//! superuser to provision, `WAMN_PG_URL` app role for the RLS-scoped seed) in an
//! EPHEMERAL schema it owns end to end — ADDITIVE ONLY, no DDL applied, dropped at
//! the end (the suiteproof shape):
//!
//!   1. provision the run-plane + flow-tests tables through the SAME `ensure_*`
//!      path production provisioning uses (`publish-catalog --runstate`);
//!   2. seed one active flow whose postgres node names entity `orders` by NAME
//!      (the config-keyed edge) + a version-bound suite;
//!   3. compile a v1→v2 plan IN MEMORY (drop a column on `orders` = destructive)
//!      and read the live edges through `wamn-ctl-ops impact-report`
//!      — asserting it names the seeded flow + suite + `/api/rest/orders`, and that
//!      the DESTRUCTIVE change with a dependent flow carries reprovision guidance
//!      while an ADDITIVE change on the same entity does NOT.
//!
//! Self-contained: it seeds only its ephemeral schema and drops it at the end. The
//! event-registration edge (which reads the shared `catalog.event_registrations`)
//! is proven by the throwaway-PG live gate (`wamn-ctl tests/impact_report_live.rs`),
//! not here — an in-cluster Job must never mutate a shared schema.

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};

use wamn_gate_harness::{check, scope_session, seed_flow_version, seed_test_suite};
use wamn_schema_model::Catalog;

use crate::ctl_process;
const FLOW_ID: &str = "impactproof-flow";
const SUITE_ID: &str = "smoke";
const REPROVISION_NOTE: &str = "NOTE: this destructive change has dependent flows or suites; use \
     this report when reprovisioning the environment.";

#[derive(Debug, Args)]
pub struct ImpactProofArgs {
    /// App (wamn_app, NOSUPERUSER) Postgres URL. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL — provisions the ephemeral schema + run-plane/test tables AND
    /// runs the cross-tenant (RLS-bypassing) impact reads.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The ephemeral schema this gate owns (dropped at the end).
    #[arg(long, default_value = "wamn_impactproof")]
    pub schema: String,

    /// The owning tenant the flow + suite are seeded under.
    #[arg(long, default_value = "demo-tenant")]
    pub tenant: String,

    /// Keep the schema at the end (default drops it).
    #[arg(long)]
    pub keep: bool,
}

/// A catalog document with `orders` (the entity the seeded flow references) and
/// `audit` (a bystander). `orders_fields` / `audit_fields` are the field-id lists.
fn cat(version: u32, orders_fields: &[&str], audit_fields: &[&str]) -> Catalog {
    let flds = |fs: &[&str]| -> String {
        fs.iter()
            .map(|f| format!(r#"{{"id":"{f}","name":"{f}","type":{{"kind":"text"}}}}"#))
            .collect::<Vec<_>>()
            .join(",")
    };
    let json = format!(
        r#"{{"schema-version":"0.1","catalog-id":"impactproof-shop","version":{version},"entities":[
             {{"id":"orders","name":"orders","fields":[{}]}},
             {{"id":"audit","name":"audit","fields":[{}]}}
           ]}}"#,
        flds(orders_fields),
        flds(audit_fields),
    );
    Catalog::from_json(&json).expect("impactproof catalog fixture parses")
}

/// The seeded flow: one active postgres node reading entity `orders` BY NAME.
fn flow_graph() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{FLOW_ID}","version":1,"nodes":[{{"id":"n","type":"postgres","config":{{"entity":"orders","op":"get"}}}}],"edges":[]}}"#
    )
}

async fn connect(url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .context("postgres connect")?;
    let task = tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok((client, task))
}

pub async fn run(args: ImpactProofArgs) -> anyhow::Result<()> {
    if !is_bare_ident(&args.schema) {
        bail!(
            "--schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.schema
        );
    }
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;

    println!(
        "# wamn-gates impactproof — 11.8 schema-change impact analysis (schema {}, tenant {})",
        args.schema, args.tenant
    );

    // --- provision (superuser) through the production ensure_* path ---
    let (admin, admin_task) = connect(&admin_url).await?;
    provision(&admin, &admin_url, &args.schema).await?;

    // --- the analysis: v1 -> v2 drops orders.note (DESTRUCTIVE) -------------
    let mut ok = true;
    let v1 = cat(1, &["status", "note"], &["kind"]);
    let v2_destructive = cat(2, &["status"], &["kind", "ts"]); // orders drops note; audit adds ts
    let v1_path = write_catalog_fixture("v1", &v1)?;
    let destructive_path = write_catalog_fixture("destructive", &v2_destructive)?;
    ctl_process::run_checked([
        "migrate-catalog",
        "--admin-database-url",
        &admin_url,
        "--tenant",
        &args.tenant,
        "--schema",
        &args.schema,
        "--target",
        v1_path.to_str().context("v1 fixture path is not UTF-8")?,
        "--skip-reconcile-replica-identity",
    ])
    .await
    .context("apply current catalog through wamn-ctl")?;

    // Seed the dependent flow only after the first release exists. First-release
    // publication deliberately refuses legacy flow rows without release sources.
    let (app, app_task) = connect(&app_url).await?;
    scope_session(&app, &args.tenant, &args.schema).await?;
    seed_flow_version(&app, &args.tenant, FLOW_ID, 1, true, &flow_graph(), true)
        .await
        .context("seed active flow")?;
    seed_test_suite(
        &app,
        &args.tenant,
        FLOW_ID,
        1,
        SUITE_ID,
        "impactproof smoke",
    )
    .await
    .context("seed suite")?;
    drop(app);
    let _ = app_task.await;

    let destructive = ctl_process::run_ops_checked([
        "impact-report",
        "--admin-database-url",
        &admin_url,
        "--tenant",
        &args.tenant,
        "--schema",
        &args.schema,
        "--target",
        destructive_path
            .to_str()
            .context("destructive fixture path is not UTF-8")?,
    ])
    .await
    .context("run destructive impact report")?;
    let report = String::from_utf8(destructive.stdout).context("impact output is UTF-8")?;
    print!("{report}");

    check(
        &mut ok,
        "EDGE: the destructively-changed `orders` entity is reported",
        report.contains("[DESTRUCTIVE] entity \"orders\" (id \"orders\")"),
    );
    check(
        &mut ok,
        "NODE-CONFIG: the flow reading `orders` by NAME is found",
        report.contains(&format!(
            "flow via node config:  tenant {:?} flow {:?} v1",
            args.tenant, FLOW_ID
        )) && report.contains("(config entity \"orders\")"),
    );
    check(
        &mut ok,
        "SUITE: the affected flow's suite is enumerated",
        report.contains(&format!(
            "suite: tenant {:?} flow {:?} v1 suite {:?}",
            args.tenant, FLOW_ID, SUITE_ID
        )),
    );
    check(
        &mut ok,
        "API: the entity's generated REST resource is named",
        report.contains("api: /api/rest/orders"),
    );
    check(
        &mut ok,
        "BYSTANDER: `audit` is reported additive (not destructive)",
        report.contains("[additive   ] entity \"audit\" (id \"audit\")"),
    );
    check(
        &mut ok,
        "OPS: a destructive change with a dependent flow carries reprovision guidance",
        report.contains(REPROVISION_NOTE),
    );

    // --- the negative: an ADDITIVE change omits reprovision guidance --------
    let v2_additive = cat(2, &["status", "note", "extra"], &["kind"]); // orders ADDs a column
    let additive_path = write_catalog_fixture("additive", &v2_additive)?;
    let additive = ctl_process::run_ops_checked([
        "impact-report",
        "--admin-database-url",
        &admin_url,
        "--tenant",
        &args.tenant,
        "--schema",
        &args.schema,
        "--target",
        additive_path
            .to_str()
            .context("additive fixture path is not UTF-8")?,
    ])
    .await
    .context("run additive impact report")?;
    let add_report = String::from_utf8(additive.stdout).context("impact output is UTF-8")?;
    check(
        &mut ok,
        "OPS: an additive change on the SAME dependent entity omits reprovision guidance",
        !add_report.contains(REPROVISION_NOTE)
            && add_report.contains("[additive   ] entity \"orders\" (id \"orders\")"),
    );
    for path in [&v1_path, &destructive_path, &additive_path] {
        let _ = std::fs::remove_file(path);
    }

    // --- teardown ---
    if !args.keep {
        admin
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {} CASCADE", args.schema))
            .await
            .context("drop ephemeral schema")?;
    }
    drop(admin);
    let _ = admin_task.await;

    println!("\nimpactproof complete — overall PASS: {ok}");
    if !ok {
        bail!("impactproof failed");
    }
    Ok(())
}

/// Fresh ephemeral schema reconciled through the public ctl boundary.
async fn provision(admin: &Client, admin_url: &str, schema: &str) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
               CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
             END IF; END $$; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE \
                 ALTER ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE \
                 ALTER ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$; \
             REVOKE wamn_scenario_author FROM wamn_app; \
             DO $$ BEGIN \
               EXECUTE format( \
                 'REVOKE CONNECT ON DATABASE %I FROM PUBLIC', current_database() \
               ); \
               EXECUTE format( \
                 'GRANT CONNECT ON DATABASE %I TO wamn_app', current_database() \
               ); \
             END $$;"
        ))
        .await
        .context("reset schema + ensure run-plane roles")?;
    ctl_process::run_checked([
        "reconcile-run-plane",
        "--admin-database-url",
        admin_url,
        "--schema",
        schema,
    ])
    .await
    .context("reconcile run-plane through wamn-ctl")?;
    println!("## provisioned schema {schema} (run-state + flows + test_suites/test_cases)");
    Ok(())
}

fn write_catalog_fixture(label: &str, catalog: &Catalog) -> anyhow::Result<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "wamn-impactproof-{}-{label}.json",
        std::process::id()
    ));
    std::fs::write(&path, catalog.to_json())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// A bare lowercase SQL identifier (the ephemeral schema is interpolated).
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate's fixture catalogs are a valid destructive-then-additive pair: a
    /// broken fixture fails here, not only against a live Postgres.
    #[test]
    fn fixture_catalogs_are_valid_and_shaped() {
        let v1 = cat(1, &["status", "note"], &["kind"]);
        assert_eq!(v1.entities.len(), 2);
        let plan =
            wamn_schema_compiler::Migration::migrate(&v1, &cat(2, &["status"], &["kind", "ts"]))
                .expect("destructive plan compiles");
        assert!(plan.is_destructive(), "dropping a column is destructive");
        // The seeded flow references entity `orders` by name.
        assert!(flow_graph().contains(r#""entity":"orders""#));
    }

    #[test]
    fn bare_ident_rejects_injection() {
        assert!(is_bare_ident("wamn_impactproof"));
        assert!(!is_bare_ident("wamn-impactproof"));
        assert!(!is_bare_ident("a; DROP"));
        assert!(!is_bare_ident("Cap"));
    }
}

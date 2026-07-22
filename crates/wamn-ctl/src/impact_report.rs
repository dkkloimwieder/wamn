//! The `impact-report` subcommand (11.8): the read-only **effect shell** for
//! `wamn-impact` — it reads the current applied catalog + a `--target`, compiles
//! the migration plan (the same wamn-ddl compiler `migrate-catalog` uses), reads
//! the dependency edges (event registrations, active flow graphs, test suites)
//! across ALL tenants on a superuser connection, and prints the typed
//! [`wamn_impact::ImpactReport`]. It **mutates nothing** — the schema-designer
//! surface for "what breaks if I apply this".
//!
//! The heavy lifting ([`gather_impact`]) is shared with `migrate-catalog`, which
//! renders the same report on every dry-run and apply and gates a destructive
//! plan with dependents behind `--acknowledge-impact`. The pure decision is
//! `wamn_impact::analyze`; this shell only holds the connection (SR6).
//!
//! **Tenant scoping.** The registration/flow/suite reads are CROSS-TENANT (the
//! superuser bypasses RLS): a shared entity's change hits every tenant's flows and
//! suites, so the report must see them all — the per-edge lines carry their
//! tenant. Suite EXECUTION is out of scope (parked wamn-0lfu); the report
//! enumerates the `(tenant, flow_id, flow_version, suite_id)` tuples that WOULD
//! run.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;

use wamn_catalog::Catalog;
use wamn_ddl::{Migration, MigrationPlan};
use wamn_impact::{FlowGraph, ImpactInput, ImpactReport, RegistrationEdge, SuiteEdge, analyze};
use wamn_migrate::Env;

use crate::migrate_catalog::{is_bare_ident, read_current_applied};

#[derive(Debug, Args)]
pub struct ImpactReportArgs {
    /// Superuser Postgres URL to the PROJECT database (the `catalog` metadata
    /// schema + the data/flow schema). Cross-tenant reads need the superuser (RLS
    /// bypass), like the D24 guard. Env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Tenant the catalog version is scoped to (the current-applied lookup key).
    /// The impact picture is inherently multi-tenant for the registration edge —
    /// the report is grouped by the affected entity and each edge names its tenant.
    #[arg(long)]
    pub tenant: String,

    /// Environment slug the catalog version is tagged with (default `dev`).
    #[arg(long, default_value = "dev")]
    pub environment: String,

    /// The schema holding the data tables AND the flow registry / test suites
    /// (`<schema>.flows`, `<schema>.test_suites`; the `catalog` metadata schema is
    /// fixed). This is the schema `publish-catalog --runstate` provisions them into.
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// Path to the target catalog JSON (crates/wamn-catalog `Catalog`).
    #[arg(long)]
    pub target: PathBuf,
}

pub async fn run(args: ImpactReportArgs) -> anyhow::Result<()> {
    if !is_bare_ident(&args.schema) {
        bail!(
            "--schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.schema
        );
    }
    let target_json = std::fs::read_to_string(&args.target)
        .with_context(|| format!("read target catalog {}", args.target.display()))?;
    let target = Catalog::from_json(&target_json).context("parse target catalog JSON")?;
    let env = Env::new(&args.environment);

    let (mut client, conn) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);

    // Read the current applied catalog read-only (drop the tx before any edge
    // read): the whole verb mutates nothing.
    let tx = client.transaction().await.context("begin")?;
    tx.batch_execute(&format!(
        "SET LOCAL search_path = {schema}, catalog",
        schema = args.schema
    ))
    .await
    .context("set search_path")?;
    let current = read_current_applied(&tx, &args.tenant, &target.catalog_id, env.as_str()).await?;
    drop(tx);

    let plan = compile_plan(current.as_ref(), &target)?;
    println!("-- schema diff --\n{}", plan.report());

    let impact = gather_impact(&client, &plan, current.as_ref(), &target, &args.schema).await?;
    conn_task.abort();
    println!("{}", impact.render());
    if impact.requires_acknowledgement() {
        println!(
            "NOTE: applying this migration requires --acknowledge-impact \
             (a destructive change with dependent flows/suites)."
        );
    }
    Ok(())
}

/// Compile the migration plan for impact analysis with the SAME wamn-ddl compiler
/// `migrate-catalog` applies — `migrate` from the current applied version, or a
/// whole-catalog `create` for a first materialization. The per-op entity +
/// additive/destructive classification is the authoritative "affected entities"
/// source (no SQL re-parse).
pub fn compile_plan(current: Option<&Catalog>, target: &Catalog) -> anyhow::Result<MigrationPlan> {
    match current {
        Some(c) => Migration::migrate(c, target),
        None => Migration::create(target),
    }
    .map_err(|e| anyhow::anyhow!("compile migration for impact analysis: {e}"))
}

/// Read the dependency edges for `plan` and fold them through
/// `wamn_impact::analyze`. Shared by `impact-report` and `migrate-catalog`.
///
/// Cross-tenant, superuser (RLS bypassed). Each read is `to_regclass`-probed so a
/// project that is not registration- or run-state-provisioned yet simply
/// contributes no edges (an absent table is a clean empty, not an error) — the
/// report still shows the entity change + its generated-API resources.
pub async fn gather_impact(
    client: &tokio_postgres::Client,
    plan: &MigrationPlan,
    current: Option<&Catalog>,
    target: &Catalog,
    schema: &str,
) -> anyhow::Result<ImpactReport> {
    // Edge 2: event registrations (id-keyed) — the D24 read + flow_id.
    let mut registrations = Vec::new();
    if table_present(client, "catalog.event_registrations").await? {
        let rows = client
            .query(
                &wamn_migrate::sql::select_registration_flow_refs_for_catalog_sql(),
                &[&target.catalog_id],
            )
            .await
            .context("read event registrations for impact analysis")?;
        for row in &rows {
            registrations.push(RegistrationEdge {
                registration_id: row.get(0),
                tenant: row.get(1),
                entity_id: row.get(2),
                flow_id: row.get(3),
            });
        }
    }

    // Edge 3: active flow graphs (name-keyed node config).
    let mut flows = Vec::new();
    if table_present(client, &format!("{schema}.flows")).await? {
        let rows = client
            .query(&wamn_migrate::sql::select_active_flows_sql(schema), &[])
            .await
            .context("read active flows for impact analysis")?;
        for row in &rows {
            let tenant: String = row.get(0);
            let graph_json: String = row.get(3);
            // A stored graph the CURRENT flow-schema cannot parse contributes no
            // node-config edge rather than failing the whole report (a report is
            // advisory; a poison row must not blind the operator to the rest).
            match wamn_flow::Flow::from_json(&graph_json) {
                Ok(flow) => flows.push(FlowGraph { tenant, flow }),
                Err(_) => continue,
            }
        }
    }

    // Edge 4: test suites (of the flows a change touches; the pure decision filters).
    let mut suites = Vec::new();
    if table_present(client, &format!("{schema}.test_suites")).await? {
        let rows = client
            .query(&wamn_migrate::sql::select_all_suites_sql(schema), &[])
            .await
            .context("read test suites for impact analysis")?;
        for row in &rows {
            suites.push(SuiteEdge {
                tenant: row.get(0),
                flow_id: row.get(1),
                flow_version: row.get(2),
                suite_id: row.get(3),
            });
        }
    }

    Ok(analyze(&ImpactInput {
        plan,
        current,
        target,
        registrations: &registrations,
        flows: &flows,
        suites: &suites,
    }))
}

/// Whether a (schema-qualified) relation exists — the D24 guard's probe shape.
/// `qualified` is built from a validated bare-identifier schema (or a fixed
/// `catalog.*` name), so the interpolation is safe.
async fn table_present(client: &tokio_postgres::Client, qualified: &str) -> anyhow::Result<bool> {
    Ok(client
        .query_one(
            &format!("SELECT to_regclass('{qualified}') IS NOT NULL"),
            &[],
        )
        .await
        .with_context(|| format!("probe {qualified}"))?
        .get(0))
}

//! The operations-only `impact-report` subcommand (11.8): the read-only **effect shell** for
//! `wamn-schema-control` — it reads the current applied catalog + a `--target`, compiles
//! the migration plan (the same wamn-schema-compiler compiler `migrate-catalog` uses), reads
//! the dependency edges (event registrations)
//! across ALL tenants on a superuser connection, and prints the typed
//! [`wamn_schema_control::impact::ImpactReport`]. It **mutates nothing** — the schema-designer
//! surface for "what breaks if I apply this".
//!
//! The pure decision is `wamn_schema_control::impact::analyze`; this shell only
//! holds the connection (SR6).
//!
//! **Tenant scoping.** The registration read is CROSS-TENANT (the superuser
//! bypasses RLS): a shared entity's change hits every tenant's flows, so the
//! report must see them all — the per-edge lines carry their tenant.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;

use wamn_schema_control::Env;
use wamn_schema_control::MigrationPlan;
use wamn_schema_control::impact::{ImpactInput, ImpactReport, RegistrationEdge, analyze};
use wamn_schema_model::Catalog;

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

    /// The schema holding the data tables (the `catalog` metadata schema is
    /// fixed).
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// Path to the target catalog JSON (crates/schema/model `Catalog`).
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

    let impact = gather_impact(&client, &plan, current.as_ref(), &target).await?;
    conn_task.abort();
    println!("{}", impact.render());
    if impact
        .entities
        .iter()
        .any(|entity| entity.destructive && entity.has_downstream_impact())
    {
        println!(
            "NOTE: this destructive change has dependent flows; use this report \
             when reprovisioning the environment."
        );
    }
    Ok(())
}

/// Compile a migration plan for operations impact analysis with the same
/// wamn-schema-compiler compiler the catalog applier uses — `migrate` from the
/// current applied version, or a whole-catalog `create` for a first
/// materialization. The per-op entity +
/// additive/destructive classification is the authoritative "affected entities"
/// source (no SQL re-parse).
pub fn compile_plan(current: Option<&Catalog>, target: &Catalog) -> anyhow::Result<MigrationPlan> {
    wamn_schema_control::ops::compile_migration(current, target)
        .map_err(|e| anyhow::anyhow!("compile migration for impact analysis: {e}"))
}

/// Read the dependency edges for `plan` and fold them through
/// `wamn_schema_control::impact::analyze`.
///
/// Cross-tenant, superuser (RLS bypassed). The read is `to_regclass`-probed so a
/// project that is not registration-provisioned yet simply contributes no edges
/// (an absent table is a clean empty, not an error) — the report still shows the
/// entity change + its generated-API resources.
pub async fn gather_impact(
    client: &tokio_postgres::Client,
    plan: &MigrationPlan,
    current: Option<&Catalog>,
    target: &Catalog,
) -> anyhow::Result<ImpactReport> {
    // Edge 2: event registrations (id-keyed) — the D24 read + flow_id.
    let mut registrations = Vec::new();
    if table_present(client, "catalog.event_registrations").await? {
        let rows = client
            .query(
                &wamn_schema_control::sql::select_registration_flow_refs_for_catalog_sql(),
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

    Ok(analyze(&ImpactInput {
        plan,
        current,
        target,
        registrations: &registrations,
    }))
}

/// Whether a (schema-qualified) relation exists — the D24 guard's probe shape.
/// `qualified` is a fixed `catalog.*` name, so the interpolation is safe.
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

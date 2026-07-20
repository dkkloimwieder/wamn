//! The `reconcile-replica-identity` subcommand (EVT-REPLICA-IDENT, wamn-l5i9.31):
//! the **effect shell** for the pure `wamn_migrate` REPLICA IDENTITY reconciler.
//!
//! `REPLICA IDENTITY FULL` is a per-entity knob the platform manages (l5i9.1
//! decision d): an entity runs FULL only when a registered row-event needs the
//! OLD image — any registration whose condition reads root `old` ("changed-to")
//! or that subscribes to `delete` (delete tenant-scoping / delete-payload
//! conditions need the old image) — and DEFAULT everywhere else (WAL stays
//! minimal; the global default is NEVER flipped). This shell reads the catalog's
//! registrations across ALL tenants (RI is per-TABLE, tables are shared, so the
//! requirement is the union), reads each table's CURRENT `pg_class.relreplident`,
//! calls the pure planner, and — unless `--dry-run` — runs the idempotent
//! `ALTER TABLE … REPLICA IDENTITY FULL|DEFAULT` flips.
//!
//! **Ownership:** `ALTER TABLE … REPLICA IDENTITY` needs table ownership — the
//! `wamn_app` role cannot run it — so this connects as a **superuser** (or the
//! schema owner), like `publish-catalog --provision` / `migrate-catalog`.
//!
//! **NON-RETROACTIVE:** a flip to FULL enriches only WAL written AFTER it. Events
//! captured before the flip permanently lack the old image; a newly registered
//! changed-to condition evaluates only from the flip forward (the materializer
//! treats an absent old image as cannot-evaluate, never condition-false).
//!
//! **Operational note:** run this whenever the catalog or its registrations
//! change (after `publish-catalog` / `migrate-catalog`, and after a
//! registration create/delete that adds or removes an old-image / delete
//! subscription). It is idempotent — a reconcile at the target state is a no-op.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;

use wamn_migrate::{
    EventRegistration, ReplicaIdentity, ReplicaIdentityPlan, reconcile_replica_identity, sql,
    select_replica_identity_sql,
};

#[derive(Debug, Args)]
pub struct ReconcileReplicaIdentityArgs {
    /// Superuser Postgres URL to the PROJECT database (holds the `catalog`
    /// metadata schema and the data schema). ALTER … REPLICA IDENTITY needs table
    /// ownership, so a superuser/schema-owner is required. Env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Path to the applied catalog JSON (the entity-id → table-name map the
    /// reconciler flips against — the same document you `publish-catalog`).
    #[arg(long)]
    pub catalog: PathBuf,

    /// The data schema the entity tables live in.
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// Print the reconcile plan (flips + no-ops + skipped) without applying it.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: ReconcileReplicaIdentityArgs) -> anyhow::Result<()> {
    if !crate::migrate_catalog::is_bare_ident(&args.schema) {
        bail!(
            "--schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.schema
        );
    }
    let catalog_src = std::fs::read_to_string(&args.catalog)
        .with_context(|| format!("read catalog {}", args.catalog.display()))?;
    let catalog = wamn_catalog::Catalog::from_json(&catalog_src)
        .map_err(|e| anyhow::anyhow!("catalog parse/validate: {e}"))?;

    let (client, conn) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let result = reconcile(&client, &catalog, &args.schema, !args.dry_run).await;
    drop(client);
    let _ = conn_task.await;
    let plan = result?;

    print_plan(&plan, args.dry_run);
    Ok(())
}

/// The reusable core: read the catalog's registrations (across all tenants) and
/// the schema's current `pg_class.relreplident`, plan the reconcile, and — when
/// `apply` — run the flips. Returns the plan (for reporting / gate assertions).
/// Shared by the CLI verb and the live gate so both exercise one code path.
pub async fn reconcile(
    client: &tokio_postgres::Client,
    catalog: &wamn_catalog::Catalog,
    schema: &str,
    apply: bool,
) -> anyhow::Result<ReplicaIdentityPlan> {
    let registrations = read_registrations(client, &catalog.catalog_id).await?;
    let current = read_current_identities(client, schema).await?;
    let plan = reconcile_replica_identity(catalog, &registrations, &current, schema);
    if apply {
        for flip in &plan.flips {
            client
                .batch_execute(&flip.sql)
                .await
                .with_context(|| format!("apply {}", flip.sql))?;
        }
    }
    Ok(plan)
}

/// Every event registration DOCUMENT for `catalog_id`, parsed. Across ALL tenants
/// (superuser bypasses RLS). A project not yet registration-provisioned (no
/// `catalog.event_registrations` table) has no registrations — a clean empty set,
/// so every entity reconciles to DEFAULT.
async fn read_registrations(
    client: &tokio_postgres::Client,
    catalog_id: &str,
) -> anyhow::Result<Vec<EventRegistration>> {
    let table_present: bool = client
        .query_one(
            "SELECT to_regclass('catalog.event_registrations') IS NOT NULL",
            &[],
        )
        .await
        .context("probe catalog.event_registrations")?
        .get(0);
    if !table_present {
        return Ok(Vec::new());
    }
    let rows = client
        .query(&sql::select_registration_docs_for_catalog_sql(), &[&catalog_id])
        .await
        .context("read event registrations for the RI reconcile")?;
    let mut regs = Vec::with_capacity(rows.len());
    for row in &rows {
        let doc: String = row.get(0);
        // A malformed stored registration is a hard error: RI is a correctness
        // knob, and silently skipping a delete/old-condition registration would
        // under-provision FULL (a cross-tenant delete leak / a corrupt eval).
        let reg = EventRegistration::from_json(&doc)
            .with_context(|| format!("parse stored registration document: {doc}"))?;
        regs.push(reg);
    }
    Ok(regs)
}

/// Every ordinary table's current REPLICA IDENTITY in `schema`, keyed by table
/// name (the pure planner folds it through `ReplicaIdentity`). Tables absent here
/// (floor not applied) are skipped by the planner.
async fn read_current_identities(
    client: &tokio_postgres::Client,
    schema: &str,
) -> anyhow::Result<BTreeMap<String, ReplicaIdentity>> {
    let rows = client
        .query(select_replica_identity_sql(), &[&schema])
        .await
        .context("read pg_class.relreplident")?;
    let mut current = BTreeMap::new();
    for row in &rows {
        let table: String = row.get(0);
        let ident: String = row.get(1);
        let c = ident.chars().next().unwrap_or('d');
        current.insert(table, ReplicaIdentity::from_relreplident(c));
    }
    Ok(current)
}

fn ident_kw(i: ReplicaIdentity) -> &'static str {
    match i {
        ReplicaIdentity::Full => "FULL",
        ReplicaIdentity::Default => "DEFAULT",
    }
}

fn print_plan(plan: &ReplicaIdentityPlan, dry_run: bool) {
    let verb = if dry_run { "would flip" } else { "flipped" };
    if plan.flips.is_empty() {
        println!("replica identity already reconciled — no flips ({} entities already at target)", plan.unchanged.len());
    } else {
        for f in &plan.flips {
            println!(
                "{verb} {} ({}): REPLICA IDENTITY {} -> {}",
                f.table,
                f.entity_id,
                ident_kw(f.from),
                ident_kw(f.to),
            );
        }
    }
    for t in &plan.skipped_absent {
        println!("  [skip] table {t} does not exist yet (floor not applied) — reconcile after publish-catalog --provision");
    }
}

//! The `publish-catalog` subcommand: write a project's catalog snapshot into the
//! `wamn_catalog` table the api-gateway component (4.1) reads at startup.
//!
//! In production the schema-designer→gateway seam writes this row whenever a
//! catalog version is applied/promoted (3.4); 4.1b provides the mechanism as a
//! reusable, idempotent host subcommand so a per-project gateway has a snapshot
//! to serve. It reads a catalog JSON (the applied catalog for a project),
//! `Catalog::to_json`s the canonical document, and UPSERTs it under the project's
//! tenant — connecting as a **superuser** so it bypasses the snapshot table's RLS
//! `WITH CHECK` and the runtime role's SELECT-only grant.
//!
//! `--provision` additionally stands up the schema and the 3.2 tenant floor (the
//! entity tables) when they are absent — used by the in-cluster `apiproof` gate
//! to give the deployed gateway real data to serve (the demo-row seeding rides
//! the gates-side `wamn-gates publish-catalog --seed` wrapper; the prod tool
//! carries no fixture content). Everything is **additive**: the schema is created
//! `IF NOT EXISTS`, the floor is applied only when missing, and no existing object
//! is ever dropped or altered (the shared-cluster guardrail).
//!
//! POC-F1 extended this into the one project-provisioning tool: `--runstate`
//! applies the run-state storage (`deploy/sql/run-state.sql`: runs/node_runs) and
//! the flow registry (`deploy/sql/flows.sql`) into the project schema — the
//! canonical deploy files, embedded at compile time and rewritten from
//! `wamn_run` to the target schema — when their tables are absent;
//! `--seed-dataset` compiles a wamn-seed (3.6) dataset against the catalog and
//! applies it (deterministic ids, `ON CONFLICT DO NOTHING` — idempotent); and
//! `--flow` validates a wamn-flow (5.1) graph and registers it ACTIVE in the
//! registry (deactivating prior versions of the same flow). The flows-table
//! `flow_id` column is written from the graph's own embedded flow-id, so the
//! column==graph equality the dispatcher enforces (wi4) holds by construction.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;

#[derive(Debug, Args)]
pub struct PublishCatalogArgs {
    /// Path to the catalog JSON to snapshot (the applied catalog for the project).
    #[arg(long)]
    pub catalog: PathBuf,

    /// Superuser Postgres URL — bypasses RLS to write the snapshot and (with
    /// `--provision`) create the schema/tables (env `WAMN_PG_ADMIN_URL`).
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Tenant the snapshot is published under (the gateway's `app.tenant` claim).
    #[arg(long)]
    pub tenant: String,

    /// Schema the `wamn_catalog` table (and, with `--provision`, the entity
    /// tables) live in; the gateway reaches them via the host-injected
    /// `search_path`.
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// Also create the schema + apply the 3.2 tenant floor for the catalog (the
    /// entity tables) when they are absent. Additive: never drops or alters.
    #[arg(long)]
    pub provision: bool,

    /// Also apply the run-state storage (runs/node_runs, `deploy/sql/run-state.sql`)
    /// and the flow registry (`deploy/sql/flows.sql`) into the schema when their
    /// tables are absent. Additive: never drops or alters.
    #[arg(long)]
    pub runstate: bool,

    /// Seed dataset JSON (wamn-seed, 3.6) compiled against the catalog and
    /// applied under `--tenant` (deterministic ids; idempotent re-apply).
    #[arg(long)]
    pub seed_dataset: Option<PathBuf>,

    /// Flow graph JSON (wamn-flow, 5.1) to validate, register, and ACTIVATE in
    /// the flow registry (repeatable; prior versions of the flow deactivate).
    #[arg(long)]
    pub flow: Vec<PathBuf>,

    /// Skip the post-publish REPLICA IDENTITY reconcile (EVT-RI-ORCH, l5i9.61).
    /// By default publish reconciles RI for the catalog's data schema so an
    /// entity that needs the old image is never left on DEFAULT; pass this to run
    /// `reconcile-replica-identity` separately instead.
    #[arg(long)]
    pub skip_reconcile_replica_identity: bool,
}

/// A bare SQL identifier safe to embed after validating: starts with a
/// LOWERCASE letter or `_`, then lowercase letters/digits/`_`. Lowercase-only
/// on purpose: the run-state rewrite emits the schema UNQUOTED (Postgres would
/// case-fold an uppercase name there while the quoted `publish` statements
/// preserved it — two different schemas). Mirrors the `wamn:postgres` check.
fn valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub async fn run(args: PublishCatalogArgs) -> anyhow::Result<()> {
    // Parse (and thereby validate) the catalog; this is the snapshot document.
    let catalog_src = std::fs::read_to_string(&args.catalog)
        .with_context(|| format!("read catalog {}", args.catalog.display()))?;
    let cat = wamn_catalog::Catalog::from_json(&catalog_src)
        .map_err(|e| anyhow::anyhow!("catalog parse/validate: {e}"))?;
    let document = cat.to_json();

    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin database url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;

    if !valid_ident(&args.schema) {
        bail!(
            "invalid schema name {:?}: must be a bare SQL identifier",
            args.schema
        );
    }

    let (client, conn) = tokio_postgres::connect(&admin_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let result = publish(&client, &cat, &args, &document).await;
    drop(client);
    let _ = conn_task.await;
    result?;

    println!(
        "published catalog snapshot: schema={} tenant={} (provision={})",
        args.schema, args.tenant, args.provision
    );
    Ok(())
}

async fn publish(
    client: &tokio_postgres::Client,
    cat: &wamn_catalog::Catalog,
    args: &PublishCatalogArgs,
    document: &str,
) -> anyhow::Result<()> {
    let schema = &args.schema;

    // D24 (EVT-REG, wamn-rmxa): refuse a publish that would drop an entity still
    // referenced by an event registration — BEFORE any mutation, naming every
    // orphan. The owner deletes the registrations via the API first; publish
    // never seeds or prunes them.
    guard_registration_orphans(client, cat).await?;

    // Ensure the non-superuser runtime role exists (pre-created in production).
    client
        .batch_execute(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await
        .context("ensure wamn_app role")?;

    // Create the schema if absent and pin this session's search_path to it, so
    // every statement below — and the parameterized UPSERT — resolves unqualified
    // names there, exactly as the gateway does via the host-injected search_path.
    client
        .batch_execute(&format!(
            "CREATE SCHEMA IF NOT EXISTS \"{schema}\"; \
             GRANT USAGE ON SCHEMA \"{schema}\" TO wamn_app; \
             SET search_path TO \"{schema}\";"
        ))
        .await
        .context("ensure schema")?;

    // The catalog snapshot table (idempotent): tenant-scoped, read-only to wamn_app.
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS wamn_catalog ( \
               id uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
               tenant_id text NOT NULL, \
               document jsonb NOT NULL); \
             ALTER TABLE wamn_catalog ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE wamn_catalog FORCE ROW LEVEL SECURITY; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_policies WHERE schemaname = current_schema() \
                              AND tablename = 'wamn_catalog' AND policyname = 'wamn_catalog_tenant') THEN \
                 CREATE POLICY wamn_catalog_tenant ON wamn_catalog \
                   USING (tenant_id = current_setting('app.tenant', true)) \
                   WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
               END IF; \
             END $$; \
             GRANT SELECT ON wamn_catalog TO wamn_app;",
        )
        .await
        .context("ensure wamn_catalog table")?;

    // Optionally stand up the entity tables (the 3.2 floor). The floor DDL is not
    // idempotent (plain CREATE TABLE / CREATE POLICY), so apply it only when the
    // first entity table is absent; a re-run against a provisioned schema is a
    // clean no-op that still refreshes the snapshot below.
    if args.provision {
        let first = cat
            .entities
            .first()
            .map(|e| e.name.clone())
            .context("catalog has no entities to provision")?;
        let exists: bool = client
            .query_one(
                "SELECT EXISTS ( SELECT FROM information_schema.tables \
                 WHERE table_schema = current_schema() AND table_name = $1 )",
                &[&first],
            )
            .await
            .context("probe floor")?
            .get(0);
        if exists {
            println!("floor already present in schema {schema}; skipping provision");
        } else {
            let floor = wamn_ddl::Migration::create(cat)
                .map_err(|e| anyhow::anyhow!("floor compile: {e}"))?
                .sql(wamn_ddl::Confirmation::None)
                .map_err(|e| anyhow::anyhow!("floor sql: {e}"))?;
            client.batch_execute(&floor).await.context("apply floor")?;
            println!("provisioned tenant floor in schema {schema}");
        }
    }

    // Optionally apply the run-state storage + flow registry (POC-F1).
    if args.runstate {
        if ensure_runstate(client, schema).await? {
            println!("applied run-state storage (runs/node_runs) in schema {schema}");
        } else {
            println!("run-state storage already present in schema {schema}; skipping");
        }
        if ensure_flow_registry(client, schema).await? {
            println!("applied flow registry (flows) in schema {schema}");
        } else {
            println!("flow registry already present in schema {schema}; skipping");
        }
    }

    // Optionally compile + apply a wamn-seed dataset against this catalog.
    if let Some(path) = &args.seed_dataset {
        let src = std::fs::read_to_string(path)
            .with_context(|| format!("read seed dataset {}", path.display()))?;
        let sql = seed_dataset_sql(&src, cat, &args.tenant)?;
        client
            .batch_execute(&sql)
            .await
            .context("apply seed dataset")?;
        println!("applied seed dataset {} in schema {schema}", path.display());
    }

    // Optionally register + activate flow graphs in the registry.
    for path in &args.flow {
        let src = std::fs::read_to_string(path)
            .with_context(|| format!("read flow {}", path.display()))?;
        let (flow_id, version) = register_flow(client, &args.tenant, &src).await?;
        println!("registered flow {flow_id} v{version} (active) in schema {schema}");
    }

    // Snapshot UPSERT: replace this tenant's row. The document (arbitrary jsonb)
    // is a bound parameter — never string-interpolated — so it can carry no SQL;
    // the superuser connection bypasses the RLS WITH CHECK + the SELECT-only grant.
    // `$2::text::jsonb` types the parameter as `text` (so a Rust `&str` binds) and
    // then casts to jsonb — a bare `$2::jsonb` types the parameter as jsonb, which
    // tokio_postgres cannot serialize a `&str` into.
    client
        .execute(
            "DELETE FROM wamn_catalog WHERE tenant_id = $1",
            &[&args.tenant],
        )
        .await
        .context("clear old snapshot")?;
    client
        .execute(
            "INSERT INTO wamn_catalog (tenant_id, document) VALUES ($1, $2::text::jsonb)",
            &[&args.tenant, &document],
        )
        .await
        .context("write snapshot")?;

    // Refresh the decode-time entity map (wamn-l5i9.11): entity id → the
    // table's CURRENT pg_class OID, for every catalog entity whose table
    // exists (absent tables upsert nothing). This is also the BACKFILL path
    // for an env CDC-enabled after its catalog was published — re-running
    // publish-catalog populates the map.
    upsert_entity_map(client, cat, schema).await?;

    // EVT-RI-ORCH (wamn-l5i9.61): reconcile REPLICA IDENTITY for this schema as
    // the automatic operational caller — the catalog's table/registration set
    // just changed, so an entity that needs the old image must be flipped to FULL
    // here rather than waiting for a manual verb run (the flip is non-retroactive,
    // so the gap would be permanent for events captured meanwhile). Idempotent and
    // scoped strictly to `schema`; a schema without the floor yet is a clean no-op.
    if !args.skip_reconcile_replica_identity {
        crate::reconcile_replica_identity::reconcile_after_apply(client, cat, schema).await?;
    }

    Ok(())
}

/// Ensure + upsert the `wamn_entities` map for every entity of `cat` (rows are
/// upsert-only; a dropped entity's row keeps old-WAL decode resolvable).
/// Generic over [`tokio_postgres::GenericClient`] so `migrate-catalog` can run
/// it INSIDE its apply transaction (atomic with the rename DDL).
pub async fn upsert_entity_map(
    client: &impl tokio_postgres::GenericClient,
    cat: &wamn_catalog::Catalog,
    schema: &str,
) -> anyhow::Result<()> {
    client
        .batch_execute(&wamn_provision::sql::ensure_entity_map_sql(schema))
        .await
        .context("ensure entity map")?;
    let upsert = wamn_provision::sql::upsert_entity_map_sql(schema);
    for e in &cat.entities {
        client
            .execute(upsert.as_str(), &[&e.id.as_str(), &e.name])
            .await
            .with_context(|| format!("upsert entity map row for {:?}", e.id))?;
    }
    Ok(())
}

/// The D24 registration-orphan guard (EVT-REG, wamn-rmxa), shared by
/// publish-catalog and migrate-catalog. Reads every event registration for
/// `cat`'s catalog id across ALL tenants (the caller connects as a superuser, so
/// RLS is bypassed) and refuses when any references an entity `cat` does not
/// keep, naming every orphan (the pure decision `wamn_migrate::check_registration_orphans`).
/// A DB with no `catalog.event_registrations` table (a project not yet
/// registration-provisioned) has nothing to orphan, so the probe returns a clean
/// pass. Read-only: a refusal mutates nothing.
pub(crate) async fn guard_registration_orphans(
    client: &impl tokio_postgres::GenericClient,
    cat: &wamn_catalog::Catalog,
) -> anyhow::Result<()> {
    let table_present: bool = client
        .query_one(
            "SELECT to_regclass('catalog.event_registrations') IS NOT NULL",
            &[],
        )
        .await
        .context("probe catalog.event_registrations")?
        .get(0);
    if !table_present {
        return Ok(());
    }
    let rows = client
        .query(
            &wamn_migrate::sql::select_registrations_for_catalog_sql(),
            &[&cat.catalog_id],
        )
        .await
        .context("read event registrations for the D24 orphan guard")?;
    let referenced: Vec<wamn_migrate::RegistrationRef> = rows
        .iter()
        .map(|row| wamn_migrate::RegistrationRef {
            registration_id: row.get(0),
            tenant: row.get(1),
            entity_id: row.get(2),
        })
        .collect();
    let present: std::collections::BTreeSet<&str> =
        cat.entities.iter().map(|e| e.id.as_str()).collect();
    wamn_migrate::check_registration_orphans(&present, &referenced)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

// ---------------------------------------------------------------------------
// Run-state / flow-registry provisioning + flow registration. Shared with the
// f1bench gate so the bench provisions through the same code path production
// provisioning uses.
// ---------------------------------------------------------------------------

/// The canonical deploy DDL, embedded at compile time and rewritten from the
/// `wamn_run` schema to the target project schema. The dot-anchored replace
/// leaves prose mentions like `wamn_run_store` untouched; `schema` has already
/// passed [`valid_ident`], so bare interpolation is safe.
fn rewrite_schema(ddl: &str, schema: &str) -> String {
    ddl.replace("wamn_run.", &format!("{schema}."))
        .replace("SCHEMA wamn_run", &format!("SCHEMA {schema}"))
}

/// Apply `deploy/sql/run-state.sql` (runs + node_runs) into `schema` when its
/// `runs` table is absent. Returns whether it applied (false = already there).
pub async fn ensure_runstate(
    client: &tokio_postgres::Client,
    schema: &str,
) -> anyhow::Result<bool> {
    if table_exists(client, schema, "runs").await? {
        return Ok(false);
    }
    let ddl = rewrite_schema(include_str!("../../../deploy/sql/run-state.sql"), schema);
    client
        .batch_execute(&ddl)
        .await
        .context("apply run-state")?;
    Ok(true)
}

/// Apply `deploy/sql/flows.sql` (the flow registry) into `schema` when its `flows`
/// table is absent. Returns whether it applied.
pub async fn ensure_flow_registry(
    client: &tokio_postgres::Client,
    schema: &str,
) -> anyhow::Result<bool> {
    if table_exists(client, schema, "flows").await? {
        return Ok(false);
    }
    let ddl = rewrite_schema(include_str!("../../../deploy/sql/flows.sql"), schema);
    client
        .batch_execute(&ddl)
        .await
        .context("apply flow registry")?;
    Ok(true)
}

async fn table_exists(
    client: &tokio_postgres::Client,
    schema: &str,
    table: &str,
) -> anyhow::Result<bool> {
    Ok(client
        .query_one(
            "SELECT EXISTS ( SELECT FROM information_schema.tables \
             WHERE table_schema = $1 AND table_name = $2 )",
            &[&schema, &table],
        )
        .await
        .with_context(|| format!("probe {schema}.{table}"))?
        .get(0))
}

/// Compile a wamn-seed dataset against the catalog into idempotent INSERTs.
pub fn seed_dataset_sql(
    dataset_json: &str,
    cat: &wamn_catalog::Catalog,
    tenant: &str,
) -> anyhow::Result<String> {
    let dataset = wamn_seed::Dataset::from_json(dataset_json).context("parse seed dataset")?;
    let plan = wamn_seed::compile(&dataset, cat, tenant)
        .map_err(|e| anyhow::anyhow!("seed compile: {e}"))?;
    plan.sql(wamn_ddl::Confirmation::None)
        .map_err(|e| anyhow::anyhow!("seed sql: {e}"))
}

/// Validate a flow graph and register it ACTIVE under `tenant` (assumes the
/// session `search_path` already points at the project schema). Prior versions
/// of the same flow deactivate; re-registering a version refreshes its graph.
/// The `flow_id` column is written from the graph's embedded id, so the
/// dispatcher's column==graph equality guard (wi4) holds by construction. The
/// superuser connection bypasses RLS, hence the explicit tenant predicates.
///
/// A webhook path already served by another ACTIVE flow of the tenant is
/// rejected before any write (the ingress routes a path to ONE flow — a second
/// claimant would be silently shadowed); the flows_active_webhook_path unique
/// index backstops the check under concurrent registration.
pub async fn register_flow(
    client: &tokio_postgres::Client,
    tenant: &str,
    graph_json: &str,
) -> anyhow::Result<(String, u32)> {
    let flow =
        wamn_flow::Flow::from_json(graph_json).map_err(|e| anyhow::anyhow!("flow parse: {e}"))?;
    let issues = flow.issues();
    if issues
        .iter()
        .any(|i| i.severity == wamn_flow::Severity::Error)
    {
        bail!("flow {} does not validate: {issues:?}", flow.flow_id);
    }
    let version = i32::try_from(flow.version).context("flow version")?;
    if let wamn_flow::Trigger::Webhook {
        path: Some(path), ..
    } = &flow.trigger
    {
        let holder = client
            .query_opt(
                "SELECT flow_id FROM flows \
                 WHERE tenant_id = $1 AND flow_id <> $2 AND active \
                   AND graph_json->'trigger'->>'type' = 'webhook' \
                   AND graph_json->'trigger'->>'path' = $3",
                &[&tenant, &flow.flow_id, &path],
            )
            .await
            .context("webhook path collision pre-check")?;
        if let Some(row) = holder {
            let holder: String = row.get(0);
            bail!(
                "webhook path collision: active flow {holder:?} already serves path {path:?} \
                 for tenant {tenant:?}; deactivate it or change the path before registering {:?}",
                flow.flow_id
            );
        }
    }
    // Deactivate-prior + insert are ONE transaction: a failed insert (e.g. the
    // collision index catching a racing registration) must roll the deactivate
    // back, never stranding the flow with no active version.
    client
        .batch_execute("BEGIN")
        .await
        .context("begin registration")?;
    let writes = async {
        client
            .execute(
                "UPDATE flows SET active = false, updated_at = now() \
                 WHERE tenant_id = $1 AND flow_id = $2",
                &[&tenant, &flow.flow_id],
            )
            .await
            .context("deactivate prior versions")?;
        client
            .execute(
                "INSERT INTO flows (tenant_id, flow_id, version, active, graph_json) \
                 VALUES ($1, $2, $3, true, $4::text::jsonb) \
                 ON CONFLICT (tenant_id, flow_id, version) \
                   DO UPDATE SET graph_json = EXCLUDED.graph_json, active = true, updated_at = now()",
                &[&tenant, &flow.flow_id, &version, &graph_json],
            )
            .await
            .context("register flow")?;
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = writes {
        let _ = client.batch_execute("ROLLBACK").await;
        return Err(e);
    }
    client
        .batch_execute("COMMIT")
        .await
        .context("commit registration")?;
    Ok((flow.flow_id.clone(), flow.version))
}

#[cfg(test)]
mod tests {
    use super::{rewrite_schema, valid_ident};

    /// The embedded deploy DDL is the canonical file (include_str!), and the
    /// schema rewrite must touch only schema references: qualified names and
    /// the schema header — never prose like `wamn_run_store`.
    #[test]
    fn schema_rewrite_is_dot_anchored() {
        let run_state = include_str!("../../../deploy/sql/run-state.sql");
        let flows = include_str!("../../../deploy/sql/flows.sql");
        for (ddl, table) in [(run_state, "runs"), (flows, "flows")] {
            let out = rewrite_schema(ddl, "poc_f1");
            assert!(
                out.contains(&format!("CREATE TABLE poc_f1.{table}")),
                "{table}"
            );
            assert!(!out.contains("wamn_run."), "no qualified wamn_run left");
            assert!(!out.contains("SCHEMA wamn_run"), "schema header rewritten");
        }
        // The prose mention of the wamn_run_store crate must survive verbatim.
        assert!(rewrite_schema(run_state, "poc_f1").contains("wamn_run_store"));
        // node_runs rides along with runs in run-state.sql.
        assert!(rewrite_schema(run_state, "poc_f1").contains("CREATE TABLE poc_f1.node_runs"));
        // The webhook-path collision backstop rewrites into the project schema
        // (register_flow's pre-check relies on this index existing there).
        assert!(
            rewrite_schema(flows, "poc_f1")
                .contains("CREATE UNIQUE INDEX flows_active_webhook_path ON poc_f1.flows")
        );
    }

    #[test]
    fn identifier_validation() {
        assert!(valid_ident("api_proof"));
        assert!(valid_ident("public"));
        assert!(valid_ident("_x1"));
        assert!(!valid_ident(""));
        assert!(!valid_ident("1bad"));
        assert!(!valid_ident("has-hyphen"));
        assert!(!valid_ident("drop table x; --"));
        assert!(!valid_ident("a b"));
        // Uppercase rejected: the unquoted run-state rewrite would case-fold
        // it into a DIFFERENT schema than the quoted publish statements.
        assert!(!valid_ident("Poc"));
        assert!(!valid_ident("POC_F1"));
    }
}

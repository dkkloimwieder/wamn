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
//! entity tables) when they are absent, and `--seed` loads the bundled two-tenant
//! demo rows — both used by the in-cluster `apiproof` gate to give the deployed
//! gateway real data to serve. Everything is **additive**: the schema is created
//! `IF NOT EXISTS`, the floor is applied only when missing, and no existing object
//! is ever dropped or altered (the shared-cluster guardrail).

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;

use crate::apifixture;

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

    /// Also seed the bundled two-tenant demo rows (proof scaffolding matching the
    /// bundled `deploy/proof-catalog.json`; idempotent). Implies the floor.
    #[arg(long)]
    pub seed: bool,
}

/// A bare SQL identifier safe to embed after validating: starts with a letter or
/// `_`, then letters/digits/`_`. Mirrors the `wamn:postgres` schema check.
fn valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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
        "published catalog snapshot: schema={} tenant={} (provision={}, seed={})",
        args.schema, args.tenant, args.provision, args.seed
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

    // Optionally seed the bundled two-tenant demo rows (ON CONFLICT DO NOTHING).
    if args.seed {
        client
            .batch_execute(&apifixture::entity_seed_sql())
            .await
            .context("seed demo rows")?;
        println!("seeded demo rows in schema {schema}");
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::valid_ident;

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
    }
}

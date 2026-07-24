//! Ephemeral schema lifecycle owned by the test runner (production delta 4).
//!
//! [`EphemeralSchemaProvisioner`] owns a persistent superuser (admin) connection
//! and creates a FRESH schema per test CASE from a caller-supplied template DDL,
//! then drops it. The runner's `wamn_app` role is NOSUPERUSER/NOCREATEDB and
//! cannot create schemas — exactly as in production — so all provisioning goes
//! through this admin path, never the guest's pool. Host-side flow seeding runs
//! on the same superuser session ([`admin`](EphemeralSchemaProvisioner::admin),
//! RLS-bypassing; the caller re-scopes `search_path` per case).
//!
//! Per-case isolation has a second half: [`case_pool`] builds a FRESH `wamn_app`
//! pool bound to the case's schema. A new plugin per case means the pool's
//! cached prepared-statement plans never alias a prior case's schema (a plan
//! pins its `search_path`), so N sequential `create schema_case_i → run → drop`
//! cases stay isolated.

use std::sync::Arc;

use anyhow::Context as _;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

use crate::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};

/// Owns the superuser connection lifecycle for the test runner's ephemeral
/// schemas.
pub struct EphemeralSchemaProvisioner {
    admin: Client,
    _conn: JoinHandle<()>,
    template: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

impl EphemeralSchemaProvisioner {
    /// Connect the persistent superuser session. `template` renders a case's
    /// table DDL given the schema name (e.g. the flow tables + RLS).
    pub async fn connect(
        admin_url: &str,
        template: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let (admin, conn) = tokio_postgres::connect(admin_url, NoTls)
            .await
            .context("admin connect for the ephemeral schema provisioner")?;
        let handle = tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(Self {
            admin,
            _conn: handle,
            template: Arc::new(template),
        })
    }

    /// The persistent superuser client. Host-side flow seeding runs here
    /// (RLS-bypassing); the caller re-scopes its `search_path` per case.
    pub fn admin(&self) -> &Client {
        &self.admin
    }

    /// Drop-and-recreate `schema` from the template DDL — a fresh case. The
    /// schema starts empty (`AUTHORIZATION postgres`), `wamn_app` gets `USAGE`,
    /// and the template lays down the tables + RLS.
    pub async fn provision_case(&self, schema: &str) -> anyhow::Result<()> {
        self.admin
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; \
                 CREATE SCHEMA {schema} AUTHORIZATION postgres; \
                 GRANT USAGE ON SCHEMA {schema} TO wamn_app;"
            ))
            .await
            .context("create ephemeral schema")?;
        self.admin
            .batch_execute(&(self.template)(schema))
            .await
            .context("apply template DDL")?;
        Ok(())
    }

    /// Drop `schema` (case teardown).
    pub async fn drop_case(&self, schema: &str) -> anyhow::Result<()> {
        self.admin
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE;"))
            .await
            .context("drop ephemeral schema")?;
        Ok(())
    }
}

/// Build a FRESH `wamn_app` pool bound to `schema` for one test case, keyed to
/// `component_id` with the `tenant` claim. A new plugin per case keeps the
/// pool's cached prepared statements from aliasing a prior case's schema, so
/// per-case isolation holds. `cfg` carries the app-role connection URL + pool
/// sizing (its `database_url` must be the NOSUPERUSER `wamn_app` URL).
pub fn case_pool(
    cfg: &WamnPostgresConfig,
    tenant: &str,
    schema: &str,
    component_id: &str,
) -> anyhow::Result<Arc<WamnPostgres>> {
    let pg = Arc::new(WamnPostgres::new(cfg.clone())?);
    pg.set_tenant(component_id, tenant)?;
    pg.set_schema(component_id, schema)?;
    Ok(pg)
}

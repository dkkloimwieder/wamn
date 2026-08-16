//! Host-owned global-FIFO claim handoff proof.
//!
//! The MVP flow interpreter remains hard-refused until wamn-0h0g.5.4 activates
//! effect attempts. This retained gate therefore stops at the honest boundary:
//! it publishes exact plans bound to the loaded flowrunner revision, seeds three
//! release-pinned runs, and calls the sole production claim transaction. It
//! proves exact FIFO handoff, complete resolution-map materialization, fresh
//! lease generations, authoritative payloads, and an empty fourth claim without
//! invoking the guest or manufacturing a completion.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};
use wamn_execution_host::TrustedExecutionRuntimeRevision;
use wamn_run_state::queue::{enqueue_sql, write_ahead_triggered_run_sql};
use wamn_runtime::plugins::wamn_postgres::{
    ProductionClaimResult, WamnPostgres, WamnPostgresConfig,
};

const SCHEMA: &str = "wamn_runner_bench";
const TENANT: &str = "runner-tenant";
const OWNER: &str = "runner-bench";
const FLOW_ID: &str = "poc-receipt";

fn published_fixtures() -> Vec<String> {
    vec![crate::flowbench::flow_json(1)]
}

#[derive(Debug, Args)]
pub struct RunnerBenchArgs {
    /// Exact flowrunner component bytes used to bind the fixture plan's trusted
    /// runtime revision. The guest is deliberately not invoked by this gate.
    #[arg(long)]
    pub flowrunner: PathBuf,

    /// App Postgres URL for the NOSUPERUSER wamn_app role.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL used only to provision and remove the ephemeral schema.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,
}

fn runner_ddl(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}.runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
            flow_version int NOT NULL, catalog_id text, catalog_version int, \
            environment text, execution_bundle_hash text, \
            event_source_run_id text, event_root_run_id text, event_depth int, \
            status text NOT NULL DEFAULT 'running' \
              CHECK (status IN ('dispatched','running','completed','failed',\
                                'infrastructure-failure','effect-uncertain')), \
            trigger_source text, capture_mode text NOT NULL DEFAULT 'off', \
            release_version int, manifest_digest text, \
            input_json jsonb, result_json jsonb, state_json jsonb, \
            invocation_context jsonb NOT NULL DEFAULT '{{}}'::jsonb, \
            caller_outcome_kind text, caller_outcome_json jsonb, caller_http_status int, \
            caller_release_node_id text, caller_outcome_hash text, \
            caller_released_at timestamptz, \
            fail_kind text, fail_node text, fail_reason text, \
            updated_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY runs_tenant ON {schema}.runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.runs TO wamn_app;\
         CREATE TABLE {schema}.node_runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, \
            frame_id bigint NOT NULL DEFAULT 0, local_node_id text NOT NULL, \
            occurrence int NOT NULL DEFAULT 0, \
            PRIMARY KEY (tenant_id, run_id, frame_id, local_node_id, occurrence));\
         ALTER TABLE {schema}.node_runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.node_runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY node_runs_tenant ON {schema}.node_runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT ON {schema}.node_runs TO wamn_app;\
         CREATE TABLE {schema}.effect_attempts (tenant_id text NOT NULL, run_id text NOT NULL);\
         ALTER TABLE {schema}.effect_attempts ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.effect_attempts FORCE ROW LEVEL SECURITY;\
         CREATE POLICY effect_attempts_tenant ON {schema}.effect_attempts \
            USING (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT ON {schema}.effect_attempts TO wamn_app;\
         CREATE TABLE {schema}.run_queue (\
            tenant_id text NOT NULL, run_id text NOT NULL, \
            priority int NOT NULL DEFAULT 0, \
            available_at timestamptz NOT NULL DEFAULT now(), \
            stream_seq bigint NOT NULL DEFAULT 0, \
            lease_owner text, lease_expires_at timestamptz, \
            lease_generation bigint NOT NULL DEFAULT 0 CHECK (lease_generation >= 0), \
            attempts int NOT NULL DEFAULT 0, max_attempts int NOT NULL DEFAULT 20, \
            enqueued_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) \
              REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         CREATE INDEX run_queue_claimable ON {schema}.run_queue \
            (tenant_id, available_at, stream_seq, run_id, lease_expires_at);\
         ALTER TABLE {schema}.run_queue ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_queue FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_queue_tenant ON {schema}.run_queue \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.run_queue TO wamn_app;"
    )
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect for runnerbench schema")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
                 CREATE SCHEMA {SCHEMA} AUTHORIZATION postgres; \
                 GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app;"
            ))
            .await
            .context("create runnerbench schema")?;
        client
            .batch_execute(&runner_ddl(SCHEMA))
            .await
            .context("apply runnerbench run plane")?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

async fn teardown(admin_url: &str) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(admin_url, NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let result = client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;"))
        .await
        .context("drop runnerbench schema");
    drop(client);
    let _ = connection_task.await;
    result
}

async fn connect_app(app_url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(app_url, NoTls)
        .await
        .context("runnerbench app connect")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(&format!(
            "SET search_path TO {SCHEMA}; SET app.tenant TO '{TENANT}';"
        ))
        .await
        .context("scope runnerbench app session")?;
    Ok(client)
}

async fn seed_run(client: &mut Client, run_id: &str) -> anyhow::Result<()> {
    let transaction = client.transaction().await?;
    transaction
        .execute(
            &write_ahead_triggered_run_sql(),
            &[&run_id, &FLOW_ID, &1i32, &"cron", &"\"receipt\""],
        )
        .await
        .context("write ahead fixture run")?;
    transaction
        .execute(&enqueue_sql(), &[&run_id, &0i32, &0i64])
        .await
        .context("enqueue fixture run")?;
    transaction.commit().await?;
    crate::catalog_pin::pin_run(client, run_id).await
}

pub async fn run(args: RunnerBenchArgs) -> anyhow::Result<()> {
    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("failed to read {}", args.flowrunner.display()))?;
    let runtime_revision =
        TrustedExecutionRuntimeRevision::from_flowrunner_bytes(&guest).execution_runtime_revision();
    let app_url = args
        .database_url
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args.admin_database_url.context(
        "runnerbench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;

    crate::catalog_pin::publish_release(
        &admin_url,
        TENANT,
        &published_fixtures(),
        &runtime_revision,
    )
    .await?;
    provision(&admin_url).await?;

    let outcome = async {
        let mut seed = connect_app(&app_url).await?;
        for run_id in ["fifo-z", "fifo-b", "fifo-a"] {
            seed_run(&mut seed, run_id).await?;
        }
        seed.execute(
            "UPDATE run_queue \
                SET available_at = TIMESTAMPTZ '2000-01-01 00:00:00+00', \
                    stream_seq = CASE run_id \
                        WHEN 'fifo-z' THEN 11 ELSE 10 END",
            &[],
        )
        .await?;

        let mut config = WamnPostgresConfig::from_env();
        config.database_url = Some(app_url.clone());
        let postgres = WamnPostgres::new(config)?;
        postgres.set_tenant(OWNER, TENANT)?;
        postgres.set_schema(OWNER, SCHEMA)?;
        postgres.set_runner(OWNER, OWNER)?;

        let mut claimed = Vec::new();
        for _ in 0..3 {
            match postgres.claim_next_production(OWNER, 30_000).await? {
                ProductionClaimResult::Ready {
                    run_id,
                    payload,
                    lease_generation,
                } => claimed.push((run_id, payload, lease_generation)),
                other => bail!("expected ready production handoff, got {other:?}"),
            }
        }
        let empty = postgres.claim_next_production(OWNER, 30_000).await?;

        let order = claimed
            .iter()
            .map(|(run_id, _, _)| run_id.as_str())
            .collect::<Vec<_>>();
        let exact_order = order == ["fifo-a", "fifo-b", "fifo-z"];
        let exact_payloads = claimed
            .iter()
            .all(|(_, payload, _)| payload == "\"receipt\"");
        let fresh_generations = claimed
            .iter()
            .all(|(_, _, lease_generation)| *lease_generation == 1);
        let empty_after_handoff = matches!(empty, ProductionClaimResult::Empty);
        let leased: i64 = seed
            .query_one(
                "SELECT count(*) FROM run_queue \
                  WHERE lease_owner = $1 AND lease_generation = 1 \
                    AND lease_expires_at > now()",
                &[&OWNER],
            )
            .await?
            .get(0);
        let running: i64 = seed
            .query_one("SELECT count(*) FROM runs WHERE status = 'running'", &[])
            .await?
            .get(0);
        let pass = exact_order
            && exact_payloads
            && fresh_generations
            && empty_after_handoff
            && leased == 3
            && running == 3;
        println!(
            "# runnerbench global claim handoff\n\
             order={order:?} exact={exact_order}; payloads={exact_payloads}; \
             generations={fresh_generations}; empty={empty_after_handoff}; \
             live-leases={leased}/3; running={running}/3; pass={pass}"
        );
        anyhow::Ok(pass)
    }
    .await;

    let cleanup = teardown(&admin_url).await;
    let pass = outcome?;
    cleanup?;
    if !pass {
        bail!("runnerbench global claim handoff failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_drift::{Need, assert_stand_in};

    #[test]
    fn published_runnerbench_fixture_compiles_to_a_release_plan() {
        for flow_json in published_fixtures() {
            crate::catalog_pin::assert_releasable("runnerbench fixture", &flow_json);
        }
    }

    #[test]
    fn runnerbench_stand_in_tracks_global_queue_schema_of_record() {
        assert_stand_in(
            "runnerbench",
            &runner_ddl("wamn_run"),
            &[("run_queue", Need::Required)],
        );
    }

    #[test]
    fn runnerbench_stand_in_carries_production_claim_surfaces() {
        let ddl = runner_ddl("wamn_run");
        for required in ["CREATE TABLE wamn_run.effect_attempts"] {
            assert!(ddl.contains(required), "runnerbench DDL lacks {required}");
        }

        let claim = wamn_run_state::queue::select_production_claim_sql();
        for run_column in [
            "status",
            "flow_id",
            "flow_version",
            "catalog_id",
            "catalog_version",
            "environment",
            "execution_bundle_hash",
            "input_json",
        ] {
            assert!(
                ddl.contains(run_column),
                "runnerbench DDL lacks production claim column {run_column}: {claim}"
            );
        }
    }
}

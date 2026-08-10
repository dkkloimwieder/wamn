//! Shared deployed-runner fixture schema and polling helpers.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls};

use wamn_gate_harness::scope_session;
use wamn_run_state::queue::{enqueue_sql, write_ahead_triggered_run_sql};

/// Whether a value is safe to interpolate as an unquoted fixture identifier.
pub fn valid_ident(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Flow, run, node-run, queue, and partition tables used by deployed-runner
/// system and integration proofs.
pub fn ladder_ddl(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}.flows (\
            tenant_id text NOT NULL, flow_id text NOT NULL, version int NOT NULL, \
            active boolean NOT NULL DEFAULT false, graph_json jsonb NOT NULL, \
            PRIMARY KEY (tenant_id, flow_id, version));\
         ALTER TABLE {schema}.flows ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.flows FORCE ROW LEVEL SECURITY;\
         CREATE POLICY flows_tenant ON {schema}.flows \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.flows TO wamn_app;\
         CREATE TABLE {schema}.runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
            flow_version int NOT NULL, \
            status text NOT NULL DEFAULT 'running' \
              CHECK (status IN ('dispatched','running','completed','failed','infrastructure-failure','effect-uncertain')), \
            trigger_source text, input_json jsonb, result_json jsonb, state_json jsonb, \
            updated_at timestamptz NOT NULL DEFAULT now(), \
            idempotency_key text, replay_of text, root_run_id text, \
            fail_kind text, fail_node text, fail_reason text, \
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY runs_tenant ON {schema}.runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.runs TO wamn_app;\
         CREATE TABLE {schema}.node_runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, node_id text NOT NULL, \
            occurrence int NOT NULL DEFAULT 0, seq int NOT NULL, attempt int NOT NULL DEFAULT 0, \
            status text NOT NULL, output_port text, output_json jsonb, input_json jsonb, \
            error_kind text, error_detail jsonb, \
            preview_head text, payload_size bigint, payload_hash text, capture_mode text, \
            redacted boolean NOT NULL DEFAULT false, \
            PRIMARY KEY (tenant_id, run_id, node_id, occurrence), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         ALTER TABLE {schema}.node_runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.node_runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY node_runs_tenant ON {schema}.node_runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.node_runs TO wamn_app;\
         CREATE TABLE {schema}.run_queue (\
            tenant_id text NOT NULL, run_id text NOT NULL, partition_key text, \
            partition_policy text NOT NULL DEFAULT 'blocking' CHECK (partition_policy IN ('blocking', 'leapfrog')), \
            priority int NOT NULL DEFAULT 0, available_at timestamptz NOT NULL DEFAULT now(), \
            lease_owner text, lease_expires_at timestamptz, \
            lease_generation bigint NOT NULL DEFAULT 0 CHECK (lease_generation >= 0), \
            attempts int NOT NULL DEFAULT 0, max_attempts int NOT NULL DEFAULT 20, \
            enqueued_at timestamptz NOT NULL DEFAULT now(), \
            stream_seq bigint NOT NULL DEFAULT 0, \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         CREATE INDEX run_queue_claimable ON {schema}.run_queue (tenant_id, available_at, stream_seq, lease_expires_at);\
         CREATE INDEX run_queue_partition ON {schema}.run_queue (tenant_id, partition_key) WHERE partition_key IS NOT NULL;\
         ALTER TABLE {schema}.run_queue ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_queue FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_queue_tenant ON {schema}.run_queue \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.run_queue TO wamn_app;\
         CREATE TABLE {schema}.partition_owner (\
            tenant_id text NOT NULL, partition_key text NOT NULL, \
            lease_owner text NOT NULL, lease_expires_at timestamptz NOT NULL, \
            acquired_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, partition_key));\
         ALTER TABLE {schema}.partition_owner ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.partition_owner FORCE ROW LEVEL SECURITY;\
         CREATE POLICY partition_owner_tenant ON {schema}.partition_owner \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.partition_owner TO wamn_app;"
    )
}

/// Connect as the application role and bind its schema and tenant claims.
pub async fn connect_app(app_url: &str, schema: &str, tenant: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(app_url, NoTls)
        .await
        .context("app (wamn_app) connect")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    scope_session(&client, tenant, schema)
        .await
        .context("set search_path + tenant claim")?;
    Ok(client)
}

/// Seed one dispatched run and its queue row in a single transaction.
pub async fn seed_run(
    client: &mut Client,
    flow_id: &str,
    run_id: &str,
    input_text: &str,
) -> anyhow::Result<()> {
    let transaction = client.transaction().await?;
    transaction
        .execute(
            &write_ahead_triggered_run_sql(),
            &[&run_id, &flow_id, &1i32, &"manual", &input_text],
        )
        .await
        .context("write-ahead run")?;
    transaction
        .execute(
            &enqueue_sql(),
            &[&run_id, &Option::<&str>::None, &0i32, &0i64],
        )
        .await
        .context("enqueue run")?;
    transaction.commit().await?;
    Ok(())
}

/// Whether a durable run-status literal ends polling.
pub fn is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "infrastructure-failure")
}

/// Poll a durable run until it is terminal or the deadline expires.
pub async fn poll_to_terminal(
    client: &Client,
    run_id: &str,
    deadline: Instant,
) -> anyhow::Result<String> {
    let mut status = "dispatched".to_string();
    loop {
        let row = client
            .query_opt("SELECT status FROM runs WHERE run_id = $1", &[&run_id])
            .await?;
        if let Some(row) = row {
            status = row.get(0);
            if is_terminal(&status) {
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(status)
}

/// FNV-1a 64 used as a one-way credential-delivery witness.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

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

/// Canonical flow and run-plane schema used by deployed-runner system and
/// integration proofs. The fixture intentionally rewrites the production DDL
/// instead of maintaining a second attempt/disposition protocol.
pub fn ladder_ddl(schema: &str) -> String {
    [
        include_str!("../../deploy/sql/catalog-schema.sql"),
        include_str!("../../deploy/sql/run-state.sql"),
        include_str!("../../deploy/sql/flows.sql"),
        include_str!("../../deploy/sql/run-queue.sql"),
    ]
    .join("\n")
    .replace("wamn_run", schema)
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
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "infrastructure-failure"
    )
}

#[cfg(test)]
mod tests {
    use super::ladder_ddl;

    #[test]
    fn runner_fixture_carries_the_canonical_attempt_and_disposition_protocol() {
        let ddl = ladder_ddl("fixture_run");
        for required in [
            "CREATE TABLE fixture_run.effect_attempts",
            "CREATE TABLE fixture_run.effect_disposition_requests",
            "CREATE FUNCTION fixture_run.park_effect_uncertain(",
            "SET search_path = pg_catalog, fixture_run",
            "CREATE TABLE fixture_run.run_queue",
            "CREATE SCHEMA IF NOT EXISTS catalog",
        ] {
            assert!(ddl.contains(required), "fixture DDL lost {required:?}");
        }
        assert!(!ddl.contains("wamn_run."));
    }
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

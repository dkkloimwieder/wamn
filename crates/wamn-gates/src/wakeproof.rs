//! wakeproof — the scale-to-zero / parked-project wake proof (wamn-fqg.12, POC-F3).
//!
//! Prove the DEPLOYED wake path end-to-end, OUTSIDE a bench harness: with the
//! runner Deployment scaled to 0 replicas, a LIVE dispatcher cron fire enqueues a
//! run, the waker (`deploy/platform/waker.yaml`) sees the doorbell hint and scales the
//! runner `0 -> 1` via the k8s API, and the woken runner drains the run to
//! `completed` — all within a bounded window. Like `ladderproof`, it is a pure
//! DB client (plus the shared `wamn_waker::KubeScale` scale client): it seeds a
//! cron flow and asserts terminal DB state + observed actuation. It NEVER
//! enqueues or doorbells — the LIVE dispatcher's cron fire must, which IS the
//! acceptance criterion.
//!
//! Phases (each a gate-harness `check`; any FAIL makes the process exit nonzero):
//!   * **park** — record the runner's original replicas, scale it to 0, wait
//!     until `status.replicas` reports 0 (no leftover pods to muddy the proof).
//!   * **seed** — register an ephemeral every-second cron flow into the schema
//!     the DEPLOYED dispatcher sweeps (`wamn_runner_demo` / `demo-tenant`).
//!   * **dispatcher-fires** — assert (within ~40 s, the dispatcher's 30 s max idle
//!     interval + margin) that a cron run row APPEARS. This SEPARATES a wiring
//!     gap (the dispatcher is not sweeping this project — see the projects-Secret
//!     precondition) from a wake failure.
//!   * **wake** — assert BOTH a cron-fired run reaches `completed` AND the
//!     runner's replicas were observed `> 0` (the waker actually actuated the
//!     wake — not a leftover pod, since park drained it to 0 first).
//!   * **teardown** — deactivate + delete the flow (stop fires FIRST), delete its
//!     runs/queue rows, restore the runner to its original scale, verify it took.
//!
//! LIVE PRECONDITION (verified by the `dispatcher-fires` phase, applied by the
//! runbook): the dispatcher's `wamn-dispatch-projects` Secret must carry a
//! project entry for this schema/tenant/DB. The committed example points only at
//! `wamn_dispatch_demo`; the runbook adds a `runner-demo` entry pointing at
//! `wamn_runner_demo` before running this gate.

use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::Client;

use wamn_gate_harness::{check, seed_flow_version};
use wamn_waker::{KubeScale, Scale};

// Reuse ladderproof's app-connection + identifier guard (the same demo schema +
// RLS floor the deployed runner claims under).
use crate::ladderproof::{connect_app, valid_ident};

#[derive(Debug, Args)]
pub struct WakeProofArgs {
    /// App (wamn_app) Postgres URL — seeds the cron flow + reads results.
    /// Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// The schema the DEPLOYED dispatcher sweeps AND the runner drains — they
    /// MUST be the same project. Matches the runner's --schema.
    #[arg(long, default_value = "wamn_runner_demo")]
    pub schema: String,

    /// The tenant the dispatcher, runner, and waker share. Matches the runner's
    /// --tenant and the waker's `--wake <tenant>=...`.
    #[arg(long, default_value = "demo-tenant")]
    pub tenant: String,

    /// The runner Deployment this gate parks + expects the waker to wake.
    #[arg(long, default_value = "runner")]
    pub deployment: String,

    /// How long to wait for the waker wake + runner drain to `completed`.
    #[arg(long, default_value_t = 120)]
    pub timeout_secs: u64,

    /// How long to wait for the runner to drop to 0 pods after parking.
    #[arg(long, default_value_t = 60)]
    pub park_timeout_secs: u64,

    /// The ephemeral cron flow id this gate seeds (deleted in teardown).
    #[arg(long, default_value = "wakeproof-cron")]
    pub flow_id: String,
}

/// The seeded cron flow: `webhook-in -> respond` (ladderproof rung 1's proven
/// completable graph) under an every-second cron trigger. The dispatcher fires
/// it; the woken runner drives it to `completed`. Built in code (no fixture) —
/// the run result is not asserted, only that a cron-fired run completes.
fn cron_flow_json(flow_id: &str, schedule: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "flow-id": flow_id,
        "version": 1,
        "name": "wakeproof scale-to-zero cron",
        "trigger": { "type": "cron", "schedule": schedule },
        "entry": "in",
        "nodes": [
            { "id": "in", "type": "webhook-in" },
            { "id": "out", "type": "respond" }
        ],
        "edges": [
            { "from": "in", "to": "out" }
        ]
    })
    .to_string()
}

/// Count this flow's cron-fired runs in ANY state (the dispatcher-fires probe).
async fn cron_run_count(client: &Client, flow_id: &str) -> anyhow::Result<i64> {
    Ok(client
        .query_one(
            "SELECT count(*) FROM runs WHERE flow_id = $1 AND trigger_source = 'cron'",
            &[&flow_id],
        )
        .await?
        .get(0))
}

/// Count this flow's cron-fired runs that reached `completed` (the wake probe).
async fn completed_cron_run_count(client: &Client, flow_id: &str) -> anyhow::Result<i64> {
    Ok(client
        .query_one(
            "SELECT count(*) FROM runs \
             WHERE flow_id = $1 AND trigger_source = 'cron' AND status = 'completed'",
            &[&flow_id],
        )
        .await?
        .get(0))
}

/// Delete the seeded flow row + all its runs. Deleting the `flows` row FIRST
/// removes it from `active_flows_sql`'s scan, so the dispatcher fires no new
/// cron tick; then the runs go (their `run_queue` + `node_runs` rows cascade via
/// the FK). Idempotent — used to clear prior-run residue at setup AND in teardown.
async fn cleanup(client: &Client, flow_id: &str) -> anyhow::Result<()> {
    client
        .execute("DELETE FROM flows WHERE flow_id = $1", &[&flow_id])
        .await
        .context("delete seeded flow row")?;
    client
        .execute("DELETE FROM runs WHERE flow_id = $1", &[&flow_id])
        .await
        .context("delete seeded runs")?;
    Ok(())
}

/// Remaining flow + run rows for this flow id (0 == zero residue).
async fn flow_residue(client: &Client, flow_id: &str) -> anyhow::Result<i64> {
    let flows: i64 = client
        .query_one("SELECT count(*) FROM flows WHERE flow_id = $1", &[&flow_id])
        .await?
        .get(0);
    let runs: i64 = client
        .query_one("SELECT count(*) FROM runs WHERE flow_id = $1", &[&flow_id])
        .await?
        .get(0);
    Ok(flows + runs)
}

/// Poll the Deployment's scale until `pred` holds or the deadline passes. A GET
/// error is transient (the loop retries); returns whether `pred` was met.
async fn wait_for_scale(
    scale: &KubeScale,
    deployment: &str,
    timeout_secs: u64,
    pred: impl Fn(Scale) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if let Ok(s) = scale.get_scale(deployment).await
            && pred(s)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

pub async fn run(args: WakeProofArgs) -> anyhow::Result<()> {
    if !valid_ident(&args.schema) {
        bail!("invalid schema {:?}", args.schema);
    }
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;

    println!(
        "# wamn-gates wakeproof — scale-to-zero wake (deployment {}, schema {}, tenant {})",
        args.deployment, args.schema, args.tenant
    );

    let scale = KubeScale::in_cluster().context(
        "build in-cluster k8s scale client (is the wakeproof-gate ServiceAccount bound?)",
    )?;
    let client = connect_app(&app_url, &args.schema, &args.tenant).await?;
    let mut overall = true;

    // --- park -------------------------------------------------------------
    println!(
        "\n## park — scale {} to 0 and wait for its pods to drain",
        args.deployment
    );
    let original = scale
        .get_scale(&args.deployment)
        .await
        .context("read runner scale (is the Role/RoleBinding applied?)")?;
    println!(
        "   original scale: spec={} status={}",
        original.spec_replicas, original.status_replicas
    );
    // Restore target floors at 1: scale-to-zero automation is out of scope, so
    // the gate must never leave the runner parked even if it started at 0.
    let restore_to = original.spec_replicas.max(1);

    scale
        .set_replicas(&args.deployment, 0)
        .await
        .context("scale runner to 0")?;
    let parked = wait_for_scale(&scale, &args.deployment, args.park_timeout_secs, |s| {
        s.status_replicas == 0
    })
    .await;
    check(
        &mut overall,
        &format!(
            "runner drained to 0 pods (status.replicas == 0) within {}s",
            args.park_timeout_secs
        ),
        parked,
    );

    // --- seed -------------------------------------------------------------
    println!("\n## seed — register the every-second cron flow (the LIVE dispatcher must fire it)");
    // Clear any residue from a prior (perhaps crashed) run so this run starts clean.
    cleanup(&client, &args.flow_id).await?;
    let graph = cron_flow_json(&args.flow_id, "* * * * * *");
    seed_flow_version(&client, &args.tenant, &args.flow_id, 1, true, &graph, true)
        .await
        .context("register the cron flow")?;
    println!(
        "   seeded cron flow {} active (schedule '* * * * * *')",
        args.flow_id
    );

    // --- dispatcher-fires -------------------------------------------------
    println!(
        "\n## dispatcher-fires — wait for the LIVE dispatcher to enqueue a cron run (~40s: its max idle interval is 30s)"
    );
    let fire_deadline = Instant::now() + Duration::from_secs(40);
    let mut fired = false;
    loop {
        if cron_run_count(&client, &args.flow_id).await? > 0 {
            fired = true;
            break;
        }
        if Instant::now() >= fire_deadline {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    check(
        &mut overall,
        "the LIVE dispatcher fired a cron run for the seeded flow \
         (else the dispatcher is not sweeping this project — check the wamn-dispatch-projects Secret)",
        fired,
    );

    // --- wake -------------------------------------------------------------
    if fired {
        println!(
            "\n## wake — the waker scales the runner up and it drives a cron run to completion (<= {}s)",
            args.timeout_secs
        );
        let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
        // Assigned at the top of every loop iteration before any break, so it is
        // definitely set by the time it is read after the loop.
        let mut completed: i64;
        // The waker never scales DOWN, so once it raises spec.replicas the value
        // stays > 0 for the rest of the window — observing it any poll proves the
        // actuation (park drained to 0 first, so it cannot be a leftover pod).
        let mut observed_up = false;
        loop {
            completed = completed_cron_run_count(&client, &args.flow_id).await?;
            if let Ok(s) = scale.get_scale(&args.deployment).await
                && s.spec_replicas > 0
            {
                observed_up = true;
            }
            if completed > 0 && observed_up {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        check(
            &mut overall,
            "a cron-fired run reached completed (the woken runner drove it end-to-end)",
            completed > 0,
        );
        check(
            &mut overall,
            "the runner's replicas were observed > 0 during the window (the waker actuated the wake)",
            observed_up,
        );
    } else {
        // No fire => no doorbell => nothing to wake. Skip the long wake wait and
        // report the wake asserts as failed so the verdict is unambiguous.
        check(
            &mut overall,
            "wake could not be observed (no cron fire, so no doorbell)",
            false,
        );
    }

    // --- teardown ---------------------------------------------------------
    println!(
        "\n## teardown — deactivate + delete the flow, delete its runs, restore the runner scale"
    );
    // Stop further cron fires FIRST (delete removes it from active_flows_sql).
    if let Err(e) = cleanup(&client, &args.flow_id).await {
        println!("   (cleanup warning: {e})");
    }
    // An in-flight dispatcher sweep may fire one straggler between the flow
    // delete and the run delete; a second pass after a short beat clears it.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let _ = cleanup(&client, &args.flow_id).await;
    let residue = flow_residue(&client, &args.flow_id).await.unwrap_or(1);
    check(
        &mut overall,
        "seeded flow + its runs fully deleted (zero residue)",
        residue == 0,
    );

    scale
        .set_replicas(&args.deployment, restore_to)
        .await
        .context("restore runner scale")?;
    let restored = wait_for_scale(&scale, &args.deployment, 30, |s| {
        s.spec_replicas == restore_to
    })
    .await;
    check(
        &mut overall,
        &format!("runner scale restored to {restore_to}"),
        restored,
    );

    println!("\nwakeproof complete — overall PASS: {overall}");
    if !overall {
        bail!("wakeproof failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seeded cron flow parses + VALIDATES by the same engine the runner
    /// compiles it with, and carries the cron trigger the dispatcher fires — a
    /// single-source drift guard so a broken graph breaks the build, not the
    /// in-cluster gate.
    #[test]
    fn cron_flow_parses_validates_and_carries_the_cron_trigger() {
        let json = cron_flow_json("wakeproof-cron", "* * * * * *");
        let v: serde_json::Value = serde_json::from_str(&json).expect("fixture parses");
        assert_eq!(v["flow-id"], serde_json::json!("wakeproof-cron"));
        assert_eq!(v["trigger"]["type"], serde_json::json!("cron"));
        let flow = wamn_flow::Flow::from_json(&json).expect("cron flow is a wamn-flow");
        flow.validate().expect("cron flow validates");
        assert_eq!(flow.flow_id.as_str(), "wakeproof-cron");
        match &flow.trigger {
            wamn_flow::Trigger::Cron { schedule } => assert_eq!(schedule, "* * * * * *"),
            _ => panic!("expected a cron trigger"),
        }
    }
}

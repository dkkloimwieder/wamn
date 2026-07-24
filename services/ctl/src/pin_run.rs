//! The `pin-run` subcommand (11.3 record-and-replay fixtures): pin a recorded
//! run (its 9.6-captured `runs` + `node_runs` rows) as a stored test case.
//!
//! The effect shell for the PURE `wamn_testkit::pin_run` transform: this READS
//! the run + its completed node executions (through the single-source run-state
//! read builders), folds them into a canonical `wamn_testkit::TestCase`, and
//! WRITES the case into the flow's `test_suites`/`test_cases` (11.2 storage) — so
//! a real run becomes a regression fixture, versioned WITH the flow it exercised.
//!
//! **Secret redaction is at pin time:** every payload that becomes part of the
//! case (the trigger input, the pinned node output) is `capture::scrub`'d by the
//! pure transform, so a pinned case NEVER contains a secret even when the source
//! run captured `full`. **Volatile fields** (server-minted ids, timestamps) are
//! normalized — the case turns on `canonicalize` and carries any `--ignore-path`
//! pointers — so replay tolerates a minted id without a spurious diff.
//!
//! **Capture-mode policy:** an `off`/`preview` run has no stored terminal-node
//! output, so it is not replayable — `pin-run` REFUSES it (`PinError::NotCaptured`)
//! and writes nothing. A `scrubbed` or `full` run pins (re-scrubbed).
//!
//! **Role:** connects as the APP role (`wamn_app`, NOSUPERUSER/NOBYPASSRLS) under
//! the tenant floor — ordinary tenant-scoped SELECTs + INSERTs the app role is
//! granted. The suite/case FK to `flows(tenant, flow_id, version)` means the
//! run's flow version must be REGISTERED, or the INSERT FK-fails.

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::Value;
use tokio_postgres::{Client, NoTls};

use wamn_run_store::sql::{select_node_runs_for_pin_sql, select_run_for_pin_sql};
use wamn_run_store::{FailKind, NodeRunRecord, NodeRunStatus, RunRecord, RunStatus};
use wamn_testkit::{PinError, PinOptions, TestCase, pin_run};

#[derive(Debug, Args)]
pub struct PinRunArgs {
    /// App (wamn_app) Postgres URL to the project-env database. Env `WAMN_PG_URL`.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// The run-plane schema the `runs`/`node_runs`/`test_*` tables live in (set as
    /// the session `search_path`). Bare identifier.
    #[arg(long, default_value = "wamn_run")]
    pub schema: String,

    /// The tenant whose run to pin — the `app.tenant` claim RLS scopes the reads
    /// and writes to.
    #[arg(long)]
    pub tenant: String,

    /// The recorded run to pin (its `runs.run_id`).
    #[arg(long)]
    pub run_id: String,

    /// The suite the pinned case joins (created if absent, version-bound to the
    /// run's flow version).
    #[arg(long)]
    pub suite_id: String,

    /// The id the pinned case is stored under (`test_cases.case_id`).
    #[arg(long)]
    pub case_id: String,

    /// The pinned case's ordinal within the suite.
    #[arg(long, default_value_t = 0)]
    pub ordinal: i32,

    /// An RFC-6901 pointer into the pinned node output to drop as volatile —
    /// beyond the UUID/timestamp canonicalization pin turns on by default.
    /// Repeatable.
    #[arg(long = "ignore-path")]
    pub ignore_path: Vec<String>,
}

/// The outcome of a pin attempt.
pub enum PinResult {
    /// The run was pinned: the case was written to the suite.
    Pinned {
        flow_id: String,
        flow_version: i32,
        assertions: usize,
    },
    /// The run was not pinnable (capture off/preview) — nothing written.
    Refused(PinError),
}

pub async fn run(args: PinRunArgs) -> anyhow::Result<()> {
    if !crate::migrate_catalog::is_bare_ident(&args.schema) {
        bail!(
            "--schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.schema
        );
    }
    if args.tenant.trim().is_empty() {
        bail!("--tenant must be non-empty (it is the app.tenant claim the pin is scoped to)");
    }

    let (client, conn) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("app (wamn_app) connect")?;
    let conn_task = tokio::spawn(conn);
    let result = pin(
        &client,
        &args.schema,
        &args.tenant,
        &args.run_id,
        &args.suite_id,
        &args.case_id,
        args.ordinal,
        args.ignore_path.clone(),
    )
    .await;
    drop(client);
    let _ = conn_task.await;

    match result? {
        PinResult::Refused(pe) => bail!("pin-run refused: {pe}"),
        PinResult::Pinned {
            flow_id,
            flow_version,
            assertions,
        } => {
            println!(
                "pin-run: pinned run {} as case {} in suite {} ({} v{}, {} assertion(s)) — \
                 secrets scrubbed, volatile fields normalized",
                args.run_id, args.case_id, args.suite_id, flow_id, flow_version, assertions
            );
            Ok(())
        }
    }
}

/// The reusable core: pin the session to the project (`search_path` + tenant
/// claim), read the run + its completed node executions, fold them into a
/// `TestCase` (the pure `pin_run`), and — unless the run is non-replayable
/// (`off`/`preview` → `Refused`) — write the case into `test_suites`/`test_cases`.
/// Shared by the CLI verb and the `pinproof` gate so both exercise ONE path.
#[allow(clippy::too_many_arguments)]
pub async fn pin(
    client: &Client,
    schema: &str,
    tenant: &str,
    run_id: &str,
    suite_id: &str,
    case_id: &str,
    ordinal: i32,
    ignore_paths: Vec<String>,
) -> anyhow::Result<PinResult> {
    // Both GUCs bound as parameters (set_config) — the tenant is arbitrary text,
    // never interpolated. Session-level so the later reads/writes inherit them.
    client
        .execute("SELECT set_config('search_path', $1, false)", &[&schema])
        .await
        .context("set search_path")?;
    client
        .execute("SELECT set_config('app.tenant', $1, false)", &[&tenant])
        .await
        .context("set app.tenant claim")?;

    let opts = PinOptions {
        case_id: case_id.to_string(),
        ignore_paths,
    };
    match build_pinned_case(client, run_id, &opts).await? {
        Err(pe) => Ok(PinResult::Refused(pe)),
        Ok(case) => {
            let assertions = case.expect.len();
            let (flow_id, flow_version) =
                insert_suite_and_case(client, tenant, &case, suite_id, case_id, ordinal).await?;
            Ok(PinResult::Pinned {
                flow_id,
                flow_version,
                assertions,
            })
        }
    }
}

/// Read the run + its completed node executions and fold them into a `TestCase`
/// via the pure `pin_run`, WITHOUT writing — so a caller (the gate) can observe
/// the typed `PinError` refusal. The session must already be scoped.
pub async fn build_pinned_case(
    client: &Client,
    run_id: &str,
    opts: &PinOptions,
) -> anyhow::Result<Result<TestCase, PinError>> {
    let run = read_run(client, run_id).await?;
    let node_runs = read_node_runs(client, run_id).await?;
    Ok(pin_run(&run, &node_runs, opts))
}

/// Decode the run's pinnable facts (flow + terminal outcome + input) into a
/// `RunRecord`. Errors if the run does not exist for the claimed tenant.
async fn read_run(client: &Client, run_id: &str) -> anyhow::Result<RunRecord> {
    let Some(row) = client
        .query_opt(&select_run_for_pin_sql(), &[&run_id])
        .await
        .context("read run for pin")?
    else {
        bail!("run {run_id:?} not found for the claimed tenant");
    };
    let flow_id: String = row.get(0);
    let flow_version: i32 = row.get(1);
    let status_s: String = row.get(2);
    let input_text: Option<String> = row.get(3);
    let fail_kind_s: Option<String> = row.get(4);
    let fail_node: Option<String> = row.get(5);

    let status = RunStatus::from_sql(&status_s)
        .ok_or_else(|| anyhow::anyhow!("unknown run status {status_s:?}"))?;
    let fail_kind = fail_kind_s
        .as_deref()
        .map(|s| FailKind::from_sql(s).ok_or_else(|| anyhow::anyhow!("unknown fail_kind {s:?}")))
        .transpose()?;
    let input = input_text
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .context("parse run input_json")?;

    Ok(RunRecord {
        run_id: run_id.to_string(),
        flow_id,
        flow_version: flow_version as u32,
        status,
        trigger_source: None,
        input,
        result: None,
        idempotency_key: None,
        replay_of: None,
        root_run_id: None,
        fail_kind,
        fail_node,
        fail_reason: None,
    })
}

/// Decode the run's completed node executions (with 9.6 capture provenance) into
/// `NodeRunRecord`s the pure `pin_run` reads.
async fn read_node_runs(client: &Client, run_id: &str) -> anyhow::Result<Vec<NodeRunRecord>> {
    let rows = client
        .query(&select_node_runs_for_pin_sql(), &[&run_id])
        .await
        .context("read node_runs for pin")?;
    let mut node_runs = Vec::with_capacity(rows.len());
    for row in &rows {
        let node_id: String = row.get(0);
        let occurrence: i32 = row.get(1);
        let seq: i32 = row.get(2);
        let status_s: String = row.get(3);
        let output_port: Option<String> = row.get(4);
        let output_text: Option<String> = row.get(5);
        let input_text: Option<String> = row.get(6);
        let capture_mode: Option<String> = row.get(7);
        let redacted: bool = row.get(8);

        let status = NodeRunStatus::from_sql(&status_s)
            .ok_or_else(|| anyhow::anyhow!("unknown node-run status {status_s:?}"))?;
        // A NULL output_json (capture off/preview) decodes to None — the pure
        // pin refuses off it, exactly as reconstruction maps CaptureOff.
        let output = output_text
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()
            .context("parse node output_json")?;
        let input = input_text
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()
            .context("parse node input_json")?;

        node_runs.push(NodeRunRecord {
            run_id: run_id.to_string(),
            node_id,
            occurrence: occurrence as u32,
            seq: seq as u32,
            attempt: 0,
            status,
            output_port,
            output,
            input,
            error_kind: None,
            error_detail: None,
            capture_mode,
            redacted,
            preview_head: None,
            payload_size: None,
            payload_hash: None,
        });
    }
    Ok(node_runs)
}

/// Write the pinned case: ensure the version-bound suite exists (FK to `flows`),
/// then insert the case body (the serialized `TestCase`). Idempotent — a re-pin
/// of the same `(suite_id, case_id)` overwrites. Unqualified table names (the
/// caller's `search_path` selects the schema); the same INSERT shape the
/// copy-project-env definition pass writes.
async fn insert_suite_and_case(
    client: &Client,
    tenant: &str,
    case: &TestCase,
    suite_id: &str,
    case_id: &str,
    ordinal: i32,
) -> anyhow::Result<(String, i32)> {
    let flow_ref = case
        .flow_ref
        .as_ref()
        .expect("pin_run always produces a flow-level case");
    let flow_id = flow_ref.flow_id.clone();
    let flow_version = flow_ref.version as i32;

    client
        .execute(
            "INSERT INTO test_suites (tenant_id, flow_id, flow_version, suite_id, name) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (tenant_id, flow_id, flow_version, suite_id) \
               DO UPDATE SET name = excluded.name, updated_at = now()",
            &[&tenant, &flow_id, &flow_version, &suite_id, &suite_id],
        )
        .await
        .context("insert pinned test suite (flow version must be registered)")?;

    let case_body = serde_json::to_string(case).context("serialize pinned case")?;
    client
        .execute(
            "INSERT INTO test_cases \
               (tenant_id, flow_id, flow_version, suite_id, case_id, ordinal, case_body) \
             VALUES ($1, $2, $3, $4, $5, $6, $7::text::jsonb) \
             ON CONFLICT (tenant_id, flow_id, flow_version, suite_id, case_id) \
               DO UPDATE SET ordinal = excluded.ordinal, case_body = excluded.case_body",
            &[
                &tenant,
                &flow_id,
                &flow_version,
                &suite_id,
                &case_id,
                &ordinal,
                &case_body,
            ],
        )
        .await
        .context("insert pinned test case")?;

    Ok((flow_id, flow_version))
}

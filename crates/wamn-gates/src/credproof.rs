//! credproof — the credential-vault conformance proof (wamn-17o [5.9]).
//!
//! Prove the vault END-TO-END on the LIVE runner, the ladderproof shape: a
//! pure DB client seeds ONE manual run of the committed fixture
//! (`deploy/cred/notify.flow.json` — `in -> http-request{credential:
//! notify-token} -> transform{status} -> respond`), then WAITS for the
//! separately-deployed `run-worker` (whose vault carries the secret from a
//! mounted credentials file) to claim + drive it. The notify step targets
//! serve-echo (the 9.2 reflector, which reflects the `authorization` header it
//! received), so the proof asserts BOTH halves of the 5.9 acceptance:
//!
//! * **Delivery** — the secret REACHED the target: serve-echo reflects a
//!   ONE-WAY FNV-1a digest of the `authorization` header it received
//!   (recorded as the http node's output payload), and the proof matches it
//!   against the digest of the expected secret. The flow references the
//!   credential only BY NAME, so a matching digest at the target can only
//!   have come from the vault (host resolution → per-dispatch node context →
//!   request header).
//! * **Containment** — the secret did NOT leak: because the witness is a
//!   digest (the target never echoes the raw value), the scan is TOTAL — the
//!   secret substring must appear NOWHERE the platform recorded: the run's
//!   `input_json`/`result_json`/`state_json`, the registered `graph_json`,
//!   and every `node_runs` row's input/output/error.
//!
//! Since fqg.11 the proof also carries the per-flow egress gate, using the
//! same live runner and target:
//!
//! * **Flow-level ALLOW** — `cred-notify` completing at all now also proves
//!   the flow's declared `allowed-hosts` admits the echo target (deny-all
//!   default: an undeclared flow could not have reached it).
//! * **Flow-level DENY, discriminated from the host list** — a second run of
//!   `egress-deny` (`deploy/cred/deny.flow.json`), which targets the SAME
//!   echo the runner's host-level `--allowed-hosts` admits but declares no
//!   `allowed-hosts` of its own: the run must fail terminally with the
//!   `egress-denied` code. Because the first flow just proved the host list
//!   allows this target, the denial is attributable to the flow layer alone.
//!
//! `--setup` provisions the ephemeral schema + registers both flows (the
//! LOCAL self-contained path); without it, credproof is a client against a
//! schema the deploy pipeline provisioned (the in-cluster gate of record).

use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio_postgres::Client;

use wamn_gate_harness::{check, seed_flow_version};

use crate::ladderproof::{connect_app, ladder_ddl, poll_to_terminal, seed_run, valid_ident};
use crate::traceproof::fnv1a_64;

/// The committed fixture (single source of truth; the drift-guard tests pin
/// the shape the proof asserts against).
const FLOW_JSON: &str = include_str!("../../../deploy/cred/notify.flow.json");
const FLOW_ID: &str = "cred-notify";
/// The fqg.11 deny fixture: same echo target, NO `allowed-hosts` declared.
const DENY_FLOW_JSON: &str = include_str!("../../../deploy/cred/deny.flow.json");
const DENY_FLOW_ID: &str = "egress-deny";
/// The credential name the fixture declares (`credentials[0].name` +
/// `nodes[notify].credential`).
const CREDENTIAL_NAME: &str = "notify-token";

/// The demo secret the example runner Secret carries
/// (deploy/runner-credentials.example.yaml) — distinctive enough that a
/// substring scan over the recorded rows is a meaningful leak assert.
pub const DEMO_SECRET: &str = "wamn-cred-proof-7f3a9b2e41d05c68";

#[derive(Debug, Args)]
pub struct CredProofArgs {
    /// App (wamn_app, NOSUPERUSER) Postgres URL — seeds the run + reads the
    /// result. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL — required only for --setup / --teardown.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The demo schema the deployed runner claims from (matches the runner's
    /// --schema).
    #[arg(long, default_value = "wamn_runner_demo")]
    pub schema: String,

    /// The tenant the seeded run + the runner share (matches the runner's
    /// --tenant).
    #[arg(long, default_value = "demo-tenant")]
    pub tenant: String,

    /// The serve-echo base URL the notify step targets — seeded as the run
    /// input (`{{echo}}` in the fixture's url). In-cluster this is the
    /// serve-echo Service; locally a `wamn-gates serve-echo` port.
    #[arg(long, default_value = "http://serve-echo:8091")]
    pub echo_url: String,

    /// The secret the runner's credentials file maps `notify-token` to — the
    /// value the delivery assert expects reflected, and the leak scans hunt.
    #[arg(long, default_value = DEMO_SECRET)]
    pub secret: String,

    /// Provision a fresh ephemeral schema (admin) + register the flow (app) —
    /// the LOCAL self-contained path. Omit it in-cluster.
    #[arg(long)]
    pub setup: bool,

    /// Drop the schema at the end (admin) — LOCAL cleanup only.
    #[arg(long)]
    pub teardown: bool,

    /// How long to wait for the deployed runner to drive the seeded run
    /// (covers its max idle poll — a directly-seeded run gets no doorbell).
    #[arg(long, default_value_t = 45)]
    pub timeout_secs: u64,
}

/// Provision the flow tables (superuser; the ladderproof schema shape) and
/// register the fixture active (app, under the tenant claim).
async fn setup(admin_url: &str, app_url: &str, schema: &str, tenant: &str) -> anyhow::Result<()> {
    let (admin, conn) = tokio_postgres::connect(admin_url, tokio_postgres::NoTls)
        .await
        .context("admin connect for --setup")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        admin
            .batch_execute(
                "DO $$ BEGIN \
                   IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                     CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
                   END IF; \
                 END $$;",
            )
            .await
            .context("ensure wamn_app role")?;
        admin
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; \
                 CREATE SCHEMA {schema} AUTHORIZATION postgres; \
                 GRANT USAGE ON SCHEMA {schema} TO wamn_app;"
            ))
            .await
            .context("create ephemeral schema")?;
        admin
            .batch_execute(&ladder_ddl(schema))
            .await
            .context("apply flow-table DDL")?;
        anyhow::Ok(())
    }
    .await;
    drop(admin);
    let _ = conn_task.await;
    result?;

    let app = connect_app(app_url, schema, tenant).await?;
    seed_flow_version(&app, tenant, FLOW_ID, 1, true, FLOW_JSON, true)
        .await
        .context("register cred-notify")?;
    seed_flow_version(&app, tenant, DENY_FLOW_ID, 1, true, DENY_FLOW_JSON, true)
        .await
        .context("register egress-deny")?;
    Ok(())
}

async fn teardown(admin_url: &str, schema: &str) -> anyhow::Result<()> {
    let (admin, conn) = tokio_postgres::connect(admin_url, tokio_postgres::NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let r = admin
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE;"))
        .await
        .map_err(|e| anyhow::anyhow!("drop ephemeral schema: {e}"));
    drop(admin);
    let _ = conn_task.await;
    r.map(|_| ())
}

pub async fn run(args: CredProofArgs) -> anyhow::Result<()> {
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
        "# wamn-gates credproof — credential-vault delivery + containment (schema {}, tenant {}, echo {})",
        args.schema, args.tenant, args.echo_url
    );

    if args.setup {
        let admin_url = args.admin_database_url.clone().context(
            "--setup needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
        )?;
        setup(&admin_url, &app_url, &args.schema, &args.tenant)
            .await
            .context("setup: provision schema + register the flows")?;
        println!("## setup — provisioned schema + registered {FLOW_ID} + {DENY_FLOW_ID} (active)");
    }

    let mut client = connect_app(&app_url, &args.schema, &args.tenant).await?;

    // The seeded input carries only the target URL — the credential is
    // referenced by NAME in the graph and never appears in flow data.
    let input = json!({ "echo": args.echo_url });
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let run_id = format!("cred-{nanos}");
    seed_run(
        &mut client,
        FLOW_ID,
        &run_id,
        &serde_json::to_string(&input)?,
    )
    .await?;
    println!(
        "\n## seed — manual run {run_id} written-ahead + enqueued; awaiting the deployed runner"
    );

    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let status = poll_to_terminal(&client, &run_id, deadline).await?;
    let mut ok = assert_cred_run(&client, &run_id, &status, &args.secret).await?;

    // ---- fqg.11 deny half: same echo target (the host list provably allows
    // it — the run above reached it), but the flow declares no allowed-hosts,
    // so the flow layer alone must refuse the call.
    let deny_run_id = format!("deny-{nanos}");
    seed_run(
        &mut client,
        DENY_FLOW_ID,
        &deny_run_id,
        &serde_json::to_string(&input)?,
    )
    .await?;
    println!("\n## seed — egress-deny run {deny_run_id} enqueued; awaiting the deployed runner");
    let deny_deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let deny_status = poll_to_terminal(&client, &deny_run_id, deny_deadline).await?;
    ok &= assert_deny_run(&client, &deny_run_id, &deny_status).await?;

    if args.teardown
        && let Some(admin_url) = args.admin_database_url.clone()
    {
        let _ = teardown(&admin_url, &args.schema).await;
    }

    println!("\ncredproof complete — overall PASS: {ok}");
    if !ok {
        bail!("credproof failed");
    }
    Ok(())
}

/// The two 5.9 halves over the recorded run: DELIVERY (the secret reached the
/// target, witnessed by serve-echo's reflection in the http node's output) and
/// CONTAINMENT (the secret appears nowhere else the platform recorded).
async fn assert_cred_run(
    client: &Client,
    run_id: &str,
    final_status: &str,
    secret: &str,
) -> anyhow::Result<bool> {
    println!("## assert — vault delivery + containment");
    let mut ok = true;

    check(
        &mut ok,
        &format!("run reached completed (status = {final_status})"),
        final_status == "completed",
    );

    let run = client
        .query_one(
            "SELECT input_json::text, result_json::text, state_json::text, \
                    fail_reason, trigger_source \
             FROM runs WHERE run_id = $1",
            &[&run_id],
        )
        .await?;
    let input_text: Option<String> = run.get(0);
    let result_text: Option<String> = run.get(1);
    let state_text: Option<String> = run.get(2);
    let fail_reason: Option<String> = run.get(3);
    let trigger: Option<String> = run.get(4);

    check(
        &mut ok,
        "trigger_source recorded as manual",
        trigger.as_deref() == Some("manual"),
    );
    // The transform strips the echo body, so the run result is the bare
    // status — the secret's echo never reaches the result.
    let result_val = result_text
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    check(
        &mut ok,
        "result_json is the stripped status (200)",
        result_val == Some(json!(200)),
    );

    // ---- delivery: the http node's recorded output carries the target's
    // reflection of the authorization header == the vault secret.
    let rows = client
        .query(
            "SELECT node_id, seq, status, output_port, output_json::text, input_json::text, \
                    error_detail::text \
             FROM node_runs WHERE run_id = $1 ORDER BY seq",
            &[&run_id],
        )
        .await?;
    let ids: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
    check(
        &mut ok,
        &format!("node_runs are in/notify/status/out (got {ids:?})"),
        ids == ["in", "notify", "status", "out"],
    );

    let notify_output: Option<Value> = rows
        .iter()
        .find(|r| r.get::<_, String>(0) == "notify")
        .and_then(|r| r.get::<_, Option<String>>(4))
        .and_then(|s| serde_json::from_str(&s).ok());
    let reflected_digest = notify_output
        .as_ref()
        .and_then(|v| v.get("body"))
        .and_then(|b| b.get("authorization-fnv1a"))
        .and_then(Value::as_str)
        .map(String::from);
    let expected_digest = format!("{:016x}", fnv1a_64(secret.as_bytes()));
    check(
        &mut ok,
        "DELIVERY: serve-echo's reflected digest == fnv1a(the vault secret)",
        reflected_digest.as_deref() == Some(expected_digest.as_str()),
    );

    // ---- containment: the secret substring appears NOWHERE the platform
    // recorded, except the http node's own output (the target's echo of it).
    let graph: Option<String> = client
        .query_opt(
            "SELECT graph_json::text FROM flows WHERE flow_id = $1 AND active",
            &[&FLOW_ID],
        )
        .await?
        .and_then(|r| r.get(0));
    let clean = |label: &str, text: &Option<String>, ok: &mut bool| {
        let leaked = text.as_deref().is_some_and(|t| t.contains(secret));
        check(ok, &format!("CONTAINMENT: no secret in {label}"), !leaked);
    };
    clean("flows.graph_json (by-name reference only)", &graph, &mut ok);
    clean("runs.input_json", &input_text, &mut ok);
    clean("runs.result_json", &result_text, &mut ok);
    clean("runs.state_json", &state_text, &mut ok);
    clean("runs.fail_reason", &fail_reason, &mut ok);
    for row in &rows {
        let node: String = row.get(0);
        let output: Option<String> = row.get(4);
        let input: Option<String> = row.get(5);
        let error: Option<String> = row.get(6);
        clean(&format!("node_runs[{node}].input_json"), &input, &mut ok);
        clean(&format!("node_runs[{node}].error_detail"), &error, &mut ok);
        clean(&format!("node_runs[{node}].output_json"), &output, &mut ok);
    }

    Ok(ok)
}

/// The fqg.11 deny half: the run failed terminally on the egress refusal —
/// the flow-layer denial (the host list admits this target; the completed
/// cred-notify run proved it). A failed node writes NO `node_runs` row (only
/// completed nodes checkpoint); the terminal error lands on the run itself.
async fn assert_deny_run(
    client: &Client,
    run_id: &str,
    final_status: &str,
) -> anyhow::Result<bool> {
    println!("## assert — per-flow egress deny (fqg.11)");
    let mut ok = true;

    check(
        &mut ok,
        &format!("egress-deny run reached failed (status = {final_status})"),
        final_status == "failed",
    );

    let run = client
        .query_one(
            "SELECT fail_reason, fail_kind FROM runs WHERE run_id = $1",
            &[&run_id],
        )
        .await?;
    let fail_reason: Option<String> = run.get(0);
    let fail_kind: Option<String> = run.get(1);
    check(
        &mut ok,
        &format!("DENY: terminal egress refusal recorded (fail_reason = {fail_reason:?})"),
        fail_kind.as_deref() == Some("terminal")
            && fail_reason.as_deref().is_some_and(|r| r.contains("egress")),
    );
    // The denied node never completes, so it must have NO checkpoint row —
    // only the entry node ran.
    let node_ids: Vec<String> = client
        .query(
            "SELECT node_id FROM node_runs WHERE run_id = $1 ORDER BY seq",
            &[&run_id],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    check(
        &mut ok,
        &format!("DENY: the http node has no node_runs checkpoint (got {node_ids:?})"),
        node_ids == ["in"],
    );
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed fixture parses + VALIDATES under the same engine the
    /// runner compiles it with, and pins the 5.9 shape: the credential is
    /// declared + referenced BY NAME on the notify node only, the url comes
    /// from the seeded input, and the transform strips the echo before the
    /// result (the containment spine).
    #[test]
    fn cred_fixture_declares_the_credential_by_name_only() {
        let v: Value = serde_json::from_str(FLOW_JSON).expect("fixture parses");
        let flow = wamn_flow::Flow::from_json(FLOW_JSON).expect("fixture is a wamn-flow");
        flow.validate().expect("fixture validates");
        assert_eq!(flow.flow_id.as_str(), FLOW_ID);
        assert_eq!(v["trigger"]["type"], json!("manual"));

        // The by-ref credential surface: one declared ref, one node naming it.
        let creds = v["credentials"].as_array().expect("credentials array");
        assert_eq!(creds.len(), 1);
        assert_eq!(creds[0]["name"], json!(CREDENTIAL_NAME));

        let nodes = v["nodes"].as_array().expect("nodes array");
        assert_eq!(nodes.len(), 4, "in -> notify -> status -> out");
        assert_eq!(nodes[1]["id"], json!("notify"));
        assert_eq!(nodes[1]["type"], json!("http-request"));
        assert_eq!(nodes[1]["credential"], json!(CREDENTIAL_NAME));
        assert_eq!(
            nodes[1]["config"]["url"],
            json!("{{echo}}"),
            "the target url comes from the seeded input, not the fixture"
        );
        // The stripping transform is load-bearing for containment: without it
        // the respond node would echo the target's reflection (secret and
        // all) into result_json.
        assert_eq!(nodes[2]["id"], json!("status"));
        assert_eq!(nodes[2]["type"], json!("transform"));
        assert_eq!(nodes[2]["config"]["expression"], json!("status"));
        assert_eq!(nodes[3]["type"], json!("respond"));

        // No secret material anywhere in the graph.
        assert!(
            !FLOW_JSON.contains(DEMO_SECRET),
            "the fixture must reference the credential by name only"
        );

        // fqg.11: the flow DECLARES its egress — both echo authorities (the
        // in-cluster Service and the local recipe's port). Deny-all default:
        // without this the live proof could never reach serve-echo.
        assert_eq!(
            flow.allowed_hosts,
            vec!["serve-echo:8091".to_string(), "127.0.0.1:8093".to_string()],
            "notify fixture declares exactly the two echo authorities"
        );
    }

    /// The fqg.11 deny fixture pins the discriminating shape: same `{{echo}}`
    /// target as cred-notify, but NO `allowed-hosts` (the deny-all default is
    /// what the live gate proves) and no credential.
    #[test]
    fn deny_fixture_declares_no_egress() {
        let flow = wamn_flow::Flow::from_json(DENY_FLOW_JSON).expect("fixture is a wamn-flow");
        flow.validate().expect("fixture validates");
        assert_eq!(flow.flow_id.as_str(), DENY_FLOW_ID);
        assert!(
            flow.allowed_hosts.is_empty(),
            "the deny fixture must declare NO hosts — its denial IS the gate"
        );
        assert!(flow.credentials.is_empty(), "no credential in play");
        let v: Value = serde_json::from_str(DENY_FLOW_JSON).expect("fixture parses");
        assert_eq!(
            v["nodes"][1]["config"]["url"],
            json!("{{echo}}"),
            "the deny flow targets the SAME echo the host list admits — the \
             denial discriminates the flow layer from the host layer"
        );
    }

    /// The example runner Secret carries the SAME demo mapping the proof
    /// expects: `notify-token` -> the demo secret, under the default project.
    #[test]
    fn example_runner_secret_matches_the_demo_mapping() {
        let manifest = include_str!("../../../deploy/runner-credentials.example.yaml");
        assert!(
            manifest.contains(CREDENTIAL_NAME),
            "credential name present"
        );
        assert!(manifest.contains(DEMO_SECRET), "demo secret present");
        assert!(manifest.contains("\"default\""), "keyed by project");
    }
}

//! f3proof — the POC F3 `escalate-stale-holds` end-to-end proof (wamn-24i).
//!
//! F3 is the nightly cron escalation flow: query quality holds open past 48h →
//! mark them `escalated` → notify the manager over a CREDENTIALED webhook, under
//! a fail-closed egress allowlist, in a project that idles to zero between
//! nights. This gate proves the whole chain on the LIVE runner + `wamn:postgres`
//! plugin + vault + egress guard, exercising the pieces F3 exists to validate:
//!
//! * **time-shift + structural cycle** — the flow computes `cutoff = fire-at-ms
//!   − 48h` (seconds-scale here, virtual time) with the `time-shift` node JMESPath
//!   cannot express, lists the stale open holds ONCE, and drains them through a
//!   `conditional`/`transform` cycle (`gate → advance → gate`), `escalate`/`notify`
//!   on a dead-end branch. The proof asserts BOTH stale holds end `escalated`, the
//!   FRESH hold is untouched, and the cycle ran once per hold plus the empty tail
//!   (the `gate` node's per-visit occurrences — R24).
//! * **credential vault (5.9)** — each `notify` targets serve-echo, which reflects
//!   a one-way FNV-1a digest of the `authorization` header it received. The proof
//!   matches every notify's recorded digest against `fnv1a(secret)` (delivery) and
//!   scans every recorded row for the raw secret (containment) — the credproof
//!   pattern, once per escalated hold.
//! * **egress allowlist (fqg.11)** — the flow declares `allowed-hosts: [echo]`;
//!   completing at all proves the fail-closed default admitted exactly the target.
//!
//! Two modes, one preamble (provision the holds catalog + table + seed 2 stale +
//! 1 fresh + register the gate flow):
//!   * LOCAL (default): seed a cron-shaped run directly; a separately-started
//!     run-worker (its vault from a credentials file, its host allowlist admitting
//!     the local echo) drains it. The `--setup` self-contained path.
//!   * IN-CLUSTER (`--deployment`): PARK the runner to 0 (scale-to-zero proof),
//!     let the LIVE dispatcher fire the registered CRON flow, the waker wake it
//!     0→1, and the runner drain — then assert, teardown, and restore scale
//!     floored at 1 (the wakeproof shape).

use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};

use wamn_gate_harness::{check, seed_flow_version};
use wamn_waker::KubeScale;

use crate::ladderproof::{connect_app, ladder_ddl, poll_to_terminal, seed_run, valid_ident};
use crate::traceproof::fnv1a_64;

const FLOW_ID: &str = "escalate-stale-holds";
const TENANT_DEFAULT: &str = "demo-tenant";

/// The demo secret the runner's credentials file maps `notify-webhook` to — the
/// value the delivery assert expects reflected and the containment scan hunts.
/// Distinct from credproof's so a shared substrate can carry both.
pub const DEMO_SECRET: &str = "wamn-f3-proof-1c4e77a90b2d5f83";

#[derive(Debug, Args)]
pub struct F3ProofArgs {
    /// App (wamn_app, NOSUPERUSER) Postgres URL. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL — required for --setup / --teardown.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The schema the deployed runner claims from (matches the runner's --schema).
    #[arg(long, default_value = "wamn_runner_demo")]
    pub schema: String,

    /// The tenant the seeded holds + the runner share (matches --tenant).
    #[arg(long, default_value = TENANT_DEFAULT)]
    pub tenant: String,

    /// The serve-echo authority the notify step targets — baked into the gate
    /// flow's url + allowed-hosts (F3's notify url is a constant, not input-
    /// templated, because notify runs downstream of the cycle where the run
    /// input is gone). In-cluster the Service `serve-echo:8091`; locally a
    /// `wamn-gates serve-echo` port like `127.0.0.1:8097`.
    #[arg(long, default_value = "serve-echo:8091")]
    pub echo_host: String,

    /// The secret the runner's credentials file maps `notify-webhook` to.
    #[arg(long, default_value = DEMO_SECRET)]
    pub secret: String,

    /// The `time-shift` offset (ms, signed): `cutoff = fire-at-ms + offset`. The
    /// 48h wall-clock maps to a seconds-scale offset under the gate's virtual
    /// time (default −60s). Stale holds are seeded 1h old, the fresh one now, so
    /// any offset between a second and an hour separates them.
    #[arg(long, default_value_t = -60_000)]
    pub offset_ms: i64,

    /// IN-CLUSTER: the runner Deployment to park→0 and wake. Present ⇒ the
    /// park→dispatcher-fires→wake path; absent ⇒ the LOCAL directly-seeded path.
    #[arg(long)]
    pub deployment: Option<String>,

    /// Provision schema + catalog + holds + register the gate flow (admin+app).
    #[arg(long)]
    pub setup: bool,

    /// Drop the schema at the end (admin) — LOCAL cleanup.
    #[arg(long)]
    pub teardown: bool,

    /// How long to wait for the run to reach a terminal status.
    #[arg(long, default_value_t = 90)]
    pub timeout_secs: u64,
}

/// The gate flow: the committed F3 shape (`time-shift → list → gate → {escalate
/// → notify (dead-end), advance → gate}`), but with the notify url + allowed-host
/// baked to the echo authority and a seconds-scale offset. CRON-triggered so the
/// in-cluster dispatcher fires it; the local path seeds a run directly.
fn gate_flow_json(echo_host: &str, offset_ms: i64) -> String {
    format!(
        r#"{{
  "schema-version": "0.1",
  "flow-id": "{FLOW_ID}",
  "version": 1,
  "name": "F3 escalate-stale-holds (gate)",
  "trigger": {{ "type": "cron", "schedule": "* * * * *" }},
  "entry": "shift",
  "nodes": [
    {{ "id": "shift", "type": "time-shift",
       "config": {{ "base": "\"fire-at-ms\"", "offset-ms": {offset_ms}, "format": "iso", "key": "cutoff" }} }},
    {{ "id": "list-stale", "type": "postgres",
       "config": {{ "entity": "quality_holds", "op": "list",
                    "filters": {{ "status": "eq.open", "opened_at": "lt.{{{{cutoff}}}}" }},
                    "sort": "opened_at", "limit": 500 }} }},
    {{ "id": "gate", "type": "conditional", "config": {{ "expression": "length(@) > `0`" }} }},
    {{ "id": "escalate", "type": "postgres",
       "config": {{ "entity": "quality_holds", "op": "update", "id": "[0].id", "body": "{{status: 'escalated'}}" }} }},
    {{ "id": "notify", "type": "http-request", "credential": "notify-webhook",
       "config": {{ "method": "POST", "url": "http://{echo_host}/holds",
                    "body": "{{hold: id, status: status, opened_at: opened_at}}" }} }},
    {{ "id": "advance", "type": "transform", "config": {{ "expression": "[1:]" }} }},
    {{ "id": "done", "type": "respond" }}
  ],
  "edges": [
    {{ "from": "shift", "to": "list-stale" }},
    {{ "from": "list-stale", "to": "gate" }},
    {{ "from": "gate", "from-port": "true", "to": "escalate" }},
    {{ "from": "gate", "from-port": "true", "to": "advance" }},
    {{ "from": "escalate", "to": "notify" }},
    {{ "from": "advance", "to": "gate" }},
    {{ "from": "gate", "from-port": "false", "to": "done" }}
  ],
  "credentials": [ {{ "name": "notify-webhook", "kind": "api-key" }} ],
  "allowed-hosts": ["{echo_host}"]
}}"#
    )
}

/// The minimal quality_holds catalog the `postgres` node compiles against —
/// `status` (enum) + `opened_at` (timestamptz); `id`/`tenant_id` are managed.
fn holds_catalog_json() -> String {
    json!({
        "schema-version": "0.1",
        "catalog-id": "poc-f3",
        "version": 1,
        "entities": [
            { "id": "quality_holds", "name": "quality_holds", "fields": [
                { "id": "status", "name": "status",
                  "type": { "kind": "enum", "variants": ["open", "disposed", "escalated"] } },
                { "id": "opened_at", "name": "opened_at", "type": { "kind": "timestamptz" } }
            ]}
        ]
    })
    .to_string()
}

/// The entity table the node reads/writes, under the tenant RLS floor (the 3.2
/// pattern: `id` uuid pk + `tenant_id` + the declared fields).
fn holds_ddl(schema: &str) -> String {
    format!(
        "DROP TABLE IF EXISTS {schema}.quality_holds CASCADE; \
         DROP TABLE IF EXISTS {schema}.wamn_catalog CASCADE; \
         CREATE TABLE {schema}.quality_holds ( \
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
           tenant_id text NOT NULL, \
           status text NOT NULL DEFAULT 'open' CHECK (status IN ('open','disposed','escalated')), \
           opened_at timestamptz NOT NULL DEFAULT now()); \
         ALTER TABLE {schema}.quality_holds ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {schema}.quality_holds FORCE ROW LEVEL SECURITY; \
         CREATE POLICY quality_holds_tenant ON {schema}.quality_holds \
           USING (tenant_id = current_setting('app.tenant', true)) \
           WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.quality_holds TO wamn_app; \
         CREATE TABLE {schema}.wamn_catalog ( \
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
           tenant_id text NOT NULL, document jsonb NOT NULL); \
         ALTER TABLE {schema}.wamn_catalog ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {schema}.wamn_catalog FORCE ROW LEVEL SECURITY; \
         CREATE POLICY wamn_catalog_tenant ON {schema}.wamn_catalog \
           USING (tenant_id = current_setting('app.tenant', true)) \
           WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
         GRANT SELECT ON {schema}.wamn_catalog TO wamn_app;"
    )
}

/// Provision the runner tables + the holds catalog/table (superuser), then
/// register the gate flow + seed 2 stale + 1 fresh hold (app, under the claim).
async fn setup(args: &F3ProofArgs, admin_url: &str, app_url: &str) -> anyhow::Result<()> {
    let schema = &args.schema;
    let (admin, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect for --setup")?;
    let conn_task = tokio::spawn(conn);
    // LOCAL (no --deployment) provisions a FRESH throwaway schema + the runner
    // tables. IN-CLUSTER adds only the (idempotent) holds/catalog tables to the
    // runner's EXISTING wamn_runner_demo — never dropping the live run-state.
    let fresh_schema = args.deployment.is_none();
    let result = async {
        if fresh_schema {
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
                .context("apply runner-table DDL")?;
        }
        admin
            .batch_execute(&holds_ddl(schema))
            .await
            .context("apply holds + catalog DDL")?;
        // The catalog snapshot the node resolves through `catalog_json` — written
        // by the SUPERUSER (bypasses RLS), like the f1bench precedent; `wamn_app`
        // holds only SELECT on wamn_catalog (it is read-only for the runtime).
        admin
            .execute(
                &format!(
                    "INSERT INTO {schema}.wamn_catalog (tenant_id, document) VALUES ($1, $2::text::jsonb)"
                ),
                &[&args.tenant, &holds_catalog_json()],
            )
            .await
            .context("write catalog snapshot")?;
        anyhow::Ok(())
    }
    .await;
    drop(admin);
    let _ = conn_task.await;
    result?;

    let app = connect_app(app_url, schema, &args.tenant).await?;
    seed_holds(&app, &args.tenant).await?;
    seed_flow_version(
        &app,
        &args.tenant,
        FLOW_ID,
        1,
        true,
        &gate_flow_json(&args.echo_host, args.offset_ms),
        true,
    )
    .await
    .context("register the gate flow")?;
    Ok(())
}

/// Seed the discriminating hold set:
///   * 2 STALE OPEN holds (opened 1h ago) — the drain MUST escalate exactly these;
///   * 1 FRESH OPEN hold (opened now) — newer than the seconds-scale cutoff, so
///     the `opened_at < cutoff` filter must leave it open;
///   * 1 STALE DISPOSED hold (opened 1h ago, already `disposed`) — old enough to
///     match the cutoff, so ONLY the `status = open` predicate keeps it out. A
///     list filter that dropped `status = open` would escalate + notify it too,
///     breaking the "2 escalated / 2 notifies / disposed untouched" asserts
///     (mutant ii).
async fn seed_holds(app: &Client, tenant: &str) -> anyhow::Result<()> {
    app.execute(
        "INSERT INTO quality_holds (tenant_id, status, opened_at) VALUES \
           ($1, 'open', now() - interval '1 hour'), \
           ($1, 'open', now() - interval '1 hour'), \
           ($1, 'open', now()), \
           ($1, 'disposed', now() - interval '1 hour')",
        &[&tenant],
    )
    .await
    .context("seed holds")?;
    Ok(())
}

/// Zero-residue teardown. LOCAL drops the throwaway schema; IN-CLUSTER removes
/// only what the gate added to the live runner schema — the holds/catalog tables
/// and the gate flow's `flows`/`runs` rows — leaving the runner's own state.
async fn teardown(admin_url: &str, schema: &str, fresh_schema: bool) -> anyhow::Result<()> {
    let (admin, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let sql = if fresh_schema {
        format!("DROP SCHEMA IF EXISTS {schema} CASCADE;")
    } else {
        format!(
            "DROP TABLE IF EXISTS {schema}.quality_holds CASCADE; \
             DROP TABLE IF EXISTS {schema}.wamn_catalog CASCADE; \
             DELETE FROM {schema}.node_runs WHERE run_id IN \
               (SELECT run_id FROM {schema}.runs WHERE flow_id = '{FLOW_ID}'); \
             DELETE FROM {schema}.run_queue WHERE run_id IN \
               (SELECT run_id FROM {schema}.runs WHERE flow_id = '{FLOW_ID}'); \
             DELETE FROM {schema}.runs WHERE flow_id = '{FLOW_ID}'; \
             DELETE FROM {schema}.flows WHERE flow_id = '{FLOW_ID}';"
        )
    };
    let r = admin
        .batch_execute(&sql)
        .await
        .map_err(|e| anyhow::anyhow!("teardown: {e}"));
    drop(admin);
    let _ = conn_task.await;
    r.map(|_| ())
}

pub async fn run(args: F3ProofArgs) -> anyhow::Result<()> {
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
        "# wamn-gates f3proof — cron escalation + vault + egress (schema {}, tenant {}, echo {}, offset {}ms)",
        args.schema, args.tenant, args.echo_host, args.offset_ms
    );

    if args.setup {
        let admin_url = args
            .admin_database_url
            .clone()
            .context("--setup needs a superuser url: --admin-database-url / WAMN_PG_ADMIN_URL")?;
        setup(&args, &admin_url, &app_url)
            .await
            .context("setup: provision schema + catalog + holds + register flow")?;
        println!("## setup — schema + catalog + 2 stale/1 fresh holds + gate flow (active)");
    }

    let mut client = connect_app(&app_url, &args.schema, &args.tenant).await?;

    // Park-and-wake (in-cluster) or direct-seed (local) — either way the LIVE
    // runner drains the run, and the assertions read the same DB outcome.
    let (run_id, mut ok, scale_restore) = if let Some(deployment) = args.deployment.clone() {
        drive_in_cluster(&client, &args, &deployment).await?
    } else {
        (drive_local(&mut client, &args).await?, true, None)
    };

    ok &= assert_f3(&client, &run_id, &args.secret).await?;

    // Restore scale floored at 1 (in-cluster only).
    if let (Some(scale), Some(deployment)) = (scale_restore, args.deployment.clone()) {
        let kube = KubeScale::in_cluster()?;
        kube.set_replicas(&deployment, scale).await?;
        println!("## restore — {deployment} scaled back to {scale}");
    }

    if args.teardown
        && let Some(admin_url) = args.admin_database_url.clone()
    {
        let _ = teardown(&admin_url, &args.schema, args.deployment.is_none()).await;
    }

    println!("\nf3proof complete — overall PASS: {ok}");
    if !ok {
        bail!("f3proof failed");
    }
    Ok(())
}

/// LOCAL: seed a cron-shaped run directly and let the separately-started
/// run-worker drain it. `fire-at-ms` = now, so the seconds-scale cutoff lands
/// between the stale (1h old) and fresh (now) holds.
async fn drive_local(client: &mut Client, args: &F3ProofArgs) -> anyhow::Result<String> {
    let now_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let input = json!({ "trigger": "cron", "schedule": "* * * * *", "fire-at-ms": now_ms });
    let run_id = format!("f3-{now_ms}");
    seed_run(client, FLOW_ID, &run_id, &serde_json::to_string(&input)?).await?;
    println!("## seed — cron-shaped run {run_id} (fire-at-ms {now_ms}); awaiting the runner");
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let status = poll_to_terminal(client, &run_id, deadline).await?;
    println!("## drained — run reached {status}");
    Ok(run_id)
}

/// IN-CLUSTER: park the runner to 0 (scale-to-zero proof), let the LIVE
/// dispatcher fire the registered CRON flow (a DISTINCT phase — isolating a
/// projects-config failure from a wake failure, the wakeproof precedent), then
/// the waker wakes 0→1 and the runner drains. Returns the fired run id, the
/// running verdict, and the replica count to restore (floored at 1).
async fn drive_in_cluster(
    client: &Client,
    args: &F3ProofArgs,
    deployment: &str,
) -> anyhow::Result<(String, bool, Option<i32>)> {
    let mut ok = true;
    let kube = KubeScale::in_cluster()?;

    // --- park ---
    let original = kube.get_scale(deployment).await?;
    let restore_to = original.spec_replicas.max(1);
    kube.set_replicas(deployment, 0).await?;
    let park_deadline = Instant::now() + Duration::from_secs(60);
    let parked = wait_scale(&kube, deployment, park_deadline, |s| s.status_replicas == 0).await?;
    check(&mut ok, "PARK: runner scaled to 0 replicas", parked);

    // --- dispatcher fires (distinct phase) ---
    let fire_deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let run_id = loop {
        if let Some(id) = latest_cron_run(client).await? {
            break id;
        }
        if Instant::now() > fire_deadline {
            check(
                &mut ok,
                "DISPATCH: a cron run was written by the dispatcher",
                false,
            );
            return Ok((String::new(), ok, Some(restore_to)));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    check(&mut ok, "DISPATCH: the dispatcher fired a cron run", true);

    // --- wake 0→1 + drain ---
    let wake_deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let woke = wait_scale(&kube, deployment, wake_deadline, |s| s.spec_replicas > 0).await?;
    check(&mut ok, "WAKE: the waker scaled the runner 0→1", woke);
    let status = poll_to_terminal(client, &run_id, wake_deadline).await?;
    check(
        &mut ok,
        &format!("DRAIN: cron run completed (status {status})"),
        status == "completed",
    );

    Ok((run_id, ok, Some(restore_to)))
}

async fn wait_scale(
    kube: &KubeScale,
    deployment: &str,
    deadline: Instant,
    pred: impl Fn(&wamn_waker::Scale) -> bool,
) -> anyhow::Result<bool> {
    loop {
        if pred(&kube.get_scale(deployment).await?) {
            return Ok(true);
        }
        if Instant::now() > deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn latest_cron_run(client: &Client) -> anyhow::Result<Option<String>> {
    Ok(client
        .query_opt(
            "SELECT run_id FROM runs WHERE flow_id = $1 AND trigger_source = 'cron' \
             ORDER BY updated_at DESC LIMIT 1",
            &[&FLOW_ID],
        )
        .await?
        .map(|r| r.get(0)))
}

/// The F3 acceptance over the drained run: DB escalation state, credential
/// delivery per notify (fnv1a digest), containment (no raw secret recorded), and
/// the cycle proof (the `gate` node visited once per hold + the empty tail).
async fn assert_f3(client: &Client, run_id: &str, secret: &str) -> anyhow::Result<bool> {
    println!("## assert — escalation + vault delivery + containment + cycle drain");
    let mut ok = true;

    // --- DB: exactly the 2 stale holds escalated; the fresh one untouched. ---
    let escalated: i64 = client
        .query_one(
            "SELECT count(*) FROM quality_holds WHERE status = 'escalated'",
            &[],
        )
        .await?
        .get(0);
    let still_open: i64 = client
        .query_one(
            "SELECT count(*) FROM quality_holds WHERE status = 'open'",
            &[],
        )
        .await?
        .get(0);
    let still_disposed: i64 = client
        .query_one(
            "SELECT count(*) FROM quality_holds WHERE status = 'disposed'",
            &[],
        )
        .await?
        .get(0);
    check(
        &mut ok,
        &format!("DB: 2 stale OPEN holds escalated (got {escalated})"),
        escalated == 2,
    );
    check(
        &mut ok,
        &format!("DB: the fresh hold is untouched — still open (got {still_open})"),
        still_open == 1,
    );
    check(
        &mut ok,
        &format!(
            "DB: the stale DISPOSED hold is untouched — status=open filter held (got {still_disposed})"
        ),
        still_disposed == 1,
    );

    // --- run completed; the cycle drained (gate visited 3x: 2 holds + empty). ---
    let run_status: String = client
        .query_one("SELECT status FROM runs WHERE run_id = $1", &[&run_id])
        .await?
        .get(0);
    check(
        &mut ok,
        &format!("CYCLE: run completed (status {run_status})"),
        run_status == "completed",
    );
    let gate_visits: i64 = client
        .query_one(
            "SELECT count(*) FROM node_runs WHERE run_id = $1 AND node_id = 'gate'",
            &[&run_id],
        )
        .await?
        .get(0);
    check(
        &mut ok,
        &format!("CYCLE: the gate node was visited 3x (2 holds + empty tail; got {gate_visits})"),
        gate_visits == 3,
    );

    // --- delivery: every notify visit reflected fnv1a(secret) from serve-echo. ---
    let notify_rows = client
        .query(
            "SELECT occurrence, output_json::text FROM node_runs \
             WHERE run_id = $1 AND node_id = 'notify' ORDER BY occurrence",
            &[&run_id],
        )
        .await?;
    let expected = format!("{:016x}", fnv1a_64(secret.as_bytes()));
    let delivered = notify_rows.len() == 2
        && notify_rows.iter().all(|r| {
            r.get::<_, Option<String>>(1)
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .as_ref()
                .and_then(|v| v.get("body"))
                .and_then(|b| b.get("authorization-fnv1a"))
                .and_then(Value::as_str)
                == Some(expected.as_str())
        });
    check(
        &mut ok,
        &format!(
            "DELIVERY: 2 credentialed notifies, each digest == fnv1a(secret) (got {} rows)",
            notify_rows.len()
        ),
        delivered,
    );

    // --- containment: the raw secret appears NOWHERE the platform recorded. ---
    let run = client
        .query_one(
            "SELECT input_json::text, result_json::text, state_json::text, fail_reason \
             FROM runs WHERE run_id = $1",
            &[&run_id],
        )
        .await?;
    let graph: Option<String> = client
        .query_opt(
            "SELECT graph_json::text FROM flows WHERE flow_id = $1 AND active",
            &[&FLOW_ID],
        )
        .await?
        .and_then(|r| r.get(0));
    let nodes = client
        .query(
            "SELECT node_id, output_json::text, input_json::text, error_detail::text \
             FROM node_runs WHERE run_id = $1",
            &[&run_id],
        )
        .await?;
    let clean = |label: &str, text: &Option<String>, ok: &mut bool| {
        let leaked = text.as_deref().is_some_and(|t| t.contains(secret));
        check(ok, &format!("CONTAINMENT: no secret in {label}"), !leaked);
    };
    clean("flows.graph_json", &graph, &mut ok);
    for (i, label) in ["input_json", "result_json", "state_json", "fail_reason"]
        .iter()
        .enumerate()
    {
        clean(
            &format!("runs.{label}"),
            &run.get::<_, Option<String>>(i),
            &mut ok,
        );
    }
    for row in &nodes {
        let node: String = row.get(0);
        clean(
            &format!("node_runs[{node}].output_json"),
            &row.get::<_, Option<String>>(1),
            &mut ok,
        );
        clean(
            &format!("node_runs[{node}].input_json"),
            &row.get::<_, Option<String>>(2),
            &mut ok,
        );
        clean(
            &format!("node_runs[{node}].error_detail"),
            &row.get::<_, Option<String>>(3),
            &mut ok,
        );
    }

    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate flow the proof registers is a real, valid F3 flow: the cron
    /// trigger, the declared credential + egress host, and the structural cycle
    /// (advance loops to the gate; notify is a dead-end). A malformed builder
    /// (e.g. a broken JMESPath in a config) fails here, not only in-cluster.
    #[test]
    fn gate_flow_is_a_valid_f3_flow() {
        let json = gate_flow_json("serve-echo:8091", -60_000);
        let flow = wamn_flow::Flow::from_json(&json).expect("gate flow parses");
        flow.validate().expect("gate flow validates");
        assert_eq!(flow.flow_id, FLOW_ID);
        assert_eq!(flow.allowed_hosts, vec!["serve-echo:8091".to_string()]);
        assert!(flow.credentials.iter().any(|c| c.name == "notify-webhook"));
        assert!(
            flow.edges
                .iter()
                .any(|e| e.from == "advance" && e.to == "gate"),
            "the structural cycle closes back to the gate"
        );
        assert!(
            !flow.edges.iter().any(|e| e.from == "notify"),
            "notify is a dead-end — it carries no loop state"
        );
        // The catalog document the node compiles against is well-formed JSON.
        let cat: Value = serde_json::from_str(&holds_catalog_json()).expect("catalog json");
        assert_eq!(cat["entities"][0]["name"], "quality_holds");
    }

    /// The example runner Secret carries the mapping the in-cluster gate resolves:
    /// `notify-webhook` -> the demo secret, under the default project. Keeps the
    /// manifest and the gate's expected secret from drifting apart.
    #[test]
    fn example_runner_secret_carries_the_notify_webhook() {
        let manifest = include_str!("../../../deploy/platform/runner-credentials.example.yaml");
        assert!(
            manifest.contains("notify-webhook"),
            "credential name present"
        );
        assert!(manifest.contains(DEMO_SECRET), "f3 demo secret present");
        assert!(
            manifest.contains("\"default\""),
            "keyed by the default project"
        );
    }
}

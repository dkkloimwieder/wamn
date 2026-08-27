//! The always-on dispatcher reconciles durable queue state across projects,
//! emits best-effort NATS doorbell hints, and adapts each project's sweep
//! cadence to observed work.
//!
//! MVP outcome: wake-from-zero.
//!
//! One project sweep hints every currently-due unleased queue row and then
//! tightens or decays the project's cadence. Dropped database connections are
//! re-dialed and every sweep has a deadline so one unhealthy project cannot
//! wedge the others.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio_postgres::{Client, NoTls};
use wamn_run_state::queue::parked_due_sql;
use wamn_scheduler::Cadence;

// R16b (wamn-2jkm.20): the dispatcher's pinned session `SET`s interpolate the
// tenant/schema, so these validators are the injection boundary HERE — and they
// are the SAME rule the wamn:postgres plugin enforces, held in one owner.
use wamn_control_registry::identifiers::{
    ExecutionTargetId, doorbell_subject, mvp_execution_target_id, valid_schema, valid_tenant,
};

/// [9.8] The claimable run-queue depth for the pinned session's tenant. Mirrors
/// EXACTLY the eligibility predicate of the production claim selector
/// (`crates/execution/run-state/src/queue/sql.rs`: `available_at` reached, lease NULL-or-
/// expired, budget-remaining), so the gauge counts precisely the rows a runner
/// could claim right now. Inverting a clause (e.g. `available_at > now()`) makes
/// a seeded queue read 0 — metricbench phase 2's mutant.
pub const RUN_QUEUE_DEPTH_SQL: &str = "SELECT count(*)::bigint FROM run_queue AS q \
     WHERE q.tenant_id = current_setting('app.tenant', true) \
       AND q.available_at <= now() \
       AND (q.lease_expires_at IS NULL OR q.lease_expires_at <= now()) \
       AND (q.attempts < q.max_attempts OR q.lease_expires_at IS NULL \
            OR EXISTS (SELECT 1 FROM effect_attempts AS effect \
                        WHERE effect.tenant_id = q.tenant_id \
                          AND effect.run_id = q.run_id))";

/// [9.8] One project's last-sampled claimable queue depth, with the tenant its
/// gauge series is labelled by.
#[derive(Clone, Debug)]
pub struct DepthSample {
    pub tenant: String,
    pub depth: i64,
}

/// [9.8] Shared per-project depth samples (keyed by project name) the
/// `wamn.run_queue.depth` observable gauge folds at export time. [`Dispatcher::tick_project`]
/// republishes its project's sample each sweep — no new loop, the sweep IS the
/// interval.
pub type DepthRegistry = Arc<Mutex<HashMap<String, DepthSample>>>;

#[derive(Debug, Args)]
pub struct DispatchArgs {
    /// JSON projects map the dispatcher serves:
    /// {"<name>": {"url": "...", "tenant": "...", "execution_target_id":
    /// "...", "schema": "wamn_run"}}
    /// (a mounted Secret/ConfigMap in production — the 2.2 projects-file shape).
    ///
    /// `url` carries the dispatcher's DATABASE PRINCIPAL: this file is the only
    /// place it is stated, because the dispatcher has no separate DB-URL Secret.
    /// It must be a `wamn_dispatch_reader` credential GENERATION
    /// (`wamn_dispatch_reader_<40-hex>_<a|b>`, wamn-0h0g.22.24) — a per-database
    /// LOGIN that INHERITS the stable role's schema `USAGE` and `SELECT` on
    /// `run_queue` and `effect_attempts`, which is exactly what a sweep touches
    /// and nothing more.
    ///
    /// **NOT the stable `wamn_dispatch_reader` role itself.** That role is
    /// cluster-global and NOLOGIN: a `CONNECT` granted to it is inherited by
    /// every generation into every database on the cluster, which is the reach
    /// wamn-0h0g.12.179 measured live for the guest family and wamn-0h0g.22.24
    /// closed for this one. `CONNECT` belongs to the generation, and only for
    /// the one database it was minted for.
    ///
    /// One entry is ONE database pinned to ONE `tenant`, so the credential is
    /// deliberately NOBYPASSRLS: the per-relation policy keyed on `app.tenant`
    /// admits precisely the rows this session already read under the old shared
    /// credential. Provisioning must prepare the generation and its grants
    /// BEFORE a pod carrying these URLs starts — the initial dial is fatal by
    /// design. Rotation is the ordinary A/B lifecycle: prepare the other
    /// generation, update this file, roll, then retire the old one.
    #[arg(long, env = "WAMN_DISPATCH_PROJECTS_FILE")]
    pub projects_file: Option<PathBuf>,

    /// Single-project fallback: the one app database URL this dispatcher
    /// authenticates with when no projects file is given.
    ///
    /// `WAMN_PG_URL` is the DECLARED TRANSPORT, not a fallback: naming it on
    /// the argument makes clap the single place the credential is read.
    /// `wamn-0h0g.22.30` removed the two ambient sources (`WAMN_PG_URL` and
    /// `DATABASE_URL`) that `resolve_projects` read behind it — an explicit
    /// source plus any ambient source is the conflict
    /// `credential_exactness::AmbientCredentialState` already declares, and the
    /// dispatcher was its own second and third source. That mattered here
    /// because `wamn-0h0g.22.24` cut this service onto per-database credential
    /// generations: an ambient URL resolving to a shared or stale login would
    /// quietly defeat that cutover.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: Option<String>,

    /// Tenant claim for the single-project fallback.
    #[arg(long, default_value = "default")]
    pub tenant: String,

    /// Opaque doorbell routing target for the single-project fallback. When
    /// omitted, the MVP placement adapter assigns the validated tenant value.
    #[arg(long, env = "WAMN_EXECUTION_TARGET_ID")]
    pub execution_target_id: Option<ExecutionTargetId>,

    /// search_path for the single-project fallback (e.g. wamn_run).
    #[arg(long)]
    pub schema: Option<String>,

    /// NATS URL for doorbell hints. The dispatcher runs without NATS (hints are
    /// fire-and-forget; the reconciliation sweep guarantees pickup), just slower.
    #[arg(long, default_value = "nats://localhost:4222")]
    pub nats_url: String,

    /// mTLS material for the doorbell NATS connection (mount the
    /// wasmcloud-runtime-tls secret in-cluster). Omit for plain NATS.
    #[arg(long)]
    pub nats_tls_ca: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_cert: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_key: Option<PathBuf>,

    /// Tightest per-project sweep interval (a busy project's cadence).
    #[arg(long, default_value_t = wamn_scheduler::DEFAULT_MIN_INTERVAL_MS)]
    pub min_interval_ms: i64,

    /// Widest per-project sweep interval (an idle project's reconciliation
    /// cadence).
    #[arg(long, default_value_t = wamn_scheduler::DEFAULT_MAX_INTERVAL_MS)]
    pub max_interval_ms: i64,

    /// Max wake hints processed per project per sweep (the
    /// fairness bound: one project's backlog cannot monopolize a sweep).
    #[arg(long, default_value_t = 64)]
    pub batch: usize,

    /// Read deterministic tick commands as newline-delimited JSON on stdin.
    ///
    /// This process-boundary control mode is for operational probes and gates
    /// that must supply virtual time without linking the deployable service.
    /// The normal long-running dispatcher is unchanged when this flag is absent.
    #[arg(long)]
    pub stepped_stdio: bool,
}

/// One project the dispatcher serves: its database location, tenant data key,
/// and distinct doorbell execution target.
#[derive(Debug, Clone)]
pub struct ProjectSpec {
    pub name: String,
    pub url: String,
    pub tenant: String,
    pub execution_target_id: ExecutionTargetId,
    pub schema: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ProjectSpecInput {
    url: String,
    tenant: String,
    #[serde(default)]
    execution_target_id: Option<ExecutionTargetId>,
    #[serde(default)]
    schema: Option<String>,
}

fn project_spec(name: String, input: ProjectSpecInput) -> anyhow::Result<ProjectSpec> {
    let execution_target_id = match input.execution_target_id {
        Some(target) => target,
        None => mvp_execution_target_id(&input.tenant)
            .with_context(|| format!("project {name}: derive MVP execution target from tenant"))?,
    };
    Ok(ProjectSpec {
        name,
        url: input.url,
        tenant: input.tenant,
        execution_target_id,
        schema: input.schema,
    })
}

/// Dial one project: a pinned session with `search_path` + the tenant claim set
/// (the RLS floor the queue SQL is scoped by), a connect deadline, and TCP
/// keepalives (a silently dead peer is detected in tens of seconds, not the
/// kernel's two-hour default).
async fn dial(spec: &ProjectSpec) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let mut config = tokio_postgres::Config::from_str(&spec.url)
        .with_context(|| format!("parse url for project {}", spec.name))?;
    config.connect_timeout(Duration::from_secs(10));
    config.keepalives_idle(Duration::from_secs(30));
    let (client, conn) = config
        .connect(NoTls)
        .await
        .with_context(|| format!("connect project {}", spec.name))?;
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    let mut session = String::new();
    if let Some(s) = &spec.schema {
        session.push_str(&format!("SET search_path TO {s}; "));
    }
    session.push_str(&format!("SET app.tenant TO '{}';", spec.tenant));
    client
        .batch_execute(&session)
        .await
        .with_context(|| format!("set claims for project {}", spec.name))?;
    Ok((client, handle))
}

/// A project's pinned connection and adaptive-cadence state.
pub struct ProjectState {
    pub spec: ProjectSpec,
    client: Client,
    _conn: tokio::task::JoinHandle<()>,
    /// Adaptive sweep interval (tightens on work, decays while idle).
    pub interval_ms: i64,
    pub last_sweep_ms: i64,
}

/// What one project reconciliation sweep did.
#[derive(Debug, Default, serde::Serialize)]
pub struct TickReport {
    /// Due unleased queue rows hinted this sweep (parked wakes + lost-hint
    /// reconciliation). Duplicate hints across sweeps are by design: harmless
    /// (the claim is the arbiter), and a persistently-unclaimed backlog SHOULD
    /// keep the cadence tight — waking a scale-to-zero runner is the point.
    pub woken: Vec<String>,
}

impl TickReport {
    pub fn found_work(&self) -> bool {
        !self.woken.is_empty()
    }
}

pub struct DispatcherConfig {
    /// The validated adaptive poll cadence: an inverted `min > max` band is
    /// rejected at [`Cadence::new`], so the dispatcher's interval math can never
    /// see one (R13-hardening — inverted ranges are unrepresentable here).
    pub cadence: Cadence,
    pub batch: usize,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            // The default band is a compile-time-known valid range; an Err here
            // would be a broken constant, not user input (M-PANIC-ON-BUG).
            cadence: Cadence::new(
                wamn_scheduler::DEFAULT_MIN_INTERVAL_MS,
                wamn_scheduler::DEFAULT_MAX_INTERVAL_MS,
            )
            .expect("default cadence bounds are valid"),
            batch: 64,
        }
    }
}

fn reconcile_due(now_ms: i64, last_sweep_ms: i64, interval_ms: i64) -> bool {
    now_ms - last_sweep_ms >= interval_ms
}

fn next_reconcile(last_sweep_ms: i64, interval_ms: i64) -> i64 {
    last_sweep_ms + interval_ms
}

/// The dispatcher: per-project state + the optional doorbell client + the
/// cadence config. One instance is one replica; running several is safe because
/// sweeps are read-only and downstream claims arbitrate duplicate hints.
pub struct Dispatcher {
    pub projects: Vec<ProjectState>,
    nats: Option<async_nats::Client>,
    cfg: DispatcherConfig,
    /// [9.8] Per-project claimable-queue-depth samples the `wamn.run_queue.depth`
    /// gauge reads; refreshed each sweep by [`Dispatcher::tick_project`].
    depth: DepthRegistry,
}

impl Dispatcher {
    /// Connect every project (the per-project connections D3 requires:
    /// "reconciliation follows connection ownership — no cross-DB sweep").
    pub async fn connect(
        specs: &[ProjectSpec],
        nats: Option<async_nats::Client>,
        cfg: DispatcherConfig,
    ) -> anyhow::Result<Self> {
        let mut projects = Vec::with_capacity(specs.len());
        for spec in specs {
            if !valid_tenant(&spec.tenant) {
                bail!("project {}: invalid tenant {:?}", spec.name, spec.tenant);
            }
            if let Some(s) = &spec.schema
                && !valid_schema(s)
            {
                bail!("project {}: invalid schema {:?}", spec.name, s);
            }
            let (client, handle) = dial(spec).await?;
            projects.push(ProjectState {
                spec: spec.clone(),
                client,
                _conn: handle,
                interval_ms: cfg.cadence.min(),
                last_sweep_ms: 0,
            });
        }
        Ok(Self {
            projects,
            nats,
            cfg,
            depth: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// [9.8] The shared depth registry to register the `wamn.run_queue.depth`
    /// gauge over ([`register_queue_depth_gauge`]). Cloned so the gauge callback
    /// and the sweep loop share it.
    pub fn depth_registry(&self) -> DepthRegistry {
        self.depth.clone()
    }

    /// One project reconciliation sweep at an injected cadence instant.
    pub async fn tick_project(&mut self, idx: usize, now_ms: i64) -> anyhow::Result<TickReport> {
        let (batch, cadence) = (self.cfg.batch, self.cfg.cadence);
        let nats = self.nats.as_ref();
        // [9.8] cloned before the &mut borrow of projects, updated after the scan.
        let depth = self.depth.clone();

        // A dropped connection (DB restart, failover, network blip) is
        // re-dialed rather than fatal: an always-on dispatcher must outlive its
        // projects' databases. Failure here fails the sweep; the loop decays
        // and retries.
        if self.projects[idx].client.is_closed() {
            let spec = self.projects[idx].spec.clone();
            let (client, handle) = dial(&spec).await?;
            let p = &mut self.projects[idx];
            p.client = client;
            p._conn = handle;
            tracing::info!(project = %spec.name, "dispatcher: reconnected project");
        }

        let p = &mut self.projects[idx];
        let mut report = TickReport::default();

        let mut doorbells: Vec<String> = Vec::new();

        // 1. Wake / reconciliation: hint every currently-due unleased row.
        for row in p.client.query(&parked_due_sql(batch), &[]).await? {
            let run_id: String = row.get("run_id");
            doorbells.push(run_id.clone());
            report.woken.push(run_id);
        }

        // [9.8] republish this project's CLAIMABLE queue depth (the same
        // predicate a runner claims by) for the wamn.run_queue.depth gauge —
        // piggybacked on the existing sweep, no new loop.
        let queue_depth: i64 = p.client.query_one(RUN_QUEUE_DEPTH_SQL, &[]).await?.get(0);
        if let Ok(mut d) = depth.lock() {
            d.insert(
                p.spec.name.clone(),
                DepthSample {
                    tenant: p.spec.tenant.clone(),
                    depth: queue_depth,
                },
            );
        }

        // Doorbells follow the durable queue SELECT. They are only hints; the
        // executor's direct claim remains the state-transition boundary.
        if let Some(nats) = nats
            && !doorbells.is_empty()
        {
            let subject = doorbell_subject(&p.spec.execution_target_id);
            for run_id in doorbells {
                nats.publish(subject.clone(), run_id.into_bytes().into())
                    .await?;
            }
            nats.flush().await?;
        }

        // 2. Adaptive cadence.
        p.interval_ms = cadence.next_interval(p.interval_ms, report.found_work());
        p.last_sweep_ms = now_ms;
        Ok(report)
    }

    /// Tick each project when its adaptive reconciliation interval elapses.
    /// Each sweep has a deadline so one unhealthy project cannot wedge the
    /// others; a failing project decays and retries on its next interval.
    pub async fn run_loop(
        &mut self,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        // Generous per-sweep deadline: wedge protection against hours-long
        // black holes, far above any healthy sweep.
        let sweep_deadline = Duration::from_millis((2 * self.cfg.cadence.max()).max(5_000) as u64);
        loop {
            let now = epoch_ms();
            for i in 0..self.projects.len() {
                let p = &self.projects[i];
                let due = reconcile_due(now, p.last_sweep_ms, p.interval_ms);
                if !due {
                    continue;
                }
                let outcome = tokio::time::timeout(sweep_deadline, self.tick_project(i, now)).await;
                let failed = match outcome {
                    Ok(Ok(_)) => false,
                    Ok(Err(e)) => {
                        tracing::warn!(project = %self.projects[i].spec.name, error = %e,
                            "dispatcher: sweep failed (retrying next interval)");
                        true
                    }
                    Err(_) => {
                        tracing::warn!(project = %self.projects[i].spec.name,
                            "dispatcher: read-only sweep timed out (abandoned; retrying later)");
                        true
                    }
                };
                if failed {
                    let cadence = self.cfg.cadence;
                    let p = &mut self.projects[i];
                    p.last_sweep_ms = now;
                    p.interval_ms = cadence.next_interval(p.interval_ms, false);
                }
            }

            let now = epoch_ms();
            let next = self
                .projects
                .iter()
                .map(|p| next_reconcile(p.last_sweep_ms, p.interval_ms))
                .min()
                .unwrap_or(now + self.cfg.cadence.max());
            let sleep_ms = (next - now).clamp(10, self.cfg.cadence.max()) as u64;
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// [9.8] Register the `wamn.run_queue.depth` observable gauge over the
/// dispatcher's shared depth registry, keyed by `wamn.tenant` / `wamn.project`.
/// Uses the global meter (the provider `main` installs when `OTEL_*` is set) — a
/// no-op otherwise. Call ONCE (observable instruments warn on duplicate
/// registration); the callback folds every project's last-sampled depth.
pub fn register_queue_depth_gauge(depth: &DepthRegistry) {
    let depth = depth.clone();
    let _ = opentelemetry::global::meter("wamn-dispatcher")
        .i64_observable_gauge("wamn.run_queue.depth")
        .with_description("claimable runs waiting in a project's run_queue")
        .with_callback(move |o| {
            if let Ok(d) = depth.lock() {
                for (project, sample) in d.iter() {
                    o.observe(
                        sample.depth,
                        &[
                            opentelemetry::KeyValue::new("wamn.tenant", sample.tenant.clone()),
                            opentelemetry::KeyValue::new("wamn.project", project.clone()),
                        ],
                    );
                }
            }
        })
        .build();
}

pub fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The one message a dispatcher with no project source dies on. Named so the
/// declaration-level test can assert it offers only sources that are still
/// read (`wamn-0h0g.22.30`).
const MISSING_PROJECTS_CONTEXT: &str =
    "no projects: pass --projects-file, or --database-url / WAMN_PG_URL";

/// Resolve the projects the dispatcher serves: the projects file, or the
/// single-project fallback flags.
fn resolve_projects(args: &DispatchArgs) -> anyhow::Result<Vec<ProjectSpec>> {
    if let Some(path) = &args.projects_file {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read projects file {}", path.display()))?;
        let map: std::collections::BTreeMap<String, ProjectSpecInput> =
            serde_json::from_str(&raw).context("parse projects file")?;
        if map.is_empty() {
            bail!("projects file has no projects");
        }
        return map
            .into_iter()
            .map(|(name, input)| project_spec(name, input))
            .collect();
    }
    // ONE source, read once, at trusted composition (`wamn-0h0g.22.30`). The
    // `or_else` chain this replaces made the process its own second and third
    // credential source; clap now resolves `--database-url` or its declared
    // `WAMN_PG_URL` env, and nothing else is consulted.
    let url = args
        .database_url
        .clone()
        .context(MISSING_PROJECTS_CONTEXT)?;
    Ok(vec![project_spec(
        "default".to_string(),
        ProjectSpecInput {
            url,
            tenant: args.tenant.clone(),
            execution_target_id: args.execution_target_id.clone(),
            schema: args.schema.clone(),
        },
    )?])
}

pub async fn run(args: DispatchArgs) -> anyhow::Result<()> {
    init_crypto();
    let specs = resolve_projects(&args)?;

    let nats_opts = NatsConnectionOptions {
        request_timeout: None,
        tls_ca: args.nats_tls_ca.clone(),
        tls_first: false,
        tls_cert: args.nats_tls_cert.clone(),
        tls_key: args.nats_tls_key.clone(),
    };
    let nats = match connect_nats(args.nats_url.clone(), nats_opts).await {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(url = %args.nats_url, error = %e,
                "dispatcher: no NATS — doorbell hints disabled, reconciliation sweeps still guarantee pickup");
            None
        }
    };

    // R13: validate the poll cadence once, at the boundary — an inverted band
    // (`--min-interval-ms` > `--max-interval-ms`) would otherwise panic in
    // `next_interval`'s `clamp` on the first idle sweep. Bail at startup instead.
    let cadence = wamn_scheduler::Cadence::new(args.min_interval_ms, args.max_interval_ms)
        .context("invalid poll cadence (--min-interval-ms / --max-interval-ms)")?;
    let cfg = DispatcherConfig {
        cadence,
        batch: args.batch.max(1),
    };
    let mut dispatcher = Dispatcher::connect(&specs, nats, cfg).await?;
    // [9.8] the run-queue-depth gauge reads the dispatcher's shared registry each
    // sweep refreshes; a no-op until main installs a meter provider (OTEL_*).
    register_queue_depth_gauge(&dispatcher.depth_registry());
    tracing::info!(
        projects = dispatcher.projects.len(),
        min_interval_ms = args.min_interval_ms,
        max_interval_ms = args.max_interval_ms,
        "shared queue dispatcher up (queued-work wake reconciliation)"
    );

    if args.stepped_stdio {
        return run_stepped_stdio(&mut dispatcher).await;
    }

    // SIGTERM must be handled explicitly: in-container the dispatcher is PID 1,
    // which gets NO default signal disposition — an unhandled SIGTERM is
    // IGNORED, so every pod termination would hang the full grace period and
    // die by SIGKILL. Abrupt death is safe because sweeps only read queue state
    // and publish repeatable hints, but a rollout should not take 30s per pod.
    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "dispatcher: no SIGTERM handler; Ctrl-C only");
                    let _ = tokio::signal::ctrl_c().await;
                    let _ = tx.send(true);
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
        let _ = tx.send(true);
    });
    dispatcher.run_loop(rx).await
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
enum StepCommand {
    Tick { project: usize, now_ms: i64 },
}

#[derive(Debug, serde::Serialize)]
struct StepResponse<'a> {
    project: usize,
    now_ms: i64,
    interval_ms: i64,
    #[serde(flatten)]
    outcome: StepOutcome<'a>,
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum StepOutcome<'a> {
    Ok {
        #[serde(flatten)]
        report: &'a TickReport,
    },
    Error {
        message: String,
    },
}

/// Drive deterministic sweeps through the executable boundary.
///
/// Tick input is `{"command":"tick","project":N,"now_ms":T}`. One response
/// is flushed before the next command is read, so a caller can mutate the
/// backing database between reconciliation sweeps.
async fn run_stepped_stdio(dispatcher: &mut Dispatcher) -> anyhow::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await.context("read stepped command")? {
        let command: StepCommand =
            serde_json::from_str(&line).context("parse stepped command JSON")?;
        let mut json = match command {
            StepCommand::Tick { project, now_ms } => {
                if project >= dispatcher.projects.len() {
                    bail!(
                        "stepped command project {project} is out of range ({} projects)",
                        dispatcher.projects.len()
                    );
                }
                let result = dispatcher.tick_project(project, now_ms).await;
                let interval_ms = dispatcher.projects[project].interval_ms;
                let response = match &result {
                    Ok(report) => StepResponse {
                        project,
                        now_ms,
                        interval_ms,
                        outcome: StepOutcome::Ok { report },
                    },
                    Err(error) => StepResponse {
                        project,
                        now_ms,
                        interval_ms,
                        outcome: StepOutcome::Error {
                            message: error.to_string(),
                        },
                    },
                };
                serde_json::to_vec(&response).context("encode stepped response")?
            }
        };
        json.push(b'\n');
        stdout
            .write_all(&json)
            .await
            .context("write stepped response")?;
        stdout.flush().await.context("flush stepped response")?;
    }
    Ok(())
}

/// TLS material for the doorbell connection. Local copy of the fork's
/// `wash_runtime::washlet::NatsConnectionOptions` (SR9): the doorbell is this
/// crate's only NATS use and the dispatcher artifact must not link the runtime.
struct NatsConnectionOptions {
    request_timeout: Option<Duration>,
    tls_ca: Option<PathBuf>,
    tls_first: bool,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
}

/// Local copy of the fork's `wash_runtime::washlet::connect_nats` (SR9).
async fn connect_nats(
    addr: impl async_nats::ToServerAddrs,
    options: NatsConnectionOptions,
) -> anyhow::Result<async_nats::Client> {
    let mut opts = async_nats::ConnectOptions::new();
    if let Some(timeout) = options.request_timeout {
        opts = opts.request_timeout(Some(timeout));
    }
    if let Some(ca_path) = options.tls_ca {
        opts = opts.add_root_certificates(ca_path)
    }
    if options.tls_first {
        opts = opts.tls_first();
    }
    if let (Some(cert_path), Some(key_path)) = (options.tls_cert, options.tls_key) {
        opts = opts.add_client_certificate(cert_path, key_path)
    }
    opts.connect(addr)
        .await
        .context("failed to connect to NATS")
}

/// Local copy of the fork's `wash_runtime::init_crypto` (SR9): standardize on
/// aws-lc-rs so the rustls provider is deterministic regardless of which
/// backends the dep graph enables.
fn init_crypto() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .is_err()
        {
            tracing::warn!(
                "a rustls CryptoProvider was already installed; \
                 the dispatcher standardizes on aws-lc-rs — check dependencies if this is unexpected"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::{
        DispatchArgs, DispatcherConfig, MISSING_PROJECTS_CONTEXT, ProjectSpecInput,
        RUN_QUEUE_DEPTH_SQL, doorbell_subject, next_reconcile, project_spec, reconcile_due,
        valid_tenant,
    };

    /// THE CREDENTIAL HAS EXACTLY ONE SOURCE (`wamn-0h0g.22.30`).
    ///
    /// Asserted on the clap DECLARATION rather than by mutating the process
    /// environment, which is global and racy across a test binary's threads.
    /// `resolve_projects` no longer consults the environment at all for the
    /// credential, so what clap declares here IS the whole source set: one
    /// argument, one env var. A reintroduced `DATABASE_URL` fallback would
    /// either appear as a second declared env (caught by the equality below) or
    /// as an `or_else` chain in `resolve_projects` — which this pins by leaving
    /// `WAMN_PG_URL` the only database env any dispatcher argument names.
    #[test]
    fn the_database_credential_names_one_env_and_no_second_source() {
        #[derive(clap::Parser)]
        struct TestCli {
            #[command(flatten)]
            args: DispatchArgs,
        }

        let command = TestCli::command();
        let database_envs: Vec<String> = command
            .get_arguments()
            .filter_map(|arg| arg.get_env())
            .map(|env| env.to_string_lossy().into_owned())
            .filter(|env| env.ends_with("PG_URL") || env.ends_with("DATABASE_URL"))
            .collect();
        assert_eq!(
            database_envs,
            ["WAMN_PG_URL"],
            "the dispatcher credential must have exactly one declared source"
        );

        let database_url = command
            .get_arguments()
            .find(|arg| arg.get_id() == "database_url")
            .expect("the dispatcher declares a database-url argument");
        assert_eq!(
            database_url.get_env().map(|env| env.to_string_lossy()),
            Some("WAMN_PG_URL".into()),
            "WAMN_PG_URL is the argument's declared transport, not a fallback \
             read behind it"
        );

        // The `.context(...)` a missing credential surfaces must not advertise
        // a source `resolve_projects` no longer reads. Asserted on the literal,
        // not by running `resolve_projects`, which would read the ambient
        // `WAMN_PG_URL` of whatever shell the test binary inherited.
        assert!(
            !MISSING_PROJECTS_CONTEXT.contains("DATABASE_URL"),
            "the missing-credential error must not offer a removed ambient \
             source: {MISSING_PROJECTS_CONTEXT}"
        );
        assert!(MISSING_PROJECTS_CONTEXT.contains("WAMN_PG_URL"));
    }

    #[test]
    fn depth_sql_counts_claimable_not_parked() {
        let sql = RUN_QUEUE_DEPTH_SQL;
        assert!(sql.contains("q.available_at <= now()"));
        assert!(sql.contains("q.lease_expires_at IS NULL OR q.lease_expires_at <= now()"));
        assert!(sql.contains("q.attempts < q.max_attempts OR q.lease_expires_at IS NULL"));
        assert!(sql.contains("FROM effect_attempts AS effect"));
        assert!(sql.contains("current_setting('app.tenant', true)"));
        assert!(!sql.contains("available_at > now()"));

        let claim = wamn_run_state::queue::select_production_claim_sql();
        assert!(claim.contains("available_at <= now()"));
        assert!(claim.contains("lease_expires_at IS NULL"));
        assert!(claim.contains("attempts < "));
    }

    #[test]
    fn wake_reconciliation_timing_is_dispatcher_local() {
        assert!(!reconcile_due(1_000, 900, 200));
        assert!(reconcile_due(1_100, 900, 200));
        assert_eq!(next_reconcile(900, 200), 1_100);
    }

    #[test]
    fn dispatcher_and_plugin_agree_on_a_65_char_tenant() {
        assert!(valid_tenant(&"a".repeat(64)));
        assert!(!valid_tenant(&"a".repeat(65)));
    }

    #[test]
    fn project_routing_uses_execution_target_not_tenant() {
        let input: ProjectSpecInput = serde_json::from_str(
            r#"{
                "url": "postgres://db/project-a",
                "tenant": "tenant-a",
                "execution_target_id": "worker-b"
            }"#,
        )
        .expect("projects-file entry carries a validated execution target");
        let spec = project_spec("project-a".into(), input).expect("valid project spec");

        assert_eq!(spec.tenant, "tenant-a");
        assert_eq!(
            doorbell_subject(&spec.execution_target_id),
            "wamn.doorbell.worker-b"
        );
    }

    #[test]
    fn omitted_project_target_uses_the_mvp_adapter() {
        let spec = project_spec(
            "project-a".into(),
            ProjectSpecInput {
                url: "postgres://db/project-a".into(),
                tenant: "tenant-a".into(),
                execution_target_id: None,
                schema: None,
            },
        )
        .expect("tenant-safe MVP placement");

        assert_eq!(spec.execution_target_id.as_str(), "tenant-a");
    }

    #[test]
    fn dispatcher_config_cadence_is_validated() {
        assert!(wamn_scheduler::Cadence::new(5_000, 1_000).is_err());
        let cadence = wamn_scheduler::Cadence::new(250, 30_000).expect("valid band");
        let cfg = DispatcherConfig { cadence, batch: 64 };
        assert_eq!((cfg.cadence.min(), cfg.cadence.max()), (250, 30_000));
    }
}

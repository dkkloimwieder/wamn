//! The wamn-dispatcher service (5.14; its own SR9 artifact) — the
//! always-on control-plane service that owns cron schedules
//! across ALL projects with adaptive intervals, and wakes parked runners via
//! doorbell (platform-plan Epic 5 "Triggers" + item 5.14). Row events are NOT
//! a dispatcher concern: the D19 v3 event plane (CDC reader → JetStream →
//! materializer) delivers them — the outbox poller was torn down at l5i9.19.
//!
//! Every decision is the pure crate's ([`wamn_run_queue`]): cron due-tick
//! evaluation over an injected `now` ([`due_tick`]), deterministic trigger run
//! ids, the adaptive per-project cadence ([`Cadence::next_interval`]). This module is
//! the DRIVER — tokio_postgres effects, the NATS-core doorbell, the real
//! clock — split exactly so a virtual-time driver (the `dispatchbench` gate)
//! can run the same [`Dispatcher::tick_project`] engine with stepped time and
//! get identical behaviour (the 11.1 fast-forwardable-cron discipline).
//!
//! One sweep of one project ("tick"):
//!   1. registry — scan active flows, parse each graph (wamn-flow), register
//!      cron triggers (webhook = gateway's, manual = editor's, row-event =
//!      the materializer's via its event registration). A flow that
//!      fails to parse or validate is skipped with a warning (a bad flow must
//!      not wedge the project);
//!   2. cron — recover each flow's last-fired tick (in-memory cache, else the
//!      run ids themselves via [`cron_last_run_sql`] — the runs table IS the
//!      cron state), fire the due tick via the write-ahead + enqueue
//!      co-transaction, doorbell the winner;
//!   3. wake — doorbell every currently-due unleased queue row (a parked run
//!      whose `available_at` arrived, or a run whose enqueue hint was lost) —
//!      one read-only scan doubling as the reconciliation backstop;
//!   4. cadence — tighten the project's interval on work, decay while idle.
//!
//! Exactly-once across restart AND concurrently racing replicas needs no leader:
//! run ids are deterministic per firing (`{flow}:cron:{tick}`),
//! so every duplicate path collapses on the write-ahead
//! `ON CONFLICT` — the dispatchbench `race` mode runs two live dispatchers over
//! one project and asserts it. A firing that LOSES the write-ahead skips its
//! enqueue too: the winner's queue row was created in the same past transaction
//! and either still exists or was legitimately dequeued on completion —
//! re-inserting it would resurrect a terminal run's queue row (a ghost
//! dispatch).
//!
//! The loop is hardened for always-on duty: a dropped project connection is
//! re-dialed on the next sweep (a Postgres restart must not permanently silence
//! a project's triggers), each sweep runs under a deadline (a black-holed
//! connection must not wedge every other project), and a failing sweep decays
//! that project's cadence and clears its stale cron wake-hint (the durable
//! anchor re-fires the tick exactly once on the next successful sweep).

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr as _;
use std::time::Duration;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};
use tracing::Instrument as _;
use wamn_flow::{Flow, Ordering, Trigger};
use wamn_run_queue::{
    Cadence, Firing, PartitionPolicy, active_flows_sql, cron_firing, cron_last_run_sql,
    cron_tick_of, due_tick, enqueue_sql, enqueue_with_policy_sql, next_fire, next_reconcile,
    parked_due_sql, reconcile_due, write_ahead_triggered_run_sql,
};

// R16b (wamn-2jkm.20): the dispatcher's pinned session `SET`s interpolate the
// tenant/schema, so these validators are the injection boundary HERE — and they
// are the SAME rule the wamn:postgres plugin enforces, held in one owner.
use wamn_registry::identifiers::{valid_schema, valid_tenant};

#[derive(Debug, Args)]
pub struct DispatchArgs {
    /// JSON projects map the dispatcher serves:
    /// {"<name>": {"url": "...", "tenant": "...", "schema": "wamn_run"}}
    /// (a mounted Secret/ConfigMap in production — the 2.2 projects-file shape).
    #[arg(long, env = "WAMN_DISPATCH_PROJECTS_FILE")]
    pub projects_file: Option<PathBuf>,

    /// Single-project fallback: app database URL. Overrides WAMN_PG_URL /
    /// DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Tenant claim for the single-project fallback.
    #[arg(long, default_value = "default")]
    pub tenant: String,

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
    #[arg(long, default_value_t = wamn_run_queue::DEFAULT_MIN_INTERVAL_MS)]
    pub min_interval_ms: i64,

    /// Widest per-project sweep interval (an idle project's reconciliation
    /// cadence).
    #[arg(long, default_value_t = wamn_run_queue::DEFAULT_MAX_INTERVAL_MS)]
    pub max_interval_ms: i64,

    /// Max wake hints processed per project per sweep (the
    /// fairness bound: one project's backlog cannot monopolize a sweep).
    #[arg(long, default_value_t = 64)]
    pub batch: usize,
}

/// One project the dispatcher serves: where its flow/queue tables live
/// (connection URL + search_path) and the tenant claim its session carries.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProjectSpec {
    #[serde(skip)]
    pub name: String,
    pub url: String,
    pub tenant: String,
    #[serde(default)]
    pub schema: Option<String>,
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

/// A project's live state: its pinned connection and the adaptive-cadence /
/// cron-anchor state the pure decisions fold over.
pub struct ProjectState {
    pub spec: ProjectSpec,
    client: Client,
    _conn: tokio::task::JoinHandle<()>,
    /// Adaptive sweep interval (tightens on work, decays while idle).
    pub interval_ms: i64,
    pub last_sweep_ms: i64,
    /// Earliest upcoming cron fire across this project's cron flows — the loop
    /// wakes for it even if the adaptive interval hasn't elapsed. Cleared when
    /// a sweep fails (a stale past hint would otherwise pin the loop hot
    /// against a down DB; the durable anchor recovers the tick on success).
    pub next_cron_fire: Option<i64>,
    /// Last fired tick per cron flow — an optimization only (skips the DB anchor
    /// recovery per sweep). Correctness never depends on it: a fresh replica
    /// recovers the anchor from the run ids and ON CONFLICT absorbs any re-fire.
    last_fired: HashMap<String, i64>,
    /// First-sight instant per cron flow with no fired tick yet: a cron flow
    /// starts firing from dispatcher-sight (no retroactive catch-up before the
    /// first fire).
    first_seen: HashMap<String, i64>,
    /// Quarantined cron schedules: parseable but unsatisfiable (evaluation
    /// errors). Warned once and skipped — re-evaluating one re-walks croner's
    /// whole search horizon per sweep for a flow that can never fire. Keyed by
    /// the schedule STRING, so a fixed flow (new schedule) evaluates fresh.
    bad_schedules: std::collections::HashSet<String>,
}

/// What one project sweep did — the gate's assertion surface and the cadence
/// input. Only firings that WON the write-ahead insert are counted as fired (a
/// racing replica's losing re-fire is a no-op, not work); `cron_lost` counts
/// the losses, which is how the race gate proves two replicas genuinely
/// contended.
#[derive(Debug, Default)]
pub struct TickReport {
    pub cron_fired: Vec<String>,
    pub cron_lost: usize,
    /// Due unleased queue rows hinted this sweep (parked wakes + lost-hint
    /// reconciliation). Duplicate hints across sweeps are by design: harmless
    /// (the claim is the arbiter), and a persistently-unclaimed backlog SHOULD
    /// keep the cadence tight — waking a scale-to-zero runner is the point.
    pub woken: Vec<String>,
}

impl TickReport {
    pub fn found_work(&self) -> bool {
        !self.cron_fired.is_empty() || !self.woken.is_empty()
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
                wamn_run_queue::DEFAULT_MIN_INTERVAL_MS,
                wamn_run_queue::DEFAULT_MAX_INTERVAL_MS,
            )
            .expect("default cadence bounds are valid"),
            batch: 64,
        }
    }
}

/// The trigger registry one sweep works from: the cron flows (webhook is the
/// gateway's, manual the editor's, row-event the materializer's via its event
/// registration).
#[derive(Default)]
struct Registry {
    crons: Vec<(String, i32, String)>,
    /// Flow-level record-stream ordering (5.11, wamn-fqg.20) per registered
    /// flow_id — the dispatcher evaluates it at fire() to stamp
    /// `run_queue.partition_key` ([`partition_key_for_firing`]). Every cron
    /// flow lands here (unordered ones as [`Ordering::Unordered`], so
    /// their key stays NULL); a flow absent from the map falls back to
    /// unordered too.
    ordering: HashMap<String, Ordering>,
    /// Flow-level head-unavailability policy (D20, wamn-kq0z) per registered
    /// flow_id — materialized onto the queue row at fire()
    /// ([`partition_policy_for_firing`]) so the claim SQL never joins back to the
    /// flow. Stamped only on a KEYED (strict/partitioned) row; an unordered row
    /// keeps a NULL key and the column-default policy. Every cron
    /// flow lands here alongside its ordering; a flow absent from the map falls
    /// back to [`PartitionPolicy::Blocking`] (the D20 default).
    policy: HashMap<String, PartitionPolicy>,
}

/// Parse the active-flows scan. A flow that fails to parse or validate is
/// skipped with a warning (a bad flow must not wedge the project).
fn parse_registry(project: &str, rows: &[tokio_postgres::Row]) -> Registry {
    let mut reg = Registry::default();
    for row in rows {
        let flow_id: String = row.get("flow_id");
        let version: i32 = row.get("version");
        let graph: String = row.get("graph_json");
        let parsed = Flow::from_json(&graph)
            .map_err(|e| e.to_string())
            .and_then(|f| {
                f.validate()
                    .map_err(|issues| format!("{issues:?}"))
                    .map(|_| f)
            });
        let flow = match parsed {
            Ok(f) => f,
            Err(why) => {
                tracing::warn!(project = %project, %flow_id, why,
                    "dispatcher: invalid active flow skipped");
                continue;
            }
        };
        // The run ids embed the registry id ({flow}:cron:{tick})
        // taken from the flows-table COLUMN, while the
        // slug charset rule just validated only the graph's embedded flow-id.
        // Requiring the two to be EQUAL extends the charset guarantee to the
        // id that is actually minted; a mismatched row is skipped
        // exactly like any other invalid flow.
        if flow.flow_id != flow_id {
            tracing::warn!(project = %project, %flow_id, graph_flow_id = %flow.flow_id,
                "dispatcher: flows.flow_id != graph flow-id — flow skipped");
            continue;
        }
        match &flow.trigger {
            Trigger::Cron { schedule } => {
                // Record the ordering (5.11) + head-unavailability policy (D20)
                // declarations so fire() can stamp the partition key AND its
                // materialized policy; only flows this service fires (cron)
                // need them.
                reg.ordering.insert(flow_id.clone(), flow.ordering.clone());
                reg.policy
                    .insert(flow_id.clone(), rq_policy(flow.partition_policy));
                reg.crons.push((flow_id, version, schedule.clone()));
            }
            // Webhook is routed by the API gateway; manual by the editor;
            // row-event by the materializer (its event registration is the
            // trigger record — l5i9.16/.17).
            Trigger::RowEvent { .. } | Trigger::Webhook { .. } | Trigger::Manual => {}
        }
    }
    reg
}

/// The dispatcher: per-project state + the optional doorbell client + the
/// cadence config. One instance is one replica; running several is safe (the
/// deterministic-id `ON CONFLICT` story — gated by dispatchbench `race`).
pub struct Dispatcher {
    pub projects: Vec<ProjectState>,
    nats: Option<async_nats::Client>,
    cfg: DispatcherConfig,
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
                next_cron_fire: None,
                last_fired: HashMap::new(),
                first_seen: HashMap::new(),
                bad_schedules: std::collections::HashSet::new(),
            });
        }
        Ok(Self {
            projects,
            nats,
            cfg,
        })
    }

    /// One sweep of one project at `now_ms` — the whole engine, pure decisions
    /// folded over driver effects. `now_ms` is INJECTED (the run loop passes the
    /// real clock, the gate passes stepped time); the SQL's own `now()` instants
    /// are server-side timestamps and orthogonal to the trigger decisions.
    pub async fn tick_project(&mut self, idx: usize, now_ms: i64) -> anyhow::Result<TickReport> {
        let (batch, cadence) = (self.cfg.batch, self.cfg.cadence);
        let nats = self.nats.as_ref();

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

        // 1. Registry: the trigger lives inside each active flow's graph_json.
        let reg = parse_registry(
            &p.spec.name,
            &p.client.query(&active_flows_sql(), &[]).await?,
        );

        // 2. Cron: recover the anchor, fire the due tick.
        let last_run_sql = cron_last_run_sql();
        let mut doorbells: Vec<String> = Vec::new();
        for (flow_id, version, schedule) in &reg.crons {
            // A schedule that ever errored (parseable but unsatisfiable — a
            // Feb 30) is quarantined: evaluating it re-walks croner's whole
            // search horizon EVERY sweep for a flow that can never fire. It was
            // warned once; a fixed flow ships a different schedule string.
            if p.bad_schedules.contains(schedule) {
                continue;
            }
            let anchor = match p.last_fired.get(flow_id) {
                Some(&t) => t,
                None => {
                    // Flow-exclusive recovery: the flow's OWN cron runs, never a
                    // lexical id range (collation/user-text hazards).
                    let max: Option<String> =
                        p.client.query_one(&last_run_sql, &[&flow_id]).await?.get(0);
                    match max.as_deref().and_then(|id| cron_tick_of(flow_id, id)) {
                        Some(t) => {
                            p.last_fired.insert(flow_id.clone(), t);
                            t
                        }
                        // Never fired: anchor at first sight — a cron flow
                        // starts firing from when the dispatcher first sees it.
                        None => *p.first_seen.entry(flow_id.clone()).or_insert(now_ms),
                    }
                }
            };
            let due = match due_tick(schedule, anchor, now_ms) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(project = %p.spec.name, %flow_id, error = %e,
                        "dispatcher: unsatisfiable/bad cron schedule quarantined");
                    p.bad_schedules.insert(schedule.clone());
                    continue;
                }
            };
            if let Some(tick) = due {
                let firing = cron_firing(flow_id, *version, schedule, tick);
                // 5.11 ordering + D20 policy: stamp the partition key from the
                // flow's declaration and, for a keyed row, its declared
                // head-unavailability policy (unordered cron flows keep a NULL
                // key + the column-default policy = today's behavior).
                let key = partition_key_for_firing(&reg, &firing);
                let policy = partition_policy_for_firing(&reg, &firing);
                let span = trigger_span(&firing, &p.spec.tenant);
                let won = fire(&mut p.client, &firing, key.as_deref(), policy)
                    .instrument(span)
                    .await?;
                p.last_fired.insert(flow_id.clone(), tick);
                if won {
                    doorbells.push(firing.run_id.clone());
                    report.cron_fired.push(firing.run_id);
                } else {
                    report.cron_lost += 1;
                }
            }
        }
        // The loop's cron-aware sleep: the earliest upcoming fire (quarantined
        // schedules excluded — no full-horizon walk per sweep for them).
        p.next_cron_fire = reg
            .crons
            .iter()
            .filter(|(_, _, s)| !p.bad_schedules.contains(s))
            .filter_map(|(_, _, s)| next_fire(s, now_ms).ok())
            .min();

        // 3. Wake / reconciliation: hint every currently-due unleased row.
        for row in p.client.query(&parked_due_sql(batch), &[]).await? {
            let run_id: String = row.get("run_id");
            doorbells.push(run_id.clone());
            report.woken.push(run_id);
        }

        // Doorbells strictly after the effects committed (a hint for
        // uncommitted work would wake a runner into an empty claim).
        if let Some(nats) = nats
            && !doorbells.is_empty()
        {
            let subject = format!("wamn.doorbell.{}", p.spec.tenant);
            for run_id in doorbells {
                nats.publish(subject.clone(), run_id.into_bytes().into())
                    .await?;
            }
            nats.flush().await?;
        }

        // 4. Adaptive cadence.
        p.interval_ms = cadence.next_interval(p.interval_ms, report.found_work());
        p.last_sweep_ms = now_ms;
        Ok(report)
    }

    /// The always-on loop: tick each project when its adaptive interval elapses
    /// OR its next cron fire arrives, then sleep until the earliest next event —
    /// zero continuous polling, but a cron tick is never late by a decayed
    /// interval. Each sweep runs under a deadline (a black-holed connection
    /// must not wedge the other projects), and a failing project decays and
    /// retries — it never wedges the loop.
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
                let due = {
                    let p = &self.projects[i];
                    reconcile_due(now, p.last_sweep_ms, p.interval_ms)
                        || p.next_cron_fire.is_some_and(|t| t <= now)
                };
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
                            "dispatcher: sweep timed out (abandoned; the in-flight transaction rolls back)");
                        true
                    }
                };
                if failed {
                    let cadence = self.cfg.cadence;
                    let p = &mut self.projects[i];
                    p.last_sweep_ms = now;
                    p.interval_ms = cadence.next_interval(p.interval_ms, false);
                    // A stale past wake-hint would pin the due-check (and the
                    // sleep) hot against a down DB; the durable anchor re-fires
                    // the tick exactly once on the next successful sweep.
                    p.next_cron_fire = None;
                }
            }

            let now = epoch_ms();
            let next = self
                .projects
                .iter()
                .map(|p| {
                    let sweep = next_reconcile(p.last_sweep_ms, p.interval_ms);
                    p.next_cron_fire.map_or(sweep, |c| sweep.min(c))
                })
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

/// Fire one trigger: the write-ahead run row (with the trigger payload) and —
/// only if this caller WON the insert — the queue row, in one transaction (one
/// durability domain, D3). A `false` means another replica (or an earlier
/// redelivery) already fired this deterministic id: the whole firing is a
/// no-op, and in particular the enqueue is SKIPPED — the winner's queue row is
/// either still pending or was legitimately dequeued on completion, and
/// re-inserting it would resurrect a terminal run's queue row (ghost dispatch).
/// [9.1] A `wamn.trigger` span rooting a dispatcher-fired run's trace, enriched
/// with the run context the host mints right here — flow, run_id, flow_version,
/// tenant, and the trigger source (`cron`). This is the
/// host-known-path home for `flow`/`run_id` enrichment (a webhook's trigger is
/// instead wash-runtime's inbound HTTP span; guest-minted webhook `run_id` and
/// per-node `node_id` await the 9.2 guest→host run-context contract). The
/// runner's `wamn.postgres` spans thread under this once a fired run executes;
/// cross-replica threading over the queue is 9.2 (traceparent propagation).
pub fn trigger_span(f: &Firing, tenant: &str) -> tracing::Span {
    tracing::info_span!(
        "wamn.trigger",
        wamn.flow = %f.flow_id,
        wamn.run_id = %f.run_id,
        wamn.flow_version = f.flow_version,
        wamn.trigger_source = %f.trigger_source,
        wamn.tenant = %tenant,
    )
}

/// The `run_queue.partition_key` a firing carries (wamn-fqg.20): the firing
/// flow's ordering declaration (5.11) evaluated over the run input. `None` — the
/// unordered global claim — for an unordered flow, a flow absent from the
/// registry, or (defensively) an unparseable input. Strict yields the flow id;
/// partitioned yields the JMESPath result, folded to a key by
/// [`Ordering::partition_key_for`] (a missing/non-scalar key degrades to the
/// flow-wide stream, never NULL).
fn partition_key_for_firing(reg: &Registry, f: &Firing) -> Option<String> {
    let ordering = reg.ordering.get(&f.flow_id)?;
    // The input the run is replayed from (5.7) is the same JSON the key is
    // evaluated over; a malformed input degrades to `null` (fallback to the
    // flow-wide stream for a partitioned flow, None for unordered/strict-null).
    let input = serde_json::from_str(&f.input_json).unwrap_or(serde_json::Value::Null);
    ordering.partition_key_for(&f.flow_id, &input)
}

/// The `run_queue.partition_policy` literal a firing carries (D20, wamn-kq0z):
/// the firing flow's declared head-unavailability policy, materialized onto the
/// queue row so `claim_partition_head_sql` branches on the ROW and never joins
/// back to the flow. Only consulted for a KEYED (strict/partitioned) row — an
/// unordered/absent flow's NULL-key row takes the column default. A flow absent
/// from the registry falls back to [`PartitionPolicy::Blocking`] (the D20
/// decision: choosing partitioned dispatch *is* opting into ordering).
fn partition_policy_for_firing(reg: &Registry, f: &Firing) -> &'static str {
    reg.policy
        .get(&f.flow_id)
        .copied()
        .unwrap_or_default()
        .as_sql()
}

/// Bridge the `wamn-flow` policy contract enum to the `wamn-run-queue` storage
/// enum (whose [`PartitionPolicy::as_sql`] owns the single storage literal). The
/// two are structurally mirrored; this is the one crossing point.
fn rq_policy(p: wamn_flow::PartitionPolicy) -> PartitionPolicy {
    match p {
        wamn_flow::PartitionPolicy::Blocking => PartitionPolicy::Blocking,
        wamn_flow::PartitionPolicy::Leapfrog => PartitionPolicy::Leapfrog,
    }
}

/// Enqueue one won firing's queue row, materializing the D20 policy COHERENTLY
/// with its key (wamn-kq0z): a keyed row (strict/partitioned) carries the flow's
/// declared `partition_policy` via [`enqueue_with_policy_sql`], while an
/// unordered row keeps a NULL key and the column-default policy via
/// [`enqueue_sql`] (today's behavior, byte-identical). `$3` priority and `$4`
/// delay are 0 — a trigger firing is immediately claimable.
async fn enqueue_firing(
    tx: &tokio_postgres::Transaction<'_>,
    run_id: &str,
    partition_key: Option<&str>,
    policy: &str,
) -> Result<(), tokio_postgres::Error> {
    if partition_key.is_some() {
        tx.execute(
            &enqueue_with_policy_sql(),
            &[&run_id, &partition_key, &0i32, &0i64, &policy],
        )
        .await?;
    } else {
        tx.execute(&enqueue_sql(), &[&run_id, &partition_key, &0i32, &0i64])
            .await?;
    }
    Ok(())
}

async fn fire(
    client: &mut Client,
    f: &Firing,
    partition_key: Option<&str>,
    policy: &str,
) -> anyhow::Result<bool> {
    let tx = client.transaction().await?;
    let inserted = tx
        .execute(
            &write_ahead_triggered_run_sql(),
            &[
                &f.run_id,
                &f.flow_id,
                &f.flow_version,
                &f.trigger_source,
                &f.input_json,
            ],
        )
        .await?;
    if inserted == 1 {
        enqueue_firing(&tx, &f.run_id, partition_key, policy).await?;
    }
    tx.commit().await?;
    Ok(inserted == 1)
}

pub fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Resolve the projects the dispatcher serves: the projects file, or the
/// single-project fallback flags.
fn resolve_projects(args: &DispatchArgs) -> anyhow::Result<Vec<ProjectSpec>> {
    if let Some(path) = &args.projects_file {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read projects file {}", path.display()))?;
        let map: std::collections::BTreeMap<String, ProjectSpec> =
            serde_json::from_str(&raw).context("parse projects file")?;
        if map.is_empty() {
            bail!("projects file has no projects");
        }
        return Ok(map
            .into_iter()
            .map(|(name, mut spec)| {
                spec.name = name;
                spec
            })
            .collect());
    }
    let url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no projects: pass --projects-file or --database-url / WAMN_PG_URL")?;
    Ok(vec![ProjectSpec {
        name: "default".to_string(),
        url,
        tenant: args.tenant.clone(),
        schema: args.schema.clone(),
    }])
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
    let cadence = wamn_run_queue::Cadence::new(args.min_interval_ms, args.max_interval_ms)
        .context("invalid poll cadence (--min-interval-ms / --max-interval-ms)")?;
    let cfg = DispatcherConfig {
        cadence,
        batch: args.batch.max(1),
    };
    let mut dispatcher = Dispatcher::connect(&specs, nats, cfg).await?;
    tracing::info!(
        projects = dispatcher.projects.len(),
        min_interval_ms = args.min_interval_ms,
        max_interval_ms = args.max_interval_ms,
        "shared trigger dispatcher up (cron + parked-wake)"
    );

    // SIGTERM must be handled explicitly: in-container the dispatcher is PID 1,
    // which gets NO default signal disposition — an unhandled SIGTERM is
    // IGNORED, so every pod termination would hang the full grace period and
    // die by SIGKILL. (Abrupt death is still safe — a sweep is one transaction
    // — but a rollout should not take 30s per pod.)
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
    use super::{
        DispatcherConfig, Ordering, PartitionPolicy, Registry, partition_key_for_firing,
        partition_policy_for_firing, valid_tenant,
    };
    use wamn_run_queue::Firing;

    fn firing(flow_id: &str, input_json: &str) -> Firing {
        Firing {
            run_id: format!("{flow_id}:cron:0000000000001"),
            flow_id: flow_id.to_string(),
            flow_version: 1,
            input_json: input_json.to_string(),
            trigger_source: "cron".to_string(),
        }
    }

    // wamn-fqg.20: the dispatcher stamps run_queue.partition_key from the flow's
    // ordering declaration (5.11), evaluated over the firing's run input.
    #[test]
    fn partition_key_stamped_from_the_flow_ordering() {
        let mut reg = Registry::default();
        reg.ordering.insert("plain".into(), Ordering::Unordered);
        reg.ordering.insert("whole".into(), Ordering::Strict);
        reg.ordering.insert(
            "keyed".into(),
            Ordering::Partitioned {
                partition_key: "payload.customer".into(),
            },
        );

        let input = r#"{"table":"orders","payload":{"customer":"acme"}}"#;
        // Unordered → NULL key (today's global claim, unchanged).
        assert_eq!(
            partition_key_for_firing(&reg, &firing("plain", input)),
            None
        );
        // Strict → the constant whole-flow key (the flow id).
        assert_eq!(
            partition_key_for_firing(&reg, &firing("whole", input)),
            Some("whole".to_string())
        );
        // Partitioned → the evaluated key.
        assert_eq!(
            partition_key_for_firing(&reg, &firing("keyed", input)),
            Some("acme".to_string())
        );
        // Partitioned with a missing key → the flow-wide stream, never NULL: a
        // partitioned flow must not escape to the unordered claim (D20).
        assert_eq!(
            partition_key_for_firing(&reg, &firing("keyed", r#"{"payload":{}}"#)),
            Some("keyed".to_string())
        );
        // A flow with no recorded ordering falls back to unordered.
        assert_eq!(
            partition_key_for_firing(&reg, &firing("unknown", input)),
            None
        );
    }

    // wamn-kq0z: the dispatcher also materializes the flow's declared D20
    // head-unavailability policy onto the queue row at fire(). The policy is
    // stamped only when the row is keyed (the caller branches on the key), but
    // the literal is resolved per flow here: declared leapfrog → 'leapfrog',
    // declared/absent blocking → 'blocking' (the D20 default).
    #[test]
    fn partition_policy_stamped_from_the_flow_declaration() {
        let mut reg = Registry::default();
        reg.policy.insert("blk".into(), PartitionPolicy::Blocking);
        reg.policy.insert("leap".into(), PartitionPolicy::Leapfrog);

        let input = r#"{"payload":{"customer":"acme"}}"#;
        assert_eq!(
            partition_policy_for_firing(&reg, &firing("blk", input)),
            "blocking"
        );
        assert_eq!(
            partition_policy_for_firing(&reg, &firing("leap", input)),
            "leapfrog"
        );
        // A flow absent from the map falls back to the D20 default (blocking).
        assert_eq!(
            partition_policy_for_firing(&reg, &firing("unknown", input)),
            "blocking"
        );
    }

    // R16b (wamn-2jkm.20) — the dispatcher and the wamn:postgres plugin now share
    // ONE `valid_tenant`. Exercised through the symbol the dispatcher's spec check
    // actually calls: a 64-char tenant is legal, a 65-char one is rejected. This
    // FAILS against the pre-R16b dispatch-local rule (which had no length bound,
    // so it accepted 65 chars while the plugin rejected them) — the exact
    // divergence this bead closes.
    #[test]
    fn dispatcher_and_plugin_agree_on_a_65_char_tenant() {
        assert!(valid_tenant(&"a".repeat(64)));
        assert!(!valid_tenant(&"a".repeat(65)));
    }

    // R13-hardening (wamn-2jkm.58): DispatcherConfig stores a validated Cadence,
    // so an inverted `min > max` band is unrepresentable — it is rejected at
    // Cadence::new before it can reach the config. A valid band round-trips in.
    #[test]
    fn dispatcher_config_cadence_is_validated() {
        assert!(wamn_run_queue::Cadence::new(5_000, 1_000).is_err());
        let cadence = wamn_run_queue::Cadence::new(250, 30_000).expect("valid band");
        let cfg = DispatcherConfig { cadence, batch: 64 };
        assert_eq!((cfg.cadence.min(), cfg.cadence.max()), (250, 30_000));
    }
}

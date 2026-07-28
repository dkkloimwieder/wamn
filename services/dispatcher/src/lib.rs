//! The wamn-dispatcher service (5.14; its own SR9 artifact) — the
//! always-on control-plane service that owns cron schedules
//! across ALL projects with adaptive intervals, and wakes parked runners via
//! doorbell (platform-plan Epic 5 "Triggers" + item 5.14). Row events are NOT
//! a dispatcher concern: the D19 v3 event plane (CDC reader → JetStream →
//! materializer) delivers them — the outbox poller was torn down at l5i9.19.
//!
//! Every decision is the pure crate's ([`wamn_run_state`]): cron due-tick
//! evaluation over an injected `now` ([`due_tick`]), deterministic trigger run
//! ids, the adaptive per-project cadence ([`Cadence::next_interval`]). This module is
//! the DRIVER — tokio_postgres effects, the NATS-core doorbell, the real
//! clock — split exactly so a virtual-time driver (the `dispatchbench` gate)
//! can run the same [`Dispatcher::tick_project`] engine with stepped time and
//! get identical behaviour (the 11.1 fast-forwardable-cron discipline).
//!
//! One sweep of one project ("tick"):
//!   1. registry — scan authoritative enabled cron attachments, join their
//!      immutable schedule sources and pinned release artifacts, then validate
//!      each graph. Disabled or tombstoned definitions are absent by
//!      construction;
//!   2. cron — recover each flow's last-fired tick (in-memory cache, else the
//!      durable `cron_anchor` row via [`cron_anchor_sql`], else the run ids
//!      themselves via [`cron_last_run_sql`] as a bootstrap fallback — the
//!      anchor is decoupled from prunable run history so 9.6 retention cannot
//!      make an already-fired tick re-fire, wamn-fqg.6), fire the due tick via
//!      the centralized callable-flow admission + anchor co-transaction,
//!      doorbell the winner;
//!   3. cancellation — reconcile a bounded batch of durable requests and
//!      elapsed run/response deadlines, deferring live attempts;
//!   4. wake — doorbell every currently-due unleased queue row (a parked run
//!      whose `available_at` arrived, or a run whose enqueue hint was lost) —
//!      one read-only scan doubling as the reconciliation backstop;
//!   5. cadence — tighten the project's interval on work, decay while idle.
//!
//! Exactly-once across restart AND concurrently racing replicas needs no leader:
//! run ids are deterministic per firing (`{flow}:cron:{generation}:{tick}`),
//! so every duplicate path collapses in centralized admission — the
//! dispatchbench `race` mode runs two live dispatchers over
//! one project and asserts it. A duplicate admission never recreates a queue
//! row: the winner's row either still exists or was legitimately dequeued on
//! completion, and resurrection would be a ghost dispatch.
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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio_postgres::{Client, NoTls};
use tracing::Instrument as _;
use wamn_flow::{EntryKind, Flow, Ordering};
use wamn_run_state::admission::{AdmissionProducer, AdmissionResult, admission_sql};
use wamn_run_state::cancellation::cancellation_sweep_sql;
use wamn_run_state::queue::{
    PartitionPolicy, cron_anchor_sql, cron_last_run_sql, parked_due_sql, upsert_cron_anchor_sql,
};
use wamn_scheduler::{
    Cadence, Firing, canonical_tick, cron_firing, cron_tick_of, due_tick, next_fire,
    next_reconcile, reconcile_due,
};

// R16b (wamn-2jkm.20): the dispatcher's pinned session `SET`s interpolate the
// tenant/schema, so these validators are the injection boundary HERE — and they
// are the SAME rule the wamn:postgres plugin enforces, held in one owner.
use wamn_control_registry::identifiers::{valid_schema, valid_tenant};

/// [9.8] The claimable run-queue depth for the pinned session's tenant. Mirrors
/// EXACTLY the claim predicate of `wamn_run_state::queue::claim_batch_sql`
/// (`crates/execution/run-state/src/queue/sql.rs`: `available_at` reached, lease NULL-or-
/// expired, budget-remaining), so the gauge counts precisely the rows a runner
/// could claim right now. Inverting a clause (e.g. `available_at > now()`) makes
/// a seeded queue read 0 — metricbench phase 2's mutant.
pub const RUN_QUEUE_DEPTH_SQL: &str = "SELECT count(*)::bigint FROM run_queue \
     WHERE tenant_id = current_setting('app.tenant', true) \
       AND available_at <= now() \
       AND (lease_expires_at IS NULL OR lease_expires_at <= now()) \
       AND (attempts < max_attempts OR lease_expires_at IS NULL)";

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
    /// recovers the anchor from the durable `cron_anchor` row (else the run ids,
    /// as a bootstrap fallback) and ON CONFLICT absorbs any re-fire.
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
#[derive(Debug, Default, serde::Serialize)]
pub struct TickReport {
    pub cron_fired: Vec<String>,
    pub cron_lost: usize,
    /// Due unleased queue rows hinted this sweep (parked wakes + lost-hint
    /// reconciliation). Duplicate hints across sweeps are by design: harmless
    /// (the claim is the arbiter), and a persistently-unclaimed backlog SHOULD
    /// keep the cadence tight — waking a scale-to-zero runner is the point.
    pub woken: Vec<String>,
    /// Runs terminalized by the bounded cancellation/deadline reconciliation.
    pub cancelled: Vec<String>,
}

impl TickReport {
    pub fn found_work(&self) -> bool {
        !self.cron_fired.is_empty() || !self.woken.is_empty() || !self.cancelled.is_empty()
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

/// Authoritative enabled cron definitions for the tenant's applied releases.
///
/// This is intentionally a read-only producer query. Run and queue writes must
/// pass through [`admission_sql`].
pub const CRON_ATTACHMENTS_SQL: &str = "\
SELECT a.catalog_id, a.environment, a.catalog_version, a.attachment_id, \
       a.definition_hash, a.flow_id, rf.flow_version, fa.graph_json::text, \
       source.definition_json->>'schedule' AS schedule, \
       (a.definition_json->>'run-deadline-ms')::bigint AS run_deadline_ms \
  FROM catalog.cron_attachments AS a \
  JOIN catalog.release_sources AS source \
    ON source.tenant_id = a.tenant_id AND source.catalog_id = a.catalog_id \
   AND source.catalog_version = a.catalog_version AND source.source_id = a.source_id \
   AND source.source_kind = 'schedule' \
  JOIN catalog.release_flows AS rf \
    ON rf.tenant_id = a.tenant_id AND rf.catalog_id = a.catalog_id \
   AND rf.catalog_version = a.catalog_version AND rf.flow_id = a.flow_id \
  JOIN catalog.flow_artifacts AS fa \
    ON fa.tenant_id = rf.tenant_id AND fa.flow_id = rf.flow_id \
   AND fa.flow_version = rf.flow_version \
 ORDER BY a.catalog_id, a.environment, a.attachment_id";

const PHASE_2A_CRON_GENERATION: i64 = 0;

#[derive(Clone, Debug)]
struct CronAttachment {
    catalog_id: String,
    environment: String,
    catalog_version: i32,
    attachment_id: String,
    definition_hash: String,
    flow_id: String,
    flow_version: i32,
    schedule: String,
    run_deadline_ms: i64,
}

#[derive(Default)]
struct Registry {
    crons: Vec<CronAttachment>,
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

/// Parse the authoritative attachment scan. A pinned artifact that fails to
/// parse or validate is skipped with a warning (one bad definition must not
/// wedge the project).
fn parse_registry(project: &str, rows: &[tokio_postgres::Row]) -> Registry {
    let mut reg = Registry::default();
    for row in rows {
        let flow_id: String = row.get("flow_id");
        let flow_version: i32 = row.get("flow_version");
        let graph: String = row.get("graph_json");
        let parsed = Flow::from_json(&graph)
            .map_err(|e| e.to_string())
            .and_then(|f| {
                f.validate(&Default::default())
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
        // The run ids embed the registry id
        // ({flow}:cron:{generation}:{tick}) taken from the release column, while the
        // slug charset rule just validated only the graph's embedded flow-id.
        // Requiring the two to be EQUAL extends the charset guarantee to the
        // id that is actually minted; a mismatched row is skipped
        // exactly like any other invalid flow.
        if flow.flow_id != flow_id {
            tracing::warn!(project = %project, %flow_id, graph_flow_id = %flow.flow_id,
                "dispatcher: release flow-id != graph flow-id — flow skipped");
            continue;
        }
        if i32::try_from(flow.version).ok() != Some(flow_version) {
            tracing::warn!(project = %project, %flow_id, graph_version = flow.version,
                release_version = flow_version,
                "dispatcher: release flow version != graph version — flow skipped");
            continue;
        }
        if flow
            .entry_node()
            .is_some_and(|entry| entry.entry_kind() == Some(EntryKind::Cron))
        {
            let schedule: Option<String> = row.get("schedule");
            let run_deadline_ms: Option<i64> = row.get("run_deadline_ms");
            let (Some(schedule), Some(run_deadline_ms)) = (schedule, run_deadline_ms) else {
                tracing::warn!(project = %project, %flow_id,
                    "dispatcher: cron attachment has incomplete schedule/deadline — skipped");
                continue;
            };
            if run_deadline_ms <= 0 || schedule.is_empty() {
                tracing::warn!(project = %project, %flow_id,
                    "dispatcher: cron attachment has invalid schedule/deadline — skipped");
                continue;
            }
            reg.ordering.insert(flow_id.clone(), flow.ordering);
            reg.policy.insert(
                flow_id.clone(),
                match flow.partition_policy {
                    wamn_flow::PartitionPolicy::Blocking => PartitionPolicy::Blocking,
                    wamn_flow::PartitionPolicy::Leapfrog => PartitionPolicy::Leapfrog,
                },
            );
            reg.crons.push(CronAttachment {
                catalog_id: row.get("catalog_id"),
                environment: row.get("environment"),
                catalog_version: row.get("catalog_version"),
                attachment_id: row.get("attachment_id"),
                definition_hash: row.get("definition_hash"),
                flow_id,
                flow_version,
                schedule,
                run_deadline_ms,
            });
        } else {
            tracing::warn!(project = %project, %flow_id,
                "dispatcher: cron attachment targets a non-cron entry — skipped");
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
            depth: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// [9.8] The shared depth registry to register the `wamn.run_queue.depth`
    /// gauge over ([`register_queue_depth_gauge`]). Cloned so the gauge callback
    /// and the sweep loop share it.
    pub fn depth_registry(&self) -> DepthRegistry {
        self.depth.clone()
    }

    /// One sweep of one project at `now_ms` — the whole engine, pure decisions
    /// folded over driver effects. `now_ms` is INJECTED (the run loop passes the
    /// real clock, the gate passes stepped time); the SQL's own `now()` instants
    /// are server-side timestamps and orthogonal to the trigger decisions.
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

        // 1. Registry: authoritative active cron attachments select the pinned
        // release artifact. Disabled and tombstoned definitions are absent from
        // this view and therefore cannot produce a run.
        let reg = parse_registry(
            &p.spec.name,
            &p.client.query(CRON_ATTACHMENTS_SQL, &[]).await?,
        );

        // 2. Cron: recover the anchor, fire the due tick.
        let anchor_sql = cron_anchor_sql();
        let last_run_sql = cron_last_run_sql();
        let mut doorbells: Vec<String> = Vec::new();
        for cron in &reg.crons {
            let flow_id = &cron.flow_id;
            let schedule = &cron.schedule;
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
                    // Anchor recovery order (wamn-fqg.6): the DURABLE
                    // `cron_anchor` row FIRST. It is co-transacted with the fire
                    // and is NOT subject to 9.6 retention pruning, so it survives
                    // a pruned run history that would otherwise vanish and let an
                    // already-fired tick re-fire.
                    let anchored: Option<i64> = p
                        .client
                        .query_opt(&anchor_sql, &[&flow_id])
                        .await?
                        .map(|row| row.get(0));
                    match anchored {
                        Some(t) => {
                            p.last_fired.insert(flow_id.clone(), t);
                            t
                        }
                        None => {
                            // BOOTSTRAP fallback for a PRE-ANCHOR flow (its last
                            // fire predates the anchor table): flow-exclusive
                            // recovery from the flow's OWN cron runs, never a
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
                let firing = cron_firing(
                    flow_id,
                    cron.flow_version,
                    PHASE_2A_CRON_GENERATION,
                    tick,
                    now_ms,
                )?;
                // 5.11 ordering + D20 policy: stamp the partition key from the
                // flow's declaration and, for a keyed row, its declared
                // head-unavailability policy (unordered cron flows keep a NULL
                // key + the column-default policy = today's behavior).
                let key = partition_key_for_firing(&reg, &firing);
                let policy = key
                    .as_ref()
                    .map_or(PartitionPolicy::Blocking.as_sql(), |_| {
                        partition_policy_for_firing(&reg, &firing)
                    });
                let span = trigger_span(&firing, &p.spec.tenant);
                let result = fire(
                    &mut p.client,
                    cron,
                    &firing,
                    tick,
                    now_ms,
                    key.as_deref(),
                    policy,
                )
                .instrument(span)
                .await?;
                match result {
                    AdmissionResult::Admitted { .. } => {
                        p.last_fired.insert(flow_id.clone(), tick);
                        doorbells.push(firing.run_id.clone());
                        report.cron_fired.push(firing.run_id);
                    }
                    AdmissionResult::Duplicate { .. } => {
                        p.last_fired.insert(flow_id.clone(), tick);
                        report.cron_lost += 1;
                    }
                    rejection => {
                        tracing::warn!(
                            project = %p.spec.name,
                            attachment_id = %cron.attachment_id,
                            ?rejection,
                            "dispatcher: cron admission refused"
                        );
                    }
                }
            }
        }
        // The loop's cron-aware sleep: the earliest upcoming fire (quarantined
        // schedules excluded — no full-horizon walk per sweep for them).
        p.next_cron_fire = reg
            .crons
            .iter()
            .filter(|cron| !p.bad_schedules.contains(&cron.schedule))
            .filter_map(|cron| next_fire(&cron.schedule, now_ms).ok())
            .min();

        // 3. Cancellation/deadline reconciliation. The run-state statement
        // owns locking, deferred seizure, terminalization, propagation, and
        // transactional waiter notification; the dispatcher only supplies the
        // fairness bound.
        let sweep_batch = i64::try_from(batch).unwrap_or(i64::MAX);
        for row in p
            .client
            .query(cancellation_sweep_sql(), &[&sweep_batch])
            .await?
        {
            report.cancelled.push(row.get("run_id"));
        }

        // 4. Wake / reconciliation: hint every currently-due unleased row.
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

        // 5. Adaptive cadence.
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

/// [9.1] A `wamn.trigger` span rooting a dispatcher-fired run's trace, enriched
/// with the run context the host mints here. Kept private so telemetry remains
/// an executable/service concern; tracebench exercises it through stepped
/// stdio's `emit-trigger-span` process command.
fn trigger_span(f: &Firing, tenant: &str) -> tracing::Span {
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

/// Fire one attachment through the single callable-flow admission transition.
///
/// The head lock, definition recheck, run creation, queue creation, and anchor
/// advance share one transaction. A crash at any seam rolls all of them back.
async fn fire(
    client: &mut Client,
    cron: &CronAttachment,
    f: &Firing,
    tick: i64,
    fired_at: i64,
    partition_key: Option<&str>,
    policy: &str,
) -> anyhow::Result<AdmissionResult> {
    let recipe = admission_sql();
    let tx = client.transaction().await?;
    tx.query_one(recipe.lock_head(), &[&cron.catalog_id, &cron.environment])
        .await?;

    let run_deadline_ms = fired_at
        .checked_add(cron.run_deadline_ms)
        .context("cron run deadline overflow")?;
    let run_deadline = std::time::UNIX_EPOCH
        .checked_add(Duration::from_millis(
            u64::try_from(run_deadline_ms).context("cron run deadline predates epoch")?,
        ))
        .context("cron run deadline is outside system-time range")?;
    let tick_identity = canonical_tick(tick)?;
    let invocation_context = "{}";
    let no_text: Option<&str> = None;
    let no_i64: Option<i64> = None;
    let no_timestamp: Option<std::time::SystemTime> = None;
    let run_deadline = Some(run_deadline);
    let generation = PHASE_2A_CRON_GENERATION;
    let row = tx
        .query_one(
            recipe.admit(),
            &[
                &AdmissionProducer::Cron.as_sql(),
                &cron.catalog_id,
                &cron.environment,
                &cron.catalog_version,
                &cron.attachment_id,
                &cron.definition_hash,
                &f.flow_id,
                &f.flow_version,
                &f.run_id,
                &f.input_json,
                &invocation_context,
                &env!("CARGO_PKG_VERSION"),
                &no_timestamp,
                &run_deadline,
                &no_text,
                &no_text,
                &no_text,
                &no_timestamp,
                &no_text,
                &no_i64,
                &Some(generation),
                &Some(tick_identity.as_str()),
                &no_text,
                &no_i64,
                &no_text,
                &no_text,
                &partition_key,
                &policy,
            ],
        )
        .await?;
    let code: String = row.get("result_code");
    let run_id: Option<String> = row.get("run_id");
    let result = AdmissionResult::from_parts(&code, run_id)
        .with_context(|| format!("unknown cron admission result {code:?}"))?;
    if matches!(
        result,
        AdmissionResult::Admitted { .. } | AdmissionResult::Duplicate { .. }
    ) {
        tx.execute(&upsert_cron_anchor_sql(), &[&f.flow_id, &tick])
            .await?;
    }
    tx.commit().await?;
    Ok(result)
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
        "shared trigger dispatcher up (cron + parked-wake + deadline sweep)"
    );

    if args.stepped_stdio {
        return run_stepped_stdio(&mut dispatcher).await;
    }

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

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
enum StepCommand {
    Tick {
        project: usize,
        now_ms: i64,
    },
    EmitTriggerSpan {
        run_id: String,
        flow_id: String,
        flow_version: i32,
        trigger_source: String,
        tenant: String,
    },
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

#[derive(Debug, serde::Serialize)]
struct SpanResponse<'a> {
    status: &'static str,
    run_id: &'a str,
}

/// Drive deterministic sweeps through the executable boundary.
///
/// Tick input is `{"command":"tick","project":N,"now_ms":T}`. The
/// `emit-trigger-span` command exercises the service-private production span
/// builder. One response is flushed before the next command is read, so a
/// caller can mutate the backing database between ticks while this process
/// retains its in-memory anchors.
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
            StepCommand::EmitTriggerSpan {
                run_id,
                flow_id,
                flow_version,
                trigger_source,
                tenant,
            } => {
                let firing = Firing {
                    run_id,
                    flow_id,
                    flow_version,
                    input_json: "{}".to_string(),
                    trigger_source,
                };
                {
                    let span = trigger_span(&firing, &tenant);
                    let _entered = span.enter();
                    tracing::info!("dispatcher trigger span process proof");
                }
                serde_json::to_vec(&SpanResponse {
                    status: "span-emitted",
                    run_id: &firing.run_id,
                })
                .context("encode span response")?
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
    use super::{
        CRON_ATTACHMENTS_SQL, DispatcherConfig, Ordering, PartitionPolicy, RUN_QUEUE_DEPTH_SQL,
        Registry, fire, parse_registry, partition_key_for_firing, partition_policy_for_firing,
        valid_tenant,
    };
    use wamn_run_state::admission::AdmissionResult;
    use wamn_scheduler::Firing;

    // [9.8] the run_queue.depth count must reuse the CLAIMABLE predicate (rows a
    // runner could take now), not its inverse. A mutant that counts parked rows
    // (`available_at > now()`) or drops a clause diverges here and at metricbench
    // phase 2. The clauses are asserted verbatim against the same trio
    // `wamn_run_state::queue::claim_batch_sql` fences the claim with.
    #[test]
    fn depth_sql_counts_claimable_not_parked() {
        let sql = RUN_QUEUE_DEPTH_SQL;
        assert!(
            sql.contains("available_at <= now()"),
            "delay must have elapsed"
        );
        assert!(sql.contains("lease_expires_at IS NULL OR lease_expires_at <= now()"));
        assert!(sql.contains("attempts < max_attempts OR lease_expires_at IS NULL"));
        assert!(
            sql.contains("current_setting('app.tenant', true)"),
            "tenant floor"
        );
        // The inverted-predicate mutant must not be what we count.
        assert!(!sql.contains("available_at > now()"));
        // The claim path fences with the SAME clauses (drift guard). It aliases
        // the queue table (`c.`), so match alias-surviving substrings.
        let claim = wamn_run_state::queue::claim_batch_sql(1);
        assert!(claim.contains("available_at <= now()"));
        assert!(claim.contains("lease_expires_at IS NULL"));
        assert!(claim.contains("attempts < "));
    }

    #[test]
    fn cron_registry_reads_only_authoritative_attachment_projection() {
        let sql = CRON_ATTACHMENTS_SQL;
        assert!(sql.contains("FROM catalog.cron_attachments AS a"));
        assert!(sql.contains("JOIN catalog.release_sources AS source"));
        assert!(sql.contains("source.source_kind = 'schedule'"));
        assert!(sql.contains("JOIN catalog.release_flows AS rf"));
        assert!(sql.contains("JOIN catalog.flow_artifacts AS fa"));
        assert!(sql.contains("source.definition_json->>'schedule'"));
        assert!(!sql.contains("INSERT INTO"));
        assert!(!sql.contains("wamn_run.runs"));
        assert!(!sql.contains("wamn_run.run_queue"));
        assert!(!sql.contains("FROM flows"));
    }

    #[tokio::test]
    #[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
    async fn callable_cron_attachment_live() {
        let url = std::env::var("WAMN_RUN_STORE_PG_URL")
            .expect("set WAMN_RUN_STORE_PG_URL to a throwaway PostgreSQL database");
        let (mut client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .expect("connect throwaway PostgreSQL");
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .batch_execute(
                "DO $$ BEGIN \
                   IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                     CREATE ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
                   END IF; \
                 END $$; \
                 DROP SCHEMA IF EXISTS catalog CASCADE; \
                 DROP SCHEMA IF EXISTS wamn_run CASCADE;",
            )
            .await
            .unwrap();
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        for path in [
            "/deploy/sql/catalog-schema.sql",
            "/deploy/sql/run-state.sql",
            "/deploy/sql/run-queue.sql",
        ] {
            let ddl = std::fs::read_to_string(format!("{root}{path}")).unwrap();
            client.batch_execute(&ddl).await.unwrap();
        }
        let graph = serde_json::json!({
            "schema-version": "0.1",
            "flow-id": "flow-cron",
            "version": 1,
            "nodes": [{"id": "entry", "type": "cron"}]
        })
        .to_string();
        client
            .execute(
                "INSERT INTO catalog.flow_artifacts \
                   (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash, \
                    artifact_hash,interface_bundle_json,interface_bundle_hash,component_digests) \
                 VALUES ('t1','flow-cron',1,'0.1',$1::text::jsonb,'gh','ah','[]','ih','[]')",
                &[&graph],
            )
            .await
            .unwrap();
        client
            .batch_execute(
                "INSERT INTO catalog.catalogs \
                   (tenant_id,catalog_id,version,environment,schema_version,state) \
                 VALUES ('t1','c1',1,'dev','0.1','applied'); \
                 INSERT INTO catalog.release_manifests \
                   (tenant_id,catalog_id,catalog_version,members_json) \
                 VALUES ('t1','c1',1, \
                   '[{\"flow-id\":\"flow-cron\",\"flow-version\":1,\"artifact-hash\":\"ah\"}]'); \
                 INSERT INTO catalog.release_flows \
                   (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
                 VALUES ('t1','c1',1,'flow-cron',1); \
                 INSERT INTO catalog.release_exposure_manifests \
                   (tenant_id,catalog_id,catalog_version,definitions_json) \
                 VALUES ('t1','c1',1,'{}'); \
                 INSERT INTO catalog.release_sources \
                   (tenant_id,catalog_id,catalog_version,source_id,source_kind,definition_json,source_hash) \
                 VALUES ('t1','c1',1,'schedule-a','schedule', \
                   '{\"schedule\":\"* * * * * *\",\"timezone\":\"UTC\",\"catch-up\":\"skip\"}','sh'); \
                 INSERT INTO catalog.release_attachments \
                   (tenant_id,catalog_id,catalog_version,attachment_id,attachment_kind,flow_id, \
                    source_id,definition_hash,definition_json) \
                 VALUES ('t1','c1',1,'cron-a','cron','flow-cron','schedule-a','sha256:cron', \
                   '{\"id\":\"cron-a\",\"kind\":\"cron\",\"flow-id\":\"flow-cron\", \
                     \"source-id\":\"schedule-a\",\"run-deadline-ms\":60000}'); \
                 INSERT INTO catalog.catalog_heads \
                   (tenant_id,catalog_id,environment,applied_catalog_version) \
                 VALUES ('t1','c1','dev',1); \
                 INSERT INTO catalog.attachment_activation \
                   (tenant_id,catalog_id,environment,attachment_id,confirmed_definition_hash,enabled) \
                 VALUES ('t1','c1','dev','cron-a','sha256:cron',true);",
            )
            .await
            .unwrap();
        client
            .batch_execute("SET app.tenant='t1'; SET search_path=wamn_run,public")
            .await
            .unwrap();
        let rows = client.query(CRON_ATTACHMENTS_SQL, &[]).await.unwrap();
        let registry = parse_registry("live", &rows);
        assert_eq!(registry.crons.len(), 1);
        let cron = registry.crons[0].clone();
        let tick = 1_767_225_600_000;
        let firing =
            wamn_scheduler::cron_firing(&cron.flow_id, cron.flow_version, 0, tick, tick + 5_000)
                .unwrap();

        let result = fire(
            &mut client,
            &cron,
            &firing,
            tick,
            tick + 5_000,
            None,
            "blocking",
        )
        .await
        .unwrap();
        assert!(matches!(result, AdmissionResult::Admitted { .. }));
        let duplicate = fire(
            &mut client,
            &cron,
            &firing,
            tick,
            tick + 5_000,
            None,
            "blocking",
        )
        .await
        .unwrap();
        assert!(matches!(duplicate, AdmissionResult::Duplicate { .. }));

        client
            .batch_execute(
                "CREATE FUNCTION wamn_run.reject_cron_anchor() RETURNS trigger \
                   LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fault-after-admit'; END $$; \
                 CREATE TRIGGER cron_anchor_fault BEFORE INSERT OR UPDATE ON wamn_run.cron_anchor \
                   FOR EACH ROW EXECUTE FUNCTION wamn_run.reject_cron_anchor();",
            )
            .await
            .unwrap();
        let tick2 = tick + 1_000;
        let firing2 =
            wamn_scheduler::cron_firing(&cron.flow_id, cron.flow_version, 0, tick2, tick2).unwrap();
        assert!(
            fire(&mut client, &cron, &firing2, tick2, tick2, None, "blocking")
                .await
                .is_err()
        );
        client
            .batch_execute(
                "DROP TRIGGER cron_anchor_fault ON wamn_run.cron_anchor; \
                 DROP FUNCTION wamn_run.reject_cron_anchor();",
            )
            .await
            .unwrap();
        assert!(matches!(
            fire(&mut client, &cron, &firing2, tick2, tick2, None, "blocking")
                .await
                .unwrap(),
            AdmissionResult::Admitted { .. }
        ));

        let tick3 = tick2 + 1_000;
        let firing3 =
            wamn_scheduler::cron_firing(&cron.flow_id, cron.flow_version, 0, tick3, tick3).unwrap();
        assert!(matches!(
            fire(&mut client, &cron, &firing3, tick3, tick3, None, "blocking")
                .await
                .unwrap(),
            AdmissionResult::Admitted { .. }
        ));
        let counts = client
            .query_one(
                "SELECT (SELECT count(*) FROM wamn_run.runs), \
                        (SELECT count(*) FROM wamn_run.run_queue)",
                &[],
            )
            .await
            .unwrap();
        assert_eq!((counts.get::<_, i64>(0), counts.get::<_, i64>(1)), (3, 3));

        let mut stale = cron.clone();
        stale.catalog_version = 99;
        let stale_firing =
            wamn_scheduler::cron_firing(&cron.flow_id, cron.flow_version, 0, tick3 + 1_000, tick3)
                .unwrap();
        assert_eq!(
            fire(
                &mut client,
                &stale,
                &stale_firing,
                tick3 + 1_000,
                tick3,
                None,
                "blocking"
            )
            .await
            .unwrap(),
            AdmissionResult::HeadDrift
        );
        let mut changed = cron.clone();
        changed.definition_hash = "sha256:changed".to_string();
        let changed_firing =
            wamn_scheduler::cron_firing(&cron.flow_id, cron.flow_version, 0, tick3 + 1_500, tick3)
                .unwrap();
        assert_eq!(
            fire(
                &mut client,
                &changed,
                &changed_firing,
                tick3 + 1_500,
                tick3,
                None,
                "blocking"
            )
            .await
            .unwrap(),
            AdmissionResult::DefinitionDrift
        );

        client
            .execute(
                "UPDATE catalog.attachment_activation SET enabled=false \
                 WHERE attachment_id='cron-a'",
                &[],
            )
            .await
            .unwrap();
        assert!(
            client
                .query(CRON_ATTACHMENTS_SQL, &[])
                .await
                .unwrap()
                .is_empty()
        );
        client
            .execute(
                "INSERT INTO catalog.attachment_tombstones \
                   (tenant_id,catalog_id,environment,attachment_id,removed_in_catalog_version) \
                 VALUES ('t1','c1','dev','cron-a',2)",
                &[],
            )
            .await
            .unwrap();
        assert!(
            client
                .query(CRON_ATTACHMENTS_SQL, &[])
                .await
                .unwrap()
                .is_empty()
        );
        let disabled_firing =
            wamn_scheduler::cron_firing(&cron.flow_id, cron.flow_version, 0, tick3 + 2_000, tick3)
                .unwrap();
        assert_eq!(
            fire(
                &mut client,
                &cron,
                &disabled_firing,
                tick3 + 2_000,
                tick3,
                None,
                "blocking"
            )
            .await
            .unwrap(),
            AdmissionResult::InactiveDefinition
        );
        assert_eq!(
            client
                .query_one("SELECT count(*) FROM wamn_run.runs", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            3
        );
        drop(client);
        connection.abort();
    }

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
        // The caller binds this literal only for keyed work. Unordered work
        // carries no key and admission must receive the blocking default:
        // unkeyed leapfrog is deliberately invalid at the centralized boundary.
        let unordered_key = partition_key_for_firing(&reg, &firing("leap", input));
        let admitted_policy = unordered_key
            .as_ref()
            .map_or(PartitionPolicy::Blocking.as_sql(), |_| {
                partition_policy_for_firing(&reg, &firing("leap", input))
            });
        assert_eq!(admitted_policy, "blocking");
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
        assert!(wamn_scheduler::Cadence::new(5_000, 1_000).is_err());
        let cadence = wamn_scheduler::Cadence::new(250, 30_000).expect("valid band");
        let cfg = DispatcherConfig { cadence, batch: 64 };
        assert_eq!((cfg.cadence.min(), cfg.cadence.max()), (250, 30_000));
    }
}

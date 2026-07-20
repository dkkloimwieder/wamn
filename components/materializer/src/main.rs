//! The Service-first materializer guest (EVT-MAT, D19 v3 §5 / l5i9.17) — the
//! effect shell over the PURE `wamn-materializer` decision pipeline.
//!
//! One sweep: read the tenant's registrations + each subscribed flow's ACTIVE
//! graph (ordering/policy) through `wamn:postgres`; per serviceable
//! registration bind a durable pull consumer on the org/env `EVT_` stream
//! (subject-filtered to the registration's entity) and fetch a bounded batch;
//! per delivered event run [`wamn_materializer::decide`] and map the verdict:
//!
//! - `Fire` → write-ahead run + evt enqueue in ONE transaction (both halves
//!   `ON CONFLICT DO NOTHING` — the exactly-once guarantee past the JetStream
//!   dedupe window), then ring the doorbell (best-effort, post-commit), then
//!   ack. A LOST write-ahead (another replica / an earlier redelivery won)
//!   skips the enqueue — re-inserting could resurrect a terminal run's queue
//!   row (the dispatcher's ghost-dispatch rule) — and still acks.
//! - `Skip` → ack (deterministic; the event stays on the stream for replay).
//! - `Refuse` → **alertable**: a distinct `wamn::materializer` warn + counter,
//!   then ack (a redelivery cannot change a deterministic refusal; nacking
//!   would poison-loop).
//! - Effect failures (PG down, publish/ack errors) → nack with delay: the
//!   at-least-once redelivery retries the effect, and the deterministic
//!   run id collapses any half-applied fire.
//!
//! A registration that cannot be SERVED — unparseable doc, missing/inactive/
//! invalid flow, or a syntactically invalid condition — is HELD: no consumer is
//! fetched, so its events stay on the stream (delayed, never lost — the
//! dispatcher's invalid-flow posture) and every sweep warns. A root-`old`
//! condition is NO LONGER held (l5i9.31): it is served, and an event that
//! carries no old image is refused per event (`old-image-absent`, alertable).
//!
//! Identity is host-injected: DB claims + doorbell tenant ride `wamn.tenant`
//! workload config; the guest env copy (`WAMN_MAT_TENANT`) only scopes the
//! tenant GUARD comparison (RLS holds regardless of what the env claims).

wit_bindgen::generate!({
    world: "materializer",
    path: "wit",
    generate_all,
});

use std::collections::HashMap;

use wamn_event_reg::EventRegistration;
use wamn_event_wire::Envelope;
use wamn_flow::Flow;
use wamn_materializer::{
    DecideError, FirePlan, FlowDeclaration, MAX_CAUSATION_DEPTH, RefuseReason, SkipReason, Verdict,
    decide, serviceable,
    sql::{select_active_flow_sql, select_registrations_sql},
};
use wamn_run_queue::{
    enqueue_evt_sql, enqueue_evt_with_policy_sql, mint_evt_run_id, shadow_observe_sql,
    write_ahead_triggered_run_sql,
};

use wamn::jetstream::consumer::{self, ConsumerConfig};
use wamn::jetstream::doorbell;
use wamn::postgres::client;
use wamn::postgres::types::{PgError, SqlValue};

// ---------------------------------------------------------------------------
// Config (wasi:cli env — `localResources.environment` on the Service spec)
// ---------------------------------------------------------------------------

struct Config {
    /// The org/env `EVT_` stream this workload consumes (provisioned
    /// out-of-band; recorded per project-env by enable-cdc-project-env).
    stream: String,
    /// Subject segments (`evt.<org>.<project>.<env>.<entity>.<op>`).
    org: String,
    project: String,
    env: String,
    /// The bound tenant — MUST equal the workload's `wamn.tenant` config (the
    /// host-enforced DB claim); used here only for the tenant-guard compare.
    tenant: String,
    /// Fetch batch bound per registration per sweep.
    batch: u32,
    /// Long-poll window per fetch, ms (the idle sweep's natural pacing).
    fetch_ms: u64,
    /// Idle sleep when NO registration is serviceable, ms.
    sweep_ms: u64,
    /// Stop after N sweeps (0 = run forever). Gates set a finite count so the
    /// service exits cleanly ("exited successfully" — no restart).
    max_sweeps: u64,
    /// Causation depth ceiling (l5i9.1: 16).
    max_depth: u32,
    /// Server ack-wait for the durable consumers, ms.
    ack_wait_ms: u64,
    /// Redelivery delay for nacked (effect-failed) events, ms.
    nack_delay_ms: u64,
    /// Optional counters report path (needs a volume mount / preopen).
    report_path: Option<String>,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn required(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("missing required env {name}"))
}

impl Config {
    fn from_env() -> Result<Config, String> {
        Ok(Config {
            stream: required("WAMN_MAT_STREAM")?,
            org: required("WAMN_MAT_ORG")?,
            project: required("WAMN_MAT_PROJECT")?,
            env: required("WAMN_MAT_ENV")?,
            tenant: required("WAMN_MAT_TENANT")?,
            batch: env_or("WAMN_MAT_BATCH", "64")
                .parse()
                .map_err(|e| format!("WAMN_MAT_BATCH: {e}"))?,
            fetch_ms: env_or("WAMN_MAT_FETCH_MS", "5000")
                .parse()
                .map_err(|e| format!("WAMN_MAT_FETCH_MS: {e}"))?,
            sweep_ms: env_or("WAMN_MAT_SWEEP_MS", "10000")
                .parse()
                .map_err(|e| format!("WAMN_MAT_SWEEP_MS: {e}"))?,
            max_sweeps: env_or("WAMN_MAT_MAX_SWEEPS", "0")
                .parse()
                .map_err(|e| format!("WAMN_MAT_MAX_SWEEPS: {e}"))?,
            max_depth: env_or("WAMN_MAT_MAX_DEPTH", &MAX_CAUSATION_DEPTH.to_string())
                .parse()
                .map_err(|e| format!("WAMN_MAT_MAX_DEPTH: {e}"))?,
            ack_wait_ms: env_or("WAMN_MAT_ACK_WAIT_MS", "30000")
                .parse()
                .map_err(|e| format!("WAMN_MAT_ACK_WAIT_MS: {e}"))?,
            nack_delay_ms: env_or("WAMN_MAT_NACK_DELAY_MS", "5000")
                .parse()
                .map_err(|e| format!("WAMN_MAT_NACK_DELAY_MS: {e}"))?,
            report_path: std::env::var("WAMN_MAT_REPORT_PATH").ok(),
        })
    }
}

// ---------------------------------------------------------------------------
// Counters — the alertable-refusal observability (v3 §4) + the gate's report
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Counters {
    sweeps: u64,
    fired: u64,
    /// Write-ahead lost to an earlier redelivery / racing replica — the
    /// exactly-once no-op, counted to prove dedupe fired.
    duplicate: u64,
    skip_entity: u64,
    skip_op: u64,
    skip_foreign_tenant: u64,
    skip_condition_false: u64,
    refuse_depth: u64,
    refuse_tenant_unscopable: u64,
    refuse_old_image_absent: u64,
    refuse_condition_error: u64,
    refuse_seq: u64,
    held_registrations: u64,
    poison: u64,
    effect_retry: u64,
    doorbell_failed: u64,
    /// EVT-CUTOVER (l5i9.18) shadow-mode ledger writes, by verdict — a shadow
    /// registration counts its decide() outcome in the counters above AND one
    /// of these; `fired` stays zero in shadow (nothing fires).
    shadow_fire: u64,
    shadow_skip: u64,
    shadow_refuse: u64,
}

impl Counters {
    fn to_json(&self) -> String {
        format!(
            "{{\"sweeps\":{},\"fired\":{},\"duplicate\":{},\"skip-entity\":{},\"skip-op\":{},\
             \"skip-foreign-tenant\":{},\"skip-condition-false\":{},\"refuse-depth\":{},\
             \"refuse-tenant-unscopable\":{},\"refuse-old-image-absent\":{},\
             \"refuse-condition-error\":{},\"refuse-seq\":{},\"held-registrations\":{},\
             \"poison\":{},\"effect-retry\":{},\"doorbell-failed\":{},\
             \"shadow-fire\":{},\"shadow-skip\":{},\"shadow-refuse\":{}}}",
            self.sweeps,
            self.fired,
            self.duplicate,
            self.skip_entity,
            self.skip_op,
            self.skip_foreign_tenant,
            self.skip_condition_false,
            self.refuse_depth,
            self.refuse_tenant_unscopable,
            self.refuse_old_image_absent,
            self.refuse_condition_error,
            self.refuse_seq,
            self.held_registrations,
            self.poison,
            self.effect_retry,
            self.doorbell_failed,
            self.shadow_fire,
            self.shadow_skip,
            self.shadow_refuse,
        )
    }
}

// ---------------------------------------------------------------------------
// SqlValue helpers + error naming (the flowrunner idiom)
// ---------------------------------------------------------------------------

fn text(s: impl Into<String>) -> SqlValue {
    SqlValue::Text(s.into())
}
fn int32(v: i32) -> SqlValue {
    SqlValue::Int32(v)
}
fn int64(v: i64) -> SqlValue {
    SqlValue::Int64(v)
}

fn pg_name(e: &PgError) -> String {
    match e {
        PgError::SerializationFailure => "serialization-failure".into(),
        PgError::ConnectionUnavailable => "connection-unavailable".into(),
        PgError::StatementTimeout => "statement-timeout".into(),
        PgError::RowLimitExceeded(n) => format!("row-limit-exceeded({n})"),
        PgError::UniqueViolation(c) => format!("unique-violation({c})"),
        PgError::ForeignKeyViolation(c) => format!("foreign-key-violation({c})"),
        PgError::CheckViolation(c) => format!("check-violation({c})"),
        PgError::PermissionDenied => "permission-denied".into(),
        PgError::QueryError((state, msg)) => format!("query-error({state}: {msg})"),
    }
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

/// One serviceable registration, ready to fetch: the parsed declaration pair
/// plus the compiled condition (None = unconditional).
struct Serving {
    reg: EventRegistration,
    flow: FlowDeclaration,
    condition: Option<wamn_materializer::CompiledCondition>,
}

/// A durable-consumer name from the registration identity. The charset is
/// conservative ([A-Za-z0-9_-]; NATS reserves `.`/`*`/`>`/whitespace) and the
/// identity triple keeps two registrations' floors independent.
fn durable_name(tenant: &str, catalog_id: &str, registration_id: &str) -> String {
    let sanitize = |raw: &str| -> String {
        raw.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    format!(
        "mat_{}_{}_{}",
        sanitize(tenant),
        sanitize(catalog_id),
        sanitize(registration_id)
    )
}

/// Load + pre-flight the tenant's registrations. Unserviceable ones are HELD
/// (warned, not consumed). Flow graphs are read once per distinct flow.
fn load_servings(cfg: &Config, counters: &mut Counters) -> Result<Vec<Serving>, String> {
    let rs = client::query(&select_registrations_sql(), &[]).map_err(|e| pg_name(&e))?;
    let mut flows: HashMap<String, Option<FlowDeclaration>> = HashMap::new();
    let mut servings = Vec::new();
    for row in &rs.rows {
        let (Some(SqlValue::Text(reg_id)), Some(SqlValue::Text(flow_id)), Some(doc)) =
            (row.first(), row.get(1), row.get(2))
        else {
            return Err("registration row shape".into());
        };
        let doc = match doc {
            SqlValue::Text(s) | SqlValue::Json(s) => s.as_str(),
            other => return Err(format!("registration doc shape: {other:?}")),
        };
        let reg = match EventRegistration::from_json(doc) {
            Ok(r) => r,
            Err(e) => {
                counters.held_registrations += 1;
                eprintln!(
                    "wamn::materializer HELD registration {reg_id}: unparseable document ({e}) — events stay on the stream"
                );
                continue;
            }
        };
        let condition = match serviceable(&reg) {
            Ok(c) => c,
            Err(DecideError::UnserviceableCondition(why)) => {
                counters.held_registrations += 1;
                eprintln!(
                    "wamn::materializer HELD registration {reg_id}: condition not serviceable ({why:?}) — invalid JMESPath syntax (write-time validation backstop); events stay on the stream"
                );
                continue;
            }
        };
        let decl = flows
            .entry(flow_id.clone())
            .or_insert_with(|| load_flow(flow_id))
            .clone();
        let Some(flow) = decl else {
            counters.held_registrations += 1;
            eprintln!(
                "wamn::materializer HELD registration {reg_id}: flow {flow_id} missing, inactive, or invalid — events stay on the stream"
            );
            continue;
        };
        servings.push(Serving {
            reg,
            flow,
            condition,
        });
    }
    let _ = cfg;
    Ok(servings)
}

/// One subscribed flow's ACTIVE declaration — `None` holds the registration
/// (missing, inactive, unparseable, or failing validation — the dispatcher's
/// invalid-flow posture).
fn load_flow(flow_id: &str) -> Option<FlowDeclaration> {
    let rs = client::query(&select_active_flow_sql(), &[text(flow_id)])
        .map_err(|e| pg_name(&e))
        .ok()?;
    let row = rs.rows.first()?;
    let version = match row.first() {
        Some(SqlValue::Int32(v)) => *v,
        Some(SqlValue::Int64(v)) => i32::try_from(*v).ok()?,
        _ => return None,
    };
    let graph = match row.get(1) {
        Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => s,
        _ => return None,
    };
    let flow = Flow::from_json(graph).ok()?;
    flow.validate().ok()?;
    if flow.flow_id != flow_id {
        // The flows-table column and the graph's embedded id must agree (the
        // dispatcher's charset-extension rule); a mismatch holds.
        return None;
    }
    Some(FlowDeclaration {
        flow_id: flow.flow_id.clone(),
        flow_version: version,
        ordering: flow.ordering.clone(),
        partition_policy: flow.partition_policy,
    })
}

/// The fire transaction — the dispatcher's `fire()` shape through
/// `wamn:postgres`. Returns whether this caller WON the write-ahead (a loss =
/// the exactly-once no-op; the enqueue is skipped, never resurrected).
fn fire_txn(plan: &FirePlan) -> Result<bool, String> {
    let txn = client::begin().map_err(|e| pg_name(&e))?;
    let inserted = txn
        .execute(
            &write_ahead_triggered_run_sql(),
            &[
                text(&plan.run_id),
                text(&plan.flow_id),
                int32(plan.flow_version),
                text(&plan.trigger_source),
                text(&plan.input_json),
            ],
        )
        .map_err(|e| pg_name(&e))?;
    if inserted == 1 {
        match &plan.partition_key {
            Some(key) => txn
                .execute(
                    &enqueue_evt_with_policy_sql(),
                    &[
                        text(&plan.run_id),
                        text(key),
                        int32(0),
                        int64(0),
                        int64(plan.stream_seq),
                        text(plan.policy.as_sql()),
                    ],
                )
                .map_err(|e| pg_name(&e))?,
            None => txn
                .execute(
                    &enqueue_evt_sql(),
                    &[
                        text(&plan.run_id),
                        SqlValue::Null,
                        int32(0),
                        int64(0),
                        int64(plan.stream_seq),
                    ],
                )
                .map_err(|e| pg_name(&e))?,
        };
    }
    txn.commit().map_err(|e| pg_name(&e))?;
    Ok(inserted == 1)
}

/// The SHADOW observation (EVT-CUTOVER, l5i9.18): for a `state: shadow`
/// registration the decision lands in the `evt_shadow` ledger instead of
/// firing — no run, no queue row, no doorbell. The ledger PK's `ON CONFLICT DO
/// NOTHING` makes redelivery the same exactly-once no-op as the fire path's
/// write-ahead, so the cutover comparison stays count-exact.
#[allow(clippy::too_many_arguments)]
fn shadow_observe(
    registration_id: &str,
    stream_seq: i64,
    run_id: &str,
    flow_id: &str,
    verdict: &str,
    reason: Option<&str>,
    envelope: &Envelope,
    partition_key: Option<&str>,
    partition_policy: Option<&str>,
    input_json: Option<&str>,
) -> Result<(), String> {
    let opt = |v: Option<&str>| v.map_or(SqlValue::Null, text);
    client::execute(
        &shadow_observe_sql(),
        &[
            text(registration_id),
            int64(stream_seq),
            text(run_id),
            text(flow_id),
            text(verdict),
            opt(reason),
            text(&envelope.table),
            text(envelope.op.as_str()),
            opt(envelope.entity.as_deref()),
            opt(partition_key),
            opt(partition_policy),
            opt(input_json),
        ],
    )
    .map(|_| ())
    .map_err(|e| pg_name(&e))
}

/// The ledger's skip discriminant (stable comparator tokens — the cutbench
/// divergence classes key on these).
fn skip_token(reason: &SkipReason) -> &'static str {
    match reason {
        SkipReason::EntityMismatch => "entity-mismatch",
        SkipReason::OpMismatch => "op-mismatch",
        SkipReason::ForeignTenant => "foreign-tenant",
        SkipReason::ConditionFalse => "condition-false",
    }
}

/// The ledger's refuse discriminant (stable comparator tokens).
fn refuse_token(reason: &RefuseReason) -> &'static str {
    match reason {
        RefuseReason::DepthExceeded { .. } => "depth-exceeded",
        RefuseReason::TenantUnscopable => "tenant-unscopable",
        RefuseReason::OldImageAbsent => "old-image-absent",
        RefuseReason::ConditionError(_) => "condition-error",
        RefuseReason::SeqOverflow(_) => "seq-overflow",
    }
}

/// Serve one registration for one sweep: bind its durable consumer, fetch a
/// bounded batch, decide + effect each message.
fn serve(cfg: &Config, s: &Serving, counters: &mut Counters) {
    let filter = format!(
        "evt.{}.{}.{}.{}.>",
        cfg.org,
        cfg.project,
        cfg.env,
        wamn_event_wire::subject_token(s.reg.entity.as_str())
    );
    let bound = consumer::bind(&ConsumerConfig {
        stream_name: cfg.stream.clone(),
        durable: durable_name(&cfg.tenant, &s.reg.catalog_id, &s.reg.registration_id),
        filter_subject: filter,
        ack_wait_ms: cfg.ack_wait_ms,
        max_deliver: 0,
    });
    let bound = match bound {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "wamn::materializer bind failed for registration {} (stream {}): {e:?} — retrying next sweep",
                s.reg.registration_id, cfg.stream
            );
            return;
        }
    };
    let msgs = match bound.fetch(cfg.batch, cfg.fetch_ms) {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "wamn::materializer fetch failed for registration {}: {e:?} — retrying next sweep",
                s.reg.registration_id
            );
            return;
        }
    };
    for msg in msgs {
        let meta = msg.metadata();
        let body = msg.body();
        let envelope: Envelope = match serde_json::from_slice(&body) {
            Ok(e) => e,
            Err(e) => {
                // A malformed envelope can never fire deterministically —
                // poison. Term stops redelivery; the bytes stay on the stream.
                counters.poison += 1;
                eprintln!(
                    "wamn::materializer REFUSED (poison) stream_seq={}: envelope parse: {e}",
                    meta.stream_seq
                );
                let _ = msg.term();
                continue;
            }
        };
        if meta.stream_seq == 0 {
            // JetStream seqs start at 1; a 0 is the metadata-parse fallback —
            // transient, and the run id MUST NOT be minted from it.
            counters.effect_retry += 1;
            eprintln!("wamn::materializer metadata parse failure — nack for redelivery");
            let _ = msg.nack(cfg.nack_delay_ms);
            continue;
        }
        // EVT-CUTOVER (l5i9.18): a shadow registration observes instead of
        // firing — every verdict below lands in the evt_shadow ledger and the
        // fire arm never touches runs/queue/doorbell.
        let shadow = s.reg.state.is_shadow();
        match decide(
            &s.reg,
            &s.flow,
            s.condition.as_ref(),
            &envelope,
            meta.stream_seq,
            &cfg.tenant,
            cfg.max_depth,
        ) {
            Verdict::Fire(plan) if shadow => {
                match shadow_observe(
                    &s.reg.registration_id,
                    plan.stream_seq,
                    &plan.run_id,
                    &plan.flow_id,
                    "fire",
                    None,
                    &envelope,
                    plan.partition_key.as_deref(),
                    Some(plan.policy.as_sql()),
                    Some(&plan.input_json),
                ) {
                    Ok(()) => {
                        counters.shadow_fire += 1;
                        let _ = msg.ack();
                    }
                    Err(e) => {
                        // The observation IS the shadow effect — retry it like
                        // a fire (the ledger PK absorbs the redelivery).
                        counters.effect_retry += 1;
                        eprintln!(
                            "wamn::materializer shadow observe failed for {} ({e}) — nack for redelivery",
                            plan.run_id
                        );
                        let _ = msg.nack(cfg.nack_delay_ms);
                    }
                }
            }
            Verdict::Fire(plan) => match fire_txn(&plan) {
                Ok(won) => {
                    if won {
                        counters.fired += 1;
                        // Post-commit doorbell (best-effort: a lost hint only
                        // raises latency — the run-worker sweep backstops).
                        if let Err(e) = doorbell::ring(&plan.run_id) {
                            counters.doorbell_failed += 1;
                            eprintln!(
                                "wamn::materializer doorbell failed for {}: {e:?} (wake degrades to the sweep)",
                                plan.run_id
                            );
                        }
                    } else {
                        counters.duplicate += 1;
                    }
                    let _ = msg.ack();
                }
                Err(e) => {
                    // Effect failure: the decision stands, the effect retries
                    // on redelivery; the deterministic id absorbs any half.
                    counters.effect_retry += 1;
                    eprintln!(
                        "wamn::materializer fire failed for {} ({e}) — nack for redelivery",
                        plan.run_id
                    );
                    let _ = msg.nack(cfg.nack_delay_ms);
                }
            },
            Verdict::Skip(reason) => {
                // Shadow: ledger the skip so the comparator can CLASSIFY a
                // missing new-path firing (condition-narrowed registration vs a
                // real capture gap). ForeignTenant stays unledgered — the old
                // path never sees other tenants' events either, so it carries
                // no comparison signal. A ledger failure retries via nack.
                if shadow
                    && !matches!(reason, SkipReason::ForeignTenant)
                    && let Ok(seq) = i64::try_from(meta.stream_seq)
                {
                    if let Err(e) = shadow_observe(
                        &s.reg.registration_id,
                        seq,
                        &mint_evt_run_id(&s.flow.flow_id, meta.stream_seq),
                        &s.flow.flow_id,
                        "skip",
                        Some(skip_token(&reason)),
                        &envelope,
                        None,
                        None,
                        None,
                    ) {
                        counters.effect_retry += 1;
                        eprintln!(
                            "wamn::materializer shadow observe (skip) failed ({e}) — nack for redelivery"
                        );
                        let _ = msg.nack(cfg.nack_delay_ms);
                        continue;
                    }
                    counters.shadow_skip += 1;
                }
                match reason {
                    SkipReason::EntityMismatch => counters.skip_entity += 1,
                    SkipReason::OpMismatch => counters.skip_op += 1,
                    SkipReason::ForeignTenant => counters.skip_foreign_tenant += 1,
                    SkipReason::ConditionFalse => counters.skip_condition_false += 1,
                }
                let _ = msg.ack();
            }
            Verdict::Refuse(reason) => {
                // Shadow: ledger the refusal (its token is a declared cutover
                // divergence class — e.g. a DELETE the old path fires but this
                // path refuses under REPLICA IDENTITY DEFAULT). SeqOverflow
                // cannot bind the ledger's bigint — counted-only, like live.
                if shadow
                    && !matches!(reason, RefuseReason::SeqOverflow(_))
                    && let Ok(seq) = i64::try_from(meta.stream_seq)
                {
                    if let Err(e) = shadow_observe(
                        &s.reg.registration_id,
                        seq,
                        &mint_evt_run_id(&s.flow.flow_id, meta.stream_seq),
                        &s.flow.flow_id,
                        "refuse",
                        Some(refuse_token(&reason)),
                        &envelope,
                        None,
                        None,
                        None,
                    ) {
                        counters.effect_retry += 1;
                        eprintln!(
                            "wamn::materializer shadow observe (refuse) failed ({e}) — nack for redelivery"
                        );
                        let _ = msg.nack(cfg.nack_delay_ms);
                        continue;
                    }
                    counters.shadow_refuse += 1;
                }
                // v3 §4: refusals are a DISTINCT, alertable outcome.
                match &reason {
                    RefuseReason::DepthExceeded { parent } => {
                        counters.refuse_depth += 1;
                        eprintln!(
                            "wamn::materializer REFUSED stream_seq={} flow={}: causation depth {}+1 exceeds {} (root {}) — loop bound",
                            meta.stream_seq,
                            s.flow.flow_id,
                            parent.depth,
                            cfg.max_depth,
                            parent.root
                        );
                    }
                    RefuseReason::TenantUnscopable => {
                        counters.refuse_tenant_unscopable += 1;
                        eprintln!(
                            "wamn::materializer REFUSED stream_seq={} table={}: event not tenant-scopable (DELETE under REPLICA IDENTITY DEFAULT, or no tenant_id column)",
                            meta.stream_seq, envelope.table
                        );
                    }
                    RefuseReason::OldImageAbsent => {
                        counters.refuse_old_image_absent += 1;
                        eprintln!(
                            "wamn::materializer REFUSED stream_seq={} table={}: condition reads old but the event carries no old image (REPLICA IDENTITY not FULL, or an op with no prior row) — cannot-evaluate, never condition-false (l5i9.31)",
                            meta.stream_seq, envelope.table
                        );
                    }
                    RefuseReason::ConditionError(e) => {
                        counters.refuse_condition_error += 1;
                        eprintln!(
                            "wamn::materializer REFUSED stream_seq={}: condition evaluation failed ({e}) — never silently condition-false",
                            meta.stream_seq
                        );
                    }
                    RefuseReason::SeqOverflow(seq) => {
                        counters.refuse_seq += 1;
                        eprintln!("wamn::materializer REFUSED: stream_seq {seq} overflows BIGINT");
                    }
                }
                let _ = msg.ack();
            }
        }
    }
}

fn write_report(cfg: &Config, counters: &Counters) {
    if let Some(path) = &cfg.report_path
        && let Err(e) = std::fs::write(path, counters.to_json())
    {
        eprintln!("wamn::materializer report write failed ({path}): {e}");
    }
}

fn main() {
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wamn::materializer config error: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "wamn::materializer up: stream={} filter=evt.{}.{}.{}.*.* tenant={} batch={} fetch_ms={} max_sweeps={}",
        cfg.stream,
        cfg.org,
        cfg.project,
        cfg.env,
        cfg.tenant,
        cfg.batch,
        cfg.fetch_ms,
        cfg.max_sweeps
    );
    let mut counters = Counters::default();
    loop {
        counters.sweeps += 1;
        match load_servings(&cfg, &mut counters) {
            Ok(servings) => {
                if servings.is_empty() {
                    // Nothing serviceable: pace the sweep (with consumers the
                    // fetch long-poll is the pacing).
                    std::thread::sleep(std::time::Duration::from_millis(cfg.sweep_ms));
                } else {
                    for s in &servings {
                        serve(&cfg, s, &mut counters);
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "wamn::materializer sweep failed ({e}) — retrying after {}ms",
                    cfg.sweep_ms
                );
                std::thread::sleep(std::time::Duration::from_millis(cfg.sweep_ms));
            }
        }
        write_report(&cfg, &counters);
        if cfg.max_sweeps > 0 && counters.sweeps >= cfg.max_sweeps {
            println!(
                "wamn::materializer done after {} sweeps: {}",
                counters.sweeps,
                counters.to_json()
            );
            return;
        }
    }
}

//! The Service-first materializer guest (EVT-MAT, D19 v3 §5 / l5i9.17) — the
//! effect shell over the PURE `wamn-materializer` decision pipeline.
//!
//! One sweep: read the tenant's registrations + each subscribed flow's ACTIVE
//! graph (ordering/policy) through `wamn:postgres`; per serviceable
//! registration bind a durable pull consumer on the org/env `EVT_` stream
//! (subject-filtered to the registration's entity) and fetch a bounded batch;
//! per delivered event run [`wamn_materializer::decide`] and map the verdict:
//!
//! - `Fire` → the shared callable-flow admission transaction: head lock, live
//!   registration evidence check, scoped dedupe, run + available queue row.
//!   Then ring the doorbell (best-effort, post-commit) and ack. A duplicate
//!   admission is the exactly-once no-op and still acks.
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
    sql::{select_registrations_sql, select_release_flow_sql},
};
use wamn_run_state::admission::{
    AdmissionProducer, AdmissionResult, AdmissionTransition, RunStateSchema, admission_transaction,
    registration_evidence,
};

use wamn::jetstream::consumer::{self, ConsumerConfig};
use wamn::jetstream::doorbell;
use wamn::postgres::client;
use wamn::postgres::types::{PgError, SqlValue};

// ---------------------------------------------------------------------------
// Config (wasi:cli env — `localResources.environment` on the Service spec)
// ---------------------------------------------------------------------------

struct Config {
    /// Schema containing the run-state tables claimed by this project's runner.
    run_schema: RunStateSchema,
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
            run_schema: RunStateSchema::new(env_or("WAMN_MAT_RUN_SCHEMA", "wamn_run"))
                .map_err(|e| format!("WAMN_MAT_RUN_SCHEMA: {e}"))?,
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
}

impl Counters {
    fn to_json(&self) -> String {
        format!(
            "{{\"sweeps\":{},\"fired\":{},\"duplicate\":{},\"skip-entity\":{},\"skip-op\":{},\
             \"skip-foreign-tenant\":{},\"skip-condition-false\":{},\"refuse-depth\":{},\
             \"refuse-tenant-unscopable\":{},\"refuse-old-image-absent\":{},\
             \"refuse-condition-error\":{},\"refuse-seq\":{},\"held-registrations\":{},\
             \"poison\":{},\"effect-retry\":{},\"doorbell-failed\":{}}}",
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
fn null() -> SqlValue {
    SqlValue::Null
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
    catalog_version: i32,
    registration_document: String,
    registration_hash: String,
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
    let mut flows: HashMap<(String, String), Option<(i32, FlowDeclaration)>> = HashMap::new();
    let mut servings = Vec::new();
    for row in &rs.rows {
        let (
            Some(SqlValue::Text(reg_id)),
            Some(SqlValue::Text(catalog_id)),
            Some(SqlValue::Text(flow_id)),
            Some(doc),
        ) = (row.first(), row.get(1), row.get(2), row.get(3))
        else {
            return Err("registration row shape".into());
        };
        let doc = match doc {
            SqlValue::Text(s) | SqlValue::Json(s) => s.as_str(),
            other => return Err(format!("registration doc shape: {other:?}")),
        };
        let document: serde_json::Value = match serde_json::from_str(doc) {
            Ok(document) => document,
            Err(e) => {
                counters.held_registrations += 1;
                eprintln!(
                    "wamn::materializer HELD registration {reg_id}: invalid JSON document ({e}) — events stay on the stream"
                );
                continue;
            }
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
        if reg.registration_id != *reg_id
            || reg.catalog_id != *catalog_id
            || reg.flow_id != *flow_id
        {
            counters.held_registrations += 1;
            eprintln!(
                "wamn::materializer HELD registration {reg_id}: trusted identity columns disagree with the document — events stay on the stream"
            );
            continue;
        }
        let (registration_document, registration_hash) = registration_evidence(&document);
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
        let flow_key = (catalog_id.clone(), flow_id.clone());
        let decl = flows
            .entry(flow_key)
            .or_insert_with(|| load_flow(catalog_id, &cfg.env, flow_id))
            .clone();
        let Some((catalog_version, flow)) = decl else {
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
            catalog_version,
            registration_document,
            registration_hash,
        });
    }
    Ok(servings)
}

/// One subscribed flow declaration from the environment's applied release.
fn load_flow(catalog_id: &str, environment: &str, flow_id: &str) -> Option<(i32, FlowDeclaration)> {
    let rs = client::query(
        &select_release_flow_sql(),
        &[text(catalog_id), text(environment), text(flow_id)],
    )
    .map_err(|e| pg_name(&e))
    .ok()?;
    let row = rs.rows.first()?;
    let catalog_version = match row.first() {
        Some(SqlValue::Int32(v)) => *v,
        Some(SqlValue::Int64(v)) => i32::try_from(*v).ok()?,
        _ => return None,
    };
    let flow_version = match row.get(1) {
        Some(SqlValue::Int32(v)) => *v,
        Some(SqlValue::Int64(v)) => i32::try_from(*v).ok()?,
        _ => return None,
    };
    let graph = match row.get(2) {
        Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => s,
        _ => return None,
    };
    let interfaces = match row.get(3) {
        Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => s,
        _ => return None,
    };
    let flow = Flow::from_json(graph).ok()?;
    let interfaces: Vec<wamn_flow::node_contract::NodeInterface> =
        serde_json::from_str(interfaces).ok()?;
    let resolved = interfaces
        .into_iter()
        .map(|interface| (interface.node_type, interface.output_ports))
        .collect();
    flow.validate(&resolved).ok()?;
    if flow.flow_id != flow_id {
        // The flows-table column and the graph's embedded id must agree (the
        // dispatcher's charset-extension rule); a mismatch holds.
        return None;
    }
    Some((
        catalog_version,
        FlowDeclaration {
            flow_id: flow.flow_id.clone(),
            flow_version,
            ordering: flow.ordering.clone(),
            partition_policy: flow.partition_policy,
        },
    ))
}

/// Final event admission through the shared callable-flow transition.
///
/// Returns whether this caller created the run; a duplicate is the
/// exactly-once no-op. Every typed drift/refusal rolls back and is retried from
/// candidate resolution on redelivery.
fn fire_txn(cfg: &Config, serving: &Serving, plan: &FirePlan) -> Result<bool, String> {
    let recipe = admission_transaction(AdmissionTransition::CallableFlow {
        schema: &cfg.run_schema,
    });
    let txn = client::begin().map_err(|e| pg_name(&e))?;
    txn.query(
        recipe.lock_head(),
        &[text(&serving.reg.catalog_id), text(&cfg.env)],
    )
    .map_err(|e| pg_name(&e))?;
    let admitted = txn
        .query(
            recipe.admit(),
            &[
                text(AdmissionProducer::Event.as_sql()),
                text(&serving.reg.catalog_id),
                text(&cfg.env),
                int32(serving.catalog_version),
                null(),
                null(),
                text(&plan.flow_id),
                int32(plan.flow_version),
                text(&plan.run_id),
                text(&plan.input_json),
                text(&plan.invocation_context_json),
                text(concat!("materializer@", env!("CARGO_PKG_VERSION"))),
                null(),
                null(),
                null(),
                null(),
                null(),
                null(),
                null(),
                text(&serving.reg.registration_id),
                int64(plan.stream_seq),
                text(&serving.registration_document),
                text(&serving.registration_hash),
                text(&plan.source_run_id),
                text(&plan.causation.root),
                int32(i32::try_from(plan.causation.depth).expect("causation depth is bounded")),
                plan.partition_key.as_ref().map_or_else(null, text),
                text(plan.policy.as_sql()),
            ],
        )
        .map_err(|e| pg_name(&e))?;
    let row = admitted
        .rows
        .first()
        .ok_or_else(|| "admission returned no result row".to_string())?;
    let code = match row.first() {
        Some(SqlValue::Text(code)) => code,
        _ => return Err("admission result code shape".into()),
    };
    let run_id = match row.get(1) {
        Some(SqlValue::Text(run_id)) => Some(run_id.clone()),
        Some(SqlValue::Null) | None => None,
        _ => return Err("admission run id shape".into()),
    };
    let result = AdmissionResult::from_parts(code, run_id)
        .ok_or_else(|| format!("unknown admission result: {code}"))?;
    let won = match result {
        AdmissionResult::Admitted { .. } => true,
        AdmissionResult::Duplicate { .. } => false,
        refusal => return Err(format!("event admission refused: {refusal:?}")),
    };
    txn.commit().map_err(|e| pg_name(&e))?;
    Ok(won)
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
        match decide(
            &s.reg,
            &s.flow,
            s.condition.as_ref(),
            &envelope,
            meta.stream_seq,
            &cfg.tenant,
            cfg.max_depth,
        ) {
            Verdict::Fire(plan) => match fire_txn(cfg, s, &plan) {
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
                match reason {
                    SkipReason::EntityMismatch => counters.skip_entity += 1,
                    SkipReason::OpMismatch => counters.skip_op += 1,
                    SkipReason::ForeignTenant => counters.skip_foreign_tenant += 1,
                    SkipReason::ConditionFalse => counters.skip_condition_false += 1,
                }
                let _ = msg.ack();
            }
            Verdict::Refuse(reason) => {
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
        "wamn::materializer up: stream={} filter=evt.{}.{}.{}.*.* tenant={} run_schema={} batch={} fetch_ms={} max_sweeps={}",
        cfg.stream,
        cfg.org,
        cfg.project,
        cfg.env,
        cfg.tenant,
        cfg.run_schema.as_str(),
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

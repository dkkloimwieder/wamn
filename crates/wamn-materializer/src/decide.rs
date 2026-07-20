//! The per-event decision — one delivered envelope against one registration,
//! producing exactly one [`Verdict`]: fire (with everything the enqueue
//! needs), a normal skip, or an ALERTABLE refusal. The guest maps verdicts to
//! effects: `Fire` → write-ahead + enqueue in one `wamn:postgres` transaction,
//! doorbell after commit, then ack; `Skip`/`Refuse` → ack (the decision is
//! deterministic — a redelivery cannot change it; events stay on the stream
//! for replay regardless of ack). Refusals are counted + warned DISTINCTLY
//! (v3 §4: "refusals are a distinct, alertable outcome").

use serde_json::Value;

use wamn_event_reg::EventRegistration;
use wamn_event_wire::{Causation, Envelope};
use wamn_flow::Ordering;
use wamn_run_queue::mint_evt_run_id;

use crate::condition::{CompiledCondition, ConditionOutcome, compile_condition};
use crate::context::{event_context, tenant_of};
use crate::input::evt_input_json;

/// A subscribed flow's dispatch-relevant declaration, read from the flows
/// registry (`graph_json` → `wamn_flow::Flow`) by the guest each sweep.
#[derive(Debug, Clone)]
pub struct FlowDeclaration {
    pub flow_id: String,
    pub flow_version: i32,
    /// The 5.11 ordering declaration (fqg.20) — authoritative for WHETHER the
    /// flow is ordered and how the key folds.
    pub ordering: Ordering,
    /// The D20 head-unavailability policy, materialized onto keyed rows only
    /// (kq0z coherence).
    pub partition_policy: wamn_flow::PartitionPolicy,
}

/// Everything the guest's fire transaction needs for one won firing.
#[derive(Debug, Clone, PartialEq)]
pub struct FirePlan {
    /// `mint_evt_run_id(flow, stream_seq)` — deterministic, zero-padded.
    pub run_id: String,
    pub flow_id: String,
    pub flow_version: i32,
    /// The audit `trigger_source` (`evt:<stream_seq>`, the `outbox:<seq>` grammar).
    pub trigger_source: String,
    /// The persisted run input (causation thread embedded).
    pub input_json: String,
    /// The stamped key (`None` = unordered → the global claim).
    pub partition_key: Option<String>,
    /// The policy literal for a KEYED row ([`enqueue_evt_with_policy_sql`]'s
    /// `$6`; unused when `partition_key` is `None` — kq0z coherence).
    pub policy: wamn_run_queue::PartitionPolicy,
    /// The numeric stream position ([`enqueue_evt_sql`]'s `$5`, E4).
    pub stream_seq: i64,
    /// The child causation (also embedded in `input_json`) — for logging.
    pub causation: Causation,
}

/// Normal, non-alertable outcomes: the event is simply not this consumer's to
/// fire. Ack and move on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The envelope's entity doesn't match the registration (defensive — the
    /// durable consumer's subject filter should already scope this).
    EntityMismatch,
    /// The envelope's op is not in the registration's op set (one durable
    /// consumer fetches all ops of its entity; non-registered ops pass by).
    OpMismatch,
    /// The event belongs to a different tenant — THAT tenant's own
    /// materializer workload owns it (v1 binds one tenant per workload).
    ForeignTenant,
    /// The condition evaluated falsy. The event stays on the stream — a
    /// condition edit replays it (§5 hot-editability).
    ConditionFalse,
}

/// ALERTABLE refusals (v3 §4): deterministic decisions that an operator must
/// see — counted distinctly and warned, then acked (a redelivery cannot
/// change a deterministic verdict; nacking would poison-loop).
#[derive(Debug, Clone, PartialEq)]
pub enum RefuseReason {
    /// The causation chain hit the depth ceiling — the loop-bound firing.
    /// Carries the parent stamp for the alert.
    DepthExceeded { parent: Causation },
    /// The event cannot be tenant-scoped: a DELETE under REPLICA IDENTITY
    /// DEFAULT (old image = key only) or a table with no `tenant_id` column.
    /// Enqueuing it under the workload's tenant would be a cross-tenant leak.
    TenantUnscopable,
    /// The registration's condition references the ROOT `old` image — blocked
    /// until l5i9.31 (belt: [`serviceable`] should have HELD the registration).
    OldValueConditionBlocked,
    /// The condition failed to evaluate (never silently condition-false).
    ConditionError(String),
    /// `stream_seq` doesn't fit the queue's BIGINT (practically unreachable;
    /// refusing beats wrapping a claim key).
    SeqOverflow(u64),
}

/// One event × one registration → exactly one verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Fire(Box<FirePlan>),
    Skip(SkipReason),
    Refuse(RefuseReason),
}

/// Why a registration cannot be served at all (the guest HOLDS it: no
/// consumer bound, events delayed — never lost — and a warning each sweep;
/// the dispatcher's invalid-flow HOLD posture).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecideError {
    /// Condition present but not serviceable (bad syntax, or root-`old`
    /// blocked until l5i9.31).
    UnserviceableCondition(ConditionOutcome),
}

/// Pre-flight a registration for serving: compiles the condition (if any)
/// under the v1 REPLICA IDENTITY DEFAULT contract. Call once per sweep per
/// registration; the compiled condition is reused across the fetched batch.
pub fn serviceable(reg: &EventRegistration) -> Result<Option<CompiledCondition>, DecideError> {
    match &reg.condition {
        None => Ok(None),
        Some(expr) => compile_condition(expr)
            .map(Some)
            .map_err(DecideError::UnserviceableCondition),
    }
}

/// The child causation stamp for a run fired by `envelope`: organic writes
/// (no stamp) root a fresh chain at depth 0; a stamped write extends its
/// parent's chain (`root` carried, `depth + 1`). `Err` = over the ceiling —
/// the alertable loop-bound refusal.
pub fn child_causation(
    envelope: &Envelope,
    run_id: &str,
    max_depth: u32,
) -> Result<Causation, RefuseReason> {
    match &envelope.causation {
        None => Ok(Causation {
            run: run_id.to_string(),
            root: run_id.to_string(),
            depth: 0,
        }),
        Some(parent) => {
            let depth = parent.depth.saturating_add(1);
            if depth > max_depth {
                return Err(RefuseReason::DepthExceeded {
                    parent: parent.clone(),
                });
            }
            Ok(Causation {
                run: run_id.to_string(),
                root: parent.root.clone(),
                depth,
            })
        }
    }
}

/// Bridge the `wamn-flow` policy contract enum to the `wamn-run-queue` storage
/// enum (whose `as_sql` owns the single storage literal) — the same one
/// crossing point the dispatcher keeps (kq0z).
pub fn rq_policy(p: wamn_flow::PartitionPolicy) -> wamn_run_queue::PartitionPolicy {
    match p {
        wamn_flow::PartitionPolicy::Blocking => wamn_run_queue::PartitionPolicy::Blocking,
        wamn_flow::PartitionPolicy::Leapfrog => wamn_run_queue::PartitionPolicy::Leapfrog,
    }
}

/// The `run_queue.partition_key` an evt firing carries. The FLOW's ordering
/// declaration is authoritative (fqg.20): unordered → `None` (a declared
/// registration extractor is inert), strict → the flow id, partitioned → the
/// registration's `partition-key` extractor over the EVENT context when
/// declared (§5: "extracts the key from the payload"), else the flow's own
/// expression over the RUN INPUT (dispatcher parity). Both paths fold through
/// [`Ordering::partition_key_for`]'s rules — null/missing/non-scalar degrades
/// to the flow-wide stream, NEVER a NULL key (a NULL key would route an
/// ordered flow to the unordered global claim, the D20 corruption).
fn partition_key(
    flow: &FlowDeclaration,
    reg: &EventRegistration,
    event_ctx: &Value,
    run_input: &Value,
) -> Option<String> {
    match &flow.ordering {
        Ordering::Unordered => None,
        Ordering::Strict => Some(flow.flow_id.clone()),
        Ordering::Partitioned { .. } => match &reg.partition_key {
            Some(extractor) => Ordering::Partitioned {
                partition_key: extractor.clone(),
            }
            .partition_key_for(&flow.flow_id, event_ctx),
            None => flow.ordering.partition_key_for(&flow.flow_id, run_input),
        },
    }
}

/// Decide one delivered event against one registration. `tenant` is the
/// workload's own bound tenant (host-injected claim; the guest reads its copy
/// from config/env — the DB claims stay host-enforced regardless).
/// `condition` is [`serviceable`]'s compile for THIS registration.
pub fn decide(
    reg: &EventRegistration,
    flow: &FlowDeclaration,
    condition: Option<&CompiledCondition>,
    envelope: &Envelope,
    stream_seq: u64,
    tenant: &str,
    max_depth: u32,
) -> Verdict {
    // 1. Entity (defensive — the consumer's subject filter already scopes it;
    //    rename-proof: the STABLE id, never the physical table).
    if envelope.entity.as_deref() != Some(reg.entity.as_str()) {
        return Verdict::Skip(SkipReason::EntityMismatch);
    }
    // 2. Op set.
    if !reg.ops.contains(&envelope.op) {
        return Verdict::Skip(SkipReason::OpMismatch);
    }
    // 3. Tenant guard — BEFORE any evaluation: an unscopable event must never
    //    reach a fire, and a foreign tenant's event is not ours to judge.
    match tenant_of(envelope) {
        None => return Verdict::Refuse(RefuseReason::TenantUnscopable),
        Some(t) if t != tenant => return Verdict::Skip(SkipReason::ForeignTenant),
        Some(_) => {}
    }
    // 4. Condition, over the event context.
    let event_ctx = event_context(envelope);
    if reg.condition.is_some() {
        let Some(cond) = condition else {
            // The caller passed no compile for a condition-bearing registration
            // — the serviceable() hold should have prevented this; refuse
            // rather than fire unfiltered.
            return Verdict::Refuse(RefuseReason::OldValueConditionBlocked);
        };
        match cond.matches(&event_ctx) {
            Ok(true) => {}
            Ok(false) => return Verdict::Skip(SkipReason::ConditionFalse),
            Err(e) => return Verdict::Refuse(RefuseReason::ConditionError(e)),
        }
    }
    // 5. Numeric stream position (E4).
    let Ok(seq_i64) = i64::try_from(stream_seq) else {
        return Verdict::Refuse(RefuseReason::SeqOverflow(stream_seq));
    };
    // 6. Mint: run id, causation chain, input, key + policy.
    let run_id = mint_evt_run_id(&flow.flow_id, stream_seq);
    let child = match child_causation(envelope, &run_id, max_depth) {
        Ok(c) => c,
        Err(refuse) => return Verdict::Refuse(refuse),
    };
    let input_json = evt_input_json(envelope, stream_seq, &child);
    let run_input: Value = serde_json::from_str(&input_json).expect("minted input parses");
    let key = partition_key(flow, reg, &event_ctx, &run_input);
    Verdict::Fire(Box::new(FirePlan {
        run_id,
        flow_id: flow.flow_id.clone(),
        flow_version: flow.flow_version,
        trigger_source: format!("evt:{stream_seq}"),
        input_json,
        partition_key: key,
        policy: rq_policy(flow.partition_policy),
        stream_seq: seq_i64,
        causation: child,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wamn_event_wire::Op;

    fn envelope(op: Op, new: Value) -> Envelope {
        Envelope {
            op,
            old: None,
            new: Some(new.as_object().unwrap().clone()),
            entity: Some("receipts".into()),
            table: "receipts".into(),
            lsn: 42,
            txid: 7,
            commit_ts: chrono::DateTime::parse_from_rfc3339("2026-07-19T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            causation: None,
        }
    }

    fn registration(condition: Option<&str>, partition_key: Option<&str>) -> EventRegistration {
        EventRegistration::from_json(
            &json!({
                "schema-version": "0.1",
                "registration-id": "r1",
                "catalog-id": "cat",
                "flow-id": "f1",
                "entity": "receipts",
                "ops": ["insert", "update"],
                "condition": condition,
                "partition-key": partition_key,
            })
            .to_string(),
        )
        .expect("registration parses")
    }

    fn flow(ordering: Ordering, policy: wamn_flow::PartitionPolicy) -> FlowDeclaration {
        FlowDeclaration {
            flow_id: "f1".into(),
            flow_version: 3,
            ordering,
            partition_policy: policy,
        }
    }

    fn fire(v: Verdict) -> FirePlan {
        match v {
            Verdict::Fire(plan) => *plan,
            other => panic!("expected Fire, got {other:?}"),
        }
    }

    #[test]
    fn a_matching_insert_fires_with_the_e4_mint() {
        let reg = registration(None, None);
        let f = flow(Ordering::Unordered, wamn_flow::PartitionPolicy::Blocking);
        let env = envelope(Op::Insert, json!({"id": "7", "tenant_id": "t1"}));
        let plan = fire(decide(&reg, &f, None, &env, 9, "t1", 16));
        assert_eq!(plan.run_id, "f1:evt:00000000000000000009");
        assert_eq!(plan.trigger_source, "evt:9");
        assert_eq!(plan.stream_seq, 9);
        assert_eq!(plan.partition_key, None, "unordered flow → global claim");
        // Organic write → fresh root at depth 0.
        assert_eq!(plan.causation.depth, 0);
        assert_eq!(plan.causation.root, plan.run_id);
        let input: Value = serde_json::from_str(&plan.input_json).unwrap();
        assert_eq!(input["causation"]["depth"], 0);
    }

    #[test]
    fn entity_op_and_tenant_guards_skip() {
        let reg = registration(None, None);
        let f = flow(Ordering::Unordered, wamn_flow::PartitionPolicy::Blocking);

        let mut wrong_entity = envelope(Op::Insert, json!({"tenant_id": "t1"}));
        wrong_entity.entity = Some("orders".into());
        assert_eq!(
            decide(&reg, &f, None, &wrong_entity, 1, "t1", 16),
            Verdict::Skip(SkipReason::EntityMismatch)
        );
        // Unmapped (entity ABSENT) never matches an id-keyed registration,
        // even when the TABLE name coincides — the rename-proof half of R9b.
        let mut unmapped = envelope(Op::Insert, json!({"tenant_id": "t1"}));
        unmapped.entity = None;
        assert_eq!(
            decide(&reg, &f, None, &unmapped, 1, "t1", 16),
            Verdict::Skip(SkipReason::EntityMismatch)
        );

        let wrong_op = envelope(Op::Delete, json!({}));
        assert_eq!(
            decide(&reg, &f, None, &wrong_op, 1, "t1", 16),
            Verdict::Skip(SkipReason::OpMismatch)
        );

        let foreign = envelope(Op::Insert, json!({"tenant_id": "t2"}));
        assert_eq!(
            decide(&reg, &f, None, &foreign, 1, "t1", 16),
            Verdict::Skip(SkipReason::ForeignTenant),
            "another tenant's event is a normal skip — its own workload owns it"
        );
    }

    #[test]
    fn unscopable_events_refuse_never_fire() {
        // A DELETE under REPLICA IDENTITY DEFAULT: old = key only. Register
        // the delete op so the guard (not the op filter) is what fires.
        let reg = EventRegistration::from_json(
            &json!({
                "schema-version": "0.1", "registration-id": "r1", "catalog-id": "cat",
                "flow-id": "f1", "entity": "receipts", "ops": ["delete"],
            })
            .to_string(),
        )
        .unwrap();
        let f = flow(Ordering::Unordered, wamn_flow::PartitionPolicy::Blocking);
        let del = Envelope {
            op: Op::Delete,
            old: Some(json!({"id": "7"}).as_object().unwrap().clone()),
            new: None,
            entity: Some("receipts".into()),
            table: "receipts".into(),
            lsn: 1,
            txid: 1,
            commit_ts: chrono::DateTime::parse_from_rfc3339("2026-07-19T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            causation: None,
        };
        assert_eq!(
            decide(&reg, &f, None, &del, 1, "t1", 16),
            Verdict::Refuse(RefuseReason::TenantUnscopable)
        );
        // Same refusal for a table with no tenant_id column.
        let no_tenant = envelope(Op::Insert, json!({"id": "7"}));
        let reg2 = registration(None, None);
        assert_eq!(
            decide(&reg2, &f, None, &no_tenant, 1, "t1", 16),
            Verdict::Refuse(RefuseReason::TenantUnscopable)
        );
    }

    #[test]
    fn condition_gates_the_fire_and_stays_replayable() {
        let reg = registration(Some("new.status == 'received'"), None);
        let cond = serviceable(&reg).unwrap();
        let f = flow(Ordering::Unordered, wamn_flow::PartitionPolicy::Blocking);
        let hit = envelope(Op::Insert, json!({"tenant_id": "t1", "status": "received"}));
        let miss = envelope(Op::Insert, json!({"tenant_id": "t1", "status": "draft"}));
        assert!(matches!(
            decide(&reg, &f, cond.as_ref(), &hit, 1, "t1", 16),
            Verdict::Fire(_)
        ));
        assert_eq!(
            decide(&reg, &f, cond.as_ref(), &miss, 2, "t1", 16),
            Verdict::Skip(SkipReason::ConditionFalse)
        );
    }

    #[test]
    fn old_value_conditions_are_held_at_serviceability() {
        // The l5i9.31 gate: a changed-from condition cannot be served under
        // REPLICA IDENTITY DEFAULT — the registration is HELD, not
        // silently evaluated (old-absent = cannot-evaluate, never false).
        let reg = registration(Some("new.status != old.status"), None);
        assert!(matches!(
            serviceable(&reg),
            Err(DecideError::UnserviceableCondition(
                ConditionOutcome::OldValueBlocked
            ))
        ));
    }

    #[test]
    fn causation_chain_extends_and_refuses_over_budget() {
        let reg = registration(None, None);
        let f = flow(Ordering::Unordered, wamn_flow::PartitionPolicy::Blocking);
        let mut env = envelope(Op::Insert, json!({"tenant_id": "t1"}));
        env.causation = Some(Causation {
            run: "f0:evt:00000000000000000001".into(),
            root: "origin".into(),
            depth: 3,
        });
        let plan = fire(decide(&reg, &f, None, &env, 9, "t1", 16));
        assert_eq!(plan.causation.depth, 4, "child depth = parent + 1");
        assert_eq!(plan.causation.root, "origin", "the root carries");
        assert_eq!(plan.causation.run, plan.run_id);

        // At the ceiling: parent 15 → child 16 fires; parent 16 → 17 refuses.
        env.causation.as_mut().unwrap().depth = 15;
        assert!(matches!(
            decide(&reg, &f, None, &env, 10, "t1", 16),
            Verdict::Fire(_)
        ));
        env.causation.as_mut().unwrap().depth = 16;
        assert!(matches!(
            decide(&reg, &f, None, &env, 11, "t1", 16),
            Verdict::Refuse(RefuseReason::DepthExceeded { .. })
        ));
    }

    #[test]
    fn ordering_resolves_key_and_policy_kq0z_coherently() {
        let env = envelope(Op::Insert, json!({"tenant_id": "t1", "site": "s-9"}));

        // Strict → the flow id, policy = the flow's declaration.
        let reg = registration(None, None);
        let strict = flow(Ordering::Strict, wamn_flow::PartitionPolicy::Leapfrog);
        let plan = fire(decide(&reg, &strict, None, &env, 1, "t1", 16));
        assert_eq!(plan.partition_key.as_deref(), Some("f1"));
        assert_eq!(plan.policy, wamn_run_queue::PartitionPolicy::Leapfrog);

        // Partitioned + registration extractor: over the EVENT context.
        let reg_extract = registration(None, Some("new.site"));
        let part = flow(
            Ordering::Partitioned {
                partition_key: "payload.site".into(),
            },
            wamn_flow::PartitionPolicy::Blocking,
        );
        let plan = fire(decide(&reg_extract, &part, None, &env, 2, "t1", 16));
        assert_eq!(plan.partition_key.as_deref(), Some("s-9"));

        // Partitioned, NO extractor: the flow's own expression over the RUN
        // INPUT (whose row image is `payload` — dispatcher grammar).
        let plan = fire(decide(&reg, &part, None, &env, 3, "t1", 16));
        assert_eq!(plan.partition_key.as_deref(), Some("s-9"));

        // A missing key folds to the flow-wide stream — never NULL.
        let reg_missing = registration(None, Some("new.absent_column"));
        let plan = fire(decide(&reg_missing, &part, None, &env, 4, "t1", 16));
        assert_eq!(
            plan.partition_key.as_deref(),
            Some("f1"),
            "null/missing key degrades to the flow-wide stream (fqg.20 rule)"
        );

        // Unordered: NO key even when the registration declares an extractor.
        let unordered = flow(Ordering::Unordered, wamn_flow::PartitionPolicy::Blocking);
        let plan = fire(decide(&reg_extract, &unordered, None, &env, 5, "t1", 16));
        assert_eq!(plan.partition_key, None);
    }

    #[test]
    fn redelivery_is_deterministic() {
        // Same event, same registration → byte-identical plan (the ON CONFLICT
        // dedupe upstream depends on the run_id; determinism is the property).
        let reg = registration(None, None);
        let f = flow(Ordering::Unordered, wamn_flow::PartitionPolicy::Blocking);
        let env = envelope(Op::Insert, json!({"tenant_id": "t1"}));
        let a = fire(decide(&reg, &f, None, &env, 9, "t1", 16));
        let b = fire(decide(&reg, &f, None, &env, 9, "t1", 16));
        assert_eq!(a, b);
    }
}

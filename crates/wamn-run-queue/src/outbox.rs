//! Outbox → run decisions — the trigger dispatcher's row-event half (D4, 5.14).
//! LISTEN/NOTIFY is removed entirely (D4 locked): a producer inserts an event
//! row **in its own transaction** — sharing the user write's transaction, one
//! durability domain, so an event is durable iff its write is — and the
//! dispatcher POLLS pending rows ([`crate::outbox_poll_sql`], `FOR UPDATE SKIP
//! LOCKED`), fires one run per (matching flow × row), and acks
//! ([`crate::outbox_ack_sql`]) — poll, fire, and ack in ONE dispatcher
//! transaction. A crash anywhere before that commit redelivers the whole batch
//! and retracts its enqueues atomically (no half-state); on redelivery the
//! deterministic run ids ([`mint_outbox_run_id`]) collapse the re-fire to no-ops.

use serde_json::value::RawValue;

use crate::dispatch::Firing;

/// One pending outbox row as the poll returns it: the identity sequence, the
/// source table + event kind (wamn-flow `row-event` vocabulary:
/// insert|update|delete), and the event payload as **raw JSON text**
/// (`payload::text` from the jsonb column). The payload is deliberately opaque —
/// it is never decoded into a float-lossy `Value`, so the numeric fidelity of a
/// `row_to_json` payload (exact-decimal columns, int8 beyond 2^53 — the
/// platform's no-float rule) survives into the run input verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    pub seq: i64,
    pub table: String,
    pub event: String,
    pub payload: Option<String>,
}

/// A flow registered on a row event — the (table, event) the dispatcher matched
/// out of an active flow's `trigger` (wamn-flow `Trigger::RowEvent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowEventFlow {
    pub flow_id: String,
    pub flow_version: i32,
    pub table: String,
    pub event: String,
}

/// Deterministic run id for an outbox firing: one run per (flow, event row),
/// `{flow_id}:outbox:{seq}`. A redelivered row (crashed poll, racing replica)
/// re-mints the same id and the write-ahead `ON CONFLICT` absorbs it — which is
/// also what makes the run id safe to use as a downstream idempotency key
/// (POC-F4's ERP callback keys on `${run.id}`).
pub fn mint_outbox_run_id(flow_id: &str, seq: i64) -> String {
    format!("{flow_id}:outbox:{seq}")
}

/// One firing per (pending row × flow registered on its `(table, event)`). The
/// row payload is spliced into the firing's `input_json` envelope **verbatim**
/// (a raw-value splice, not a parse/re-serialize round trip) so numbers keep
/// their exact text; a missing payload is an explicit `null`.
pub fn match_outbox(rows: &[OutboxRow], flows: &[RowEventFlow]) -> Vec<Firing> {
    #[derive(serde::Serialize)]
    struct Envelope<'a> {
        trigger: &'static str,
        table: &'a str,
        event: &'a str,
        seq: i64,
        payload: &'a RawValue,
    }
    let null = RawValue::from_string("null".to_string()).expect("null is JSON");

    let mut firings = Vec::new();
    for row in rows {
        // A payload straight out of a jsonb column is always valid JSON; the
        // fallback to null only fires on pathological input (e.g. beyond the
        // JSON recursion limit) — degraded, never corrupted.
        let payload = row
            .payload
            .as_ref()
            .and_then(|s| RawValue::from_string(s.clone()).ok());
        let payload: &RawValue = payload.as_deref().unwrap_or(&null);
        for flow in flows
            .iter()
            .filter(|f| f.table == row.table && f.event == row.event)
        {
            let input_json = serde_json::to_string(&Envelope {
                trigger: "row-event",
                table: &row.table,
                event: &row.event,
                seq: row.seq,
                payload,
            })
            .expect("envelope serializes");
            firings.push(Firing {
                run_id: mint_outbox_run_id(&flow.flow_id, row.seq),
                flow_id: flow.flow_id.clone(),
                flow_version: flow.flow_version,
                input_json,
                trigger_source: format!("outbox:{}", row.seq),
            });
        }
    }
    firings
}

/// The seqs a poll may ACK: every polled row EXCEPT those whose `(table, event)`
/// is **held** — the events of an *active but unparseable/invalid* flow (a
/// dispatcher/flow version skew). Acking those would turn a skipped flow into
/// permanent silent event loss, so they stay pending and redeliver until the
/// flow (or the dispatcher binary) is fixed. Everything else — matched or
/// unmatched — acks: an unmatched backlog must not pin the oldest-first poll
/// window (an unmatched row is consumed-with-no-op).
pub fn plan_ack(rows: &[OutboxRow], held: &[(String, String)]) -> Vec<i64> {
    rows.iter()
        .filter(|r| !held.iter().any(|(t, e)| *t == r.table && *e == r.event))
        .map(|r| r.seq)
        .collect()
}

/// The complement of [`plan_ack`]: the seqs of the polled rows whose `(table,
/// event)` IS held. The dispatcher stamps these via [`crate::outbox_hold_sql`] in
/// the SAME transaction as the ack, so a held flow's events leave the poll window
/// (R14) instead of head-of-line-blocking the healthy ones — kept in the table,
/// never acked (no silent loss), with a `held_since` age to alert on. Every
/// polled seq is in exactly one of `plan_ack` / `plan_hold`.
pub fn plan_hold(rows: &[OutboxRow], held: &[(String, String)]) -> Vec<i64> {
    rows.iter()
        .filter(|r| held.iter().any(|(t, e)| *t == r.table && *e == r.event))
        .map(|r| r.seq)
        .collect()
}

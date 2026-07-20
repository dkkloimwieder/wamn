//! CDC event-run identity (D19 v3 §5 / E4) — the deterministic run id the
//! materializer (l5i9.17) mints per delivered stream event. Grammar parity with
//! the trigger dispatcher's ids ([`crate::mint_cron_run_id`] /
//! [`crate::mint_outbox_run_id`]): the id embeds the flow and the firing's
//! stream position, so a redelivered event re-mints the SAME id and the
//! write-ahead `ON CONFLICT` absorbs the duplicate — the exactly-once guarantee
//! past the JetStream dedupe window (the window is only the fast path).
//!
//! Always-on (no `dispatcher` feature): the materializer guest links this
//! through the same `default-features = false` core the flowrunner uses.

/// Deterministic run id for a CDC event firing: `{flow}:evt:{stream_seq}`, one
/// run per (flow, stream event).
///
/// The sequence is **zero-padded to 20 digits** (the full `u64` width) so the
/// id's LEXICAL order equals the numeric stream order — the E4 belt. The
/// braces: `run_id` is a TEXT column and rides claim `ORDER BY`s wherever the
/// numeric `stream_seq` column isn't in play (partition stream order pre-dated
/// it; external consumers key on `${run.id}`); without padding `f1:evt:10`
/// sorts before `f1:evt:9` — the R6/D20 corruption class arriving through a
/// string comparison. The suspenders are the `stream_seq` BIGINT the enqueue
/// carries ahead of `run_id` in every claim key
/// ([`crate::enqueue_evt_sql`] / [`crate::enqueue_evt_with_policy_sql`]).
pub fn mint_evt_run_id(flow_id: &str, stream_seq: u64) -> String {
    format!("{flow_id}:evt:{stream_seq:020}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evt_run_id_is_flow_evt_padded_seq() {
        // The exact grammar §5 names, with the E4 fixed-width pad.
        assert_eq!(mint_evt_run_id("f1", 9), "f1:evt:00000000000000000009");
        assert_eq!(mint_evt_run_id("f1", 10), "f1:evt:00000000000000000010");
    }

    #[test]
    fn padded_ids_sort_lexically_in_numeric_order() {
        // The E4 belt: 8 < 9 < 10 < 11 lexically — the failure the unpadded
        // grammar had (f1:evt:10 < f1:evt:9).
        let ids: Vec<String> = [8u64, 9, 10, 11]
            .iter()
            .map(|s| mint_evt_run_id("f1", *s))
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(sorted, ids, "lexical order must equal numeric stream order");
    }

    #[test]
    fn pad_width_covers_the_full_u64_range() {
        // u64::MAX is 20 digits; the pad must never truncate or overflow-wrap.
        let max = mint_evt_run_id("f", u64::MAX);
        assert_eq!(max, format!("f:evt:{}", u64::MAX));
        assert_eq!(u64::MAX.to_string().len(), 20);
    }
}

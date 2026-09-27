//! Parameterized SQL for the Postgres intent record, `wamn_run.intents`.
//!
//! The statements keep the rules of the edge's SQLite store
//! (`docs/plan/edge.md` 4.7), with a key unique per tenant, release, package
//! and operation (`wamn-an24`). Each is one statement, so no transaction spans
//! the export call. Table names stay unqualified, so the caller's transaction
//! `search_path` selects the run plane, and row-level security reads the
//! tenant from the `app.tenant` claim.
//!
//! The pure tests cover the statement text only. The live half runs the intent
//! store case set against the Postgres store over a disposable database.

/// Milliseconds since the Unix epoch, as the edge records them.
const NOW_MS: &str = "(extract(epoch FROM clock_timestamp()) * 1000)::bigint";

/// Begin one intent in one statement: insert the key when it is new, or else
/// return the stored row.
///
/// Binds: `$1` tenant, `$2` release, `$3` package, `$4` operation, `$5`
/// idempotency key, `$6` input hash, `$7` deadline in milliseconds.
/// Columns: id, new, input hash, outcome kind, outcome (JSON text), resolved
/// basis. A new row answers `new = true` and nulls; a stored row answers its
/// facts. No row means another call inserted the key in the same instant and
/// its row is not yet visible; the caller retries or fails the call.
pub fn begin_intent_sql() -> String {
    format!(
        "WITH inserted AS ( \
             INSERT INTO intents (tenant_id, release, package, operation, idempotency_key, \
                                  input_hash, deadline_ms, begun_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, {NOW_MS}) \
             ON CONFLICT ON CONSTRAINT intents_key DO NOTHING \
             RETURNING id \
         ) \
         SELECT id, true, NULL::text, NULL::text, NULL::text, NULL::text FROM inserted \
         UNION ALL \
         SELECT id, false, input_hash, outcome_kind, outcome::text, resolved_basis \
           FROM intents \
          WHERE tenant_id = $1 AND release = $2 AND package = $3 AND operation = $4 \
            AND idempotency_key = $5 \
            AND NOT EXISTS (SELECT 1 FROM inserted)"
    )
}

/// Record the outcome of an open intent. It changes no row when the intent
/// finished or an operator resolved it.
///
/// Binds: `$1` id, `$2` outcome kind (`completed` or `failed`), `$3` outcome
/// JSON text.
pub fn finish_intent_sql() -> String {
    format!(
        "UPDATE intents \
            SET finished_at = {NOW_MS}, outcome_kind = $2, outcome = $3::text::jsonb \
          WHERE id = $1 AND finished_at IS NULL AND resolved_at IS NULL"
    )
}

/// The intents that began and never finished, oldest first.
///
/// Binds: `$1` limit. Columns: id, tenant, release, package, operation,
/// idempotency key.
pub fn uncertain_intents_sql() -> &'static str {
    "SELECT id, tenant_id, release, package, operation, idempotency_key \
       FROM intents WHERE finished_at IS NULL AND resolved_at IS NULL \
      ORDER BY id LIMIT $1"
}

/// Close an uncertain intent by an operator decision. It changes no row when
/// the intent is not uncertain.
///
/// Binds: `$1` id, `$2` basis.
pub fn resolve_intent_sql() -> String {
    format!(
        "UPDATE intents SET resolved_basis = $2, resolved_at = {NOW_MS} \
          WHERE id = $1 AND finished_at IS NULL AND resolved_at IS NULL"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_begin_is_one_statement_keyed_by_the_whole_intent_key() {
        let begin = begin_intent_sql();
        assert!(begin.starts_with("WITH inserted AS ("));
        assert!(begin.contains("ON CONFLICT ON CONSTRAINT intents_key DO NOTHING"));
        assert!(
            begin.contains(
                "WHERE tenant_id = $1 AND release = $2 AND package = $3 AND operation = $4"
            )
        );
        assert_eq!(begin.matches(';').count(), 0, "one statement");
    }

    #[test]
    fn finish_and_resolve_touch_only_an_open_intent() {
        for sql in [finish_intent_sql(), resolve_intent_sql()] {
            assert!(
                sql.contains("finished_at IS NULL AND resolved_at IS NULL"),
                "{sql}"
            );
        }
    }
}

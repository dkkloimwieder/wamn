use super::quote_literal;

/// Delete expired prefixes and advance the watermark in one statement.
///
/// `$1` is the retention in days. An entry goes when its `changed_at` is before
/// the cutoff and no entry of the same row with an earlier position is at or
/// after the cutoff.
pub(super) fn prune_history_sql(table: &str) -> String {
    format!(
        "WITH removed AS (DELETE FROM {table} AS entry \
          USING (SELECT transaction_timestamp() - make_interval(days => $1) AS at) AS cutoff \
          WHERE entry.changed_at < cutoff.at \
            AND NOT EXISTS (SELECT FROM {table} AS earlier \
                             WHERE earlier.row_key = entry.row_key \
                               AND earlier.position < entry.position \
                               AND earlier.changed_at >= cutoff.at) \
          RETURNING entry.transaction_id), \
          watermark AS (INSERT INTO wamn_history.pruning_watermarks AS current \
                         (history_table, transaction_id) \
                       SELECT {table_literal}::regclass, max(transaction_id) FROM removed \
                       HAVING count(*) > 0 \
                       ON CONFLICT (history_table) DO UPDATE \
                         SET transaction_id = greatest(current.transaction_id, EXCLUDED.transaction_id)) \
          SELECT count(*) FROM removed",
        table_literal = quote_literal(table),
    )
}

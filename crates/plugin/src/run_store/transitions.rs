//! Fenced run-state transitions.
//!
//! Every executor-owned mutation joins `runs` to the queue-owned
//! `(lease_owner, lease_generation)` authority and returns one typed result row.
//! The explicit authority run id is deliberately separate from the target run
//! id: accidentally presenting one run's fence while mutating another is a
//! typed `cross-run-authority` refusal, not an empty update.

use crate::RunStatus;

static FENCED_PREFIX: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "\
WITH input AS ( \
    SELECT $1::text AS target_run_id, $2::text AS authority_run_id, \
           $3::text AS lease_owner, $4::bigint AS lease_generation \
), \
locked_run AS MATERIALIZED ( \
    SELECT r.* FROM runs AS r, input AS i \
     WHERE i.target_run_id = i.authority_run_id \
       AND r.run_id = i.target_run_id \
     FOR UPDATE OF r \
), \
locked_queue AS MATERIALIZED ( \
    SELECT q.* FROM run_queue AS q \
      JOIN locked_run AS r \
        ON r.run_id = q.run_id \
     FOR UPDATE OF q \
), \
authority AS ( \
    SELECT CASE \
             WHEN i.target_run_id <> i.authority_run_id THEN 'cross-run-authority' \
             WHEN r.run_id IS NULL THEN 'not-found' \
             WHEN r.status IN ('{completed}', '{failed}', '{infrastructure_failure}') \
               THEN 'run-terminal' \
             WHEN q.run_id IS NULL \
               OR q.lease_owner IS DISTINCT FROM i.lease_owner \
               OR q.lease_generation IS DISTINCT FROM i.lease_generation \
               OR q.lease_expires_at IS NULL OR q.lease_expires_at <= clock_timestamp() \
               THEN 'fence-lost' \
             ELSE 'ready' \
           END AS result_code, \
           r.run_id, r.status \
      FROM input AS i \
      LEFT JOIN locked_run AS r ON true \
      LEFT JOIN locked_queue AS q ON true \
)",
        completed = RunStatus::Completed.as_sql(),
        failed = RunStatus::Failed.as_sql(),
        infrastructure_failure = RunStatus::InfrastructureFailure.as_sql()
    )
});

/// Store the Response checkpoint under the same fence as terminalization.
/// Parameters are target run, authority run, owner, generation, and JSON text.
pub(super) fn checkpoint_sql() -> String {
    format!(
        "{prefix}, recorded AS ( \
             UPDATE runs AS r SET state_json = $5::text::jsonb \
               FROM authority AS a WHERE a.result_code = 'ready' \
                AND r.run_id = a.run_id \
                AND (r.state_json IS NULL OR r.state_json = $5::text::jsonb) \
             RETURNING r.run_id \
         ) SELECT EXISTS (SELECT 1 FROM recorded)",
        prefix = *FENCED_PREFIX,
    )
}

/// Take the first durable terminal result and remove its queue row atomically.
///
/// Params: target run id, authority run id, lease owner, lease generation,
/// terminal status, terminal reason, result JSON text, failure type, terminal
/// instant. The queue fence authorizes the transition.
///
/// The caller supplies the terminal timestamp.
pub(super) fn terminalize_sql() -> String {
    format!(
        "{prefix}, \
         terminalized AS ( \
             UPDATE runs AS r \
                SET status = $5, terminal_reason = $6, \
                    result_json = $7::text::jsonb, fail_type = $8, \
                    updated_at = $9::timestamptz \
               FROM authority AS c \
              WHERE c.result_code = 'ready' \
                AND r.run_id = c.run_id \
             RETURNING r.run_id, r.status \
         ), \
         dequeued AS ( \
             DELETE FROM run_queue AS q USING terminalized AS t \
              WHERE q.run_id = t.run_id \
             RETURNING q.run_id \
         ) \
         SELECT CASE WHEN t.run_id IS NOT NULL THEN 'terminalized' ELSE c.result_code END \
                    AS result_code, \
                COALESCE(t.status, c.status) AS run_status \
           FROM authority AS c LEFT JOIN terminalized AS t ON true \
          WHERE (SELECT count(*) FROM dequeued) >= 0",
        prefix = *FENCED_PREFIX,
    )
}

//! The single source of run-state SQL (docs/archive/structure-review.md SR2).
//!
//! Pure text builders over the `runs` table this crate owns
//! (`deploy/sql/run-state.sql`), in the house shape: values are ALWAYS `$n`
//! parameters, identifiers are pinned, table names are UNQUALIFIED (the host
//! injects the schema via `search_path` — the S6 schema-as-fixture pattern),
//! and the tenant comes from the session claim
//! (`current_setting('app.env_key', true)`). These are run-level statements.
//! Status literals interpolate from [`crate::RunStatus`] so the builders cannot
//! drift from the model (the same discipline this crate's `queue` module uses).
//!
//! This module is guest-compilable by construction: `String` builders only,
//! no DB driver, no clock, no tokio in the dependency closure.

use crate::status::RunStatus;

/// Build the execution-only input projection for a run-row alias.
///
/// Event lineage is durable in trusted columns, never in author-visible
/// `input_json`. At single-shot execution time the runner still needs the lineage object its
/// frozen guest contract consumes, so the production-claim selector uses this
/// exact expression. The right-hand `jsonb` object replaces any same-named author
/// field; non-event rows retain their persisted input unchanged.
pub(crate) fn execution_input_sql(run_alias: &str) -> String {
    format!(
        "CASE WHEN {run_alias}.event_root_run_id IS NOT NULL \
                    AND {run_alias}.event_depth IS NOT NULL \
              THEN {run_alias}.input_json || jsonb_build_object( \
                     'causation', jsonb_build_object( \
                         'run', {run_alias}.run_id, \
                         'root', {run_alias}.event_root_run_id, \
                         'depth', {run_alias}.event_depth)) \
              ELSE {run_alias}.input_json END"
    )
}

/// Prune terminal run history older than a retention window (9.6, wamn-srb): the
/// `prune-run-history` verb's statement. DELETE `$1`'s `runs` rows in a TERMINAL
/// state ([`RunStatus::is_terminal`] — completed / failed /
/// infrastructure-failure) whose `created_at` predates `$2` days ago.
/// Any surviving `run_queue` rows cascade
/// via their `ON DELETE CASCADE` FK to `runs`. A `dispatched`/`running` run is
/// never pruned (it may still complete). Age-based
/// only in v0; no execution-lineage metadata participates.
/// Params: `$1` `env_key`, `$2` retention_days.
///
/// # The tenant is a BOUND PARAMETER, and that is the whole point
///
/// This predicate used to read `current_setting('app.env_key', true)`, which is
/// the RETIRED tenant keying: `wamn-0h0g.22.6` re-keyed `runs`' floor onto
/// `wamn_authority.tenant_key(env_key) =
/// wamn_authority.current_tenant_key()`, and the `app.env_key` claim survives
/// only on `run_queue`. A GUC-shaped predicate has one failure mode no amount
/// of care removes: `current_setting(_, true)`
/// returns NULL when the claim was never injected, `env_key = NULL` is NULL,
/// and the statement reports ZERO ROWS DELETED with no error — measured on
/// PostgreSQL 18.6 as `DELETE 0` under the dedicated retention role
/// (`wamn-0h0g.12.69`). A bound parameter cannot be forgotten: it is either
/// supplied or the statement does not execute.
///
/// Dropping the claim costs nothing the cascade needs. Measured on the same
/// server: the `run_queue` `ON DELETE CASCADE` fires as an internal
/// referential-integrity trigger that consults neither the deleter's table
/// grants nor `run_queue`'s FORCE-RLS `app.env_key` policy, so the queue row
/// still goes with the run in a session that never set the GUC and holds no
/// privilege on that relation at all.
///
/// WHAT THIS BUILDER DOES NOT DO is decide WHOSE history may be pruned. Under
/// the shared `wamn_platform` floor arm a retention credential matches every
/// tenant's rows, so binding the tenant makes the statement exact but not
/// confined. Proving that the connected identity is the one THIS tenant's
/// retention credential was minted for belongs to the verb
/// (`services/ctl/src/prune_run_history.rs`), which refuses before it ever
/// reaches this statement.
pub fn prune_terminal_runs_sql() -> String {
    let terminal: Vec<String> = RunStatus::ALL
        .into_iter()
        .filter(|s| s.is_terminal())
        .map(|s| format!("'{}'", s.as_sql()))
        .collect();
    format!(
        "DELETE FROM runs \
          WHERE env_key = $1 \
            AND status IN ({statuses}) \
            AND created_at < now() - ($2::bigint * interval '1 day')",
        statuses = terminal.join(", "),
    )
}

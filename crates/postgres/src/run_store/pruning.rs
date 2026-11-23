//! SQL for pruning terminal run history.
use wamn_run_state::RunStatus;

/// Delete terminal runs older than the bound retention period in days.
/// The caller authorizes the connection and binds days as `$1`.
/// Queue entries follow their run through `ON DELETE CASCADE`.
pub(super) fn prune_terminal_runs_sql() -> String {
    let terminal: Vec<String> = RunStatus::ALL
        .into_iter()
        .filter(|s| s.is_terminal())
        .map(|s| format!("'{}'", s.as_sql()))
        .collect();
    format!(
        "DELETE FROM runs \
          WHERE status IN ({statuses}) \
            AND created_at < now() - ($1::bigint * interval '1 day')",
        statuses = terminal.join(", "),
    )
}

//! SQL for pruning terminal run history.
use crate::RunStatus;

/// Delete terminal runs older than the bound retention period in days.
/// The caller authorizes the connection and binds the project as `$1` and days as `$2`.
/// Queue entries follow their run through `ON DELETE CASCADE`.
pub fn prune_terminal_runs_sql() -> String {
    let terminal: Vec<String> = RunStatus::ALL
        .into_iter()
        .filter(|s| s.is_terminal())
        .map(|s| format!("'{}'", s.as_sql()))
        .collect();
    format!(
        "DELETE FROM runs \
          WHERE project_env_key = $1 \
            AND status IN ({statuses}) \
            AND created_at < now() - ($2::bigint * interval '1 day')",
        statuses = terminal.join(", "),
    )
}

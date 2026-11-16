//! Calendar schedule evaluation and host-owned schedule workflow admission
//! (docs/architecture/execution.md#workflows).
//!
//! The task wakes at every minute boundary, reads the enabled schedule
//! workflows of the current application selection in one SELECT, and
//! admits a run for each row whose cron names that minute. A row enabled or disabled between ticks takes
//! effect at the next tick.

use anyhow::Context as _;
#[cfg(test)]
use chrono::{DateTime, Utc};
use wamn_catalog::parse_workflow_schedule;

/// Whether the cron of `schedule` names the minute `tick`.
pub fn is_due(schedule: &str, tick: chrono::NaiveDateTime) -> anyhow::Result<bool> {
    parse_workflow_schedule(schedule)?
        .is_time_matching(&tick.and_utc())
        .context("match the workflow schedule")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }
}

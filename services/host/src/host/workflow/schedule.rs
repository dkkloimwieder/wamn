//! UTC calendar schedule evaluation.

use anyhow::Context as _;
use wamn_workflow_graph::GraphError;

/// Whether the cron of `schedule` names the minute `tick`.
pub fn is_due(schedule: &str, tick: chrono::NaiveDateTime) -> anyhow::Result<bool> {
    parse_workflow_schedule(schedule)?
        .is_time_matching(&tick.and_utc())
        .context("match the workflow schedule")
}

/// Parse a workflow's five-field UTC cron expression.
///
/// # Errors
/// Returns an invalid-definition error for any other schedule syntax.
pub(super) fn parse_workflow_schedule(text: &str) -> Result<croner::Cron, GraphError> {
    if text.split_whitespace().count() != 5 {
        return Err(GraphError::InvalidDefinition {
            message: "workflow-schedule-invalid: requires five-field cron in UTC".to_owned(),
        });
    }
    croner::parser::CronParser::builder()
        .seconds(croner::parser::Seconds::Disallowed)
        .year(croner::parser::Year::Disallowed)
        .build()
        .parse(text)
        .map_err(|error| GraphError::InvalidDefinition {
            message: format!("workflow-schedule-invalid: {error}"),
        })
}

#[cfg(test)]
mod tests {
    use super::is_due;
    use chrono::{DateTime, Utc};

    #[test]
    fn a_schedule_matches_its_calendar_minutes() {
        let minute = |value: &str| {
            DateTime::parse_from_rfc3339(value)
                .unwrap()
                .with_timezone(&Utc)
                .naive_utc()
        };
        assert!(is_due("0 0 * * *", minute("2026-10-21T00:00:00Z")).unwrap());
        assert!(!is_due("0 0 * * *", minute("2026-10-21T00:01:00Z")).unwrap());
        assert!(is_due("0 0 1 * *", minute("2027-01-01T00:00:00Z")).unwrap());
        assert!(!is_due("0 0 31 2 *", minute("2027-02-28T00:00:00Z")).unwrap());
        assert!(is_due("* * * * * *", minute("2027-01-01T00:00:00Z")).is_err());
    }
}

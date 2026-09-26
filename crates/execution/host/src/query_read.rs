//! The two shapes of a query reply (`docs/architecture/execution.md`).
//!
//! A query yields its rows as a stream, then one outcome: the cursor that
//! continues the read, or the error that ended it. A page gathers them into
//! one outcome, and the route's input schema bounds its limit by the
//! contract's maximum. A streamed load writes one line for each row and one
//! for the outcome, and the host refuses a cap above its ceiling before the
//! query runs.

/// The largest cap of a streamed load. Measurement replaces this value
/// (`wamn-utci.6`).
pub(crate) const STREAM_CEILING: i64 = 100_000;

/// The reply line of one row: `{"row":…}`.
pub(crate) fn row_line(row: &str) -> String {
    format!("{{\"row\":{row}}}")
}

/// How a streamed read ended, as its last reply line says.
#[derive(Debug)]
pub(crate) enum StreamEnd {
    /// The read ended. `more` says whether rows past the cap exist.
    Completed { more: bool },
    /// The query refused, with its error value.
    Refused(serde_json::Value),
    /// The host cannot say how the read ended.
    Uncertain,
}

impl StreamEnd {
    /// Read a query's outcome JSON: a value with its cursor, or an error.
    pub(crate) fn of_outcome(outcome: &str) -> Self {
        match serde_json::from_str::<serde_json::Value>(outcome) {
            Ok(serde_json::Value::Object(mut outcome)) => match outcome.remove("value") {
                Some(value) => Self::Completed {
                    more: value
                        .get("next_cursor")
                        .is_some_and(|cursor| !cursor.is_null()),
                },
                None => outcome
                    .remove("error")
                    .map_or(Self::Uncertain, Self::Refused),
            },
            _ => Self::Uncertain,
        }
    }

    /// The last reply line, with the labels of the actors its rows name.
    pub(crate) fn line(&self, actor_labels: &[(String, String)]) -> String {
        let outcome = match self {
            Self::Completed { more } => {
                let labels: serde_json::Map<String, serde_json::Value> = actor_labels
                    .iter()
                    .map(|(actor, label)| (actor.clone(), serde_json::Value::from(label.as_str())))
                    .collect();
                serde_json::json!({ "value": { "more": more }, "actor_labels": labels })
            }
            Self::Refused(error) => serde_json::json!({ "error": error }),
            Self::Uncertain => serde_json::json!({ "uncertain": {} }),
        };
        serde_json::json!({ "outcome": outcome }).to_string()
    }
}

/// The refusal of a limit above `maximum`, as the outcome list a read replies
/// with, or `None` when the request's limit is within it.
pub(crate) fn limit_refusal(payload: &serde_json::Value, maximum: i64) -> Option<String> {
    let limit = payload.get(0)?.get("limit")?.as_i64()?;
    (limit > maximum).then(|| {
        serde_json::json!([{
            "error": {
                "code": "invalid_input",
                "detail": {
                    "field": "limit",
                    "minimum": "1",
                    "maximum": maximum.to_string(),
                    "observed": limit.to_string(),
                },
            },
        }])
        .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_line_says_how_the_read_ended() {
        let completed = StreamEnd::of_outcome(r#"{"value":{"next_cursor":"abc"}}"#);
        assert_eq!(
            completed.line(&[("actor".to_owned(), "Ada".to_owned())]),
            r#"{"outcome":{"actor_labels":{"actor":"Ada"},"value":{"more":true}}}"#
        );
        let last = StreamEnd::of_outcome(r#"{"value":{"next_cursor":null}}"#);
        assert_eq!(
            last.line(&[]),
            r#"{"outcome":{"actor_labels":{},"value":{"more":false}}}"#
        );
        let refused = StreamEnd::of_outcome(r#"{"error":{"code":"timeout","detail":{}}}"#);
        assert_eq!(
            refused.line(&[]),
            r#"{"outcome":{"error":{"code":"timeout","detail":{}}}}"#
        );
        assert_eq!(
            StreamEnd::of_outcome("not json").line(&[]),
            r#"{"outcome":{"uncertain":{}}}"#
        );
        assert_eq!(row_line(r#"{"id":"a"}"#), r#"{"row":{"id":"a"}}"#);
    }

    #[test]
    fn a_limit_above_the_maximum_refuses_before_the_query_runs() {
        let refusal = limit_refusal(&serde_json::json!([{ "limit": 100_001 }]), STREAM_CEILING)
            .expect("100,001 is above the ceiling");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&refusal).unwrap()[0]["error"]["detail"]["observed"],
            "100001"
        );
        assert!(
            limit_refusal(&serde_json::json!([{ "limit": 100_000 }]), STREAM_CEILING).is_none()
        );
        assert!(limit_refusal(&serde_json::json!([{}]), STREAM_CEILING).is_none());
    }
}

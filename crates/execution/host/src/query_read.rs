//! The two shapes of a query reply (`docs/architecture/execution.md`).
//!
//! A query yields its rows as a stream, then one outcome: the cursor that
//! continues the read, or the error that ended it. The host shapes that one
//! call into a page, the whole list and its cursor in one outcome, or into a
//! streamed load. Each shape has its own limit maximum, and the host refuses a
//! larger limit before the query runs.

/// The largest limit of a page, the contract's limit maximum of every query.
pub(crate) const PAGE_MAXIMUM: i64 = 100;

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
                    "minimum": 1,
                    "maximum": maximum,
                    "observed": limit,
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
    fn a_limit_above_the_maximum_refuses_before_the_query_runs() {
        let refusal = limit_refusal(&serde_json::json!([{ "limit": 101 }]), PAGE_MAXIMUM)
            .expect("101 is above a page's maximum");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&refusal).unwrap()[0]["error"]["detail"]["observed"],
            101
        );
        assert!(limit_refusal(&serde_json::json!([{ "limit": 100 }]), PAGE_MAXIMUM).is_none());
        assert!(limit_refusal(&serde_json::json!([{}]), PAGE_MAXIMUM).is_none());
    }
}

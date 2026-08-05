//! `time-shift` applies deterministic, checked arithmetic to an RFC3339 input.
//!
//! Config `base` names the input field, `offset-ms` is the signed millisecond
//! delta, and `key` selects the sole output field (default `cutoff`). `format`
//! may be omitted or set to `iso`. The component has no clock or capabilities:
//! every result derives only from its input and config.

use chrono::{DateTime, Datelike as _, SecondsFormat, TimeDelta, Utc};
use serde_json::{Value, json};
use wamn_node_sdk::{Emission, ErrorDetail, Node, NodeCtx, NodeError, RunContext};

#[derive(Default)]
struct TimeShift;

impl Node for TimeShift {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        let base = required_config_string(run.config, "base")?;
        let offset_ms = run
            .config
            .get("offset-ms")
            .and_then(Value::as_i64)
            .ok_or_else(|| invalid_config("time-shift config requires an integer \"offset-ms\""))?;
        if let Some(format) = run.config.get("format")
            && format.as_str() != Some("iso")
        {
            return Err(invalid_config(
                "time-shift config \"format\" must be \"iso\"",
            ));
        }
        let key = match run.config.get("key") {
            None => "cutoff",
            Some(Value::String(key)) if !key.is_empty() => key,
            Some(_) => {
                return Err(invalid_config(
                    "time-shift config \"key\" must be a non-empty string",
                ));
            }
        };

        let raw = input.get(base).and_then(Value::as_str).ok_or_else(|| {
            invalid_input(
                "invalid-base",
                format!("input field {base:?} must contain an RFC3339 string"),
            )
        })?;
        let parsed = DateTime::parse_from_rfc3339(raw).map_err(|error| {
            invalid_input(
                "invalid-rfc3339",
                format!("input field {base:?} is not RFC3339: {error}"),
            )
        })?;
        let offset = TimeDelta::try_milliseconds(offset_ms).ok_or_else(|| {
            time_range_error(format!(
                "offset {offset_ms}ms is outside the supported range"
            ))
        })?;
        let shifted = parsed
            .checked_add_signed(offset)
            .ok_or_else(|| time_range_error("RFC3339 time shift overflowed or underflowed"))?
            .with_timezone(&Utc);

        // RFC3339 `date-fullyear` is exactly four digits. Chrono represents a
        // wider proleptic range, so enforce the wire contract before formatting.
        if !(0..=9999).contains(&shifted.year()) {
            return Err(time_range_error(
                "RFC3339 time shift overflowed or underflowed its four-digit year range",
            ));
        }

        let output = json!({ key: canonical_rfc3339(shifted) });
        Ok(Emission::main(output))
    }
}

fn required_config_string<'a>(config: &'a Value, key: &str) -> Result<&'a str, NodeError> {
    config
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            invalid_config(format!(
                "time-shift config requires a non-empty string {key:?}"
            ))
        })
}

fn canonical_rfc3339(value: DateTime<Utc>) -> String {
    let rendered = value.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let Some(body) = rendered.strip_suffix('Z') else {
        return rendered;
    };
    let Some(decimal) = body.rfind('.') else {
        return rendered;
    };
    let fractional = &body[decimal + 1..];
    if !fractional.bytes().all(|byte| byte.is_ascii_digit()) {
        return rendered;
    }
    let trimmed = body.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed}Z")
}

fn invalid_config(message: impl Into<String>) -> NodeError {
    NodeError::Terminal(ErrorDetail::coded("invalid-config", message))
}

fn invalid_input(code: &str, message: impl Into<String>) -> NodeError {
    NodeError::InvalidInput(ErrorDetail::coded(code, message))
}

fn time_range_error(message: impl Into<String>) -> NodeError {
    NodeError::Terminal(ErrorDetail::coded("time-range", message))
}

wamn_node_guest::export_node!(TimeShift);

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_node_guest::NoCapsCtx;

    use super::*;

    fn run(config: &Value, input: &Value) -> Result<Emission, NodeError> {
        let context = json!({});
        let run = RunContext {
            run_id: "run",
            flow_id: "flow",
            flow_version: 1,
            node_id: "shift",
            connection: None,
            attempt: 0,
            idempotency_key: "run:shift:0",
            deadline_ms: None,
            traceparent: None,
            tracestate: None,
            config,
            context: &context,
        };
        TimeShift.run(&mut NoCapsCtx, &run, input)
    }

    fn error_code(error: &NodeError) -> Option<&str> {
        match error {
            NodeError::Retryable(detail)
            | NodeError::Terminal(detail)
            | NodeError::InvalidInput(detail) => detail.code.as_deref(),
            NodeError::RateLimited(detail) => detail.detail.code.as_deref(),
            NodeError::Cancelled => None,
        }
    }

    #[test]
    fn f3_scheduled_at_shifts_back_48_hours() {
        let emission = run(
            &json!({"base": "scheduled-at", "offset-ms": -172_800_000, "format": "iso"}),
            &json!({
                "scheduled-at": "2023-11-14T22:13:20Z",
                "fired-at": "2023-11-14T22:13:23Z"
            }),
        )
        .expect("normative cron input shifts");

        assert_eq!(
            emission,
            Emission::main(json!({"cutoff": "2023-11-12T22:13:20Z"}))
        );
    }

    /// MUTANT WITNESS: keeping the input's numeric offset instead of converting
    /// to UTC changes this exact output.
    #[test]
    fn timezone_offset_is_normalized_to_canonical_utc() {
        let emission = run(
            &json!({"base": "scheduled-at", "offset-ms": 250, "key": "shifted"}),
            &json!({"scheduled-at": "2024-03-10T01:59:59.875-05:00"}),
        )
        .expect("offset timestamp shifts");

        assert_eq!(
            emission.payload,
            json!({"shifted": "2024-03-10T07:00:00.125Z"})
        );
    }

    /// MUTANT WITNESS: unchecked or saturating addition would emit a boundary
    /// value; both range crossings must fail without producing an emission.
    #[test]
    fn rfc3339_year_boundaries_reject_overflow_and_underflow() {
        let overflow = run(
            &json!({"base": "at", "offset-ms": 1}),
            &json!({"at": "9999-12-31T23:59:59.999Z"}),
        )
        .expect_err("four-digit year overflow must fail");
        let underflow = run(
            &json!({"base": "at", "offset-ms": -1}),
            &json!({"at": "0000-01-01T00:00:00Z"}),
        )
        .expect_err("four-digit year underflow must fail");

        assert_eq!(error_code(&overflow), Some("time-range"));
        assert_eq!(error_code(&underflow), Some("time-range"));
    }

    #[test]
    fn signed_offset_extremes_are_checked() {
        for offset_ms in [i64::MIN, i64::MAX] {
            let error = run(
                &json!({"base": "at", "offset-ms": offset_ms}),
                &json!({"at": "1970-01-01T00:00:00Z"}),
            )
            .expect_err("extreme signed offset must fail");
            assert_eq!(error_code(&error), Some("time-range"));
        }
    }

    #[test]
    fn malformed_timestamp_and_config_are_rejected() {
        let malformed = run(
            &json!({"base": "scheduled-at", "offset-ms": 0}),
            &json!({"scheduled-at": "2024-02-30T00:00:00Z"}),
        )
        .expect_err("malformed date must fail");
        let missing = run(
            &json!({"base": "scheduled-at", "offset-ms": 0}),
            &json!({"fired-at": "2024-02-29T00:00:00Z"}),
        )
        .expect_err("missing base must fail");
        let bad_offset = run(
            &json!({"base": "scheduled-at", "offset-ms": 1.5}),
            &json!({"scheduled-at": "2024-02-29T00:00:00Z"}),
        )
        .expect_err("fractional offset must fail");

        assert_eq!(error_code(&malformed), Some("invalid-rfc3339"));
        assert_eq!(error_code(&missing), Some("invalid-base"));
        assert_eq!(error_code(&bad_offset), Some("invalid-config"));
    }

    #[test]
    fn canonical_output_preserves_only_significant_fractional_digits() {
        let emission = run(
            &json!({"base": "at", "offset-ms": 0}),
            &json!({"at": "2024-01-01T00:00:00.123400000Z"}),
        )
        .expect("fractional timestamp shifts");

        assert_eq!(
            emission.payload,
            json!({"cutoff": "2024-01-01T00:00:00.1234Z"})
        );
    }
}

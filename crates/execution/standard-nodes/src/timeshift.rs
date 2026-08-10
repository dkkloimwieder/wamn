//! `time-shift` — deterministic time arithmetic over an RFC 3339 input.
//!
//! The JMESPath expression surface deliberately has NO arithmetic and NO clock
//! (`expr.rs`), so a flow cannot compute "48h before the cron tick" on its own.
//! This node fills exactly that gap and nothing more: it selects an RFC 3339
//! string already present in its input (e.g. the cron trigger's `scheduled-at`),
//! adds a SIGNED millisecond offset, and emits the shifted instant.
//!
//! PURE (no capabilities): the value derives from the TICK the runner already
//! handed the run, which is deterministic and virtual-time-friendly — under the
//! gate's virtual clock a 48h offset maps to wall-clock seconds by construction
//! (`docs/archive/poc/poc-material-receiving.md` :39). No `SystemClock`: parsing and
//! arithmetic are pure functions of the admitted trigger payload.
//!
//! Config:
//! ```jsonc
//! {
//!   "base": "\"scheduled-at\"", // JMESPath selecting an RFC 3339 string
//!   "offset-ms": -172800000,// signed millisecond offset to add (required;
//!                           // -48h here)
//!   "format": "iso",        // "iso" (RFC 3339, default) | "epoch-ms"
//!   "key": "cutoff"         // output object key (default "cutoff")
//! }
//! ```
//! Emission: `{ <key>: <shifted> }` — `<shifted>` is an RFC 3339 UTC string
//! (`format: "iso"`) or an epoch-ms integer (`format: "epoch-ms"`). Downstream
//! `{{<key>}}` templating (e.g. a `postgres` list filter `opened_at=lt.{{cutoff}}`)
//! consumes it.

use chrono::{DateTime, Datelike as _, SecondsFormat, TimeDelta, Utc};
use serde_json::{Value, json};
use wamn_node_sdk::{Emission, ErrorDetail, Node, NodeCtx, NodeError, RunContext};

use crate::expr::{config_str, eval_to_value};

pub(crate) struct TimeShift;

impl Node for TimeShift {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        let config = run.config;
        let base_expr = config_str(config, "base")?;
        let offset_ms = config
            .get("offset-ms")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                NodeError::Terminal(ErrorDetail::coded(
                    "invalid-config",
                    "time-shift config requires an integer \"offset-ms\"",
                ))
            })?;
        let key = config
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("cutoff");
        let format = config
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("iso");

        // The base is a JMESPath over runtime INPUT: a missing/non-RFC3339
        // value is the input's fault (invalid-input, never retried), matching
        // the postgres node's id/body faults — not a flow-config bug.
        let base = match eval_to_value(base_expr, input, run.context)? {
            Value::String(raw) => DateTime::parse_from_rfc3339(&raw).map_err(|error| {
                NodeError::InvalidInput(ErrorDetail::coded(
                    "invalid-base",
                    format!("base {base_expr:?} is not RFC 3339: {error}"),
                ))
            })?,
            other => {
                return Err(NodeError::InvalidInput(ErrorDetail::coded(
                    "invalid-base",
                    format!("base {base_expr:?} must resolve to an RFC 3339 string, got {other}"),
                )));
            }
        };

        let offset = TimeDelta::try_milliseconds(offset_ms).ok_or_else(|| {
            NodeError::Terminal(ErrorDetail::coded(
                "time-overflow",
                format!("offset {offset_ms}ms is outside the supported range"),
            ))
        })?;
        let shifted = base.checked_add_signed(offset).ok_or_else(|| {
            NodeError::Terminal(ErrorDetail::coded(
                "time-overflow",
                format!("RFC 3339 base + offset {offset_ms}ms is outside the supported range"),
            ))
        })?;
        if !(0..=9999).contains(&shifted.year()) {
            return Err(NodeError::Terminal(ErrorDetail::coded(
                "time-overflow",
                "RFC 3339 time shift exceeded its four-digit year range",
            )));
        }

        let value = match format {
            "epoch-ms" => Value::Number(shifted.timestamp_millis().into()),
            "iso" => Value::String(
                shifted
                    .with_timezone(&Utc)
                    .to_rfc3339_opts(SecondsFormat::Millis, true),
            ),
            other => {
                return Err(NodeError::Terminal(ErrorDetail::coded(
                    "invalid-config",
                    format!("time-shift \"format\" must be \"iso\" or \"epoch-ms\", got {other:?}"),
                )));
            }
        };

        Ok(Emission::main(json!({ key: value })))
    }
}

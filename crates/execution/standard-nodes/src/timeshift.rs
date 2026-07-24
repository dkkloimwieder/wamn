//! `time-shift` — deterministic time arithmetic over an epoch-ms input.
//!
//! The JMESPath expression surface deliberately has NO arithmetic and NO clock
//! (`expr.rs`), so a flow cannot compute "48h before the cron tick" on its own.
//! This node fills exactly that gap and nothing more: it selects an epoch-ms
//! number already present in its input (e.g. the cron trigger's `fire-at-ms`),
//! adds a SIGNED millisecond offset, and emits the shifted instant — as an
//! RFC 3339 string a Postgres `timestamptz` filter can compare against, or back
//! as epoch-ms.
//!
//! PURE (no capabilities): the value derives from the TICK the runner already
//! handed the run, which is deterministic and virtual-time-friendly — under the
//! gate's virtual clock a 48h offset maps to wall-clock seconds by construction
//! (`docs/poc-material-receiving.md` :39). No `SystemClock`, no chrono: the
//! epoch-ms → civil-date conversion is the closed-form proleptic-Gregorian
//! algorithm below, keeping this node linkable into `flowrunner.wasm` under the
//! no-chrono guest posture.
//!
//! Config:
//! ```jsonc
//! {
//!   "base": "fire-at-ms",   // JMESPath into the input selecting an epoch-ms
//!                           // integer (required)
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

        // The base is a JMESPath over runtime INPUT: a missing/non-integer value
        // is the input's fault (invalid-input, never retried), matching the
        // postgres node's id/body faults — not a flow-authoring bug.
        let base_ms = match eval_to_value(base_expr, input)? {
            Value::Number(n) => n.as_i64().ok_or_else(|| {
                NodeError::InvalidInput(ErrorDetail::coded(
                    "invalid-base",
                    format!("base {base_expr:?} must be an integer epoch-ms, got {n}"),
                ))
            })?,
            other => {
                return Err(NodeError::InvalidInput(ErrorDetail::coded(
                    "invalid-base",
                    format!("base {base_expr:?} must resolve to an epoch-ms number, got {other}"),
                )));
            }
        };

        let shifted = base_ms.checked_add(offset_ms).ok_or_else(|| {
            NodeError::Terminal(ErrorDetail::coded(
                "time-overflow",
                format!("epoch-ms {base_ms} + offset {offset_ms} overflows i64"),
            ))
        })?;

        let value = match format {
            "epoch-ms" => Value::Number(shifted.into()),
            "iso" => Value::String(epoch_ms_to_rfc3339(shifted)),
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

/// Milliseconds since the Unix epoch → an RFC 3339 UTC instant with millisecond
/// precision (`YYYY-MM-DDThh:mm:ss.sssZ`) — the format a Postgres `timestamptz`
/// filter parses and compares. Pure and total: `div_euclid`/`rem_euclid` handle
/// pre-epoch instants, and the civil-date step is the closed-form proleptic
/// Gregorian conversion (correct for every representable `i64` ms).
fn epoch_ms_to_rfc3339(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let ms_of_day = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let hh = ms_of_day / 3_600_000;
    let mm = (ms_of_day / 60_000) % 60;
    let ss = (ms_of_day / 1_000) % 60;
    let milli = ms_of_day % 1_000;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{milli:03}Z")
}

/// Days since 1970-01-01 → `(year, month, day)` in the proleptic Gregorian
/// calendar. Howard Hinnant's `civil_from_days` (the algorithm the `chrono`/
/// `time` crates use), reproduced so this node stays dependency-free.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (y + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The civil-date conversion pinned against known instants — the epoch, a
    /// leap day, and a 13-digit millisecond value with sub-second precision.
    #[test]
    fn epoch_ms_to_rfc3339_is_correct() {
        assert_eq!(epoch_ms_to_rfc3339(0), "1970-01-01T00:00:00.000Z");
        // unix 1_700_000_000 s = 2023-11-14T22:13:20Z.
        assert_eq!(
            epoch_ms_to_rfc3339(1_700_000_000_000),
            "2023-11-14T22:13:20.000Z"
        );
        // A leap day + milliseconds survive.
        assert_eq!(
            epoch_ms_to_rfc3339(1_582_934_400_123),
            "2020-02-29T00:00:00.123Z"
        );
    }
}

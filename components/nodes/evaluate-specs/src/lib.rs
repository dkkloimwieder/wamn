//! Pure F1 specification evaluation behind the zero-import custom-node ABI.

use std::cmp::Ordering;

use serde_json::{Value, json};
use wamn_node_sdk::{Emission, ErrorDetail, Node, NodeCtx, NodeError, RunContext};

#[cfg(test)]
mod contract;

#[derive(Default)]
struct EvaluateSpecs;

impl Node for EvaluateSpecs {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        _run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        evaluate(input).map(Emission::main).map_err(|message| {
            NodeError::InvalidInput(ErrorDetail::coded("invalid-input", message))
        })
    }
}

wamn_node_guest::export_node!(EvaluateSpecs);

fn evaluate(input: &Value) -> Result<Value, String> {
    let receipt = object_field(input, "receipt")?;
    let lines = receipt
        .get("lines")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| "receipt.lines must be an array".to_string())?;
    let specs = array_field(input, "line_specs")?;
    let line_ids = array_field(input, "line_ids")?;
    if lines.len() != specs.len() || lines.len() != line_ids.len() {
        return Err(format!(
            "receipt.lines, line_specs, and line_ids must have equal lengths; got {}, {}, {}",
            lines.len(),
            specs.len(),
            line_ids.len()
        ));
    }

    let receipt_id = string_field(input, "receipt_id")?;
    let site_id = string_field(input, "site_id")?;
    let mut out_of_spec = Vec::new();
    for (index, ((line, spec), line_id)) in lines.iter().zip(specs).zip(line_ids).enumerate() {
        let line = line
            .as_object()
            .ok_or_else(|| format!("receipt.lines[{index}] must be an object"))?;
        let spec = spec
            .as_object()
            .ok_or_else(|| format!("line_specs[{index}] must be an object"))?;
        let line_id = line_id
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("line_ids[{index}] must be a non-empty string"))?;

        let quantity = decimal_string(
            line.get("quantity"),
            &format!("receipt.lines[{index}].quantity"),
        )?;
        let moisture = decimal_string(
            line.get("moisture_pct"),
            &format!("receipt.lines[{index}].moisture_pct"),
        )?;
        let weight = decimal_string(
            line.get("weight_kg"),
            &format!("receipt.lines[{index}].weight_kg"),
        )?;
        let material = line
            .get("material")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("receipt.lines[{index}].material must be a non-empty string"))?;
        let moisture_max = decimal_string(
            spec.get("moisture_max_pct"),
            &format!("line_specs[{index}].moisture_max_pct"),
        )?;
        let tolerance = decimal_string(
            spec.get("weight_tolerance_kg"),
            &format!("line_specs[{index}].weight_tolerance_kg"),
        )?;

        let mut reasons = Vec::new();
        if moisture.0.cmp_value(moisture_max.0) == Ordering::Greater {
            reasons.push(format!(
                "moisture {} pct exceeds max {} pct",
                moisture.1, moisture_max.1
            ));
        }
        let deviation = weight.0.abs_diff(quantity.0);
        if deviation.cmp_value(tolerance.0) == Ordering::Greater {
            reasons.push(format!(
                "weight {} kg deviates {} kg from declared {} kg (tolerance {} kg)",
                weight.1, deviation, quantity.1, tolerance.1
            ));
        }
        if !reasons.is_empty() {
            out_of_spec.push(json!({
                "line": index + 1,
                "line_id": line_id,
                "material": material,
                "reason": reasons.join("; "),
            }));
        }
    }

    Ok(json!({
        "receipt_id": receipt_id,
        "site_id": site_id,
        "out_of_spec": out_of_spec,
    }))
}

fn object_field<'a>(
    value: &'a Value,
    field: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{field} must be an object"))
}

fn array_field<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{field} must be an array"))
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} must be a non-empty string"))
}

fn decimal_string<'a>(value: Option<&'a Value>, path: &str) -> Result<(Decimal, &'a str), String> {
    let raw = value.and_then(Value::as_str).ok_or_else(|| {
        format!("{path} must be an exact-decimal string; JSON numbers are rejected")
    })?;
    Decimal::parse(raw)
        .map(|decimal| (decimal, raw))
        .map_err(|message| format!("{path}: {message}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Decimal {
    units: i128,
    scale: u32,
}

impl Decimal {
    fn parse(value: &str) -> Result<Self, String> {
        let (negative, digits) = value
            .strip_prefix('-')
            .map_or((false, value), |rest| (true, rest));
        let (integer, fraction) = digits.split_once('.').unwrap_or((digits, ""));
        if integer.is_empty()
            || (digits.contains('.') && fraction.is_empty())
            || !integer.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(format!("not a decimal: {value:?}"));
        }
        if integer.len() + fraction.len() > 27 || fraction.len() > 9 {
            return Err(format!("decimal exceeds supported precision: {value:?}"));
        }
        let mut units = 0_i128;
        for byte in integer.bytes().chain(fraction.bytes()) {
            units = units * 10 + i128::from(byte - b'0');
        }
        if negative {
            units = -units;
        }
        Ok(Self {
            units,
            scale: fraction.len() as u32,
        })
    }

    fn aligned(self, other: Self) -> (i128, i128, u32) {
        let scale = self.scale.max(other.scale);
        (
            self.units * 10_i128.pow(scale - self.scale),
            other.units * 10_i128.pow(scale - other.scale),
            scale,
        )
    }

    fn cmp_value(self, other: Self) -> Ordering {
        let (left, right, _) = self.aligned(other);
        left.cmp(&right)
    }

    fn abs_diff(self, other: Self) -> Self {
        let (left, right, scale) = self.aligned(other);
        Self {
            units: (left - right).abs(),
            scale,
        }
    }
}

impl std::fmt::Display for Decimal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sign = if self.units < 0 { "-" } else { "" };
        let magnitude = self.units.unsigned_abs();
        if self.scale == 0 {
            return write!(formatter, "{sign}{magnitude}");
        }
        let factor = 10_u128.pow(self.scale);
        write!(
            formatter,
            "{sign}{}.{:0width$}",
            magnitude / factor,
            magnitude % factor,
            width = self.scale as usize
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wamn_node_guest::{NoCapsCtx, run_node, wit};

    use super::EvaluateSpecs;

    fn input() -> Value {
        json!({
            "receipt": {
                "receipt_no": "r-1001",
                "supplier": "acme",
                "site": "hq",
                "received_at": "2026-07-12T08:00:00Z",
                "lines": [{
                    "material": "resin-a",
                    "quantity": "100.000",
                    "moisture_pct": "12.50",
                    "weight_kg": "99.950"
                }]
            },
            "site_id": "22222222-2222-2222-2222-222222222222",
            "line_specs": [{
                "material_id": "33333333-3333-3333-3333-333333333333",
                "moisture_max_pct": "12.5",
                "weight_tolerance_kg": "0.050"
            }],
            "receipt_id": "44444444-4444-4444-4444-444444444444",
            "line_ids": ["55555555-5555-5555-5555-555555555555"]
        })
    }

    fn run(input: Value) -> Result<wit::Emission, wit::NodeError> {
        run_node(
            &EvaluateSpecs,
            &mut NoCapsCtx,
            &wit::RunContext {
                run_id: "run".into(),
                flow_id: "receipt-received".into(),
                flow_version: 1,
                node_id: "evaluate-specs".into(),
                attempt: 0,
                idempotency_key: "run:evaluate-specs:0".into(),
                traceparent: None,
                tracestate: None,
                deadline_ms: None,
                config: "{}".into(),
                context: "{}".into(),
            },
            &wit::Payload::Inline(input.to_string()),
        )
    }

    fn payload(output: wit::Emission) -> Value {
        assert_eq!(output.port, None, "only the declared main port is emitted");
        let wit::Payload::Inline(payload) = output.payload else {
            panic!("inline payload expected");
        };
        serde_json::from_str(&payload).unwrap()
    }

    #[test]
    fn golden_decimal_boundary_equality_is_in_spec() {
        let output = payload(run(input()).expect("boundary input"));
        assert_eq!(output["out_of_spec"], json!([]));
    }

    #[test]
    fn golden_decimal_strict_exceedance_is_out_of_spec() {
        let mut value = input();
        value["receipt"]["lines"][0]["moisture_pct"] = json!("12.51");
        value["receipt"]["lines"][0]["weight_kg"] = json!("99.949");
        let output = payload(run(value).expect("strict exceedance"));
        assert_eq!(output["out_of_spec"][0]["line"], 1);
        let reason = output["out_of_spec"][0]["reason"].as_str().unwrap();
        assert!(reason.contains("moisture 12.51 pct exceeds max 12.5 pct"));
        assert!(reason.contains("deviates 0.051 kg"));
        assert!(reason.contains("tolerance 0.050 kg"));
    }

    #[test]
    fn float_backed_payload_or_spec_decimal_is_invalid_input() {
        for pointer in [
            "/receipt/lines/0/moisture_pct",
            "/line_specs/0/weight_tolerance_kg",
        ] {
            let mut value = input();
            *value.pointer_mut(pointer).unwrap() = json!(12.5);
            assert!(matches!(run(value), Err(wit::NodeError::InvalidInput(_))));
        }
    }

    #[test]
    fn component_interface_drift_is_caught_by_main_only_emissions() {
        let mut value = input();
        value["receipt"]["lines"][0]["moisture_pct"] = json!("13.00");
        let output = run(value).expect("out-of-spec is data, not a branch port");
        assert_eq!(output.port, None);
    }
}

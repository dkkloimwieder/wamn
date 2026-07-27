//! Pure F1 receipt normalization behind the zero-import custom-node ABI.

use serde_json::{Map, Value, json};
use wamn_node_sdk::{Emission, ErrorDetail, Node, NodeCtx, NodeError, RunContext};

#[cfg(test)]
mod contract;

const MAX_LINES: usize = 100;

#[derive(Default)]
struct NormalizeReceipt;

impl Node for NormalizeReceipt {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        _run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        normalize(input).map(Emission::main).map_err(|issues| {
            NodeError::InvalidInput(ErrorDetail {
                message: "receipt input is invalid".to_string(),
                code: Some("invalid-input".to_string()),
                data: Some(json!({ "issues": issues })),
            })
        })
    }
}

wamn_node_guest::export_node!(NormalizeReceipt);

fn normalize(input: &Value) -> Result<Value, Vec<Value>> {
    let Some(receipt) = input.as_object() else {
        return Err(vec![issue("$", "payload must be a JSON object")]);
    };
    let mut issues = Vec::new();
    reject_unknown(
        receipt,
        "$",
        &["receipt_no", "supplier", "site", "received_at", "lines"],
        &mut issues,
    );

    let receipt_no = required_string(receipt, "receipt_no", "$", &mut issues);
    if receipt_no.len() > 64 {
        issues.push(issue("$.receipt_no", "longer than 64 characters"));
    }
    let supplier = required_string(receipt, "supplier", "$", &mut issues);
    let site = required_string(receipt, "site", "$", &mut issues);
    let received_at = required_string(receipt, "received_at", "$", &mut issues);
    if !received_at.is_empty() && !is_rfc3339_lite(&received_at) {
        issues.push(issue(
            "$.received_at",
            "must be an RFC 3339 instant, e.g. 2026-07-12T08:00:00Z",
        ));
    }

    let mut lines = Vec::new();
    match receipt.get("lines").and_then(Value::as_array) {
        None => issues.push(issue("$.lines", "required: a non-empty array")),
        Some(values) if values.is_empty() => issues.push(issue("$.lines", "must not be empty")),
        Some(values) if values.len() > MAX_LINES => {
            issues.push(issue("$.lines", "more than 100 lines"));
        }
        Some(values) => {
            for (index, line) in values.iter().enumerate() {
                if let Some(line) = normalize_line(line, index, &mut issues) {
                    lines.push(line);
                }
            }
        }
    }

    if issues.is_empty() {
        Ok(json!({
            "receipt_no": receipt_no,
            "supplier": supplier,
            "site": site,
            "received_at": received_at,
            "lines": lines,
        }))
    } else {
        Err(issues)
    }
}

fn normalize_line(value: &Value, index: usize, issues: &mut Vec<Value>) -> Option<Value> {
    let path = format!("$.lines[{index}]");
    let Some(line) = value.as_object() else {
        issues.push(issue(&path, "must be a JSON object"));
        return None;
    };
    reject_unknown(
        line,
        &path,
        &["material", "quantity", "moisture_pct", "weight_kg"],
        issues,
    );
    let before = issues.len();
    let material = required_string(line, "material", &path, issues);
    let quantity = decimal_field(line, "quantity", &path, 12, 3, true, issues);
    let moisture_pct = decimal_field(line, "moisture_pct", &path, 5, 2, false, issues);
    let weight_kg = decimal_field(line, "weight_kg", &path, 12, 3, true, issues);
    (issues.len() == before).then(|| {
        json!({
            "material": material,
            "quantity": quantity,
            "moisture_pct": moisture_pct,
            "weight_kg": weight_kg,
        })
    })
}

fn reject_unknown(
    object: &Map<String, Value>,
    parent: &str,
    allowed: &[&str],
    issues: &mut Vec<Value>,
) {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            issues.push(issue(&format!("{parent}.{key}"), "unknown key"));
        }
    }
}

fn required_string(
    object: &Map<String, Value>,
    key: &str,
    parent: &str,
    issues: &mut Vec<Value>,
) -> String {
    let path = format!("{parent}.{key}");
    match object.get(key) {
        Some(Value::String(value)) if !value.is_empty() => value.clone(),
        Some(Value::String(_)) => {
            issues.push(issue(&path, "must not be empty"));
            String::new()
        }
        None | Some(Value::Null) => {
            issues.push(issue(&path, "required"));
            String::new()
        }
        Some(_) => {
            issues.push(issue(&path, "must be a string"));
            String::new()
        }
    }
}

fn decimal_field(
    object: &Map<String, Value>,
    key: &str,
    parent: &str,
    precision: u32,
    scale: u32,
    positive: bool,
    issues: &mut Vec<Value>,
) -> String {
    let path = format!("{parent}.{key}");
    let raw = match object.get(key) {
        Some(Value::String(value)) => value.clone(),
        None | Some(Value::Null) => {
            issues.push(issue(&path, "required"));
            return String::new();
        }
        Some(Value::Number(_)) => {
            issues.push(issue(
                &path,
                "must be an exact-decimal string (JSON numbers are not accepted)",
            ));
            return String::new();
        }
        Some(_) => {
            issues.push(issue(&path, "must be an exact-decimal string"));
            return String::new();
        }
    };

    match Decimal::parse(&raw) {
        Err(message) => issues.push(issue(&path, &message)),
        Ok(decimal) if !decimal.fits(precision, scale) => issues.push(issue(
            &path,
            &format!("out of range for numeric({precision},{scale})"),
        )),
        Ok(decimal) if positive && decimal.units <= 0 => {
            issues.push(issue(&path, "must be positive"));
        }
        Ok(decimal) if !positive && decimal.units < 0 => {
            issues.push(issue(&path, "must not be negative"));
        }
        Ok(_) => {}
    }
    raw
}

fn issue(path: &str, message: &str) -> Value {
    json!({ "path": path, "message": message })
}

#[derive(Clone, Copy)]
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

    fn fits(self, precision: u32, scale: u32) -> bool {
        if self.scale > scale {
            return false;
        }
        let mut integer = self.units.unsigned_abs() / 10_u128.pow(self.scale);
        let mut integer_digits = 0;
        while integer > 0 {
            integer_digits += 1;
            integer /= 10;
        }
        integer_digits <= precision - scale
    }
}

fn is_rfc3339_lite(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !(digits(0..4)
        && bytes[4] == b'-'
        && digits(5..7)
        && bytes[7] == b'-'
        && digits(8..10)
        && matches!(bytes[10], b'T' | b't')
        && digits(11..13)
        && bytes[13] == b':'
        && digits(14..16)
        && bytes[16] == b':'
        && digits(17..19))
    {
        return false;
    }
    let mut index = 19;
    if bytes[index] == b'.' {
        let start = index + 1;
        index = start;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == start {
            return false;
        }
    }
    match bytes.get(index) {
        Some(b'Z') | Some(b'z') => index + 1 == bytes.len(),
        Some(b'+') | Some(b'-') => {
            bytes.len() == index + 6
                && digits(index + 1..index + 3)
                && bytes[index + 3] == b':'
                && digits(index + 4..index + 6)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wamn_node_guest::{NoCapsCtx, run_node, wit};

    use super::NormalizeReceipt;

    fn run(input: Value) -> Result<wit::Emission, wit::NodeError> {
        run_node(
            &NormalizeReceipt,
            &mut NoCapsCtx,
            &wit::RunContext {
                run_id: "run".into(),
                flow_id: "receipt-received".into(),
                flow_version: 1,
                node_id: "normalize-receipt".into(),
                attempt: 0,
                idempotency_key: "run:normalize-receipt:0".into(),
                traceparent: None,
                tracestate: None,
                deadline_ms: None,
                config: "{}".into(),
                context: "{}".into(),
            },
            &wit::Payload::Inline(input.to_string()),
        )
    }

    fn valid() -> Value {
        json!({
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
        })
    }

    #[test]
    fn valid_receipt_is_normalized_on_main_with_decimal_strings_unchanged() {
        let output = run(valid()).expect("valid receipt");
        assert_eq!(output.port, None);
        let wit::Payload::Inline(payload) = output.payload else {
            panic!("inline payload expected");
        };
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["lines"][0]["quantity"], "100.000");
        assert_eq!(payload["lines"][0]["moisture_pct"], "12.50");
        assert_eq!(payload["lines"][0]["weight_kg"], "99.950");
    }

    #[test]
    fn float_backed_decimal_is_invalid_input() {
        let mut input = valid();
        input["lines"][0]["moisture_pct"] = json!(12.5);
        match run(input) {
            Err(wit::NodeError::InvalidInput(detail)) => {
                assert_eq!(detail.code.as_deref(), Some("invalid-input"));
                assert!(
                    detail
                        .data
                        .as_deref()
                        .unwrap_or("")
                        .contains("JSON numbers")
                );
            }
            other => panic!("expected invalid-input, got {other:?}"),
        }
    }

    #[test]
    fn golden_decimal_precision_and_sign_rules_are_exact() {
        let mut input = valid();
        input["lines"][0]["quantity"] = json!("999999999.999");
        assert!(run(input).is_ok(), "numeric(12,3) maximum must fit");

        let mut excess = valid();
        excess["lines"][0]["quantity"] = json!("1000000000.000");
        assert!(matches!(run(excess), Err(wit::NodeError::InvalidInput(_))));

        let mut negative_zero = valid();
        negative_zero["lines"][0]["quantity"] = json!("-0.000");
        assert!(matches!(
            run(negative_zero),
            Err(wit::NodeError::InvalidInput(_))
        ));
    }
}

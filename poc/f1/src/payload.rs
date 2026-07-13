//! Receipt payload validation — the `validate-receipt` node's pure half. The
//! POSTed body must be exactly the documented contract (unknown keys rejected,
//! decimals as exact-decimal STRINGS or JSON integers — a JSON float is
//! refused, the no-float rule end-to-end), and every violation is collected so
//! the 400 response reports them all at once.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::decimal::Decimal;

/// `receipts.receipt_no` is `text` with `max-len: 64` in the catalog.
const RECEIPT_NO_MAX: usize = 64;
/// Guardrail on payload size; the POC's receipts carry a handful of lines.
const MAX_LINES: usize = 100;

/// One payload validation failure: where and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub path: String,
    pub message: String,
}

/// A validated receipt, fields verbatim from the payload (values are kept as
/// their original strings — no canonicalization, mirroring the dispatcher's
/// payload-spliced-verbatim rule).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub receipt_no: String,
    /// Supplier business key: `suppliers.name`.
    pub supplier: String,
    /// Site business key: `sites.code`.
    pub site: String,
    /// RFC 3339 instant the goods arrived (`receipts.received_at`).
    pub received_at: String,
    pub lines: Vec<Line>,
}

/// One receipt line: the declared quantity plus the measured values the specs
/// are evaluated against. `quantity` is persisted (`receipt_lines.quantity`);
/// the measured `moisture_pct` / `weight_kg` live in the run trace and the
/// sync response only (the catalog has no columns for them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// Material business key: `materials.name`.
    pub material: String,
    /// Declared quantity, kg — `numeric(12,3)`, positive.
    pub quantity: String,
    /// Measured moisture, pct — `numeric(5,2)`, non-negative.
    pub moisture_pct: String,
    /// Measured weight, kg — `numeric(12,3)`, positive.
    pub weight_kg: String,
}

/// Validate a POSTed receipt payload. Returns every violation found, not just
/// the first (`Err` is non-empty).
pub fn parse_receipt(v: &Value) -> Result<Receipt, Vec<Issue>> {
    let mut issues = Vec::new();
    let Some(obj) = v.as_object() else {
        return Err(vec![issue("$", "payload must be a JSON object")]);
    };

    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "receipt_no" | "supplier" | "site" | "received_at" | "lines"
        ) {
            issues.push(issue(&format!("$.{key}"), "unknown key"));
        }
    }

    let receipt_no = req_string(obj, "receipt_no", &mut issues);
    if receipt_no.len() > RECEIPT_NO_MAX {
        issues.push(issue(
            "$.receipt_no",
            &format!("longer than {RECEIPT_NO_MAX} characters"),
        ));
    }
    let supplier = req_string(obj, "supplier", &mut issues);
    let site = req_string(obj, "site", &mut issues);
    let received_at = req_string(obj, "received_at", &mut issues);
    if !received_at.is_empty() && !is_rfc3339_lite(&received_at) {
        issues.push(issue(
            "$.received_at",
            "must be an RFC 3339 instant, e.g. 2026-07-12T08:00:00Z",
        ));
    }

    let mut lines = Vec::new();
    match obj.get("lines").and_then(Value::as_array) {
        None => issues.push(issue("$.lines", "required: a non-empty array")),
        Some(arr) if arr.is_empty() => issues.push(issue("$.lines", "must not be empty")),
        Some(arr) if arr.len() > MAX_LINES => {
            issues.push(issue("$.lines", &format!("more than {MAX_LINES} lines")));
        }
        Some(arr) => {
            for (i, line) in arr.iter().enumerate() {
                if let Some(l) = parse_line(line, i, &mut issues) {
                    lines.push(l);
                }
            }
        }
    }

    if issues.is_empty() {
        Ok(Receipt {
            receipt_no,
            supplier,
            site,
            received_at,
            lines,
        })
    } else {
        Err(issues)
    }
}

fn parse_line(v: &Value, i: usize, issues: &mut Vec<Issue>) -> Option<Line> {
    let path = format!("$.lines[{i}]");
    let Some(obj) = v.as_object() else {
        issues.push(issue(&path, "must be a JSON object"));
        return None;
    };
    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "material" | "quantity" | "moisture_pct" | "weight_kg"
        ) {
            issues.push(issue(&format!("{path}.{key}"), "unknown key"));
        }
    }
    let before = issues.len();
    let material = req_string_at(obj, "material", &path, issues);
    // quantity/weight_kg are numeric(12,3) and must be positive; the measured
    // moisture is numeric(5,2) and may be zero (a perfectly dry lot).
    let quantity = decimal_field(obj, "quantity", &path, 12, 3, true, issues);
    let moisture_pct = decimal_field(obj, "moisture_pct", &path, 5, 2, false, issues);
    let weight_kg = decimal_field(obj, "weight_kg", &path, 12, 3, true, issues);
    (issues.len() == before).then_some(Line {
        material,
        quantity,
        moisture_pct,
        weight_kg,
    })
}

/// A decimal payload value: an exact-decimal STRING or a JSON integer. A JSON
/// float is rejected outright — floats are forbidden for material quantities.
fn decimal_field(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    parent: &str,
    precision: u32,
    scale: u32,
    positive: bool,
    issues: &mut Vec<Issue>,
) -> String {
    let path = format!("{parent}.{key}");
    let raw = match obj.get(key) {
        None | Some(Value::Null) => {
            issues.push(issue(&path, "required"));
            return String::new();
        }
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.to_string(),
        Some(Value::Number(_)) => {
            issues.push(issue(
                &path,
                "must be an exact-decimal string (JSON floats are not accepted)",
            ));
            return String::new();
        }
        Some(_) => {
            issues.push(issue(&path, "must be an exact-decimal string"));
            return String::new();
        }
    };
    match Decimal::parse(&raw) {
        Err(e) => issues.push(issue(&path, &e)),
        Ok(d) if !d.fits(precision, scale) => issues.push(issue(
            &path,
            &format!("out of range for numeric({precision},{scale})"),
        )),
        Ok(d) if positive && (d.is_negative() || d.is_zero()) => {
            issues.push(issue(&path, "must be positive"));
        }
        Ok(d) if !positive && d.is_negative() => {
            issues.push(issue(&path, "must not be negative"));
        }
        Ok(_) => {}
    }
    raw
}

fn req_string(obj: &serde_json::Map<String, Value>, key: &str, issues: &mut Vec<Issue>) -> String {
    req_string_at(obj, key, "$", issues)
}

fn req_string_at(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    parent: &str,
    issues: &mut Vec<Issue>,
) -> String {
    let path = if parent == "$" {
        format!("$.{key}")
    } else {
        format!("{parent}.{key}")
    };
    match obj.get(key) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
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

fn issue(path: &str, message: &str) -> Issue {
    Issue {
        path: path.to_string(),
        message: message.to_string(),
    }
}

/// Structural RFC 3339 check: `YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)`. Postgres
/// re-parses the value into `timestamptz` on insert; this gate exists so a
/// garbled timestamp is a 400 at validate, not a query error mid-flow.
fn is_rfc3339_lite(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 20 {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if !(digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && (b[10] == b'T' || b[10] == b't')
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19))
    {
        return false;
    }
    let mut i = 19;
    if b[i] == b'.' {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    match b.get(i) {
        Some(b'Z') | Some(b'z') => i + 1 == b.len(),
        Some(b'+') | Some(b'-') => {
            b.len() == i + 6 && digits(i + 1..i + 3) && b[i + 3] == b':' && digits(i + 4..i + 6)
        }
        _ => false,
    }
}

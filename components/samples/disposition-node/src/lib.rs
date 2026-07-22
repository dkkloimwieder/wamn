//! `disposition-recommendation` — the POC-F2 custom node (wamn-1ab) and the
//! reference disposition example. A zero-import `world node` (5.4): pure logic
//! against the [`wamn_node_sdk::Node`] trait, componentized by the one
//! `export_node!` macro at the bottom. It imports NOTHING — no `NodeCtx`
//! method is ever called — so the 5.5 builder derives EMPTY `hostInterfaces`
//! and emits a deployment with no `--allowed-hosts` (the 5.4-5.6 grant-derivation
//! proof; the F4 flow later invokes it as an audit comparison, out of scope here).
//!
//! # Contract
//! Input: `{"hold": {…}, "history": [{"decision": …}, …]}`.
//! - `hold.material` (string, required) — the material identity.
//! - Spec context (numeric-as-STRING exact decimals, F1 style; at least one
//!   dimension required, each all-or-nothing):
//!   - moisture: `moisture_pct` vs `moisture_max_pct` (catalog `numeric(5,2)` pct);
//!   - weight: `weight_kg` deviation from `quantity_kg` vs `weight_tolerance_kg`
//!     (catalog `numeric(8,3)` kg tolerance).
//! - `hold.reasons` (optional array of F1-format strings) — echoed into the
//!   rationale; not load-bearing for the decision.
//! - `history` (optional) — prior dispositions for the material/supplier; each
//!   entry's `decision` is one of the catalog `dispositions.decision` enum
//!   (`accept` / `reject` / `use-as-is`).
//!
//! Output on `main`: `{"recommended": <decision>, "confidence": <0..1 f64>,
//! "rationale": <string>}`.
//!
//! # Policy (deterministic, pure)
//! 1. Exceedance SEVERITY per dimension, by exact-decimal compare (no float in
//!    the decision boundary): a value strictly over its limit is exceeded; over
//!    DOUBLE the limit (exceedance strictly > the limit) is *severe*. The hold's
//!    overall severity is the worst dimension.
//! 2. History FREQUENCY: the strict-majority prior decision (ties → none).
//! 3. Map:
//!    - severe            → `reject`   (large deviation; history does not flip it);
//!    - mild + majority `reject` → `reject`  (peers rejected these);
//!    - mild (otherwise)  → `use-as-is` (accept the concession);
//!    - no exceedance     → `accept`.
//! 4. CONFIDENCE = base(severity) + span(severity) · support, where `support` is
//!    the fraction of history agreeing with the recommendation (empty history →
//!    0, i.e. the severity-only floor), rounded to two decimals. For a FIXED
//!    severity + recommendation it is monotonic non-decreasing in that fraction.
//!
//! Malformed / missing required fields → [`NodeError::InvalidInput`]. Zero side
//! effects, fully unit-testable (the `#[cfg(test)]` matrix below is the F2
//! builder publish gate, poc-material-receiving.md:40).

use std::cmp::Ordering;

use serde_json::{Map, Value, json};
use wamn_node_sdk::{Emission, ErrorDetail, Node, NodeCtx, NodeError, RunContext};

/// The recommendable dispositions — the catalog `dispositions.decision` enum
/// (`crates/wamn-catalog/tests/fixtures/poc-receiving.catalog.json`), pinned by
/// the drift-guard test against that fixture. Order matches the fixture.
const DECISIONS: [&str; 3] = ["accept", "reject", "use-as-is"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Accept,
    Reject,
    UseAsIs,
}

impl Decision {
    fn as_str(self) -> &'static str {
        match self {
            Decision::Accept => "accept",
            Decision::Reject => "reject",
            Decision::UseAsIs => "use-as-is",
        }
    }

    fn parse(s: &str) -> Option<Decision> {
        match s {
            "accept" => Some(Decision::Accept),
            "reject" => Some(Decision::Reject),
            "use-as-is" => Some(Decision::UseAsIs),
            _ => None,
        }
    }
}

/// The worst exceedance across a hold's evaluated spec dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    /// No dimension is strictly over its limit.
    InSpec,
    /// Over the limit, but within double it.
    Mild,
    /// Over DOUBLE the limit (exceedance strictly greater than the limit).
    Severe,
}

// ---------------------------------------------------------------------------
// Exact decimal (the no-float rule): the F1 subset needed here — parse, compare,
// subtract. A decimal travels as its canonical string; comparison/subtraction is
// on a scaled `i128`, so `12.50 == 12.5` and `|100.000 - 99.950| == 0.050` hold
// exactly. Hand-rolled (no `decimal` crate) to stay inside the builder allowlist.
// ---------------------------------------------------------------------------

/// The F1 caps: 27 significant digits + 9 fractional, so scale alignment never
/// overflows `i128` (the catalog tops out at `numeric(12,3)`).
const MAX_DIGITS: usize = 27;
const MAX_SCALE: usize = 9;

/// A parsed exact decimal: `value = units * 10^-scale` (`units` carries the sign).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dec {
    units: i128,
    scale: u32,
}

impl Dec {
    /// Parse a canonical decimal string: optional `-`, at least one integer
    /// digit, optionally `.` then at least one fractional digit. No exponent,
    /// no leading/trailing `.`, no whitespace, no `+`.
    fn parse(s: &str) -> Result<Dec, ()> {
        let (neg, digits) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let (int_part, frac_part) = match digits.split_once('.') {
            Some((i, f)) => (i, f),
            None => (digits, ""),
        };
        if int_part.is_empty() || (digits.contains('.') && frac_part.is_empty()) {
            return Err(());
        }
        if !int_part.bytes().all(|b| b.is_ascii_digit())
            || !frac_part.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(());
        }
        if int_part.len() + frac_part.len() > MAX_DIGITS || frac_part.len() > MAX_SCALE {
            return Err(());
        }
        let mut units: i128 = 0;
        for b in int_part.bytes().chain(frac_part.bytes()) {
            units = units * 10 + i128::from(b - b'0');
        }
        if neg {
            units = -units;
        }
        Ok(Dec {
            units,
            scale: frac_part.len() as u32,
        })
    }

    /// Rescale both to a common scale. Cannot overflow given the parse caps.
    fn aligned(self, other: Dec) -> (i128, i128) {
        let scale = self.scale.max(other.scale);
        let a = self.units * 10_i128.pow(scale - self.scale);
        let b = other.units * 10_i128.pow(scale - other.scale);
        (a, b)
    }

    /// Numeric comparison (scale-independent): `12.50 == 12.5`.
    fn cmp_value(self, other: Dec) -> Ordering {
        let (a, b) = self.aligned(other);
        a.cmp(&b)
    }

    /// `self - other`, at the wider of the two scales.
    fn sub(self, other: Dec) -> Dec {
        let (a, b) = self.aligned(other);
        Dec {
            units: a - b,
            scale: self.scale.max(other.scale),
        }
    }

    /// `|self|`.
    fn abs(self) -> Dec {
        Dec {
            units: self.units.abs(),
            scale: self.scale,
        }
    }
}

/// Classify a measured magnitude against its limit: exceeded iff strictly over
/// the limit; severe iff over DOUBLE it (the exceedance strictly exceeds the
/// limit). Boundary equality is IN-spec (a limit is not an exclusive bound).
fn classify(measured: Dec, limit: Dec) -> Severity {
    if measured.cmp_value(limit) != Ordering::Greater {
        return Severity::InSpec;
    }
    let exceedance = measured.sub(limit);
    if exceedance.cmp_value(limit) == Ordering::Greater {
        Severity::Severe
    } else {
        Severity::Mild
    }
}

// ---------------------------------------------------------------------------
// Input parsing (all malformed / missing paths -> InvalidInput)
// ---------------------------------------------------------------------------

fn invalid(code: &str, message: impl Into<String>) -> NodeError {
    NodeError::InvalidInput(ErrorDetail::coded(code, message))
}

/// A string field, present only. `None` = absent; `Err` = present but not a
/// string (a malformed hold).
fn opt_str<'a>(hold: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, NodeError> {
    match hold.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(invalid(
            "malformed-field",
            format!("hold.{key} must be a decimal STRING (the no-float rule)"),
        )),
    }
}

/// Parse a present decimal-string field; a bad decimal is `InvalidInput`.
fn dec_field(key: &str, raw: &str) -> Result<Dec, NodeError> {
    Dec::parse(raw).map_err(|()| {
        invalid(
            "malformed-decimal",
            format!("hold.{key} {raw:?} is not a decimal"),
        )
    })
}

/// Evaluate the moisture dimension: `Some(severity)` when the pair is present,
/// `None` when the whole dimension is absent. A half-present pair is malformed.
fn moisture_severity(hold: &Map<String, Value>) -> Result<Option<Severity>, NodeError> {
    match (
        opt_str(hold, "moisture_pct")?,
        opt_str(hold, "moisture_max_pct")?,
    ) {
        (None, None) => Ok(None),
        (Some(m), Some(mx)) => {
            let measured = dec_field("moisture_pct", m)?;
            let limit = dec_field("moisture_max_pct", mx)?;
            Ok(Some(classify(measured, limit)))
        }
        _ => Err(invalid(
            "incomplete-dimension",
            "moisture needs BOTH moisture_pct and moisture_max_pct",
        )),
    }
}

/// Evaluate the weight dimension: `Some(severity)` when the triple is present.
/// Deviation is `|weight_kg - quantity_kg|` against `weight_tolerance_kg`.
fn weight_severity(hold: &Map<String, Value>) -> Result<Option<Severity>, NodeError> {
    let w = opt_str(hold, "weight_kg")?;
    let q = opt_str(hold, "quantity_kg")?;
    let t = opt_str(hold, "weight_tolerance_kg")?;
    match (w, q, t) {
        (None, None, None) => Ok(None),
        (Some(w), Some(q), Some(t)) => {
            let weight = dec_field("weight_kg", w)?;
            let quantity = dec_field("quantity_kg", q)?;
            let tolerance = dec_field("weight_tolerance_kg", t)?;
            let deviation = weight.sub(quantity).abs();
            Ok(Some(classify(deviation, tolerance)))
        }
        _ => Err(invalid(
            "incomplete-dimension",
            "weight needs weight_kg, quantity_kg AND weight_tolerance_kg",
        )),
    }
}

/// Parse the prior-disposition history into decisions. Each entry must be an
/// object carrying a `decision` in the catalog enum.
fn parse_history(input: &Value) -> Result<Vec<Decision>, NodeError> {
    let entries = match input.get("history") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(invalid("malformed-history", "history must be an array")),
    };
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let decision = entry
            .get("decision")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                invalid(
                    "malformed-history",
                    "each history entry needs a \"decision\" string",
                )
            })?;
        let decision = Decision::parse(decision).ok_or_else(|| {
            invalid(
                "unknown-decision",
                format!("history decision {decision:?} is not one of {DECISIONS:?}"),
            )
        })?;
        out.push(decision);
    }
    Ok(out)
}

/// The strict-majority decision in `history` (a count strictly greater than
/// every other), or `None` on an empty history or a tie.
fn majority(history: &[Decision]) -> Option<Decision> {
    let count = |d: Decision| history.iter().filter(|&&h| h == d).count();
    let (accept, reject, use_as_is) = (
        count(Decision::Accept),
        count(Decision::Reject),
        count(Decision::UseAsIs),
    );
    let top = accept.max(reject).max(use_as_is);
    if top == 0 {
        return None;
    }
    let leaders: Vec<Decision> = [
        (Decision::Accept, accept),
        (Decision::Reject, reject),
        (Decision::UseAsIs, use_as_is),
    ]
    .into_iter()
    .filter(|&(_, c)| c == top)
    .map(|(d, _)| d)
    .collect();
    match leaders.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// Round a confidence to two decimals so the emitted `f64` is stable + pinnable.
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// The pure recommendation — the whole node logic, host-testable without wasm.
// ---------------------------------------------------------------------------

/// Recommend a disposition for `input`. Pure: no ctx, no I/O. `Err` is the
/// `InvalidInput` contract for a malformed hold.
fn recommend(input: &Value) -> Result<Value, NodeError> {
    let hold = input
        .get("hold")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("missing-hold", "input requires a \"hold\" object"))?;

    let material = hold
        .get("material")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            invalid(
                "missing-material",
                "hold.material (a non-empty string) is required",
            )
        })?;

    // reasons: optional F1-format strings, echoed into the rationale.
    let reasons = match hold.get("reasons") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| invalid("malformed-reasons", "hold.reasons must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(invalid(
                "malformed-reasons",
                "hold.reasons must be an array",
            ));
        }
    };

    let dims = [moisture_severity(hold)?, weight_severity(hold)?];
    let present: Vec<Severity> = dims.into_iter().flatten().collect();
    if present.is_empty() {
        return Err(invalid(
            "no-spec-context",
            "hold carries no spec dimension (need the moisture pair and/or the weight triple)",
        ));
    }
    // Worst dimension wins.
    let severity = if present.contains(&Severity::Severe) {
        Severity::Severe
    } else if present.contains(&Severity::Mild) {
        Severity::Mild
    } else {
        Severity::InSpec
    };

    let history = parse_history(input)?;
    let majority = majority(&history);

    // Severity -> (recommendation, confidence base, span over history support).
    // MUTATION (i) TARGET: swapping Reject<->Accept in the Severe arm flips
    // `severe_large_deviation_recommends_reject`.
    let (rec, base, span) = match severity {
        Severity::Severe => (Decision::Reject, 0.80, 0.15),
        Severity::Mild => match majority {
            Some(Decision::Reject) => (Decision::Reject, 0.50, 0.30),
            _ => (Decision::UseAsIs, 0.50, 0.30),
        },
        Severity::InSpec => (Decision::Accept, 0.70, 0.20),
    };

    let support = if history.is_empty() {
        0.0
    } else {
        history.iter().filter(|&&d| d == rec).count() as f64 / history.len() as f64
    };
    let confidence = round2(base + span * support);

    let severity_word = match severity {
        Severity::Severe => "severe",
        Severity::Mild => "mild",
        Severity::InSpec => "in-spec",
    };
    let history_note = match (history.len(), majority) {
        (0, _) => "no prior dispositions".to_string(),
        (n, Some(d)) => format!("{n} prior disposition(s), majority {}", d.as_str()),
        (n, None) => format!("{n} prior disposition(s), no majority"),
    };
    let reason_note = if reasons.is_empty() {
        String::new()
    } else {
        format!("; out-of-spec: {}", reasons.join("; "))
    };
    let rationale = format!(
        "{material}: {severity_word} exceedance with {history_note}{reason_note} — recommend {}",
        rec.as_str()
    );

    Ok(json!({
        "recommended": rec.as_str(),
        "confidence": confidence,
        "rationale": rationale,
    }))
}

#[derive(Default)]
struct DispositionRecommendation;

impl Node for DispositionRecommendation {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        _run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        // Zero-import node: `_ctx` (a `NoCapsCtx`) is NEVER touched.
        recommend(input).map(Emission::main)
    }
}

wamn_node_guest::export_node!(DispositionRecommendation);

#[cfg(test)]
mod tests {
    use super::*;

    fn hold_moisture(measured: &str, max: &str) -> Value {
        json!({"hold": {"material": "resin-A", "moisture_pct": measured, "moisture_max_pct": max}})
    }

    fn with_history(mut input: Value, decisions: &[&str]) -> Value {
        let hist: Vec<Value> = decisions.iter().map(|d| json!({"decision": d})).collect();
        input
            .as_object_mut()
            .unwrap()
            .insert("history".into(), json!(hist));
        input
    }

    fn rec_of(out: &Value) -> &str {
        out["recommended"].as_str().unwrap()
    }
    fn conf_of(out: &Value) -> f64 {
        out["confidence"].as_f64().unwrap()
    }

    // ---- decimal ---------------------------------------------------------

    #[test]
    fn decimal_compares_scale_independently_and_subtracts_exactly() {
        assert_eq!(
            Dec::parse("12.50")
                .unwrap()
                .cmp_value(Dec::parse("12.5").unwrap()),
            Ordering::Equal
        );
        let d = Dec::parse("100.000")
            .unwrap()
            .sub(Dec::parse("99.950").unwrap())
            .abs();
        assert_eq!(d.cmp_value(Dec::parse("0.050").unwrap()), Ordering::Equal);
        assert!(Dec::parse(".5").is_err());
        assert!(Dec::parse("12.").is_err());
        assert!(Dec::parse("1e5").is_err());
    }

    #[test]
    fn classify_boundary_is_in_spec_and_double_is_severe() {
        let limit = Dec::parse("5.00").unwrap();
        // exactly the limit -> in-spec (a limit is not an exclusive bound).
        assert_eq!(
            classify(Dec::parse("5.00").unwrap(), limit),
            Severity::InSpec
        );
        // just over -> mild.
        assert_eq!(classify(Dec::parse("5.01").unwrap(), limit), Severity::Mild);
        // exactly double (exceedance == limit) -> still mild (strict boundary).
        assert_eq!(
            classify(Dec::parse("10.00").unwrap(), limit),
            Severity::Mild
        );
        // over double -> severe.
        assert_eq!(
            classify(Dec::parse("10.01").unwrap(), limit),
            Severity::Severe
        );
    }

    // ---- policy matrix: each disposition reachable -----------------------

    /// MUTATION (i) TARGET. A large (severe) exceedance recommends `reject`
    /// regardless of history — swapping Reject<->Accept in the Severe arm of
    /// `recommend` flips this assertion.
    #[test]
    fn severe_large_deviation_recommends_reject() {
        let out = recommend(&hold_moisture("12.00", "5.00")).unwrap();
        assert_eq!(rec_of(&out), "reject");
        // even a use-as-is-heavy history does not override a severe reject.
        let out = recommend(&with_history(
            hold_moisture("12.00", "5.00"),
            &["use-as-is", "use-as-is"],
        ))
        .unwrap();
        assert_eq!(rec_of(&out), "reject");
    }

    #[test]
    fn mild_exceedance_default_is_use_as_is() {
        let out = recommend(&hold_moisture("6.00", "5.00")).unwrap();
        assert_eq!(rec_of(&out), "use-as-is");
    }

    #[test]
    fn mild_with_reject_majority_history_recommends_reject() {
        let out = recommend(&with_history(
            hold_moisture("6.00", "5.00"),
            &["reject", "reject", "use-as-is"],
        ))
        .unwrap();
        assert_eq!(rec_of(&out), "reject");
    }

    #[test]
    fn no_exceedance_recommends_accept() {
        let out = recommend(&hold_moisture("4.00", "5.00")).unwrap();
        assert_eq!(rec_of(&out), "accept");
    }

    #[test]
    fn weight_dimension_drives_severity() {
        // deviation |110.000 - 100.000| = 10.000 > 2*tolerance(2.000) -> severe.
        let input = json!({"hold": {
            "material": "billet-9", "weight_kg": "110.000",
            "quantity_kg": "100.000", "weight_tolerance_kg": "2.000"
        }});
        assert_eq!(rec_of(&recommend(&input).unwrap()), "reject");
    }

    #[test]
    fn worst_dimension_wins_across_moisture_and_weight() {
        // moisture mild, weight severe -> overall severe -> reject.
        let input = json!({"hold": {
            "material": "mix-1",
            "moisture_pct": "6.00", "moisture_max_pct": "5.00",
            "weight_kg": "50.000", "quantity_kg": "10.000", "weight_tolerance_kg": "1.000"
        }});
        assert_eq!(rec_of(&recommend(&input).unwrap()), "reject");
    }

    // ---- confidence ------------------------------------------------------

    #[test]
    fn empty_history_uses_the_severity_only_floor() {
        // severe floor 0.80; mild floor 0.50; accept floor 0.70 — support == 0.
        assert_eq!(
            conf_of(&recommend(&hold_moisture("12.00", "5.00")).unwrap()),
            0.80
        );
        assert_eq!(
            conf_of(&recommend(&hold_moisture("6.00", "5.00")).unwrap()),
            0.50
        );
        assert_eq!(
            conf_of(&recommend(&hold_moisture("4.00", "5.00")).unwrap()),
            0.70
        );
    }

    /// Confidence is monotonic non-decreasing in the fraction of history that
    /// agrees with the recommendation, at a FIXED severity/recommendation. A
    /// severe hold always recommends `reject`, so more reject-history == more
    /// support == higher confidence.
    #[test]
    fn confidence_is_monotonic_in_agreeing_history_support() {
        let severe = || hold_moisture("12.00", "5.00");
        let none = conf_of(&recommend(&severe()).unwrap()); // support 0 -> 0.80
        let half = conf_of(&recommend(&with_history(severe(), &["reject", "accept"])).unwrap()); // 0.5 -> 0.875 -> 0.88
        let full = conf_of(&recommend(&with_history(severe(), &["reject", "reject"])).unwrap()); // 1.0 -> 0.95
        assert!(
            none < half && half < full,
            "expected {none} < {half} < {full}"
        );
        assert_eq!(full, 0.95);
    }

    // ---- malformed input -> InvalidInput ---------------------------------

    #[test]
    fn malformed_and_missing_inputs_are_invalid_input() {
        let cases = [
            json!({}),                                                             // no hold
            json!({"hold": {"moisture_pct": "6.00", "moisture_max_pct": "5.00"}}), // no material
            json!({"hold": {"material": "x"}}), // no spec dimension
            json!({"hold": {"material": "x", "moisture_pct": "6.00"}}), // half a dimension
            json!({"hold": {"material": "x", "moisture_pct": "abc", "moisture_max_pct": "5.00"}}), // bad decimal
            json!({"hold": {"material": "x", "moisture_pct": 6.0, "moisture_max_pct": "5.00"}}), // float, not string
        ];
        for case in cases {
            assert!(
                matches!(recommend(&case), Err(NodeError::InvalidInput(_))),
                "expected InvalidInput for {case}"
            );
        }
    }

    #[test]
    fn history_with_an_unknown_decision_is_invalid_input() {
        let input = with_history(hold_moisture("6.00", "5.00"), &["approve"]);
        assert!(matches!(recommend(&input), Err(NodeError::InvalidInput(_))));
    }

    // ---- output-shape + catalog-enum drift guards ------------------------

    #[test]
    fn output_carries_exactly_the_pinned_keys() {
        let out = recommend(&hold_moisture("6.00", "5.00")).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.len(), 3);
        for key in ["recommended", "confidence", "rationale"] {
            assert!(obj.contains_key(key), "output missing {key}");
        }
        assert!(obj["rationale"].as_str().unwrap().contains("resin-A"));
    }

    /// Drift guard: our recommendable set + `Decision` mapping must track the
    /// catalog `dispositions.decision` enum verbatim. Read the fixture in the
    /// test (the `include_str!` convention) so a catalog enum change breaks here.
    #[test]
    fn recommendation_enum_tracks_the_catalog_fixture() {
        let catalog: Value = serde_json::from_str(include_str!(
            "../../../../crates/wamn-catalog/tests/fixtures/poc-receiving.catalog.json"
        ))
        .expect("catalog fixture parses");
        let variants: Vec<&str> = catalog["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == "dispositions")
            .expect("dispositions entity")["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == "decision")
            .expect("decision field")["type"]["variants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            variants, DECISIONS,
            "catalog decision enum drifted from DECISIONS"
        );
        for v in &variants {
            assert_eq!(Decision::parse(v).unwrap().as_str(), *v);
        }
    }
}

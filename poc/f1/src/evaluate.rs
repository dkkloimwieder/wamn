//! Spec evaluation — the `evaluate-specs` node's pure core. Each line is
//! checked against its material's two spec fields with EXACT-decimal
//! arithmetic: measured moisture must not exceed `materials.moisture_max_pct`,
//! and the measured weight must not deviate from the declared quantity by more
//! than `materials.weight_tolerance_kg`. Boundary equality is IN-spec (a spec
//! is a limit, not an exclusive bound); only a strict exceedance opens a hold.

use crate::decimal::Decimal;

/// Evaluate one line. Inputs are the validated payload strings plus the
/// material's resolved spec strings (as read back from `materials`, canonical
/// `numeric` text). Returns the out-of-spec reasons — empty means in-spec.
/// `Err` means a value failed to parse, which the validate node's checks make
/// unreachable for payload values; spec values come from `numeric` columns.
pub fn evaluate_line(
    quantity: &str,
    moisture_pct: &str,
    weight_kg: &str,
    moisture_max_pct: &str,
    weight_tolerance_kg: &str,
) -> Result<Vec<String>, String> {
    let quantity_d = Decimal::parse(quantity)?;
    let moisture = Decimal::parse(moisture_pct)?;
    let weight = Decimal::parse(weight_kg)?;
    let moisture_max = Decimal::parse(moisture_max_pct)?;
    let tolerance = Decimal::parse(weight_tolerance_kg)?;

    let mut reasons = Vec::new();
    if moisture.cmp_value(&moisture_max) == std::cmp::Ordering::Greater {
        reasons.push(format!(
            "moisture {moisture_pct} pct exceeds max {moisture_max_pct} pct"
        ));
    }
    let deviation = weight.abs_diff(&quantity_d);
    if deviation.cmp_value(&tolerance) == std::cmp::Ordering::Greater {
        reasons.push(format!(
            "weight {weight_kg} kg deviates {deviation} kg from declared {quantity} kg \
             (tolerance {weight_tolerance_kg} kg)"
        ));
    }
    Ok(reasons)
}

//! Shared POC-F1 fixtures: the catalog / flow / seed dataset (embedded from
//! their canonical committed files, so the bench, the proof, and the deploy
//! ConfigMaps provision the identical world), plus the receipt payloads the
//! f1bench and f1proof gates POST — including the acceptance-script burst of
//! 20 receipts, 3 of them out-of-spec.
//!
//! Seeded specs (deploy/poc/f1-seed.dataset.json):
//!   resin-a    moisture_max 12.50 pct   weight_tolerance 0.050 kg
//!   solvent-b  moisture_max  0.10 pct   weight_tolerance 0.500 kg
//!   pigment-c  moisture_max  5.00 pct   weight_tolerance 0.010 kg

use serde_json::{Value, json};

/// The POC data model — the wamn-catalog fixture verbatim (same file the
/// wamn-catalog/wamn-ddl/wamn-f1 tests pin).
pub const F1_CATALOG_JSON: &str =
    include_str!("../../wamn-catalog/tests/fixtures/poc-receiving.catalog.json");

/// The production F1 flow graph (deploy/poc/f1-flow.json — drift-guarded against
/// the wamn-f1 node set by that crate's tests).
pub const F1_FLOW_JSON: &str = include_str!("../../../deploy/poc/f1-flow.json");

/// The F1 business seed (deploy/poc/f1-seed.dataset.json, a wamn-seed dataset).
pub const F1_SEED_JSON: &str = include_str!("../../../deploy/poc/f1-seed.dataset.json");

/// The tenant every F1 gate runs under.
pub const F1_TENANT: &str = "f1-tenant";

/// Total quality holds the burst must create (3 out-of-spec receipts, one of
/// them with two offending lines).
pub const BURST_HOLDS: usize = 4;

pub fn catalog() -> anyhow::Result<wamn_catalog::Catalog> {
    wamn_catalog::Catalog::from_json(F1_CATALOG_JSON)
        .map_err(|e| anyhow::anyhow!("f1 catalog fixture: {e}"))
}

/// The 3.2 tenant floor for the POC catalog.
pub fn floor_ddl() -> anyhow::Result<String> {
    let cat = catalog()?;
    wamn_ddl::Migration::create(&cat)
        .map_err(|e| anyhow::anyhow!("floor compile: {e}"))?
        .sql(wamn_ddl::Confirmation::None)
        .map_err(|e| anyhow::anyhow!("floor sql: {e}"))
}

/// Build a receipt payload. `lines` = (material, quantity, moisture_pct,
/// weight_kg).
pub fn receipt(
    receipt_no: &str,
    supplier: &str,
    site: &str,
    lines: &[(&str, &str, &str, &str)],
) -> Value {
    json!({
        "receipt_no": receipt_no,
        "supplier": supplier,
        "site": site,
        "received_at": "2026-07-12T08:00:00Z",
        "lines": lines.iter().map(|(material, quantity, moisture, weight)| json!({
            "material": material,
            "quantity": quantity,
            "moisture_pct": moisture,
            "weight_kg": weight,
        })).collect::<Vec<_>>(),
    })
}

/// A single always-in-spec receipt (resin-a: moisture 11.20 <= 12.50, weight
/// deviation 0.020 <= 0.050).
pub fn in_spec_receipt(receipt_no: &str) -> Value {
    receipt(
        receipt_no,
        "acme",
        "hq",
        &[("resin-a", "100.000", "11.20", "99.980")],
    )
}

/// The acceptance-script burst: 20 receipts, 3 out-of-spec. Returns
/// `(payload, expected_hold_count)` per receipt; the expected counts sum to
/// [`BURST_HOLDS`].
pub fn burst() -> Vec<(Value, usize)> {
    (1..=20)
        .map(|i| {
            let no = format!("r-10{i:02}");
            match i {
                // Moisture exceedance: 13.10 > 12.50 (weight dev 0.010 fine).
                5 => (
                    receipt(
                        &no,
                        "acme",
                        "hq",
                        &[("resin-a", "50.000", "13.10", "49.990")],
                    ),
                    1,
                ),
                // Weight exceedance: |25.020 - 25.000| = 0.020 > 0.010.
                11 => (
                    receipt(
                        &no,
                        "globex",
                        "west",
                        &[("pigment-c", "25.000", "4.00", "25.020")],
                    ),
                    1,
                ),
                // Two offending lines (one doubly so) plus one clean line.
                17 => (
                    receipt(
                        &no,
                        "globex",
                        "hq",
                        &[
                            ("solvent-b", "10.000", "0.20", "10.000"),
                            ("resin-a", "30.000", "12.51", "30.100"),
                            ("pigment-c", "5.000", "1.00", "5.005"),
                        ],
                    ),
                    2,
                ),
                // In-spec, varying supplier/site/material.
                _ => {
                    let supplier = if i % 2 == 0 { "acme" } else { "globex" };
                    let site = if i % 3 == 0 { "west" } else { "hq" };
                    (
                        receipt(
                            &no,
                            supplier,
                            site,
                            &[("resin-a", "100.000", "11.20", "99.980")],
                        ),
                        0,
                    )
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded fixtures parse and agree with each other: the catalog
    /// validates, the flow validates and matches the F1 contract, the seed
    /// dataset compiles against the catalog, and the burst carries exactly the
    /// advertised out-of-spec load.
    #[test]
    fn f1_fixtures_are_coherent() {
        let cat = catalog().expect("catalog");
        assert!(cat.validate().is_ok());

        let flow = wamn_flow::Flow::from_json(F1_FLOW_JSON).expect("flow");
        assert!(flow.issues().is_empty(), "{:?}", flow.issues());
        assert_eq!(flow.flow_id, "receipt-received");

        let dataset = wamn_seed::Dataset::from_json(F1_SEED_JSON).expect("seed dataset");
        let plan = wamn_seed::compile(&dataset, &cat, F1_TENANT).expect("seed compiles");
        let sql = plan.sql(wamn_ddl::Confirmation::None).expect("additive");
        for name in [
            "'resin-a'",
            "'solvent-b'",
            "'pigment-c'",
            "'acme'",
            "'globex'",
        ] {
            assert!(sql.contains(name), "seed sql missing {name}");
        }

        let burst = burst();
        assert_eq!(burst.len(), 20);
        let out_of_spec = burst.iter().filter(|(_, holds)| *holds > 0).count();
        assert_eq!(out_of_spec, 3, "3 receipts out-of-spec");
        let total: usize = burst.iter().map(|(_, holds)| holds).sum();
        assert_eq!(total, BURST_HOLDS);
        // Every receipt_no is unique — 20 distinct runs.
        let mut nos: Vec<String> = burst
            .iter()
            .map(|(v, _)| v["receipt_no"].as_str().unwrap().to_string())
            .collect();
        nos.sort();
        nos.dedup();
        assert_eq!(nos.len(), 20);
    }
}

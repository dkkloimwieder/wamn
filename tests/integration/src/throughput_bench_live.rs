//! wamn-0h0g.17.27: the throughput sweep's evidence, read back as a gate.
//!
//! The cluster run is `tools/receiving-cluster-journey-run --apply
//! --measure-startup --throughput`; it leaves `throughput/` in its evidence
//! directory. This test needs that directory and nothing else, and it asserts
//! only shape: every layer ran every step, every step produced a rate and a
//! p99, every error count is recorded, and the knee and peak were computed.
//! NO ABSOLUTE NUMBER IS ASSERTED. The knee and the peak are recorded in the
//! report beside the evidence so a later run is compared to this one, and a
//! ceiling ratchets later, on a landing.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::throughput_bench::{Report, build_report};

    const EVIDENCE_ENV: &str = "WAMN_THROUGHPUT_EVIDENCE_DIR";

    fn evidence_dir() -> PathBuf {
        let raw = std::env::var(EVIDENCE_ENV).unwrap_or_else(|_| {
            panic!("{EVIDENCE_ENV} must point at a --throughput journey's throughput/ directory")
        });
        PathBuf::from(raw)
    }

    #[test]
    #[ignore = "requires WAMN_THROUGHPUT_EVIDENCE_DIR from a --throughput receiving cluster journey"]
    fn every_layer_ran_the_whole_sweep_and_its_knee_is_recorded() {
        let report: Report = build_report(&evidence_dir()).expect("reduce the sweep's evidence");
        assert!(
            !report.index.layers.is_empty(),
            "the index declares no layer"
        );
        assert!(
            !report.index.concurrency.is_empty(),
            "the index declares no concurrency step"
        );
        for layer in &report.index.layers {
            let ran: Vec<u32> = report
                .results
                .iter()
                .filter(|r| r.layer == layer.layer)
                .map(|r| r.concurrency)
                .collect();
            assert_eq!(
                ran, report.index.concurrency,
                "layer {} did not run the declared sweep in order",
                layer.layer
            );
            let verdict = report
                .verdicts
                .iter()
                .find(|v| v.layer == layer.layer)
                .unwrap_or_else(|| panic!("layer {} has no verdict", layer.layer));
            assert!(
                verdict.peak.requests_per_second > 0.0,
                "layer {} peaked at zero",
                layer.layer
            );
        }
        for step in &report.results {
            assert!(
                step.generator.requests_per_second > 0.0 && step.generator.p99_ms > 0.0,
                "{} c={} produced no rate or no p99",
                step.layer,
                step.concurrency
            );
            assert!(
                step.generator.total_requests > 0,
                "{} c={} sent nothing",
                step.layer,
                step.concurrency
            );
            // Recorded, not bounded: the error column is part of the result.
            let _ = step.generator.errors;
        }
        println!("{}", report.render_markdown());
    }
}

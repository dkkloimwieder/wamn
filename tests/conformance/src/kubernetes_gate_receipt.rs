//! Typed consumer contract for Kubernetes gate verdict receipts.

use serde::Deserialize;

/// Stable protocol identifier emitted by `tools/kubernetes-gate-run`.
pub const PROTOCOL: &str = "wamn-kubernetes-gate-verdict/v1";

/// A complete aggregate verdict for one freshly applied manifest.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema_version: u32,
    pub protocol: String,
    pub manifest: String,
    pub namespace: String,
    pub run_started_at: String,
    pub timeout_seconds: u64,
    pub verdict: Verdict,
    pub failure_classes: Vec<String>,
    pub jobs: Vec<JobReceipt>,
    pub snapshot_probe: Option<SnapshotProbe>,
}

/// Aggregate or per-Job machine verdict.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Pass,
    Fail,
}

/// Whether a Job must complete normally or fail in one exact expected way.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Expectation {
    Positive,
    ExpectedNegative,
}

/// One Job's expected and observed execution evidence.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct JobReceipt {
    pub name: String,
    pub container: String,
    pub expectation: Expectation,
    pub expected_exit_code: i32,
    pub expected_image: String,
    pub observed: ObservedJob,
    pub verdict: Verdict,
    pub failure_classes: Vec<String>,
}

/// Kubernetes identities and temporal evidence for a Job run.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObservedJob {
    pub uid: String,
    pub previous_uid: String,
    pub created_at: String,
    pub condition: String,
    pub condition_transition_at: String,
    pub logs_sha256: String,
    pub pods: Vec<PodObservation>,
}

/// Runtime evidence for one Pod owned by the Job.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PodObservation {
    pub name: String,
    pub uid: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub phase: String,
    pub init_exit_codes: Vec<Option<i32>>,
    pub container_exit_code: Option<i32>,
    pub image_id: String,
}

/// Exact executable/argv probe and its before/after stdout identities.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SnapshotProbe {
    pub executable: String,
    pub argv: Vec<String>,
    pub before_exit_code: i32,
    pub after_exit_code: i32,
    pub before_stdout_sha256: String,
    pub after_stdout_sha256: String,
    pub unchanged: bool,
}

/// Parse a receipt and reject unknown fields or a different protocol version.
pub fn parse_receipt(bytes: &[u8]) -> Result<Receipt, String> {
    let receipt: Receipt =
        serde_json::from_slice(bytes).map_err(|error| format!("invalid receipt JSON: {error}"))?;
    if receipt.schema_version != 1 || receipt.protocol != PROTOCOL {
        return Err(format!(
            "unsupported Kubernetes gate receipt {}/{}",
            receipt.protocol, receipt.schema_version
        ));
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_receipt_fields_are_rejected() {
        let error = parse_receipt(
            br#"{"schema_version":1,"protocol":"wamn-kubernetes-gate-verdict/v1",
                "manifest":"gate.yaml","namespace":"ns","run_started_at":"now",
                "timeout_seconds":1,"verdict":"pass","failure_classes":[],"jobs":[],
                "snapshot_probe":null,"decorative_green":true}"#,
        )
        .expect_err("unknown fields must fail closed");
        assert!(error.contains("unknown field"), "{error}");
    }

    #[test]
    fn future_protocol_versions_are_rejected() {
        let error = parse_receipt(
            br#"{"schema_version":2,"protocol":"wamn-kubernetes-gate-verdict/v2",
                "manifest":"gate.yaml","namespace":"ns","run_started_at":"now",
                "timeout_seconds":1,"verdict":"pass","failure_classes":[],"jobs":[],
                "snapshot_probe":null}"#,
        )
        .expect_err("unknown protocol must fail closed");
        assert!(
            error.contains("unsupported Kubernetes gate receipt"),
            "{error}"
        );
    }
}

//! Typed consumer contract for Kubernetes gate verdict records.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Stable protocol identifier emitted by `tools/kubernetes-gate-run`.
pub const PROTOCOL: &str = "wamn-kubernetes-gate-verdict/v0.1";

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    // Integer syntax is decoded only so preserved rejection fixtures reach the
    // invariant check; the producer emits text and only textual `0.1` passes.
    match Value::deserialize(deserializer)? {
        Value::String(version) => Ok(version),
        Value::Number(version) if version.is_i64() || version.is_u64() => Ok(version.to_string()),
        _ => Err(serde::de::Error::custom(
            "schema_version must be a textual version",
        )),
    }
}

/// A complete aggregate verdict for one freshly created or applied manifest.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GateVerdictRecord {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: String,
    pub protocol: String,
    pub manifest: String,
    pub namespace: String,
    pub run_started_at: String,
    pub timeout_seconds: u64,
    pub verdict: Verdict,
    pub failure_classes: Vec<String>,
    pub jobs: Vec<JobVerdictRecord>,
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
pub struct JobVerdictRecord {
    pub name: String,
    pub container: String,
    pub expectation: Expectation,
    pub expected_exit_code: i32,
    pub expected_image: String,
    pub sidecar: Option<String>,
    pub expected_sidecar_image: Option<String>,
    pub expected_sidecar_image_id: Option<String>,
    pub preflight_sidecar_config_id: Option<String>,
    pub sidecar_upstream_index: Option<String>,
    pub sidecar_upstream_child: Option<String>,
    pub sidecar_preflight_sha256: Option<String>,
    pub claimed_image_id: Option<String>,
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
    pub claimed_image_id: Option<String>,
    pub logs_sha256: String,
    pub pods: Vec<PodObservation>,
}

/// Runtime evidence for one Pod owned by the Job.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PodObservation {
    pub name: String,
    pub uid: String,
    pub node: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub phase: String,
    pub init_exit_codes: Vec<Option<i32>>,
    pub container_exit_code: Option<i32>,
    pub image_id: String,
    pub sidecar_exit_code: Option<i32>,
    pub sidecar_image_id: Option<String>,
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

/// Parse a record and reject unknown fields or a different protocol version.
pub fn parse_verdict_record(bytes: &[u8]) -> Result<GateVerdictRecord, String> {
    let record: GateVerdictRecord =
        serde_json::from_slice(bytes).map_err(|error| format!("invalid record JSON: {error}"))?;
    if record.schema_version != "0.1" || record.protocol != PROTOCOL {
        return Err(format!(
            "unsupported Kubernetes gate record {}/{}",
            record.protocol, record.schema_version
        ));
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_version_identity_is_rejected() {
        let error = parse_verdict_record(
            br#"{"schema_version":1,"protocol":"wamn-kubernetes-gate-verdict/v1",
                "manifest":"gate.yaml","namespace":"ns","run_started_at":"now",
                "timeout_seconds":1,"verdict":"pass","failure_classes":[],"jobs":[],
                "snapshot_probe":null}"#,
        )
        .expect_err("legacy schema and protocol versions must fail closed");
        assert_eq!(
            error,
            "unsupported Kubernetes gate record wamn-kubernetes-gate-verdict/v1/1"
        );
    }

    #[test]
    fn unknown_record_fields_are_rejected() {
        let error = parse_verdict_record(
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
        let error = parse_verdict_record(
            br#"{"schema_version":2,"protocol":"wamn-kubernetes-gate-verdict/v2",
                "manifest":"gate.yaml","namespace":"ns","run_started_at":"now",
                "timeout_seconds":1,"verdict":"pass","failure_classes":[],"jobs":[],
                "snapshot_probe":null}"#,
        )
        .expect_err("unknown protocol must fail closed");
        assert!(
            error.contains("unsupported Kubernetes gate record"),
            "{error}"
        );
    }
}

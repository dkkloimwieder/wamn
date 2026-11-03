//! Platform kind steps and saved inputs for a resumed run.
use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use wamn_control::environment::document::EnvironmentDocument;
use wamn_control::package_artifact::PackageRegistry;
use wamn_control::platform::document::ClusterDocument;

#[derive(Debug)]
pub struct Step {
    pub name: &'static str,
    pub needs: &'static [&'static str],
}

pub const STEPS: &[Step] = &[
    Step {
        name: "4.3 through 4.14",
        needs: &[],
    },
    Step {
        name: "installed objects and NATS",
        needs: &["4.3 through 4.14"],
    },
    Step {
        name: "4.15 unqualified release refusal",
        needs: &["4.3 through 4.14"],
    },
    Step {
        name: "4.16.1 WMS integration",
        needs: &["4.15 unqualified release refusal"],
    },
    Step {
        name: "qualify-release Receiving",
        needs: &["4.3 through 4.14"],
    },
    Step {
        name: "4.17 Apply with the release",
        needs: &["qualify-release Receiving"],
    },
    Step {
        name: "4.18 CDC readers",
        needs: &["4.17 Apply with the release"],
    },
    Step {
        name: "4.19 Web",
        needs: &["4.17 Apply with the release"],
    },
    Step {
        name: "4.19.1 Control host",
        needs: &["4.17 Apply with the release"],
    },
    Step {
        name: "4.21 HTTPS",
        needs: &["4.17 Apply with the release"],
    },
    Step {
        name: "4.20 Rust edge",
        needs: &["4.17 Apply with the release"],
    },
    Step {
        name: "4.19.1 Owner invite",
        needs: &["4.17 Apply with the release"],
    },
    Step {
        name: "unchanged rerun / 4.11 keeps the release",
        needs: &["4.17 Apply with the release"],
    },
    Step {
        name: "4.21 unchanged HTTPS state",
        needs: &["4.21 HTTPS", "unchanged rerun / 4.11 keeps the release"],
    },
    Step {
        name: "4.20 unchanged edge Deployment",
        needs: &["4.20 Rust edge", "unchanged rerun / 4.11 keeps the release"],
    },
    Step {
        name: "4.19.1 unchanged invitation and Secrets",
        needs: &[
            "4.19.1 Owner invite",
            "unchanged rerun / 4.11 keeps the release",
        ],
    },
    Step {
        name: "remaining checks",
        needs: &["unchanged rerun / 4.11 keeps the release"],
    },
    Step {
        name: "4.12 unchanged Secrets",
        needs: &[
            "4.19.1 Owner invite",
            "unchanged rerun / 4.11 keeps the release",
        ],
    },
    Step {
        name: "private logs",
        needs: &[
            "4.19.1 Owner invite",
            "unchanged rerun / 4.11 keeps the release",
        ],
    },
];

pub fn reuse_from<'a>(
    reuse: Option<&'a str>,
    from: Option<&str>,
) -> anyhow::Result<Option<(&'a str, usize)>> {
    match (reuse, from) {
        (None, None) => Ok(None),
        (Some(path), Some(name)) => {
            ensure!(!path.is_empty(), "WAMN_KIND_REUSE must name kept.json");
            let index = STEPS
                .iter()
                .position(|step| step.name == name)
                .with_context(|| {
                    format!(
                        "unknown or empty WAMN_KIND_FROM name {name:?}: known names: {}",
                        STEPS
                            .iter()
                            .map(|step| step.name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            Ok(Some((path, index)))
        }
        _ => anyhow::bail!("set WAMN_KIND_REUSE and WAMN_KIND_FROM together"),
    }
}

pub fn missing<'a>(step: &'a Step, completed: &[String]) -> Option<&'a str> {
    step.needs
        .iter()
        .copied()
        .find(|name| !completed.iter().any(|done| done == name))
}

pub fn resume(index: usize, completed: &mut Vec<String>) -> anyhow::Result<()> {
    if let Some(name) = missing(&STEPS[index], completed) {
        anyhow::bail!(
            "step {} needs missing completed step {name}",
            STEPS[index].name
        );
    }
    // Later outputs belong to the previous attempt, not this rerun.
    completed.retain(|name| STEPS[..index].iter().any(|step| step.name == name));
    Ok(())
}

pub fn unchanged_versions(
    before: &[(String, Value)],
    after: &[(String, Value)],
) -> anyhow::Result<()> {
    let mut failures = wamn_gate_harness::Failures::default();
    for (name, before) in before {
        failures.record(
            name,
            (|| {
                let (_, after) = after
                    .iter()
                    .find(|(key, _)| key == name)
                    .with_context(|| format!("Secret {name} disappeared"))?;
                ensure!(
                    before["metadata"]["resourceVersion"] == after["metadata"]["resourceVersion"],
                    "unchanged apply rewrote Secret {name}"
                );
                Ok(())
            })(),
        );
    }
    failures.finish()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    pub version: String,
    pub document: ClusterDocument,
    pub artifacts: PackageRegistry,
    pub environment: EnvironmentDocument,
    pub wms_environment: EnvironmentDocument,
    pub host_group: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Data {
    pub first: Vec<String>,
    pub digest: String,
    pub wms_digest: String,
    pub publication_log: String,
    pub refused_lines: Vec<String>,
    pub second: Vec<String>,
    pub secrets: Vec<(String, Value)>,
    pub applied: Vec<String>,
    pub https_before: Vec<Value>,
    pub edge: Value,
    pub invite_log: String,
    pub mail: String,
    pub invite_modified: Option<SystemTime>,
    pub control_secrets: Vec<(String, Value)>,
    pub repeated: Vec<String>,
}

/// The charts a rerun stamps again: the fixture chart and the install chart, whose
/// runner image runs the provisioning Jobs. The install keeps its chart while the
/// registry and version match, so a rerun must replace it.
pub fn rerun_charts(directory: &Path, work: &Path) -> [PathBuf; 2] {
    [directory.join("fixture-chart"), work.join("release-chart")]
}

/// The copied platform images whose digest in `fixture` is not the digest in `source`.
pub fn stale_images(source: &Value, fixture: &Value) -> Vec<&'static str> {
    const IMAGES: [&str; 5] = [
        "/runtime-operator/runtime/image/tag",
        "/platform/runner/image",
        "/platform/gates/image",
        "/platform/identity/image",
        "/platform/edge/image",
    ];
    let digest = |values: &Value, path: &str| {
        values
            .pointer(path)
            .and_then(Value::as_str)
            .and_then(|image| image.split_once('@'))
            .map(|(_, digest)| digest.to_owned())
    };
    IMAGES
        .into_iter()
        .filter(|path| {
            digest(source, path).is_none() || digest(source, path) != digest(fixture, path)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedCheck {
    pub step: String,
    pub check: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Kept {
    pub cluster: String,
    pub kubeconfig: PathBuf,
    pub registry: String,
    pub registry_port: u16,
    pub artifact_registry: String,
    pub artifact_registry_port: u16,
    pub directory: PathBuf,
    pub work: PathBuf,
    pub completed: Vec<String>,
    pub failed_step: Option<String>,
    pub failed: Vec<FailedCheck>,
    pub not_run: Vec<String>,
    pub fixture: Option<Fixture>,
    pub data: Data,
    pub cleanup: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_steps_have_only_earlier_dependencies() {
        for (index, step) in STEPS.iter().enumerate() {
            assert!(
                !STEPS[..index]
                    .iter()
                    .any(|earlier| earlier.name == step.name)
            );
            for dependency in step.needs {
                assert!(
                    STEPS[..index]
                        .iter()
                        .any(|earlier| &earlier.name == dependency),
                    "{} needs {dependency}",
                    step.name
                );
            }
        }
    }

    #[test]
    fn from_selects_suffix_and_refuses_unknown_or_empty_names() -> anyhow::Result<()> {
        for (index, step) in STEPS.iter().enumerate() {
            assert_eq!(
                reuse_from(Some("kept.json"), Some(step.name))?,
                Some(("kept.json", index))
            );
        }
        for name in ["", "unknown"] {
            let error = reuse_from(Some("kept.json"), Some(name))
                .unwrap_err()
                .to_string();
            assert!(error.contains("known names:"));
            for step in STEPS {
                assert!(error.contains(step.name));
            }
        }
        Ok(())
    }

    #[test]
    fn reuse_and_from_must_be_paired() -> anyhow::Result<()> {
        assert!(reuse_from(None, None)?.is_none());
        for (reuse, from) in [
            (Some("kept.json"), None),
            (None, Some(STEPS[0].name)),
            (Some(""), Some(STEPS[0].name)),
        ] {
            assert!(reuse_from(reuse, from).is_err());
        }
        Ok(())
    }

    #[test]
    fn resume_refuses_missing_dependencies_and_invalidates_later_steps() -> anyhow::Result<()> {
        for (index, step) in STEPS.iter().enumerate().skip(1) {
            let mut completed = Vec::new();
            let error = resume(index, &mut completed).unwrap_err().to_string();
            assert!(error.contains(step.needs[0]));
            completed = STEPS.iter().map(|step| step.name.to_owned()).collect();
            resume(index, &mut completed)?;
            assert_eq!(
                completed,
                STEPS[..index]
                    .iter()
                    .map(|step| step.name.to_owned())
                    .collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    #[test]
    fn failed_prerequisite_blocks_only_its_dependents() {
        let mut completed = Vec::new();
        let mut not_run = Vec::new();
        for step in STEPS {
            if missing(step, &completed).is_some() {
                not_run.push(step.name);
            } else if step.name != "qualify-release Receiving" {
                completed.push(step.name.to_owned());
            }
        }
        assert!(
            completed
                .iter()
                .any(|name| name == "installed objects and NATS")
        );
        assert!(
            completed
                .iter()
                .any(|name| name == "4.16.1 WMS integration")
        );
        for name in [
            "4.17 Apply with the release",
            "4.21 HTTPS",
            "unchanged rerun / 4.11 keeps the release",
        ] {
            assert!(not_run.contains(&name));
        }
    }

    #[test]
    fn secret_comparison_records_each_failure_and_matches_names() -> anyhow::Result<()> {
        let a = (
            "a".into(),
            serde_json::json!({"metadata":{"resourceVersion":"1"}}),
        );
        let b = (
            "b".into(),
            serde_json::json!({"metadata":{"resourceVersion":"2"}}),
        );
        unchanged_versions(&[a.clone(), b.clone()], &[b.clone(), a.clone()])?;
        let error = unchanged_versions(
            &[a, b],
            &[(
                "b".into(),
                serde_json::json!({"metadata":{"resourceVersion":"3"}}),
            )],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("Secret a disappeared"));
        assert!(error.contains("rewrote Secret b"));
        Ok(())
    }

    #[test]
    fn kept_json_round_trip_preserves_resume_inputs() -> anyhow::Result<()> {
        let mut kept = Kept {
            cluster: "platform-apply-test".into(),
            kubeconfig: "/var/tmp/platform-apply-test/work/test.kubeconfig".into(),
            registry: "platform-apply-test-registry".into(),
            registry_port: 12345,
            artifact_registry: "platform-apply-test-artifacts".into(),
            artifact_registry_port: 5000,
            directory: "/var/tmp/platform-apply-test".into(),
            work: "/var/tmp/platform-apply-test/work".into(),
            completed: vec![STEPS[0].name.into()],
            failed_step: Some(STEPS[2].name.into()),
            failed: vec![FailedCheck {
                step: STEPS[2].name.into(),
                check: "refusal".into(),
                error: "cause".into(),
            }],
            cleanup: "tools/kind-kept-delete /var/tmp/platform-apply-test/kept.json".into(),
            ..Kept::default()
        };
        kept.data.digest = "sha256:saved-release".into();
        kept.data.invite_modified = Some(SystemTime::UNIX_EPOCH);
        kept.data.secrets = vec![(
            "namespace/secret".into(),
            serde_json::json!({"data":{"key":"private"}}),
        )];
        kept.fixture = Some(Fixture {
            version: "0.1.0".into(),
            document: serde_json::from_value(serde_json::json!({
                "provider":"kind", "project_id":"", "zone":"", "cluster_name":kept.cluster,
                "org":"test", "owner_email":"owner@example.com", "registry":"localhost:12345",
                "buckets":{"web":"", "otel":"", "state":""},
                "route_hosts":{"control":"control.example.com"}, "resend_from":"sender@example.com"
            }))?,
            artifacts: PackageRegistry {
                artifact_base: "172.18.0.3:5000/wamn/packages".into(),
                registry_auth_file: kept.directory.join("artifact-registry/config.json"),
                insecure_registry: false,
                oci_ca_paths: vec![kept.directory.join("artifact-registry/ca.crt")],
            },
            environment: serde_json::from_value(serde_json::json!({
                "org":"test", "project":"receiving", "env":"dev", "target":"kind",
                "release":"none", "route_host":"receiving.example.com", "policy":"dev", "applications":{}
            }))?,
            wms_environment: serde_json::from_value(serde_json::json!({
                "org":"test", "project":"wms", "env":"dev", "target":"kind",
                "release":"none", "route_host":"wms.example.com", "policy":"dev", "applications":{}
            }))?,
            host_group: "host-group-receiving.yaml".into(),
        });
        kept.data.first = vec!["saved publication output".into()];
        kept.data.wms_digest = "sha256:saved-wms".into();
        kept.data.publication_log = "saved Job output".into();
        kept.data.refused_lines = vec!["saved first refusal".into()];
        kept.data.second = vec!["saved repeated refusal".into()];
        kept.data.applied = vec!["4.17 Apply with the release: done 1.0s".into()];
        kept.data.https_before = vec![serde_json::json!("saved-version")];
        kept.data.edge = serde_json::json!({"metadata":{"resourceVersion":"edge-version"}});
        kept.data.invite_log = "saved invitation log".into();
        kept.data.mail = "saved private mail".into();
        kept.data.control_secrets = kept.data.secrets.clone();
        kept.data.repeated = vec!["saved rerun progress".into()];
        kept.not_run = vec!["4.17 Apply with the release".into()];
        let bytes = serde_json::to_vec(&kept)?;
        let decoded: Kept = serde_json::from_slice(&bytes)?;
        assert_eq!(
            serde_json::to_value(&decoded)?,
            serde_json::to_value(&kept)?
        );
        assert_eq!(
            reuse_from(Some("kept.json"), Some(STEPS[2].name))?
                .unwrap()
                .1,
            2
        );
        resume(2, &mut decoded.completed.clone())?;
        Ok(())
    }

    #[test]
    fn a_fixture_chart_with_an_old_digest_is_stale() {
        let chart = |digest: &str| {
            serde_json::json!({
                "runtime-operator": {"runtime": {"image": {"tag": format!("0.1.1@sha256:{digest}")}}},
                "platform": {
                    "runner": {"image": format!("r/wamn-runner:0.1.1@sha256:{digest}")},
                    "gates": {"image": format!("r/wamn-gates:0.1.1@sha256:{digest}")},
                    "identity": {"image": format!("r/wamn-identity:0.1.1@sha256:{digest}")},
                    "edge": {"image": format!("r/wamn-edge-web:0.1.1@sha256:{digest}")},
                },
            })
        };
        assert!(stale_images(&chart("new"), &chart("new")).is_empty());
        let mut fixture = chart("new");
        fixture["platform"]["gates"]["image"] = "localhost:1/wamn-gates:0.1.1@sha256:old".into();
        assert_eq!(
            stale_images(&chart("new"), &fixture),
            ["/platform/gates/image"]
        );
        assert_eq!(stale_images(&serde_json::json!({}), &fixture).len(), 5);
        let mut install = chart("new");
        install["platform"]["runner"]["image"] = "localhost:1/wamn-runner:0.1.1@sha256:old".into();
        assert_eq!(
            stale_images(&chart("new"), &install),
            ["/platform/runner/image"]
        );
    }

    #[test]
    fn a_rerun_restamps_the_install_chart() {
        let charts = rerun_charts(Path::new("/var/tmp/p"), Path::new("/var/tmp/p/work"));
        assert!(charts.contains(&PathBuf::from("/var/tmp/p/work/release-chart")));
        assert!(charts.contains(&PathBuf::from("/var/tmp/p/fixture-chart")));
    }
}

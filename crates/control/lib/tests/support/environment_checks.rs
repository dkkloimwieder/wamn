//! Environment fixture decisions and retained metadata.
use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use wamn_control::release_chart::ImageSet;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub cluster: String,
    pub kubeconfig: PathBuf,
    pub registry: String,
    pub work: PathBuf,
    pub secret_work: PathBuf,
    pub failed: Vec<String>,
    pub phases: Vec<String>,
    pub images: BTreeMap<String, String>,
    pub role_inputs: BTreeMap<String, String>,
    pub image_set: Option<ImageSet>,
    pub registry_address: String,
    pub release_digest: String,
    pub releases: BTreeMap<String, Vec<u8>>,
    pub file_setup_error: Option<String>,
    pub identity_ready: bool,
    pub fixture_source: String,
    pub brokers: BTreeMap<String, PathBuf>,
    pub cleanup: String,
}

#[path = "kind_checks.rs"]
mod kind_checks;
pub use kind_checks::keep_on_failure;

pub fn cluster_name(repository: &Path) -> String {
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        repository.as_os_str().as_encoded_bytes(),
    );
    format!("wamn-apply-{}", &hex::encode(digest.as_ref())[..8])
}

#[derive(Debug)]
pub struct Check<F> {
    pub name: &'static str,
    pub phase: &'static str,
    pub run: F,
}

pub const fn checks<F: Copy>(runs: [F; 7]) -> [Check<F>; 7] {
    [
        Check {
            name: "4 rest_connector",
            phase: "environment",
            run: runs[0],
        },
        Check {
            name: "4 netsuite",
            phase: "environment",
            run: runs[1],
        },
        Check {
            name: "native files on kind MinIO",
            phase: "environment",
            run: runs[2],
        },
        Check {
            name: "14 / A1 heartbeat labels",
            phase: "release",
            run: runs[3],
        },
        Check {
            name: "15 wrong release placement",
            phase: "release",
            run: runs[4],
        },
        Check {
            name: "F2 failed host report",
            phase: "release",
            run: runs[5],
        },
        Check {
            name: "4 show reapply",
            phase: "bindings",
            run: runs[6],
        },
    ]
}

pub type SelectedChecks<'a, F> = Vec<&'a Check<F>>;

pub fn select_checks<'a, F>(
    checks: &'a [Check<F>],
    value: &str,
) -> anyhow::Result<SelectedChecks<'a, F>> {
    let mut selected = Vec::new();
    for name in value.split(',').map(str::trim) {
        let check = checks
            .iter()
            .find(|check| check.name == name)
            .with_context(|| {
                format!(
                    "unknown or empty WAMN_KIND_CHECKS name {name:?}: known names: {}",
                    checks
                        .iter()
                        .map(|check| check.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        if !selected.iter().any(|entry: &&Check<F>| entry.name == name) {
            selected.push(check);
        }
    }
    Ok(selected)
}

pub fn require_phases<F>(checks: &[&Check<F>], phases: &[String]) -> anyhow::Result<()> {
    for check in checks {
        ensure!(
            phases.iter().any(|phase| phase == check.phase),
            "check {} needs missing phase {}",
            check.name,
            check.phase
        );
    }
    Ok(())
}

pub fn reuse_checks<'a, F>(
    available: &'a [Check<F>],
    reuse: Option<&'a str>,
    checks: Option<&str>,
) -> anyhow::Result<Option<(&'a str, SelectedChecks<'a, F>)>> {
    match (reuse, checks) {
        (None, None) => Ok(None),
        (Some(path), Some(names)) => {
            ensure!(!path.is_empty(), "WAMN_KIND_REUSE must name kept.json");
            Ok(Some((path, select_checks(available, names)?)))
        }
        _ => anyhow::bail!("set WAMN_KIND_REUSE and WAMN_KIND_CHECKS together"),
    }
}

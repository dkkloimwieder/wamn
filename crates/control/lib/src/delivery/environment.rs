//! The environment file of an installed environment
//! (docs/plan/upgrade-environment.md §4.1).
//!
//! `deploy/gcp/environments/<org>--<project>--<env>.json` holds only what
//! `registry.project_envs` does not hold. The tenant and the database names
//! come from the registry, so the file does not repeat them. It holds no
//! credential.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde::Deserialize;

/// The directory of the environment files, relative to the repository.
pub const ENVIRONMENTS: &str = "deploy/gcp/environments";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentFile {
    /// The Kubernetes cluster.
    pub cluster: String,
    /// The kubeconfig context of the cluster.
    pub context: String,
    /// The `<registry>/<repository>` of the images and the release artifacts.
    pub registry: String,
    pub system_database: SystemDatabase,
    /// The package roots that the environment publishes, relative to the
    /// repository. The registry names no package directory.
    pub packages: Vec<PathBuf>,
    /// Package IDs mapped to serving WorkloadDeployment names. This is topology
    /// only. Qualification reads runtime schemas from the live resources.
    pub package_workloads: BTreeMap<String, String>,
    pub web_client: WebClient,
    pub route_host: String,
    /// The host group of the application in the host values.
    pub host_group: String,
    pub edge: Edge,
    pub url_map: UrlMap,
    /// The guest workload files, relative to the repository.
    pub workloads: Vec<PathBuf>,
    /// The pinned gates image of `qualify-release`, with its digest.
    pub gates_image: String,
}

/// The CloudNativePG cluster and database of `wamn_system`. The registry
/// cannot name its own location. The superuser Secret is `<cluster>-superuser`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SystemDatabase {
    pub namespace: String,
    pub cluster: String,
    pub database: String,
}

/// The bucket and prefix that `wamn web upload` writes the client to.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebClient {
    pub bucket: String,
    pub prefix: String,
}

/// The Helm release of the edge and its values file.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub release: String,
    pub namespace: String,
    pub values: PathBuf,
}

/// The load balancer URL map and the file that it is imported from.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UrlMap {
    pub name: String,
    pub file: PathBuf,
}

impl EnvironmentFile {
    /// The path of the environment file of one project environment.
    pub fn path(repository: &Path, org: &str, project: &str, environment: &str) -> PathBuf {
        repository
            .join(ENVIRONMENTS)
            .join(format!("{org}--{project}--{environment}.json"))
    }

    /// Reads the environment file, and refuses an environment without one.
    pub fn read(
        repository: &Path,
        org: &str,
        project: &str,
        environment: &str,
    ) -> anyhow::Result<Self> {
        let path = Self::path(repository, org, project, environment);
        let bytes = std::fs::read(&path).with_context(|| {
            format!(
                "the environment {org}/{project}/{environment} has no environment file {}",
                path.display()
            )
        })?;
        let file: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("read the environment file {}", path.display()))?;
        ensure!(
            file.gates_image.contains("@sha256:"),
            "the gates image of {} is not pinned by digest",
            path.display()
        );
        ensure!(
            !file.packages.is_empty(),
            "the environment file {} names no package",
            path.display()
        );
        let named = file
            .workloads
            .iter()
            .cloned()
            .chain([file.edge.values.clone(), file.url_map.file.clone()]);
        for named in named {
            ensure!(
                repository.join(&named).is_file(),
                "the environment file {} names the absent file {}",
                path.display(),
                named.display()
            );
        }
        for root in &file.packages {
            ensure!(
                repository.join(root).is_dir(),
                "the environment file {} names the absent package {}",
                path.display(),
                root.display()
            );
        }
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root")
            .to_path_buf()
    }

    #[test]
    fn the_wamn_dev_environments_read_and_differ_only_in_the_application() {
        let receiving =
            EnvironmentFile::read(&repository(), "dkk", "receiving", "dev").expect("Receiving");
        let wms = EnvironmentFile::read(&repository(), "dkk", "wms", "dev").expect("WMS");
        assert_eq!(receiving.route_host, "receiving.wamn.dev");
        assert_eq!(wms.route_host, "wms.wamn.dev");
        assert_eq!(
            receiving.package_workloads,
            BTreeMap::from([("wamn_receiving".to_owned(), "flow-http".to_owned())])
        );
        assert_eq!(
            wms.package_workloads,
            BTreeMap::from([("wamn_wms".to_owned(), "wms-flow-http".to_owned())])
        );
        assert_eq!(
            (receiving.host_group.as_str(), wms.host_group.as_str()),
            ("default", "wms")
        );
        assert_eq!(
            EnvironmentFile {
                route_host: wms.route_host.clone(),
                host_group: wms.host_group.clone(),
                workloads: wms.workloads.clone(),
                packages: wms.packages.clone(),
                package_workloads: wms.package_workloads.clone(),
                ..receiving
            },
            wms
        );
    }

    #[test]
    fn an_environment_without_a_file_is_refused() {
        let error = EnvironmentFile::read(&repository(), "dkk", "receiving", "absent")
            .expect_err("no file");
        assert!(
            format!("{error:#}")
                .contains("the environment dkk/receiving/absent has no environment file"),
            "{error:#}"
        );
    }
}

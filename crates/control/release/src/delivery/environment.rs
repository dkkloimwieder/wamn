//! The environment file of an installed environment
//! (docs/history/upgrade-environment.md §4.1).
//!
//! `deploy/gcp/environments/<org>--<project>--<env>.json` holds only what
//! `registry.project_envs` does not hold. The tenant and the database names
//! come from the registry, so the file does not repeat them. It holds no
//! credential.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentFile {
    /// The Kubernetes cluster.
    pub cluster: String,
    /// The kubeconfig context of the cluster.
    pub context: String,
    /// The `<registry>/<repository>` of images, releases, and package artifacts.
    pub registry: String,
    pub system_database: SystemDatabase,
    /// The package roots that the environment publishes, relative to the
    /// repository. The registry names no package directory.
    pub packages: Vec<PathBuf>,
    pub web_client: WebClient,
    pub route_host: String,
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

/// The bucket and prefix for package web uploads. The platform UI uses its
/// fixed bucket and platform version, independently of this environment.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebClient {
    pub bucket: String,
    pub prefix: String,
}

//! Provider-specific kubeconfig creation in a caller-owned file.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Output, Stdio};

use anyhow::{Context as _, ensure};
use tokio::process::Command;

use super::document::{ClusterDocument, Provider};

impl Provider {
    /// Write the cluster's kubeconfig and return its context name.
    ///
    /// # Errors
    ///
    /// Returns an error if a command fails or the kind cluster list is not UTF-8.
    pub async fn kubeconfig(
        self,
        document: &ClusterDocument,
        path: &Path,
    ) -> anyhow::Result<String> {
        match self {
            Self::Gcp => {
                run(Command::new("gcloud")
                    .args(gcp_arguments(document))
                    .env("KUBECONFIG", path))
                .await?;
            }
            Self::Kind => {
                let output = run(Command::new("kind").args(kind_list_arguments())).await?;
                let clusters =
                    std::str::from_utf8(&output.stdout).context("decode kind get clusters")?;
                run(Command::new("kind").args(kind_arguments(
                    &document.cluster_name,
                    path,
                    clusters,
                )))
                .await?;
            }
        }
        Ok(context_name(self, document))
    }
}

fn gcp_arguments(document: &ClusterDocument) -> [&str; 8] {
    [
        "container",
        "clusters",
        "get-credentials",
        &document.cluster_name,
        "--zone",
        &document.zone,
        "--project",
        &document.project_id,
    ]
}

fn kind_list_arguments() -> [&'static str; 2] {
    ["get", "clusters"]
}

fn kind_arguments(cluster_name: &str, path: &Path, clusters: &str) -> [OsString; 6] {
    let (verb, object) = if clusters.lines().any(|name| name.trim() == cluster_name) {
        ("export", "kubeconfig")
    } else {
        ("create", "cluster")
    };
    [
        verb.into(),
        object.into(),
        "--name".into(),
        cluster_name.into(),
        "--kubeconfig".into(),
        path.as_os_str().to_owned(),
    ]
}

fn context_name(provider: Provider, document: &ClusterDocument) -> String {
    match provider {
        Provider::Gcp => format!(
            "gke_{}_{}_{}",
            document.project_id, document.zone, document.cluster_name
        ),
        Provider::Kind => format!("kind-{}", document.cluster_name),
    }
}

async fn run(command: &mut Command) -> anyhow::Result<Output> {
    let name = std::iter::once(command.as_std().get_program())
        .chain(command.as_std().get_args())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let output = command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("run {name}"))?;
    ensure!(
        output.status.success(),
        "{name} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{context_name, gcp_arguments, kind_arguments, kind_list_arguments};
    use crate::platform::document::{Buckets, ClusterDocument, Provider, RouteHosts};
    use std::ffi::OsString;
    use std::path::Path;

    fn document(provider: Provider) -> ClusterDocument {
        ClusterDocument {
            provider,
            project_id: "test-project".into(),
            zone: "us-central1-a".into(),
            cluster_name: "install-test".into(),
            org: "test".into(),
            owner_email: "owner@example.com".into(),
            registry: "localhost:5000".into(),
            buckets: Buckets {
                web: String::new(),
                otel: String::new(),
            },
            route_hosts: RouteHosts {
                control: "control.example.com".into(),
            },
            resend_from: "sender@example.com".into(),
        }
    }

    #[test]
    fn gcp_only_gets_credentials_and_returns_the_gke_context() {
        let document = document(Provider::Gcp);
        assert_eq!(
            gcp_arguments(&document),
            [
                "container",
                "clusters",
                "get-credentials",
                "install-test",
                "--zone",
                "us-central1-a",
                "--project",
                "test-project",
            ]
        );
        assert_eq!(
            context_name(document.provider, &document),
            "gke_test-project_us-central1-a_install-test"
        );
    }

    #[test]
    fn kind_creates_only_when_the_exact_cluster_is_absent() {
        assert_eq!(kind_list_arguments(), ["get", "clusters"]);
        for clusters in ["", "install-test-other\nother-install-test\n"] {
            assert_eq!(
                kind_arguments(
                    "install-test",
                    Path::new("/var/tmp/install kubeconfig"),
                    clusters
                ),
                [
                    "create",
                    "cluster",
                    "--name",
                    "install-test",
                    "--kubeconfig",
                    "/var/tmp/install kubeconfig",
                ]
                .map(OsString::from)
            );
        }
    }

    #[test]
    fn kind_exports_on_a_second_run_and_returns_the_kind_context() {
        let document = document(Provider::Kind);
        assert_eq!(
            kind_arguments(
                &document.cluster_name,
                Path::new("/var/tmp/install kubeconfig"),
                "other\ninstall-test\n",
            ),
            [
                "export",
                "kubeconfig",
                "--name",
                "install-test",
                "--kubeconfig",
                "/var/tmp/install kubeconfig",
            ]
            .map(OsString::from)
        );
        assert_eq!(
            context_name(document.provider, &document),
            "kind-install-test"
        );
    }
}

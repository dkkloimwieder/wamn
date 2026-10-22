//! Install the org control host and its first-install credentials.

use std::path::Path;

use anyhow::Context as _;
use serde::Serialize;
use serde_json::json;

use crate::{kubectl, release_chart::Target};

use super::document::ClusterDocument;
use super::job::{self, Access, JobRequest, PolicyRule, SecretMount};

// Neither command output nor an API error can copy credential bytes to a log.
const PROVISION: &str = r#"prepare() {
  umask 077
  scratch=$(mktemp -d)
  trap 'rm -rf "$scratch"' EXIT
  database=$(cat /etc/wamn/postgres/uri)
  export WAMN_SYSTEM_ADMIN_URL="${database%/*}/wamn_system"
  if [ -z "$(kubectl -n hosts get secret "wamn-control-$ORG" --ignore-not-found -o name)" ]; then
    /usr/local/bin/wamn-ctl provision-org --org "$ORG" --template trials --pool wamn-pg \
      --admin "$OWNER_EMAIL" --prepare-control-generation a --namespace hosts \
      --db-host "$DATABASE_HOST" --emit-control-secret "$scratch/control.json" || return 1
    kubectl apply --server-side -f "$scratch/control.json" || return 1
  fi
  if [ -z "$(kubectl -n hosts get secret "wamn-control-administration-$ORG" --ignore-not-found -o name)" ]; then
    /usr/local/bin/wamn-ctl provision-org --org "$ORG" --template trials --pool wamn-pg \
      --admin "$OWNER_EMAIL" --namespace hosts \
      --emit-control-administration-secret "$scratch/administration.json" || return 1
    kubectl create -f "$scratch/administration.json" || return 1
  fi
}
prepare >/dev/null 2>/dev/null || { echo 'prepare control credentials failed' >&2; exit 1; }
"#;

const COPY_OPERATOR: &str = r#"copy_operator() {
  umask 077
  scratch=$(mktemp -d)
  trap 'rm -rf "$scratch"' EXIT
  kubectl -n identity get secret "$OPERATOR" -o json > "$scratch/source.json" || return 1
  kubectl patch --local --type=json -f "$scratch/source.json" \
    --patch-file /inputs/metadata.json -o json > "$scratch/hosts.json" || return 1
  kubectl apply --server-side -f "$scratch/hosts.json" || return 1
}
copy_operator >/dev/null 2>/dev/null || { echo 'copy operator Secret failed' >&2; exit 1; }
"#;

#[derive(Serialize)]
struct SecretMetadata<'a> {
    name: &'a str,
    namespace: &'a str,
}

#[derive(Serialize)]
struct MetadataPatch<'a> {
    op: &'static str,
    path: &'static str,
    value: SecretMetadata<'a>,
}

fn secret_access(namespace: &str, verbs: &[&str]) -> Access {
    Access {
        namespace: namespace.into(),
        rules: vec![PolicyRule {
            api_groups: vec![String::new()],
            resources: vec!["secrets".into()],
            verbs: verbs.iter().map(|verb| (*verb).into()).collect(),
        }],
    }
}

fn provision_request(document: &ClusterDocument, image: &str) -> JobRequest {
    JobRequest {
        name: format!("platform-control-credentials-{}", document.org),
        image: image.into(),
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec![PROVISION.into()],
        calls_kubernetes: true,
        once: true,
        environment: [
            ("ORG".into(), document.org.clone()),
            ("OWNER_EMAIL".into(), document.owner_email.clone()),
            ("DATABASE_HOST".into(), super::install::DATABASE_HOST.into()),
        ]
        .into(),
        files: [(
            "kubeconfig".into(),
            super::environments::kubeconfig().to_string(),
        )]
        .into(),
        secret_mounts: vec![SecretMount {
            name: "wamn-pg-superuser".into(),
            path: "/etc/wamn/postgres".into(),
            keys: vec!["uri".into()],
        }],
        permissions: vec![secret_access("hosts", &["get", "create", "patch"])],
        ..JobRequest::default()
    }
}

pub(super) async fn credentials(
    target: &Target,
    document: &ClusterDocument,
    directory: &Path,
) -> anyhow::Result<bool> {
    let image = super::install::runner_image(directory)?;
    let mut changed = job::run(
        target,
        directory,
        &provision_request(document, &image),
        &json!({"org": document.org}),
        &[],
    )
    .await?;
    let operator = format!("operator-{}", document.org);
    // Read only metadata on the verb's machine. The Job copies the private data.
    let version = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        "identity",
        &[
            "get",
            "secret",
            &operator,
            "-o",
            "jsonpath={.metadata.resourceVersion}",
        ],
        None,
    )
    .await?;
    anyhow::ensure!(
        !version.is_empty(),
        "operator Secret has no resource version"
    );
    let request = JobRequest {
        name: format!("platform-control-operator-{}", document.org),
        image,
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec![COPY_OPERATOR.into()],
        calls_kubernetes: true,
        environment: [("OPERATOR".into(), operator.clone())].into(),
        files: [(
            "metadata.json".into(),
            serde_json::to_string(&[MetadataPatch {
                op: "replace",
                path: "/metadata",
                value: SecretMetadata {
                    name: &operator,
                    namespace: "hosts",
                },
            }])?,
        )]
        .into(),
        permissions: vec![
            secret_access("identity", &["get"]),
            secret_access("hosts", &["get", "create", "patch"]),
        ],
        ..JobRequest::default()
    };
    changed |= job::run(
        target,
        directory,
        &request,
        &json!({"source_version": version}),
        &[],
    )
    .await
    .context("copy the control operator Secret")?;
    Ok(changed)
}

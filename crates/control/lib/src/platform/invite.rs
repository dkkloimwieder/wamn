//! Invite the document's owner only when no password or pending invitation exists.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};

use crate::{kubectl, release_chart::Target};

use super::document::ClusterDocument;
use super::install::SecretData;
use super::job::{self, Access, JobRequest, PolicyRule, SecretMount};

const STATE: &str = "SELECT EXISTS (
    SELECT FROM identity.principals p WHERE p.type = 'user' AND p.email = :'owner'
    AND (EXISTS (SELECT FROM identity.password_credentials c WHERE c.principal_id = p.id)
      OR EXISTS (SELECT FROM identity.password_tokens t WHERE t.principal_id = p.id
        AND t.purpose = 'invitation' AND t.consumed_at IS NULL AND t.expires_at > clock_timestamp()))
);";

const INVITE: &str = r#"invite_owner() {
  umask 077
  scratch=$(mktemp -d)
  trap 'rm -rf "$scratch"' EXIT
  database=$(cat /etc/wamn/postgres/uri)
  export WAMN_SYSTEM_ADMIN_URL="${database%/*}/wamn_system"
  for key in crt key; do
    kubectl -n identity get secret "operator-$ORG" -o "jsonpath={.data.tls\\.$key}" > "$scratch/encoded" || return 1
    base64 -d "$scratch/encoded" > "$scratch/tls.$key" || return 1
  done
  kubectl -n identity get secret identity-tls -o 'jsonpath={.data.ca\.crt}' > "$scratch/encoded" || return 1
  base64 -d "$scratch/encoded" > "$scratch/ca.crt" || return 1
  export WAMN_PAT_CLIENT_CERT="$scratch/tls.crt" WAMN_PAT_CLIENT_KEY="$scratch/tls.key" WAMN_PAT_SERVER_CA="$scratch/ca.crt"
  /usr/local/bin/wamn-ctl invite --org "$ORG" --email "$OWNER_EMAIL" --display-name "$OWNER_EMAIL" --org-admin
}
invite_owner >/dev/null 2>/dev/null || { echo 'invite owner failed' >&2; exit 1; }
echo 'owner invitation accepted'
"#;

fn postgres_mount() -> SecretMount {
    SecretMount {
        name: "wamn-pg-superuser".into(),
        path: "/etc/wamn/postgres".into(),
        keys: vec!["uri".into(), "password".into()],
    }
}

pub(super) async fn apply(
    target: &Target,
    document: &ClusterDocument,
    directory: &Path,
) -> anyhow::Result<bool> {
    let cluster: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "cluster.postgresql.cnpg.io",
        "wamn-pg",
    )
    .await?
    .context("missing Cluster platform/wamn-pg")?;
    let probe = JobRequest {
        name: format!("platform-owner-state-{}", document.org),
        image: cluster["spec"]["imageName"].as_str().context("PostgreSQL Cluster has no image")?.into(),
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec!["export PGPASSWORD=\"$(cat /etc/wamn/postgres/password)\"; exec psql --no-psqlrc --tuples-only --no-align --set ON_ERROR_STOP=1 --set owner=\"$OWNER_EMAIL\" --file /inputs/state.sql".into()],
        environment: [
            ("PGHOST".into(), super::install::DATABASE_HOST.into()),
            ("PGUSER".into(), "postgres".into()), ("PGDATABASE".into(), "wamn_system".into()),
            ("OWNER_EMAIL".into(), document.owner_email.clone()),
        ].into(),
        files: [("state.sql".into(), STATE.into())].into(),
        secret_mounts: vec![postgres_mount()],
        capture_output: true,
        refresh: true,
        run_as_user: Some(0),
        ..JobRequest::default()
    };
    job::run(
        target,
        directory,
        &probe,
        &json!({"org": document.org}),
        &[],
    )
    .await?;
    match job::output(target, &probe).await?.trim() {
        "t" => return Ok(false),
        "f" => {}
        _ => anyhow::bail!("owner invitation state is not a boolean"),
    }
    // The Job runs in the platform namespace, so it copies the ctl auth client
    // like the package and environment Jobs.
    let auth: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "wamn-ctl-auth-nats",
    )
    .await?
    .context("missing Secret platform/wamn-ctl-auth-nats")?;
    let request = request(document, &super::install::runner_image(directory)?, &auth)?;
    job::run(
        target,
        directory,
        &request,
        &json!({"org": document.org}),
        &[],
    )
    .await
}

pub(super) fn request(
    document: &ClusterDocument,
    image: &str,
    auth: &SecretData,
) -> anyhow::Result<JobRequest> {
    let mut request = JobRequest {
        name: format!("platform-invite-{}", document.org),
        image: image.into(),
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec![INVITE.into()],
        calls_kubernetes: true,
        refresh: true,
        environment: [
            ("ORG".into(), document.org.clone()),
            ("OWNER_EMAIL".into(), document.owner_email.clone()),
            (
                "WAMN_PAT_ISSUER".into(),
                "https://identity.identity.svc.cluster.local".into(),
            ),
        ]
        .into(),
        // The input volume appears only after the Job runner grants its access.
        files: [(
            "kubeconfig".into(),
            super::environments::kubeconfig().to_string(),
        )]
        .into(),
        secret_mounts: vec![postgres_mount()],
        permissions: vec![Access {
            namespace: "identity".into(),
            rules: vec![PolicyRule {
                api_groups: vec![String::new()],
                resources: vec!["secrets".into()],
                verbs: vec!["get".into()],
            }],
        }],
        ..JobRequest::default()
    };
    job::auth_nats_inputs(&mut request, Some(auth))?;
    Ok(request)
}

//! Identity service rendering and key setup.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

use crate::kubectl;
use wamn_release_control::release_chart::Target;

use super::bootstrap::{DATABASE_HOST, SecretData, secret_text};
use super::document::ClusterDocument;
use super::job::{self, JobRequest, JobSecret};
use super::resources::helm;

pub(super) const ISSUER: &str = "https://identity.identity.svc.cluster.local";
const RESEND_COMMAND: &str = "kubectl -n identity create secret generic identity-resend --from-literal=api-key=\"$(tr -d '[:space:]' < <file>)\"";
const RESEND_SECRET_MANAGER: &str = "gcloud secrets versions access latest --secret identity-resend --project <project> must print a nonempty key with no whitespace; see gcp.md section 1";
const CURL_IMAGE: &str =
    "curlimages/curl@sha256:58adaa4e8dca9c988bae2aba4ab3434a0bb2da16bbe3f92dec39ec7785166777";

/// The identity endpoints enabled by this chart input.
#[derive(Debug)]
pub enum Mode {
    /// Key discovery without NATS or session exchange.
    Foundation,
    /// Session exchange with explicit targets and optional revocation transport.
    Session {
        targets: Vec<String>,
        nats: Option<(String, String)>,
    },
}

/// Identity chart inputs shared by platform apply and cluster journeys.
#[derive(Debug)]
pub struct ChartInput<'a> {
    pub chart: &'a Path,
    pub name: &'a str,
    pub namespace: &'a str,
    pub image: &'a str,
    pub pull_policy: &'a str,
    pub issuer: &'a str,
    pub database_secret: &'a str,
    pub tls_secret: &'a str,
    pub operator_ca_secret: &'a str,
    pub resend_secret: &'a str,
    pub resend_from: &'a str,
    pub invite_url: &'a str,
    pub mode: Mode,
}

/// Build the chart entry used by platform apply and local rendering.
///
/// # Errors
/// Returns an error when the image lacks a tag or digest, or the chart lacks a version.
pub fn chart_input(input: &ChartInput<'_>) -> anyhow::Result<Value> {
    let (repository, tag) = input
        .image
        .rsplit_once(':')
        .context("identity image has no tag or digest")?;
    let (targets, nats) = match &input.mode {
        Mode::Foundation => (Vec::new(), None),
        Mode::Session { targets, nats } => (targets.clone(), nats.as_ref()),
    };
    let (nats_url, nats_secret) =
        nats.map_or(("", ""), |(url, secret)| (url.as_str(), secret.as_str()));
    Ok(
        json!({"chart":input.chart, "version":wamn_release_control::release_chart::read_platform_version(&input.chart.join("Chart.yaml"))?,
        "name":input.name, "namespace":input.namespace,
        "values": {"image":{"repository":repository,"tag":tag,"pullPolicy":input.pull_policy},
            "issuer":input.issuer,"databaseSecret":input.database_secret,"tlsSecret":input.tls_secret,
            "operatorCaSecret":input.operator_ca_secret,"resendSecret":input.resend_secret,
            "resendFrom":input.resend_from,"inviteUrl":input.invite_url,
            "authNats":{"url":nats_url,"tlsSecret":nats_secret},"sessionTargetSecrets":targets}}),
    )
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && !key.chars().any(char::is_whitespace)
}

fn resend(secret: Option<&SecretData>) -> anyhow::Result<()> {
    let valid = secret
        .and_then(|secret| secret_text(secret, "api-key").ok())
        .is_some_and(|key| valid_key(&key));
    ensure!(
        valid,
        "owner input: Secret identity/identity-resend needs a nonempty api-key with no whitespace; run the section 1 command:\n{RESEND_COMMAND}"
    );
    Ok(())
}

/// The cluster Secret that carries a key read from the provider's key store.
fn resend_secret(key: &str) -> anyhow::Result<Value> {
    ensure!(valid_key(key), "owner input: {RESEND_SECRET_MANAGER}");
    Ok(json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {"name": "identity-resend", "namespace": "identity"},
        "type": "Opaque",
        "stringData": {"api-key": key},
    }))
}

pub async fn apply(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    directory: &Path,
) -> anyhow::Result<bool> {
    // Where the provider keeps the key, the verb writes the cluster Secret
    // from it, so a teardown loses nothing the owner made (wamn-ie1j2).
    if let Some(key) = document.provider.resend_key(document).await? {
        kubectl::apply(
            &target.kubeconfig,
            &target.context,
            "identity",
            &resend_secret(&key)?,
        )
        .await?;
    }
    let owner = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "identity",
        "secret",
        "identity-resend",
    )
    .await?;
    resend(owner.as_ref())?;
    let identity = Target {
        kubeconfig: target.kubeconfig.clone(),
        context: target.context.clone(),
        namespace: "identity".into(),
    };
    let superuser: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "wamn-pg-superuser",
    )
    .await?
    .context("missing Secret platform/wamn-pg-superuser")?;
    let mut changed = false;
    let chart: Value =
        serde_yaml::from_slice(&std::fs::read(directory.join("release-chart/values.yaml"))?)?;
    let image = chart["platform"]["identity"]["image"]
        .as_str()
        .filter(|image| image.contains("@sha256:"))
        .context("the chart has no pinned identity image")?;
    let batch = chart_objects(repository, document, image, Vec::new())?;
    let values = &batch[0]["values"];
    changed |= install(&identity, directory, &batch[0]).await?;
    let (keys_changed, kid) = keys(target, &identity, directory, &superuser, image).await?;
    changed |= keys_changed;
    let ca: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "identity",
        "secret",
        "identity-tls",
    )
    .await?
    .context("missing Secret identity/identity-tls")?;
    let (check, check_secrets, inputs) = check_inputs(&ca, &kid, values)?;
    let hosts = Target {
        namespace: "hosts".into(),
        ..identity
    };
    changed |= job::run(&hosts, directory, &check, &inputs, &check_secrets).await?;
    let jwks: Value = serde_json::from_str(&job::output(&hosts, &check).await?)
        .context("decode identity public keys")?;
    ensure!(
        jwks["keys"]
            .as_array()
            .is_some_and(|keys| keys.iter().any(|key| key["kid"] == kid)),
        "identity does not serve its active session key"
    );
    Ok(changed)
}

pub(super) fn key_probe(
    image: &str,
    superuser: &SecretData,
    identity_image: &str,
) -> anyhow::Result<job::JobInputs> {
    Ok((
        key_state_request(image),
        vec![JobSecret {
            name: "platform-key-probe".into(),
            key: "password".into(),
            value: secret_text(superuser, "password")?,
            environment: "PGPASSWORD".into(),
        }],
        json!({"issuer":ISSUER,"identity_image":identity_image}),
    ))
}

pub(super) fn key_publish(image: &str, credential: &SecretData) -> anyhow::Result<job::JobInputs> {
    Ok((
        key_publish_request(image),
        vec![JobSecret {
            name: "platform-key-identity".into(),
            key: "url".into(),
            value: secret_text(credential, "url")?,
            environment: "WAMN_IDENTITY_DATABASE_URL".into(),
        }],
        json!({"issuer":ISSUER}),
    ))
}

pub(super) fn key_activate_inputs(image: &str, kid: &str, secrets: &[JobSecret]) -> job::JobInputs {
    (
        key_activate_request(image, kid),
        secrets.to_vec(),
        json!({"issuer":ISSUER,"kid":kid}),
    )
}

pub(super) fn check_inputs(
    ca: &SecretData,
    kid: &str,
    values: &Value,
) -> anyhow::Result<job::JobInputs> {
    Ok((
        check_request(&secret_text(ca, "ca.crt")?),
        Vec::new(),
        json!({"kid":kid,"values":values}),
    ))
}

pub(super) fn chart_objects(
    repository: &Path,
    document: &ClusterDocument,
    image: &str,
    targets: Vec<String>,
) -> anyhow::Result<Vec<Value>> {
    Ok(vec![chart_input(&ChartInput {
        chart: &repository.join("deploy/platform/identity"),
        name: "identity",
        namespace: "identity",
        image,
        pull_policy: "IfNotPresent",
        issuer: ISSUER,
        database_secret: "wamn-identity-db",
        tls_secret: "identity-tls",
        operator_ca_secret: "identity-operator-ca",
        resend_secret: "identity-resend",
        resend_from: &document.resend_from,
        invite_url: &format!("https://{}", document.route_hosts.control),
        mode: Mode::Session {
            targets,
            nats: Some((
                "nats://wamn-nats.platform.svc.cluster.local:4222".into(),
                "wamn-identity-nats".into(),
            )),
        },
    })?])
}

async fn install(target: &Target, directory: &Path, entry: &Value) -> anyhow::Result<bool> {
    let chart = Path::new(entry["chart"].as_str().context("identity chart input")?);
    let version = entry["version"]
        .as_str()
        .context("identity chart version")?;
    let values = &entry["values"];
    let releases: Vec<Value> = serde_json::from_slice(
        &helm(
            target,
            &[
                "list".as_ref(),
                "-n".as_ref(),
                "identity".as_ref(),
                "-o".as_ref(),
                "json".as_ref(),
            ],
        )
        .await?,
    )?;
    let desired = serde_json::to_value(values)?;
    if releases.iter().any(|release| {
        release["name"] == "identity"
            && release["chart"] == format!("wamn-identity-{version}")
            && release["status"] == "deployed"
    }) {
        let current: Value = serde_json::from_slice(
            &helm(
                target,
                &[
                    "get".as_ref(),
                    "values".as_ref(),
                    "identity".as_ref(),
                    "-n".as_ref(),
                    "identity".as_ref(),
                    "-o".as_ref(),
                    "json".as_ref(),
                ],
            )
            .await?,
        )?;
        if current == desired {
            return Ok(false);
        }
    }
    let path = directory.join("values-identity.yaml");
    std::fs::write(&path, serde_yaml::to_string(values)?)?;
    helm(
        target,
        &[
            "upgrade".as_ref(),
            "--install".as_ref(),
            "identity".as_ref(),
            chart.as_os_str(),
            "-n".as_ref(),
            "identity".as_ref(),
            "-f".as_ref(),
            path.as_os_str(),
            "--wait".as_ref(),
            "--timeout".as_ref(),
            "5m".as_ref(),
        ],
    )
    .await?;
    Ok(true)
}

async fn keys(
    target: &Target,
    identity: &Target,
    directory: &Path,
    superuser: &SecretData,
    identity_image: &str,
) -> anyhow::Result<(bool, String)> {
    let cluster: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "cluster.postgresql.cnpg.io",
        "wamn-pg",
    )
    .await?
    .context("missing Cluster platform/wamn-pg")?;
    let image = cluster["spec"]["imageName"]
        .as_str()
        .context("the PostgreSQL Cluster has no image")?;
    let (probe, probe_secrets, inputs) = key_probe(image, superuser, identity_image)?;
    let mut changed = job::run(target, directory, &probe, &inputs, &probe_secrets).await?;
    let state: Value = serde_json::from_str(&job::output(target, &probe).await?)
        .context("decode the public session key state")?;
    if let Some(kid) = state["kid"].as_str() {
        return Ok((changed, kid.into()));
    }
    let credential: SecretData = kubectl::get(
        &identity.kubeconfig,
        &identity.context,
        "identity",
        "secret",
        "wamn-identity-db",
    )
    .await?
    .context("missing Secret identity/wamn-identity-db")?;
    let (publish, secrets, inputs) = key_publish(identity_image, &credential)?;
    changed |= job::run(identity, directory, &publish, &inputs, &secrets).await?;
    let public: Value = serde_json::from_str(&job::output(identity, &publish).await?)
        .context("decode the published session key")?;
    let kid = public["kid"]
        .as_str()
        .context("the published session key has no kid")?
        .to_owned();
    let (activate, secrets, inputs) = key_activate_inputs(identity_image, &kid, &secrets);
    changed |= job::run(identity, directory, &activate, &inputs, &secrets).await?;
    Ok((changed, kid))
}

pub(super) fn check_request(ca: &str) -> JobRequest {
    JobRequest {
        name: "platform-identity-check".into(),
        image: CURL_IMAGE.into(),
        command: vec!["curl".into()],
        arguments: [
            "--fail",
            "--silent",
            "--show-error",
            "--cacert",
            "/inputs/ca.crt",
            "https://identity.identity.svc.cluster.local/.well-known/jwks.json",
        ]
        .map(str::to_owned)
        .into(),
        files: [("ca.crt".into(), ca.into())].into(),
        capture_output: true,
        run_as_user: Some(0),
        ..JobRequest::default()
    }
}

pub(super) fn key_state_request(image: &str) -> JobRequest {
    JobRequest {
        name: "platform-session-key-state".into(), image: image.into(),
        command: vec!["psql".into()],
        arguments: ["--no-psqlrc", "--tuples-only", "--no-align", "--command",
            "SELECT json_build_object('kid', (SELECT active_kid::text FROM identity.session_signing_state WHERE issuer = 'https://identity.identity.svc.cluster.local'))"].map(str::to_owned).into(),
        environment: [("PGHOST".into(), DATABASE_HOST.into()), ("PGUSER".into(), "postgres".into()),
            ("PGDATABASE".into(), "wamn_system".into())].into(),
        capture_output: true,
        ..JobRequest::default()
    }
}

pub(super) fn key_publish_request(identity_image: &str) -> JobRequest {
    JobRequest {
        name: "platform-session-key-publish".into(),
        image: identity_image.into(),
        command: vec!["/usr/local/bin/wamn-identity".into()],
        arguments: vec!["publish".into()],
        environment: [("WAMN_IDENTITY_ISSUER".into(), ISSUER.into())].into(),
        capture_output: true,
        ..JobRequest::default()
    }
}

pub(super) fn key_activate_request(identity_image: &str, kid: &str) -> JobRequest {
    JobRequest {
        name: "platform-session-key-activate".into(),
        image: identity_image.into(),
        command: vec!["/usr/local/bin/wamn-identity".into()],
        arguments: vec!["activate".into(), "--kid".into(), kid.into()],
        environment: [("WAMN_IDENTITY_ISSUER".into(), ISSUER.into())].into(),
        ..JobRequest::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[test]
    fn a_secret_manager_key_refuses_empty_and_whitespace_without_printing_it() {
        for key in ["", "re_key\n", "re key"] {
            let error = resend_secret(key).unwrap_err().to_string();
            assert!(error.contains("owner input"), "{error}");
            if !key.trim().is_empty() {
                assert!(!error.contains(key.trim()), "{error}");
            }
        }
        let secret = resend_secret("re_key").unwrap();
        assert_eq!(secret["metadata"]["name"], "identity-resend");
        assert_eq!(secret["stringData"]["api-key"], "re_key");
    }

    #[test]
    fn resend_refuses_missing_empty_and_whitespace_without_printing_values() {
        assert!(
            resend(None)
                .unwrap_err()
                .to_string()
                .contains(RESEND_COMMAND)
        );
        for value in ["", "private key", "private\n", "\tprivate", "private\u{a0}"] {
            let secret =
                serde_json::from_value(json!({"data":{"api-key":STANDARD.encode(value)}})).unwrap();
            let error = resend(Some(&secret)).unwrap_err().to_string();
            assert!(error.contains(RESEND_COMMAND));
            assert!(!error.contains("private"));
        }
        let secret =
            serde_json::from_value(json!({"data":{"api-key":STANDARD.encode("private")}})).unwrap();
        assert!(resend(Some(&secret)).is_ok());
    }
}

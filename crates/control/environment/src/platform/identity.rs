//! Identity service rendering and key setup.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

use crate::kubectl;
use wamn_release_control::release_chart::{self, Target};

use super::artifact_plan::SelectedChart;
use super::bootstrap::{DATABASE_HOST, SecretData, secret_text};
use super::document::{ClusterDocument, Provider};
use super::job::{self, JobRequest, JobSecret};

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
        Mode::Foundation => (&[][..], None),
        Mode::Session { targets, nats } => (targets.as_slice(), nats.as_ref()),
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
    chart: &SelectedChart,
    directory: &Path,
) -> anyhow::Result<()> {
    // Where the provider keeps the key, the verb writes the cluster Secret
    // from it, so a teardown loses nothing the owner made (wamn-ie1j2).
    if document.provider == Provider::Kind {
        tracing::warn!(
            "optional identity mail is disabled for kind. Invitations are not delivered"
        );
    } else {
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
    }
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
    let image = chart.image("identity")?;
    let entry = platform_chart_input(
        &chart.directory().join("platform/identity"),
        document,
        image,
    )?;
    install(&identity, directory, &entry).await?;
    let active = keys(target, &identity, directory, &superuser, image).await?;
    let ca: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "identity",
        "secret",
        "identity-tls",
    )
    .await?
    .context("missing Secret identity/identity-tls")?;
    let check = check_request(&secret_text(&ca, "ca.crt")?);
    let hosts = Target {
        namespace: "hosts".into(),
        ..identity
    };
    let result = job::run(&hosts, directory, &check, &[]).await?;
    let jwks: Value =
        serde_json::from_str(&result.stdout).context("decode identity public keys")?;
    ensure!(
        jwks["keys"].as_array().is_some_and(|keys| keys
            .iter()
            .any(|key| key["kid"] == active["kid"] && key["x"] == active["x"])),
        "identity does not serve its active session key"
    );
    Ok(())
}

pub(super) fn key_probe(image: &str, superuser: &SecretData) -> anyhow::Result<job::JobInputs> {
    Ok((
        key_state_request(image),
        vec![JobSecret {
            name: "platform-key-probe".into(),
            key: "password".into(),
            value: secret_text(superuser, "password")?,
            environment: "PGPASSWORD".into(),
        }],
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
    ))
}

fn platform_chart_input(
    chart: &Path,
    document: &ClusterDocument,
    image: &str,
) -> anyhow::Result<Value> {
    let mail = document.provider != Provider::Kind;
    chart_input(&ChartInput {
        chart,
        name: "identity",
        namespace: "identity",
        image,
        pull_policy: "IfNotPresent",
        issuer: ISSUER,
        database_secret: "wamn-identity-db",
        tls_secret: "identity-tls",
        operator_ca_secret: "identity-operator-ca",
        resend_secret: if mail { "identity-resend" } else { "" },
        resend_from: if mail { &document.resend_from } else { "" },
        invite_url: &if mail {
            format!("https://{}", document.route_hosts.control)
        } else {
            String::new()
        },
        mode: Mode::Session {
            targets: Vec::new(),
            nats: Some((
                "nats://wamn-nats.platform.svc.cluster.local:4222".into(),
                "wamn-identity-nats".into(),
            )),
        },
    })
}

async fn install(target: &Target, directory: &Path, entry: &Value) -> anyhow::Result<()> {
    let chart = Path::new(entry["chart"].as_str().context("identity chart input")?);
    let values = &entry["values"];
    release_chart::recover_pending(target, "identity").await?;
    let path = directory.join("values-identity.yaml");
    std::fs::write(&path, serde_yaml::to_string(values)?)?;
    release_chart::upgrade(
        target,
        chart,
        "identity",
        &path,
        std::time::Duration::from_secs(300),
        "platform identity service",
    )
    .await
}

async fn keys(
    target: &Target,
    identity: &Target,
    directory: &Path,
    superuser: &SecretData,
    identity_image: &str,
) -> anyhow::Result<Value> {
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
    let (probe, probe_secrets) = key_probe(image, superuser)?;
    let result = job::run(target, directory, &probe, &probe_secrets).await?;
    let mut state: Value =
        serde_json::from_str(&result.stdout).context("decode the public session key state")?;
    if state["active"].is_object() {
        return Ok(state["active"].take());
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
    let (publish, secrets) = key_publish(identity_image, &credential)?;
    let kid = if let Some(kid) = state["published"].as_str() {
        kid.to_owned()
    } else {
        let result = job::run(identity, directory, &publish, &secrets).await?;
        let public: Value =
            serde_json::from_str(&result.stdout).context("decode the published session key")?;
        public["kid"]
            .as_str()
            .context("the published session key has no kid")?
            .to_owned()
    };
    let activate = key_activate_request(identity_image, &kid);
    job::run(identity, directory, &activate, &secrets).await?;
    let result = job::run(target, directory, &probe, &probe_secrets).await?;
    let mut state: Value =
        serde_json::from_str(&result.stdout).context("decode the active session key state")?;
    ensure!(
        state["active"]["kid"] == kid,
        "identity session key activation did not select the published key"
    );
    Ok(state["active"].take())
}

pub(super) fn check_request(ca: &str) -> JobRequest {
    JobRequest {
        name: "platform-identity-check".into(),
        image: CURL_IMAGE.into(),
        command: vec!["curl".into()],
        arguments: [
            "--fail",
            "--fail-early",
            "--silent",
            "--show-error",
            "--cacert",
            "/inputs/ca.crt",
            "--output",
            "/dev/null",
            "https://identity.identity.svc.cluster.local/healthz",
            "--next",
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
        run_as_user: Some(0),
        ..JobRequest::default()
    }
}

pub(super) fn key_state_request(image: &str) -> JobRequest {
    JobRequest {
        name: "platform-session-key-state".into(),
        image: image.into(),
        command: vec!["psql".into()],
        arguments: [
            "--no-psqlrc",
            "--tuples-only",
            "--no-align",
            "--command",
            "SELECT json_build_object(\
                'active', (SELECT json_build_object('kid', k.kid::text, \
                    'x', translate(rtrim(encode(k.public_key, 'base64'), '='), '+/', '-_')) \
                    FROM identity.session_signing_state s JOIN identity.session_keys k \
                    ON (k.issuer, k.kid) = (s.issuer, s.active_kid) \
                    WHERE s.issuer = 'https://identity.identity.svc.cluster.local' \
                    AND k.signing_cutoff IS NULL AND k.private_pkcs8 IS NOT NULL), \
                'published', (SELECT kid::text FROM identity.session_keys \
                    WHERE issuer = 'https://identity.identity.svc.cluster.local' \
                    AND signing_cutoff IS NULL AND private_pkcs8 IS NOT NULL \
                    ORDER BY published_at, kid LIMIT 1))",
        ]
        .map(str::to_owned)
        .into(),
        environment: [
            ("PGHOST".into(), DATABASE_HOST.into()),
            ("PGUSER".into(), "postgres".into()),
            ("PGDATABASE".into(), "wamn_system".into()),
        ]
        .into(),
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
    fn kind_chart_keeps_pinned_identity_tls_and_nats_without_mail_or_targets() {
        let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/platform/identity");
        let document = serde_json::from_value(json!({
            "provider": "kind", "project_id": "", "zone": "",
            "cluster_name": "identity-test", "org": "test",
            "owner_email": "owner@example.invalid", "registry": "localhost:5001",
            "buckets": {"web": "", "otel": "", "state": ""},
            "route_hosts": {"control": "control.example.invalid"},
            "resend_from": "unused@example.invalid"
        }))
        .unwrap();
        for reference in ["localhost:5001/identity", "localhost:5001/identity:build"] {
            let image = format!("{reference}@sha256:{}", "a".repeat(64));
            let entry = platform_chart_input(&chart, &document, &image).unwrap();
            assert_eq!(entry["chart"], chart.to_str().unwrap());
            let values = &entry["values"];
            for field in ["resendSecret", "resendFrom", "inviteUrl"] {
                assert_eq!(values[field], "");
            }
            assert!(
                values["sessionTargetSecrets"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            let objects = super::super::resources::helm_template(&entry).unwrap();
            let deployment = objects
                .iter()
                .find(|object| object["kind"] == "Deployment")
                .unwrap();
            let pod = &deployment["spec"]["template"]["spec"];
            let container = &pod["containers"][0];
            assert_eq!(container["image"], image);
            assert!(
                !container["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|argument| argument == "--session-target")
            );
            let environment = container["env"].as_array().unwrap();
            assert!(!environment.iter().any(|variable| {
                variable["name"].as_str().unwrap().starts_with("RESEND_")
                    || variable["name"] == "WAMN_IDENTITY_INVITE_URL"
            }));
            for (name, value) in [
                ("WAMN_IDENTITY_ISSUER", ISSUER),
                (
                    "WAMN_NATS_URL",
                    "nats://wamn-nats.platform.svc.cluster.local:4222",
                ),
            ] {
                assert_eq!(
                    environment
                        .iter()
                        .find(|variable| variable["name"] == name)
                        .unwrap()["value"],
                    value
                );
            }
            for (secret, path) in [
                ("identity-tls", "/var/run/wamn-identity/tls"),
                ("identity-operator-ca", "/var/run/wamn-identity/operator"),
                ("wamn-identity-nats", "/var/run/wamn-identity/nats"),
            ] {
                let source = pod["volumes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|source| source["secret"]["secretName"] == secret)
                    .unwrap();
                assert_eq!(source["secret"]["optional"], false);
                let mount = container["volumeMounts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|mount| mount["name"] == source["name"])
                    .unwrap();
                assert_eq!(mount["mountPath"], path);
                assert_eq!(mount["readOnly"], true);
                for item in source["secret"]["items"].as_array().unwrap() {
                    let file = format!("{path}/{}", item["path"].as_str().unwrap());
                    assert!(environment.iter().any(|variable| variable["value"] == file));
                }
            }
            let database = environment
                .iter()
                .find(|variable| variable["name"] == "WAMN_IDENTITY_DATABASE_URL")
                .unwrap();
            assert_eq!(
                database["valueFrom"]["secretKeyRef"]["name"],
                "wamn-identity-db"
            );
        }
    }

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

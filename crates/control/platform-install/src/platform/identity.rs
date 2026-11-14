//! Identity service rendering and key setup.

use std::path::Path;
use wamn_kubernetes::job;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

use wamn_kubernetes::kubectl;
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::bootstrap::{SecretData, secret_text};
use super::document::{ClusterDocument, Provider, UpgradeTarget};
use wamn_kubernetes::job::Secret;

/// The platform broker Service used by in-cluster clients.
pub const NATS_URL: &str = "nats://wamn-nats.platform.svc.cluster.local:4222";

pub(crate) fn issuer(namespace: &str) -> String {
    format!("https://identity.{namespace}.svc.cluster.local")
}
const LOCAL_MAIL: &str = "/var/run/wamn-identity/mail/outbox";
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
        targets: Vec<SessionTargetMount>,
        nats: Option<(String, String)>,
    },
}

/// A mounted target and the public database references used by its reader.
#[derive(Debug, serde::Serialize)]
pub struct SessionTargetMount {
    pub name: String,
    pub connection: wamn_postgres::ConnectionConfig,
}

/// Identity chart inputs.
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

/// Build the identity chart entry.
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
    for target in targets {
        ensure!(
            target.connection.credentials.namespace == input.namespace
                && target.connection.server_ca.namespace == input.namespace,
            "session credentials and CA must share the identity namespace"
        );
    }
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
fn resend_secret(namespace: &str, key: &str) -> anyhow::Result<Value> {
    ensure!(valid_key(key), "owner input: {RESEND_SECRET_MANAGER}");
    Ok(json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {"name": "identity-resend", "namespace": namespace},
        "type": "Opaque",
        "stringData": {"api-key": key},
    }))
}

/// Write the mail Secret that the identity release mounts, before that release starts.
///
/// # Errors
/// Returns an error if the provider key or the cluster Secret is absent or invalid.
pub async fn mail(target: &Target, document: &ClusterDocument) -> anyhow::Result<()> {
    // Where the provider keeps the key, the verb writes the cluster Secret
    // from it, so a teardown loses nothing the owner made (wamn-ie1j2).
    if document.provider == Provider::Kind {
        tracing::warn!(
            "email delivery is disabled for kind. Identity writes invitations to its local mail file"
        );
    } else {
        if let Some(key) = document.provider.resend_key(document).await? {
            kubectl::apply(
                &target.kubeconfig,
                &target.context,
                &document.org,
                &resend_secret(&document.org, &key)?,
            )
            .await?;
        }
        let owner = kubectl::get(
            &target.kubeconfig,
            &target.context,
            &document.org,
            "secret",
            "identity-resend",
        )
        .await?;
        resend(owner.as_ref())?;
    }
    Ok(())
}

pub async fn initialize(
    target: &Target,
    document: &ClusterDocument,
    chart: &SelectedChart,
    directory: &Path,
) -> anyhow::Result<()> {
    let identity = Target {
        kubeconfig: target.kubeconfig.clone(),
        context: target.context.clone(),
        namespace: document.org.clone(),
    };
    let image = chart.image("identity")?;
    let active = keys(&identity, directory, image).await?;
    let ca: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        &document.org,
        "secret",
        "identity-tls",
    )
    .await?
    .context("missing Secret identity/identity-tls")?;
    let (mut probe, files) = endpoint_job(&secret_text(&ca, "ca.crt")?, &document.org);
    let hosts = Target {
        namespace: document.org.clone(),
        ..identity
    };
    let result = job::run(&hosts, directory, &mut probe, &mut [files]).await?;
    let jwks: Value = serde_json::from_str(&result).context("decode identity public keys")?;
    ensure!(
        jwks["keys"].as_array().is_some_and(|keys| keys
            .iter()
            .any(|key| key["kid"] == active["kid"] && key["x"] == active["x"])),
        "identity does not serve its active session key"
    );
    invite_owner(target, document, chart, directory, &ca).await?;
    if document.provider == Provider::Kind {
        println!(
            "Local access: run kind export kubeconfig --name {} first.\nLocal invitation file (no email delivery): kubectl --context '{}' -n {} exec deployment/identity -c identity -- cat {LOCAL_MAIL}",
            document.cluster_name,
            target.context.replace('\'', "'\\''"),
            document.org,
        );
    }
    Ok(())
}

async fn invite_owner(
    target: &Target,
    document: &ClusterDocument,
    chart: &SelectedChart,
    directory: &Path,
    ca: &SecretData,
) -> anyhow::Result<()> {
    let platform = Target {
        namespace: document.org.clone(),
        ..target.clone()
    };
    let forward =
        super::database::forward(&platform, &document.org, "wamn-pg", "wamn_system").await?;
    let mut connection = wamn_postgres::connect(&forward.input).await?;
    let enrolled:bool=sqlx::query_scalar("SELECT EXISTS(SELECT FROM identity.principals p WHERE p.type='user' AND p.email=lower(btrim($1)) AND (EXISTS(SELECT FROM identity.password_credentials c WHERE c.principal_id=p.id) OR EXISTS(SELECT FROM identity.password_tokens t WHERE t.principal_id=p.id AND t.purpose='invitation' AND t.consumed_at IS NULL AND t.expires_at>clock_timestamp())))")
        .bind(&document.owner_email).fetch_one(&mut connection).await?;
    if enrolled {
        return Ok(());
    }
    drop(connection);
    drop(forward);
    let operator: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        &document.org,
        "secret",
        &wamn_registry::name::resource_suffix("operator", &document.org),
    )
    .await?
    .context("missing identity operator Secret")?;
    let (mut invitation, files) = invitation_job(document, chart.image("ctl")?, &operator, ca)?;
    super::bootstrap::administrator(&mut invitation, &document.org)?;
    job::run(&platform, directory, &mut invitation, &mut [files]).await?;
    Ok(())
}

fn invitation_job(
    document: &ClusterDocument,
    image: &str,
    operator: &SecretData,
    ca: &SecretData,
) -> anyhow::Result<(Value, Secret)> {
    let mut job = job::manifest(
        &format!("platform-invite-{}", document.org),
        image,
        &["/usr/local/bin/wamn-ctl"],
        &[
            "invite",
            "--org",
            &document.org,
            "--email",
            &document.owner_email,
            "--display-name",
            &document.owner_email,
            "--org-admin",
            "--pat-issuer",
            &issuer(&document.org),
            "--pat-client-cert",
            "/inputs/tls.crt",
            "--pat-client-key",
            "/inputs/tls.key",
            "--pat-server-ca",
            "/inputs/ca.crt",
        ],
    );
    let files = job::files(
        &mut job,
        [
            ("tls.crt".into(), secret_text(operator, "tls.crt")?),
            ("tls.key".into(), secret_text(operator, "tls.key")?),
            ("ca.crt".into(), secret_text(ca, "ca.crt")?),
        ]
        .into(),
    );
    job::environment(
        &mut job,
        &[
            ("WAMN_NATS_URL", NATS_URL),
            ("WAMN_NATS_TLS_CA", "/etc/wamn/nats/ca.crt"),
            ("WAMN_NATS_TLS_CERT", "/etc/wamn/nats/tls.crt"),
            ("WAMN_NATS_TLS_KEY", "/etc/wamn/nats/tls.key"),
        ],
    );
    job::mount(
        &mut job,
        "nats",
        "wamn-ctl-nats",
        "/etc/wamn/nats",
        &["ca.crt", "tls.crt", "tls.key"],
    );
    Ok((job, files))
}

pub(super) fn platform_chart_input(
    chart: &Path,
    document: &ClusterDocument,
    image: &str,
) -> anyhow::Result<Value> {
    let mail = document.provider != Provider::Kind;
    let mut entry = chart_input(&ChartInput {
        chart,
        name: "identity",
        namespace: &document.org,
        image,
        pull_policy: "IfNotPresent",
        issuer: &issuer(&document.org),
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
            nats: Some((NATS_URL.into(), "wamn-identity-nats".into())),
        },
    })?;
    entry["values"]["localMail"] = if mail { "" } else { LOCAL_MAIL }.into();
    Ok(entry)
}

pub(super) fn resource_entry(
    document: &ClusterDocument,
    chart: &SelectedChart,
) -> anyhow::Result<Value> {
    let mut entry = platform_chart_input(
        &chart.directory().join("platform/identity"),
        document,
        chart.image("identity")?,
    )?;
    entry["values"] =
        chart.selection_values(document, &UpgradeTarget::Identity, entry["values"].take())?;
    Ok(entry)
}

/// Read signing state through the existing PostgreSQL primary without a Job.
///
/// # Errors
/// Refuses missing or unreadable state within the complete call time limit.
pub async fn require_keys(target: &Target, limit: std::time::Duration) -> anyhow::Result<()> {
    inspect_keys(target, tokio::time::Instant::now() + limit).await.context(
        "Cannot inspect identity signing state. If no org control release is recorded, run platform deploy \
         without --only to initialize identity. Otherwise restore a verified system database backup. \
         No resources changed.",
    )
}

async fn inspect_keys(target: &Target, deadline: tokio::time::Instant) -> anyhow::Result<()> {
    let state = tokio::time::timeout_at(deadline, key_state(target)).await??;
    ensure!(
        state["active"].is_object(),
        "identity has no active private signing key"
    );
    Ok(())
}

async fn key_state(target: &Target) -> anyhow::Result<Value> {
    let forward =
        super::database::forward(target, &target.namespace, "wamn-pg", "wamn_system").await?;
    let mut connection = wamn_postgres::connect(&forward.input).await?;
    Ok(sqlx::query_scalar("SELECT json_build_object(        'active',(SELECT json_build_object('kid',k.kid::text,'x',translate(rtrim(encode(k.public_key,'base64'),'='),'+/','-_'))          FROM identity.session_signing_state s JOIN identity.session_keys k ON (k.issuer,k.kid)=(s.issuer,s.active_kid)          WHERE s.issuer=$1 AND k.signing_cutoff IS NULL AND k.private_pkcs8 IS NOT NULL),        'published',(SELECT kid::text FROM identity.session_keys WHERE issuer=$1 AND signing_cutoff IS NULL AND private_pkcs8 IS NOT NULL ORDER BY published_at,kid LIMIT 1))")
        .bind(issuer(&target.namespace)).fetch_one(&mut connection).await?)
}

async fn keys(identity: &Target, directory: &Path, identity_image: &str) -> anyhow::Result<Value> {
    let mut state = key_state(identity).await?;
    if state["active"].is_object() {
        return Ok(state["active"].take());
    }
    let config: Value = kubectl::get(
        &identity.kubeconfig,
        &identity.context,
        &identity.namespace,
        "configmap",
        "wamn-identity-db-configuration",
    )
    .await?
    .context("identity connection configuration is absent")?;
    let connection = config["data"]["connection.json"]
        .as_str()
        .context("identity connection data is absent")?;
    let kid = if let Some(kid) = state["published"].as_str() {
        kid.to_owned()
    } else {
        let mut publish = key_publish_job(identity_image, &identity.namespace);
        publish["spec"]["template"]["spec"]["automountServiceAccountToken"] = true.into();
        job::environment(&mut publish, &[("WAMN_IDENTITY_DATABASE_URL", connection)]);
        let result = job::run(identity, directory, &mut publish, &mut []).await?;
        let public: Value = serde_json::from_str(&result)?;
        public["kid"]
            .as_str()
            .context("published key has no id")?
            .to_owned()
    };
    let mut activate = key_activate_job(identity_image, &kid, &identity.namespace);
    activate["spec"]["template"]["spec"]["automountServiceAccountToken"] = true.into();
    job::environment(&mut activate, &[("WAMN_IDENTITY_DATABASE_URL", connection)]);
    job::run(identity, directory, &mut activate, &mut []).await?;
    let mut state = key_state(identity).await?;
    ensure!(
        state["active"].is_object(),
        "identity key activation did not persist"
    );
    Ok(state["active"].take())
}

fn endpoint_job(ca: &str, namespace: &str) -> (Value, Secret) {
    let mut job = job::manifest(
        "platform-identity-probe",
        CURL_IMAGE,
        &["curl"],
        &[
            "--fail",
            "--fail-early",
            "--silent",
            "--show-error",
            "--cacert",
            "/inputs/ca.crt",
            "--output",
            "/dev/null",
            &format!("{}/healthz", issuer(namespace)),
            "--next",
            "--fail",
            "--silent",
            "--show-error",
            "--cacert",
            "/inputs/ca.crt",
            &format!("{}/.well-known/jwks.json", issuer(namespace)),
        ],
    );
    let files = job::files(&mut job, [("ca.crt".into(), ca.into())].into());
    job["spec"]["template"]["spec"]["securityContext"] = json!({"runAsUser":0});
    (job, files)
}

fn key_publish_job(image: &str, namespace: &str) -> Value {
    let mut job = job::manifest(
        "platform-session-key-publish",
        image,
        &["/usr/local/bin/wamn-identity"],
        &["publish"],
    );
    job::environment(&mut job, &[("WAMN_IDENTITY_ISSUER", &issuer(namespace))]);
    job
}

fn key_activate_job(image: &str, kid: &str, namespace: &str) -> Value {
    let mut job = job::manifest(
        "platform-session-key-activate",
        image,
        &["/usr/local/bin/wamn-identity"],
        &["activate", "--kid", kid],
    );
    job::environment(&mut job, &[("WAMN_IDENTITY_ISSUER", &issuer(namespace))]);
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[test]
    fn a_secret_manager_key_refuses_empty_and_whitespace_without_printing_it() {
        for key in ["", "re_key\n", "re key"] {
            assert!(resend_secret("acme", key).is_err());
        }
        assert!(resend_secret("acme", "re_key").is_ok());
    }

    #[test]
    fn resend_refuses_missing_empty_and_whitespace_without_printing_values() {
        assert!(resend(None).is_err());
        for value in ["", "private key", "private\n", "\tprivate", "private\u{a0}"] {
            let secret =
                serde_json::from_value(json!({"data":{"api-key":STANDARD.encode(value)}})).unwrap();
            assert!(resend(Some(&secret)).is_err());
        }
        let secret =
            serde_json::from_value(json!({"data":{"api-key":STANDARD.encode("private")}})).unwrap();
        assert!(resend(Some(&secret)).is_ok());
    }
}

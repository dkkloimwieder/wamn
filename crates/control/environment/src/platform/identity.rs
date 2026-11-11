//! Identity service rendering and key setup.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

use crate::kubectl;
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::bootstrap::{SecretData, database_host, secret_text};
use super::document::{ClusterDocument, Provider, UpgradeTarget};
use super::job::{self, Secret};

/// The platform broker Service used by in-cluster clients.
pub(crate) const NATS_URL: &str = "nats://wamn-nats.platform.svc.cluster.local:4222";

pub(super) const ISSUER: &str = "https://identity.identity.svc.cluster.local";
const LOCAL_MAIL: &str = "/var/run/wamn-identity/mail/outbox";
const OWNER_STATE: &str = "SELECT EXISTS (
    SELECT FROM identity.principals p WHERE p.type = 'user' AND p.email = lower(btrim(:'owner'))
    AND (EXISTS (SELECT FROM identity.password_credentials c WHERE c.principal_id = p.id)
      OR EXISTS (SELECT FROM identity.password_tokens t WHERE t.principal_id = p.id
        AND t.purpose = 'invitation' AND t.consumed_at IS NULL AND t.expires_at > clock_timestamp()))
);";
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
    let (mut check, files) = endpoint_job(&secret_text(&ca, "ca.crt")?);
    let hosts = Target {
        namespace: "hosts".into(),
        ..identity
    };
    let result = job::run(&hosts, directory, &mut check, &mut [files]).await?;
    let jwks: Value = serde_json::from_str(&result).context("decode identity public keys")?;
    ensure!(
        jwks["keys"].as_array().is_some_and(|keys| keys
            .iter()
            .any(|key| key["kid"] == active["kid"] && key["x"] == active["x"])),
        "identity does not serve its active session key"
    );
    invite_owner(target, document, chart, directory, &superuser, &ca).await?;
    if document.provider == Provider::Kind {
        println!(
            "Local access: run kind export kubeconfig --name {} first.\nLocal invitation file (no email delivery): kubectl --context '{}' -n identity exec deployment/identity -c identity -- cat {LOCAL_MAIL}",
            document.cluster_name,
            target.context.replace('\'', "'\\''"),
        );
    }
    Ok(())
}

async fn invite_owner(
    target: &Target,
    document: &ClusterDocument,
    chart: &SelectedChart,
    directory: &Path,
    superuser: &SecretData,
    ca: &SecretData,
) -> anyhow::Result<()> {
    let platform = Target {
        namespace: "platform".into(),
        ..target.clone()
    };
    let cluster: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "cluster.postgresql.cnpg.io",
        "wamn-pg",
    )
    .await?
    .context("missing Cluster platform/wamn-pg")?;
    let (mut probe, files) = owner_state_job(
        document,
        cluster["spec"]["imageName"]
            .as_str()
            .context("PostgreSQL Cluster has no image")?,
    );
    let password = job::credential(
        &mut probe,
        "platform-owner-probe",
        "PGPASSWORD",
        "password",
        &secret_text(superuser, "password")?,
    );
    let result = job::run(&platform, directory, &mut probe, &mut [password, files]).await?;
    match result.trim() {
        "t" => return Ok(()),
        "f" => {}
        _ => anyhow::bail!("owner invitation state is not a boolean"),
    }
    let operator: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "identity",
        "secret",
        &format!("operator-{}", document.org),
    )
    .await?
    .context("missing identity operator Secret")?;
    let (mut invitation, files) = invitation_job(document, chart.image("ctl")?, &operator, ca)?;
    let admin = super::bootstrap::administrator(&mut invitation, superuser)?;
    job::run(&platform, directory, &mut invitation, &mut [admin, files]).await?;
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
            ISSUER,
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

fn owner_state_job(document: &ClusterDocument, image: &str) -> (Value, Secret) {
    let mut job = job::manifest(
        &format!("platform-owner-state-{}", document.org),
        image,
        &["psql"],
        &[
            "--no-psqlrc",
            "--tuples-only",
            "--no-align",
            "--set",
            "ON_ERROR_STOP=1",
            "--set",
            &format!("owner={}", document.owner_email),
            "--file",
            "/inputs/state.sql",
        ],
    );
    job::environment(
        &mut job,
        &[
            ("PGHOST", &database_host()),
            ("PGUSER", "postgres"),
            ("PGDATABASE", "wamn_system"),
        ],
    );
    let files = job::files(&mut job, [("state.sql".into(), OWNER_STATE.into())].into());
    job["spec"]["template"]["spec"]["securityContext"] = json!({"runAsUser":0});
    (job, files)
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
    kubectl::with_timeout(limit, inspect_keys(target)).await.context(
        "Cannot inspect identity signing state. If no org control release is recorded, run platform deploy \
         without --only to initialize identity. Otherwise restore a verified system database backup. \
         No resources changed.",
    )
}

async fn inspect_keys(target: &Target) -> anyhow::Result<()> {
    const RECOVERY: &str = "If no org control release is recorded, run platform deploy without --only \
         to initialize identity. Otherwise restore a verified system database backup. No resources changed.";
    let cluster: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "cluster.postgresql.cnpg.io",
        "wamn-pg",
    )
    .await
    .with_context(|| {
        format!("cannot read Cluster platform/wamn-pg for identity signing state. {RECOVERY}")
    })?
    .with_context(|| {
        format!("missing Cluster platform/wamn-pg for identity signing state. {RECOVERY}")
    })?;
    let primary = cluster["status"]["currentPrimary"]
        .as_str()
        .filter(|primary| !primary.is_empty())
        .with_context(|| {
            format!("missing PostgreSQL primary for identity signing state. {RECOVERY}")
        })?;
    // The server statement limit also applies within the complete inspection limit.
    let query = format!(
        "BEGIN READ ONLY; SET LOCAL statement_timeout = '5s'; \
         SELECT to_json(EXISTS (SELECT FROM identity.session_signing_state s \
         JOIN identity.session_keys k ON (k.issuer, k.kid) = (s.issuer, s.active_kid) \
         WHERE s.issuer = '{ISSUER}' \
         AND k.signing_cutoff IS NULL AND k.private_pkcs8 IS NOT NULL)); COMMIT;"
    );
    let result = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        "platform",
        &[
            "exec",
            primary,
            "-c",
            "postgres",
            "--",
            "psql",
            "--no-psqlrc",
            "--no-password",
            "--username=postgres",
            "--dbname=wamn_system",
            "--quiet",
            "--tuples-only",
            "--no-align",
            "--set",
            "ON_ERROR_STOP=1",
            "--command",
            &query,
        ],
        None,
    )
    .await
    .with_context(|| {
        format!("cannot read identity signing state through the PostgreSQL primary. {RECOVERY}")
    })?;
    let active: bool = serde_json::from_str(&result).with_context(|| {
        format!("cannot decode identity signing state as a boolean. {RECOVERY}")
    })?;
    ensure!(
        active,
        "missing active identity signing key in identity.session_signing_state. {RECOVERY}"
    );
    Ok(())
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
    let mut probe = key_state_job(image);
    let mut password = job::credential(
        &mut probe,
        "platform-key-probe",
        "PGPASSWORD",
        "password",
        &secret_text(superuser, "password")?,
    );
    let result = job::run(
        target,
        directory,
        &mut probe,
        std::slice::from_mut(&mut password),
    )
    .await?;
    let mut state: Value =
        serde_json::from_str(&result).context("decode the public session key state")?;
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

    let kid = if let Some(kid) = state["published"].as_str() {
        kid.to_owned()
    } else {
        let mut publish = key_publish_job(identity_image);
        let secret = job::credential(
            &mut publish,
            "platform-key-identity",
            "WAMN_IDENTITY_DATABASE_URL",
            "url",
            &secret_text(&credential, "url")?,
        );
        let result = job::run(identity, directory, &mut publish, &mut [secret]).await?;
        let public: Value =
            serde_json::from_str(&result).context("decode the published session key")?;
        public["kid"]
            .as_str()
            .context("the published session key has no kid")?
            .to_owned()
    };
    let mut activate = key_activate_job(identity_image, &kid);
    let secret = job::credential(
        &mut activate,
        "platform-key-identity",
        "WAMN_IDENTITY_DATABASE_URL",
        "url",
        &secret_text(&credential, "url")?,
    );
    job::run(identity, directory, &mut activate, &mut [secret]).await?;
    let result = job::run(
        target,
        directory,
        &mut probe,
        std::slice::from_mut(&mut password),
    )
    .await?;
    let mut state: Value =
        serde_json::from_str(&result).context("decode the active session key state")?;
    ensure!(
        state["active"]["kid"] == kid,
        "identity session key activation did not select the published key"
    );
    Ok(state["active"].take())
}

fn endpoint_job(ca: &str) -> (Value, Secret) {
    let mut job = job::manifest(
        "platform-identity-check",
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
            "https://identity.identity.svc.cluster.local/healthz",
            "--next",
            "--fail",
            "--silent",
            "--show-error",
            "--cacert",
            "/inputs/ca.crt",
            "https://identity.identity.svc.cluster.local/.well-known/jwks.json",
        ],
    );
    let files = job::files(&mut job, [("ca.crt".into(), ca.into())].into());
    job["spec"]["template"]["spec"]["securityContext"] = json!({"runAsUser":0});
    (job, files)
}

const KEY_STATE_SQL: &str = "SELECT json_build_object(\
                'active', (SELECT json_build_object('kid', k.kid::text, \
                    'x', translate(rtrim(encode(k.public_key, 'base64'), '='), '+/', '-_')) \
                    FROM identity.session_signing_state s JOIN identity.session_keys k \
                    ON (k.issuer, k.kid) = (s.issuer, s.active_kid) \
                    WHERE s.issuer = 'https://identity.identity.svc.cluster.local' \
                    AND k.signing_cutoff IS NULL AND k.private_pkcs8 IS NOT NULL), \
                'published', (SELECT kid::text FROM identity.session_keys \
                    WHERE issuer = 'https://identity.identity.svc.cluster.local' \
                    AND signing_cutoff IS NULL AND private_pkcs8 IS NOT NULL \
                    ORDER BY published_at, kid LIMIT 1))";

fn key_state_job(image: &str) -> Value {
    let mut job = job::manifest(
        "platform-session-key-state",
        image,
        &["psql"],
        &[
            "--no-psqlrc",
            "--tuples-only",
            "--no-align",
            "--command",
            KEY_STATE_SQL,
        ],
    );
    job::environment(
        &mut job,
        &[
            ("PGHOST", &database_host()),
            ("PGUSER", "postgres"),
            ("PGDATABASE", "wamn_system"),
        ],
    );
    job
}

fn key_publish_job(image: &str) -> Value {
    let mut job = job::manifest(
        "platform-session-key-publish",
        image,
        &["/usr/local/bin/wamn-identity"],
        &["publish"],
    );
    job::environment(&mut job, &[("WAMN_IDENTITY_ISSUER", ISSUER)]);
    job
}

fn key_activate_job(image: &str, kid: &str) -> Value {
    let mut job = job::manifest(
        "platform-session-key-activate",
        image,
        &["/usr/local/bin/wamn-identity"],
        &["activate", "--kid", kid],
    );
    job::environment(&mut job, &[("WAMN_IDENTITY_ISSUER", ISSUER)]);
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[test]
    fn owner_state_uses_a_bound_psql_value_and_a_read_only_query() {
        let document = serde_json::from_value(json!({
            "provider": "kind", "project_id": "", "zone": "",
            "cluster_name": "identity-test", "org": "test",
            "owner_email": "o'brien@example.invalid", "registry": "localhost:5001",
            "buckets": {"web": "", "otel": "", "state": ""},
            "route_hosts": {"control": "control.example.invalid"}, "resend_from": ""
        }))
        .unwrap();
        let (mut job, files) = owner_state_job(&document, "postgres:18");
        let container = job::container(&mut job);
        let arguments: Vec<String> = serde_json::from_value(container["args"].clone()).unwrap();
        assert!(
            arguments
                .windows(2)
                .any(|args| args == ["--set", "ON_ERROR_STOP=1"])
        );
        assert!(
            arguments
                .windows(2)
                .any(|args| args == ["--set", "owner=o'brien@example.invalid"])
        );
        let file = arguments
            .windows(2)
            .find(|args| args[0] == "--file")
            .unwrap();
        let sql = files.0["stringData"][Path::new(&file[1]).file_name().unwrap().to_str().unwrap()]
            .as_str()
            .unwrap();
        assert!(!sql.contains(&document.owner_email));
        let parsed = pg_query::parse(&sql.replace(":'owner'", "$1")).unwrap();
        assert_eq!(parsed.statement_types(), vec!["SelectStmt"]);
        assert!(
            container["env"]
                .as_array()
                .unwrap()
                .iter()
                .any(|env| env == &json!({"name":"PGDATABASE", "value":"wamn_system"}))
        );
        assert!(
            !container["env"]
                .as_array()
                .unwrap()
                .iter()
                .any(|env| env["name"] == "PGPASSWORD")
        );
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

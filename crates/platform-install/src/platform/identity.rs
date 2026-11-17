//! Identity service rendering and key setup.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

use super::artifact_plan::SelectedChart;
use super::document::{ClusterDocument, UpgradeTarget};

/// The platform broker Service used by in-cluster clients.
pub const NATS_URL: &str = "nats://wamn-nats.platform.svc.cluster.local:4222";

pub(crate) fn issuer(namespace: &str) -> String {
    format!("https://identity.{namespace}.svc.cluster.local")
}
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
        json!({"chart":input.chart, "version":crate::release_chart::read_platform_version(&input.chart.join("Chart.yaml"))?,
        "name":input.name, "namespace":input.namespace,
        "values": {"image":{"repository":repository,"tag":tag,"pullPolicy":input.pull_policy},
            "issuer":input.issuer,"databaseSecret":input.database_secret,"tlsSecret":input.tls_secret,
            "operatorCaSecret":input.operator_ca_secret,"resendSecret":input.resend_secret,
            "resendFrom":input.resend_from,"inviteUrl":input.invite_url,
            "authNats":{"url":nats_url,"tlsSecret":nats_secret},"sessionTargetSecrets":targets}}),
    )
}

pub(super) fn platform_chart_input(
    chart: &Path,
    document: &ClusterDocument,
    image: &str,
) -> anyhow::Result<Value> {
    let entry = chart_input(&ChartInput {
        chart,
        name: "identity",
        namespace: document.org.as_str(),
        image,
        pull_policy: "IfNotPresent",
        issuer: &issuer(document.org.as_str()),
        database_secret: "wamn-identity-db",
        tls_secret: "identity-tls",
        operator_ca_secret: "identity-operator-ca",
        resend_secret: "",
        resend_from: "",
        invite_url: "",
        mode: Mode::Session {
            targets: Vec::new(),
            nats: Some((NATS_URL.into(), "wamn-identity-nats".into())),
        },
    })?;
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
pub async fn require_keys(
    input: &str,
    org: &wamn_registry::OrgId,
    limit: std::time::Duration,
) -> anyhow::Result<()> {
    tokio::time::timeout(limit, async {
        let mut connection=wamn_postgres::connect(input).await?;
        let active: bool=sqlx::query_scalar("SELECT EXISTS(SELECT FROM identity.session_signing_state s JOIN identity.session_keys k ON (k.issuer,k.kid)=(s.issuer,s.active_kid) WHERE s.issuer=$1 AND k.signing_cutoff IS NULL AND k.private_key_envelope IS NOT NULL)")
            .bind(issuer(org.as_str())).fetch_one(&mut connection).await?;
        ensure!(active,"identity has no active signing key");
        anyhow::Ok(())
    }).await.context("inspect identity signing state within the call limit")?
}

//! The identity chart and fixture CA used by the integration environment.

use std::path::Path;

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;
use wamn_control::environment::Platform;
use wamn_control::identity_issuer::{IdentityIssuerRequest, provision_identity_issuer};
use wamn_control_provision::CredentialGeneration;

use super::{NAMESPACE, PG_CLUSTER, Run};

pub async fn deploy(
    run: &mut Run,
    repository: &Path,
    platform: &Platform,
) -> anyhow::Result<String> {
    let name = "integration-session";
    let hostname = format!("{name}.{NAMESPACE}.svc.cluster.local");
    let issuer = format!("https://{hostname}");
    if run.reuse && run.kept.identity_ready {
        return Ok(issuer);
    }
    prepare_image(run, &platform.chart).await?;
    let image_repository = format!("{}-identity", run.cluster);

    // As in gate-harness::session_issuer::prepare_application, provision the
    // issuer credential and publish a real key through the identity library.
    let secret = run.kept.secret_work.join("integration-session-db.json");
    provision_identity_issuer(IdentityIssuerRequest {
        issuer: issuer.clone(),
        system_database_url: platform.system_database_url.clone(),
        prepare_generation: Some(CredentialGeneration::A),
        retire_generation: None,
        abort_generation: None,
        emit_secret: Some(secret.clone()),
        db_host: Some(format!("{PG_CLUSTER}-rw.{NAMESPACE}.svc.cluster.local")),
        db_port: 5432,
        namespace: NAMESPACE.into(),
        secret_name: "integration-session-db".into(),
    })
    .await?;
    run.run("kubectl", &["apply", "-f", &secret.to_string_lossy()])
        .await?;
    let credential = wamn_control::provision_project_env::secret_value(&secret, "url")?;
    let mut local = url::Url::parse(&credential)?;
    let forwarded = url::Url::parse(&platform.system_database_url)?;
    local.set_host(forwarded.host_str())?;
    local
        .set_port(forwarded.port())
        .map_err(|()| anyhow::anyhow!("identity database port"))?;
    let (mut database, connection) =
        tokio_postgres::connect(local.as_str(), tokio_postgres::NoTls).await?;
    let task = tokio::spawn(connection);
    let published = async {
        let key = wamn_platform_identity::session_keys::publish_session_key(&mut database, &issuer)
            .await?;
        wamn_platform_identity::session_keys::activate_session_key(&mut database, &issuer, &key.kid)
            .await
    }
    .await;
    task.abort();
    published?;

    // The operator already owns this disposable cluster's CA. cert-manager
    // keeps the serving key in a Secret; hosts receive only its public CA.
    let certificate = run.work.join("integration-session-certificate.json");
    std::fs::write(
        &certificate,
        serde_json::to_vec(&json!({
            "apiVersion":"v1", "kind":"List", "items":[
                {"apiVersion":"cert-manager.io/v1", "kind":"Issuer",
                 "metadata":{"name":name,"namespace":NAMESPACE},
                 "spec":{"ca":{"secretName":"wasmcloud-ca"}}},
                {"apiVersion":"cert-manager.io/v1", "kind":"Certificate",
                 "metadata":{"name":name,"namespace":NAMESPACE},
                 "spec":{"secretName":name,"dnsNames":[hostname],
                    "issuerRef":{"name":name,"kind":"Issuer"}}}
            ]
        }))?,
    )?;
    run.run("kubectl", &["apply", "-f", &certificate.to_string_lossy()])
        .await?;
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "wait",
            "certificate/integration-session",
            "--for=condition=Ready",
            "--timeout=120s",
        ],
    )
    .await?;
    let tls = run
        .json(
            "kubectl",
            &["-n", NAMESPACE, "get", "secret", name, "-o", "json"],
        )
        .await?;
    let ca = String::from_utf8(
        STANDARD.decode(tls["data"]["ca.crt"].as_str().context("identity CA")?)?,
    )?;
    let public_ca = run.work.join("integration-session-ca.json");
    std::fs::write(
        &public_ca,
        serde_json::to_vec(&json!({
            "apiVersion":"v1", "kind":"ConfigMap",
            "metadata":{"name":"integration-session-ca","namespace":NAMESPACE},
            "data":{"ca.crt":ca}
        }))?,
    )?;
    run.run("kubectl", &["apply", "-f", &public_ca.to_string_lossy()])
        .await?;
    let values = run.work.join("integration-session-values.yaml");
    std::fs::write(
        &values,
        serde_yaml::to_string(&json!({
            "issuer":issuer, "databaseSecret":"integration-session-db", "tlsSecret":name,
            "image":{"repository":image_repository,"tag":"debug","pullPolicy":"Never"}
        }))?,
    )?;
    run.run(
        "helm",
        &[
            "upgrade",
            "--install",
            name,
            &repository
                .join("deploy/platform/identity")
                .to_string_lossy(),
            "--kube-context",
            &platform.target.context,
            "-n",
            NAMESPACE,
            "-f",
            &values.to_string_lossy(),
            "--wait",
            "--timeout",
            "180s",
        ],
    )
    .await?;
    run.kept.identity_ready = true;
    Ok(issuer)
}

pub async fn prepare_image(run: &mut Run, chart: &Path) -> anyhow::Result<()> {
    let image = wamn_control::release_chart::qualification_images(chart)
        .await?
        .identity;
    let local_image = format!("{}-identity:debug", run.cluster);
    run.run("docker", &["pull", "--quiet", &image]).await?;
    run.run("docker", &["tag", &image, &local_image]).await?;
    run.load_image(&local_image).await
}

//! Real session issuer for the authentication modes declared by WMS routes.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, ensure};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_control::identity_issuer::{IdentityIssuerRequest, provision_identity_issuer};
use wamn_control::provision_project_env::{
    self, WorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction,
};
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily, workload_secret_name};
use wamn_gate_harness::journey::JourneyDocument;

use super::{CaseContext, checked, deployment, kubectl};
use crate::environment::{ENVIRONMENT, ORG, PROJECT, TENANT};

const IDENTITY: &str = "wms-session-identity";

pub(super) struct Session {
    issuer: String,
    instance: String,
}

pub(super) async fn prepare(
    context: &CaseContext<'_>,
    document: &JourneyDocument,
) -> anyhow::Result<Session> {
    let CaseContext {
        repository,
        cluster,
        work,
        target,
        evidence,
        source_head,
        ..
    } = *context;
    let hostname = format!("{IDENTITY}.{cluster}.svc.cluster.local");
    let issuer = format!("https://{hostname}");
    let (database, task) = wamn_control::dev::environment::connect(&document.system_pg_url).await?;
    let instance: String = database.query_one(
        "SELECT instance_suffix FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
        &[&ORG, &PROJECT, &ENVIRONMENT],
    ).await?.get(0);
    drop(database);
    task.abort();

    // Reuse the native identity binary built by the cluster's retained build.
    let directory = work.join("identity-image");
    fs::create_dir(&directory)?;
    fs::copy(
        target.join("debug/wamn-identity"),
        directory.join("wamn-identity"),
    )?;
    fs::write(
        directory.join("Dockerfile"),
        "FROM debian:trixie-slim\nCOPY --chmod=0755 wamn-identity /usr/local/bin/wamn-identity\nENTRYPOINT [\"/usr/local/bin/wamn-identity\"]\n",
    )?;
    let image = format!("wamn-identity:{cluster}");
    // Record ownership before building so every failure path removes the run tag.
    fs::write(work.join("session-identity-image"), &image)?;
    let output = checked(
        Command::new(repository.join("tools/journey-image-cache"))
            .arg("ensure-context")
            .arg(&directory)
            .args(["identity", source_head, cluster, cluster, "debug"]),
    )
    .await?;
    fs::write(evidence.join("identity-image-build.log"), output)?;
    checked(Command::new("kind").args(["load", "docker-image", &image, "--name", cluster])).await?;

    let secret = work.join("session-identity-db.json");
    provision_identity_issuer(IdentityIssuerRequest {
        issuer: issuer.clone(),
        system_database_url: document.system_pg_url.clone(),
        prepare_generation: Some(CredentialGeneration::A),
        retire_generation: None,
        abort_generation: None,
        emit_secret: Some(secret.clone()),
        namespace: cluster.to_owned(),
        secret_name: "wms-session-identity-db".to_owned(),
    })
    .await?;
    let issuer_url = provision_project_env::secret_value(&secret, "url")?;
    let (mut database, connection) =
        tokio_postgres::connect(&issuer_url, tokio_postgres::NoTls).await?;
    let task = tokio::spawn(connection);
    let key =
        wamn_platform_identity::session_keys::publish_session_key(&mut database, &issuer).await?;
    wamn_platform_identity::session_keys::activate_session_key(&mut database, &issuer, &key.kid)
        .await?;
    drop(database);
    task.abort();
    checked(kubectl(cluster, work).args(["apply", "-f"]).arg(&secret)).await?;

    let target_name = workload_secret_name(
        WorkloadRoleFamily::SessionRoleReader,
        ORG,
        PROJECT,
        ENVIRONMENT,
    );
    let target_file = work.join("session-target.json");
    let mut target_url = reqwest::Url::parse(&document.system_pg_url)?;
    target_url.set_path(&format!(
        "/{}",
        wamn_control_provision::project_env_database_name(ORG, PROJECT, ENVIRONMENT, &instance)
    ));
    provision_project_env::run_workload_action(&WorkloadActionRequest {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        tenant: Some(TENANT.to_owned()),
        system_database_url: Some(document.system_pg_url.clone()),
        target_admin_database_url: Some(target_url.to_string()),
        namespace: cluster.to_owned(),
        action: WorkloadGenerationAction {
            family: WorkloadRoleFamily::SessionRoleReader,
            verb: WorkloadActionVerb::Prepare,
            generation: CredentialGeneration::A,
        },
        secret: Some(target_file.clone()),
        emit_role_sql: None,
    })
    .await?;
    checked(
        kubectl(cluster, work)
            .args(["apply", "-f"])
            .arg(&target_file),
    )
    .await?;

    let ca_key = KeyPair::generate()?;
    let mut ca_params = CertificateParams::new(vec![])?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = ca_params.self_signed(&ca_key)?;
    let ca_issuer = Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate()?;
    let certificate = CertificateParams::new(vec![hostname])?.signed_by(&key, &ca_issuer)?;
    deployment::apply_secret(
        cluster,
        work,
        &json!({"apiVersion":"v1","kind":"Secret",
            "metadata":{"name":"wms-session-tls","namespace":cluster},"type":"kubernetes.io/tls",
            "stringData":{"tls.crt":certificate.pem(),"tls.key":key.serialize_pem()}
        }),
    )
    .await?;
    let public_ca = work.join("session-ca.json");
    fs::write(
        &public_ca,
        serde_json::to_vec(&json!({"apiVersion":"v1","kind":"ConfigMap",
            "metadata":{"name":"wms-session-ca","namespace":cluster},"data":{"ca.crt":ca.pem()}
        }))?,
    )?;
    checked(kubectl(cluster, work).args(["apply", "-f"]).arg(&public_ca)).await?;
    let output = checked(
        Command::new("helm")
            .args(["upgrade", "--install", IDENTITY])
            .arg(repository.join("deploy/platform/identity"))
            .arg("--kubeconfig")
            .arg(work.join("kubeconfig"))
            .args([
                "--kube-context",
                &format!("kind-{cluster}"),
                "--namespace",
                cluster,
            ])
            .args([
                "--set-string",
                &format!("issuer={issuer}"),
                "--set-string",
                "databaseSecret=wms-session-identity-db",
                "--set-string",
                "tlsSecret=wms-session-tls",
                "--set-string",
                "image.repository=wamn-identity",
                "--set-string",
                &format!("image.tag={cluster}"),
                "--set-string",
                "image.pullPolicy=Never",
                "--set-string",
                &format!("sessionTargetSecrets[0]={target_name}"),
                "--wait",
                "--timeout",
                "180s",
            ]),
    )
    .await?;
    fs::write(evidence.join("session-identity-install.log"), output)?;
    Ok(Session { issuer, instance })
}

pub(super) fn configure_host(path: &Path, session: &Session) -> anyhow::Result<()> {
    let mut values: Value = serde_yaml::from_str(&fs::read_to_string(path)?)?;
    let groups = values["runtime"]["hostGroups"]
        .as_array_mut()
        .context("host groups")?;
    ensure!(groups.len() == 1, "the WMS fixture has one host group");
    let group = &mut groups[0];
    for (name, value) in [
        ("WAMN_SESSION_ISSUER", session.issuer.as_str()),
        ("WAMN_SESSION_INSTANCE_SUFFIX", session.instance.as_str()),
        ("WAMN_SESSION_JWKS_CA", "/etc/wms-session-ca/ca.crt"),
    ] {
        group["env"]
            .as_array_mut()
            .context("host env")?
            .push(json!({"name":name,"value":value}));
    }
    group["volumes"]
        .as_array_mut()
        .context("host volumes")?
        .push(json!({"name":"session-ca","configMap":{"name":"wms-session-ca"}}));
    group["volumeMounts"]
        .as_array_mut()
        .context("host mounts")?
        .push(json!({"name":"session-ca","mountPath":"/etc/wms-session-ca","readOnly":true}));
    fs::write(path, serde_yaml::to_string(&values)?)?;
    Ok(())
}

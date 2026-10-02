//! The session issuer that the application hosts of a disposable kind cluster
//! trust. The Receiving and WMS cluster fixtures both start it here.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_control::dev::environment::connect;
use wamn_control::identity_issuer::{IdentityIssuerRequest, provision_identity_issuer};
use wamn_control::provision_project_env::{
    self, WorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction,
};
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily, workload_secret_name};

const IDENTITY: &str = "host-session-identity";

/// One disposable kind cluster and the environment whose hosts trust the issuer.
#[derive(Debug)]
pub struct IssuerCluster<'a> {
    /// The kind cluster name, which is also the namespace.
    pub name: &'a str,
    /// The private work directory that holds `kubeconfig`.
    pub work: &'a Path,
    /// The result directory of the run.
    pub evidence: &'a Path,
    pub repository: &'a Path,
    /// The lifecycle tool of the application cluster tests.
    pub lifecycle: &'a Path,
    /// The source commit that the loaded images carry.
    pub source: &'a str,
    pub system_database_url: &'a str,
    pub identity_image: Option<&'a str>,
    pub org: &'a str,
    pub project: &'a str,
    pub environment: &'a str,
    pub tenant: &'a str,
}

/// Supply the real issuer required by the published application's session routes.
pub async fn prepare_application(
    cluster: &IssuerCluster<'_>,
    manifest_digest: &str,
) -> anyhow::Result<(String, String)> {
    let (database, task) = connect(cluster.system_database_url).await?;
    let instance: String = database.query_one(
        "SELECT instance_suffix FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
        &[&cluster.org, &cluster.project, &cluster.environment],
    ).await?.get(0);
    drop(database);
    task.abort();
    let audience = wamn_control_provision::session_target::session_audience(
        &wamn_control_registry::Triple::new(cluster.org, cluster.project, cluster.environment),
        &instance,
    )?;
    let fixture = cluster.work.join("application-session.json");
    write_private(
        &fixture,
        &serde_json::to_vec(&json!({
            "manifest_digest": manifest_digest,
            "instance_suffix": instance,
            "audience": audience,
        }))?,
    )?;
    let issuer = prepare(cluster, manifest_digest, &fixture).await?;
    let database = wamn_control::provision_project_env::secret_value(
        &cluster.work.join("session-identity-db.json"),
        "url",
    )?;
    let (mut client, connection) =
        tokio_postgres::connect(&database, tokio_postgres::NoTls).await?;
    let driver = tokio::spawn(connection);
    let result = async {
        let key =
            wamn_platform_identity::session_keys::publish_session_key(&mut client, &issuer).await?;
        wamn_platform_identity::session_keys::activate_session_key(&mut client, &issuer, &key.kid)
            .await
    }
    .await;
    driver.abort();
    result?;
    Ok((issuer, instance))
}

/// Start the identity issuer for the release of `manifest_digest`, with the
/// session fixture at `fixture`, and return the issuer URL.
pub async fn prepare(
    cluster: &IssuerCluster<'_>,
    manifest_digest: &str,
    fixture: &Path,
) -> anyhow::Result<String> {
    let fixture_path = fixture;
    let fixture: Value = serde_json::from_slice(&fs::read(fixture)?)?;
    ensure!(
        fixture["manifest_digest"] == manifest_digest,
        "the session fixture differs from the deployed release"
    );
    let issuer = format!("https://{IDENTITY}.{}.svc.cluster.local", cluster.name);
    let identity_image = cluster
        .identity_image
        .context("the session identity image is built")?;
    let identity_digest = image_loaded(cluster, identity_image, "identity", false)
        .await?
        .0;
    let identity_secret = cluster.work.join("session-identity-db.json");
    provision_identity_issuer(IdentityIssuerRequest {
        issuer: issuer.clone(),
        system_database_url: cluster.system_database_url.to_owned(),
        prepare_generation: Some(CredentialGeneration::A),
        retire_generation: None,
        abort_generation: None,
        emit_secret: Some(identity_secret.clone()),
        db_host: url::Url::parse(cluster.system_database_url)?
            .host_str()
            .map(str::to_owned),
        db_port: url::Url::parse(cluster.system_database_url)?
            .port_or_known_default()
            .unwrap_or(5432),
        namespace: cluster.name.to_owned(),
        secret_name: "host-session-identity-db".to_owned(),
    })
    .await?;
    let target_file = cluster.work.join("session-target.json");
    let target_name = workload_secret_name(
        WorkloadRoleFamily::SessionRoleReader,
        cluster.org,
        cluster.project,
        cluster.environment,
    );
    let database = wamn_control_provision::project_env_database_name(
        cluster.org,
        cluster.project,
        cluster.environment,
        text(&fixture, "/instance_suffix")?,
    );
    let mut target_url = url::Url::parse(cluster.system_database_url)?;
    target_url.set_path(&format!("/{database}"));
    provision_project_env::run_workload_action(&WorkloadActionRequest {
        org: cluster.org.to_owned(),
        project: cluster.project.to_owned(),
        env: cluster.environment.to_owned(),
        tenant: Some(cluster.tenant.to_owned()),
        system_database_url: Some(cluster.system_database_url.to_owned()),
        target_admin_database_url: Some(target_url.to_string()),
        cluster: None,
        db_host: target_url.host_str().map(str::to_owned),
        db_port: target_url.port_or_known_default().unwrap_or(5432),
        namespace: cluster.name.to_owned(),
        action: WorkloadGenerationAction {
            family: WorkloadRoleFamily::SessionRoleReader,
            verb: WorkloadActionVerb::Prepare,
            generation: CredentialGeneration::A,
        },
        secret: Some(target_file.clone()),
        emit_role_sql: None,
        control_administration_patch: None,
    })
    .await?;
    for (path, name, key) in [
        (&identity_secret, "host-session-identity-db", "url"),
        (&target_file, target_name.as_str(), "target.json"),
    ] {
        let document: Value = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            document["kind"] == "Secret"
                && document["metadata"]["name"] == name
                && document["metadata"]["namespace"] == cluster.name,
            "the session Secret has the declared name and namespace"
        );
        let data = document["stringData"]
            .as_object()
            .context("the Secret has stringData")?;
        ensure!(
            data.len() == 1 && data.contains_key(key),
            "the session Secret has the declared single key"
        );
        if key == "target.json" {
            let target = wamn_control_provision::session_target::SessionTarget::from_json(
                data[key]
                    .as_str()
                    .context("the session target is JSON text")?
                    .as_bytes(),
            )?;
            ensure!(
                target.audience() == text(&fixture, "/audience")?,
                "the session target audience differs from its fixture"
            );
        }
        apply(cluster, path).await?;
    }
    create_tls(cluster).await?;
    checked(
        kubectl(cluster)
            .args([
                "-n",
                cluster.name,
                "create",
                "secret",
                "tls",
                "host-session-identity-tls",
            ])
            .arg(format!(
                "--cert={}",
                cluster.work.join("session-tls.crt").display()
            ))
            .arg(format!(
                "--key={}",
                cluster.work.join("session-tls.key").display()
            )),
    )
    .await?;
    checked(
        kubectl(cluster)
            .args([
                "-n",
                cluster.name,
                "create",
                "configmap",
                "host-session-public-ca",
            ])
            .arg(format!(
                "--from-file=ca.crt={}",
                cluster.work.join("session-ca.crt").display()
            )),
    )
    .await?;
    // The fixture stays in the private work directory and Kubernetes Secret.
    checked(
        kubectl(cluster)
            .args([
                "-n",
                cluster.name,
                "create",
                "secret",
                "generic",
                "host-session-fixture",
            ])
            .arg(format!(
                "--from-file=fixture.json={}",
                fixture_path.display()
            )),
    )
    .await?;
    let chart = cluster.repository.join("deploy/platform/identity");
    let tagged = identity_image
        .split('@')
        .next()
        .context("identity image name")?;
    let (repository, _) = tagged.rsplit_once(':').context("identity image tag")?;
    let tag = &identity_image[repository.len() + 1..];
    let args = [
        format!("issuer={issuer}"),
        "databaseSecret=host-session-identity-db".to_owned(),
        "tlsSecret=host-session-identity-tls".to_owned(),
        format!("image.repository={repository}"),
        format!("image.tag={tag}"),
        "image.pullPolicy=Never".to_owned(),
        format!("sessionTargetSecrets[0]={target_name}"),
    ];
    for install in [false, true] {
        let mut command = Command::new("helm");
        if install {
            command.args(["upgrade", "--install"]);
        } else {
            command.arg("template");
        }
        command
            .arg(IDENTITY)
            .arg(&chart)
            .arg("--kubeconfig")
            .arg(cluster.work.join("kubeconfig"))
            .arg("--kube-context")
            .arg(format!("kind-{}", cluster.name))
            .args(["--namespace", cluster.name]);
        for value in &args {
            command.arg("--set-string").arg(value);
        }
        if install {
            command.args(["--wait", "--timeout", "180s"]);
        }
        let output = checked(&mut command).await?;
        fs::write(
            cluster.evidence.join(if install {
                "session-identity-install.log"
            } else {
                "session-identity-rendered.yaml"
            }),
            output,
        )?;
    }
    let deployment = read_object(
        cluster,
        "session-identity-deployment",
        &["-n", cluster.name, "get", "deployment", IDENTITY],
    )
    .await?;
    ensure!(
        deployment["spec"]["template"]["spec"]["automountServiceAccountToken"] == false,
        "the identity deployment must not mount a service account token"
    );
    let containers = array(&deployment, "/spec/template/spec/containers")?;
    ensure!(
        containers.len() == 1 && containers[0]["image"] == identity_image,
        "the identity deployment has its exact single image"
    );
    let pods = read_object(
        cluster,
        "session-identity-pods",
        &[
            "-n",
            cluster.name,
            "get",
            "pods",
            "-l",
            "app.kubernetes.io/instance=host-session-identity",
        ],
    )
    .await?;
    let pods = array(&pods, "/items")?;
    ensure!(pods.len() == 1, "one identity pod is required");
    ready_pod(&pods[0], "identity", identity_image, &identity_digest)?;
    Ok(issuer)
}

async fn create_tls(cluster: &IssuerCluster<'_>) -> anyhow::Result<()> {
    let hostname = format!("{IDENTITY}.{}.svc.cluster.local", cluster.name);
    let requests: Vec<(&str, Vec<String>)> = vec![
        (
            "ca",
            [
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=HOST-SESSION disposable CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign,cRLSign",
                "-keyout",
                "session-ca.key",
                "-out",
                "session-ca.crt",
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
        (
            "request",
            vec![
                "req".into(),
                "-new".into(),
                "-newkey".into(),
                "rsa:2048".into(),
                "-nodes".into(),
                "-subj".into(),
                format!("/CN={IDENTITY}"),
                "-keyout".into(),
                "session-tls.key".into(),
                "-out".into(),
                "session-tls.csr".into(),
            ],
        ),
        (
            "sign",
            [
                "x509",
                "-req",
                "-days",
                "1",
                "-in",
                "session-tls.csr",
                "-CA",
                "session-ca.crt",
                "-CAkey",
                "session-ca.key",
                "-CAcreateserial",
                "-extfile",
                "session-tls.ext",
                "-out",
                "session-tls.crt",
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
    ];
    write_private(&cluster.work.join("session-tls.ext"), format!(
        "subjectAltName=DNS:{hostname},DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n"
    ).as_bytes())?;
    for (name, args) in requests {
        let output = Command::new("openssl")
            .current_dir(cluster.work)
            .args(args)
            .output()
            .await?;
        let mut log = output.stdout;
        log.extend(output.stderr);
        fs::write(
            cluster.evidence.join(format!("session-tls-{name}.log")),
            log,
        )?;
        ensure!(
            output.status.success(),
            "the session TLS {name} command failed"
        );
    }
    fs::copy(
        cluster.work.join("session-ca.crt"),
        cluster.evidence.join("session-ca.crt"),
    )?;
    Ok(())
}

/// Fields of a Kubernetes object that the fixture keeps unchanged.
pub type Preserved = BTreeMap<String, serde_yaml::Value>;

#[derive(Serialize, Deserialize)]
struct HostValues {
    runtime: HostRuntime,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Serialize, Deserialize)]
struct HostRuntime {
    #[serde(rename = "hostGroups")]
    groups: Vec<HostGroup>,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Serialize, Deserialize)]
struct HostGroup {
    env: Vec<Env>,
    volumes: Vec<Volume>,
    #[serde(rename = "volumeMounts")]
    mounts: Vec<Mount>,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Env {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(flatten)]
    pub rest: Preserved,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Volume {
    pub name: String,
    #[serde(rename = "configMap", skip_serializing_if = "Option::is_none")]
    pub config_map: Option<Named>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretVolume>,
    #[serde(flatten)]
    pub rest: Preserved,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Named {
    pub name: String,
    #[serde(flatten)]
    pub rest: Preserved,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretVolume {
    pub secret_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<u32>,
    #[serde(flatten)]
    pub rest: Preserved,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mount {
    pub name: String,
    pub mount_path: String,
    pub read_only: bool,
    #[serde(flatten)]
    pub rest: Preserved,
}

pub fn adjust_host(overlay: &str, issuer: &str, instance_suffix: &str) -> anyhow::Result<String> {
    let mut document: HostValues = serde_yaml::from_str(overlay)?;
    ensure!(
        document.runtime.groups.len() == 1,
        "the session overlay has exactly one host group"
    );
    let group = &mut document.runtime.groups[0];
    for (name, value) in [
        ("WAMN_SESSION_ISSUER", issuer),
        ("WAMN_SESSION_INSTANCE_SUFFIX", instance_suffix),
        ("WAMN_SESSION_JWKS_CA", "/etc/host-session-ca/ca.crt"),
    ] {
        ensure!(
            group.env.iter().all(|entry| entry.name != name),
            "the session environment entry already exists: {name}"
        );
        group.env.push(Env {
            name: name.to_owned(),
            value: Some(value.to_owned()),
            rest: Preserved::new(),
        });
    }
    ensure!(
        group
            .volumes
            .iter()
            .all(|volume| volume.name != "host-session-ca")
            && group
                .mounts
                .iter()
                .all(|mount| mount.name != "host-session-ca"),
        "the session CA is already mounted"
    );
    group.volumes.push(ca_volume("host-session-ca"));
    group
        .mounts
        .push(mount("host-session-ca", "/etc/host-session-ca"));
    Ok(serde_yaml::to_string(&document)?)
}

/// A volume of the public session CA ConfigMap.
pub fn ca_volume(name: &str) -> Volume {
    Volume {
        name: name.to_owned(),
        config_map: Some(Named {
            name: "host-session-public-ca".to_owned(),
            rest: Preserved::new(),
        }),
        secret: None,
        rest: Preserved::new(),
    }
}
/// A read-only mount of `name` at `path`.
pub fn mount(name: &str, path: &str) -> Mount {
    Mount {
        name: name.to_owned(),
        mount_path: path.to_owned(),
        read_only: true,
        rest: Preserved::new(),
    }
}

/// The runtime digest and config id that every node reports for a loaded image.
pub async fn image_loaded(
    cluster: &IssuerCluster<'_>,
    image: &str,
    name: &str,
    config_fallback: bool,
) -> anyhow::Result<(String, String)> {
    let labels: Value = serde_json::from_slice(
        &checked(Command::new(cluster.lifecycle).args(["image-labels", image])).await?,
    )?;
    save(cluster, &format!("{name}-image-labels"), &labels)?;
    ensure!(
        labels["wamn.dev/source-head"] == cluster.source
            && labels["wamn.dev/build-profile"] == "release",
        "the session image carries the selected source and build profile"
    );
    let nodes = String::from_utf8(
        checked(Command::new(cluster.lifecycle).args(["nodes", cluster.name])).await?,
    )?;
    let nodes = nodes.lines().collect::<BTreeSet<_>>();
    ensure!(
        nodes.len() == 3,
        "the session image must be loaded on three distinct nodes"
    );
    let loaded = session_image_digest(cluster, image, name, &nodes, config_fallback).await;
    if loaded.is_err() {
        let nodes = nodes.iter().map(|node| (*node).to_owned()).collect();
        wamn_test_infrastructure::workload::capture_node_images(
            cluster.lifecycle,
            &nodes,
            cluster.evidence,
        )
        .await;
    }
    loaded
}

/// The one digest and config id that every node reports for the session image.
async fn session_image_digest(
    cluster: &IssuerCluster<'_>,
    image: &str,
    name: &str,
    nodes: &BTreeSet<&str>,
    config_fallback: bool,
) -> anyhow::Result<(String, String)> {
    let mut expected = None;
    let mut rows = Vec::new();
    for node in nodes {
        let observed: Value = serde_json::from_slice(
            &checked(Command::new(cluster.lifecycle).args(["node-image", node, image])).await?,
        )?;
        save(cluster, &format!("{name}-image-{node}"), &observed)?;
        let tuple = image_tuple(&observed, config_fallback)?;
        if let Some(expected) = &expected {
            ensure!(expected == &tuple, "the session image differs across nodes");
        }
        expected = Some(tuple.clone());
        rows.push(json!({"node":node,"runtime_digest":tuple.0,"config_id":tuple.1}));
    }
    save(cluster, &format!("{name}-image-nodes"), &json!(rows))?;
    expected.context("the session image was observed on a node")
}

fn image_tuple(observed: &Value, config_fallback: bool) -> anyhow::Result<(String, String)> {
    let config = text(observed, "/status/id")?;
    ensure!(
        digest(config),
        "the loaded image has a SHA-256 configuration id"
    );
    let values = observed
        .pointer("/status/repoDigests")
        .and_then(Value::as_array);
    let digests = values
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|value| value.rsplit_once('@').map(|(_, digest)| digest))
        .filter(|value| digest(value))
        .collect::<BTreeSet<_>>();
    let runtime = if digests.is_empty() && config_fallback {
        config
    } else {
        ensure!(
            digests.len() == 1,
            "the loaded session image has one runtime digest"
        );
        digests
            .into_iter()
            .next()
            .context("the runtime digest exists")?
    };
    Ok((runtime.to_owned(), config.to_owned()))
}

fn digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn ready_pod(pod: &Value, container: &str, image: &str, digest: &str) -> anyhow::Result<()> {
    ensure!(
        pod["status"]["phase"] == "Running"
            && array(pod, "/spec/containers")?
                .iter()
                .any(|entry| entry["name"] == container && entry["image"] == image)
            && array(pod, "/status/containerStatuses")?
                .iter()
                .any(|entry| entry["name"] == container
                    && entry["ready"] == true
                    && entry["imageID"]
                        .as_str()
                        .is_some_and(|id| id.ends_with(digest))),
        "the {container} pod runs the selected ready image"
    );
    Ok(())
}
fn text<'a>(value: &'a Value, path: &str) -> anyhow::Result<&'a str> {
    value
        .pointer(path)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("the native observation has text at {path}"))
}
fn array<'a>(value: &'a Value, path: &str) -> anyhow::Result<&'a Vec<Value>> {
    value
        .pointer(path)
        .and_then(Value::as_array)
        .with_context(|| format!("the native observation has an array at {path}"))
}
fn save(cluster: &IssuerCluster<'_>, name: &str, value: &Value) -> anyhow::Result<()> {
    fs::write(
        cluster.evidence.join(format!("{name}.json")),
        serde_json::to_vec_pretty(value)?,
    )?;
    Ok(())
}
async fn read_object(
    cluster: &IssuerCluster<'_>,
    name: &str,
    args: &[&str],
) -> anyhow::Result<Value> {
    let bytes = checked(
        kubectl(cluster)
            .arg("--request-timeout=30s")
            .args(args)
            .args(["-o", "json"]),
    )
    .await?;
    let value = serde_json::from_slice(&bytes)?;
    save(cluster, name, &value)?;
    Ok(value)
}

fn kubectl(cluster: &IssuerCluster<'_>) -> Command {
    let mut command = Command::new("kubectl");
    command
        .arg("--kubeconfig")
        .arg(cluster.work.join("kubeconfig"))
        .arg("--context")
        .arg(format!("kind-{}", cluster.name));
    command
}

async fn apply(cluster: &IssuerCluster<'_>, path: &Path) -> anyhow::Result<()> {
    checked(kubectl(cluster).args(["apply", "-f"]).arg(path)).await?;
    Ok(())
}

async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .context("run the session issuer command")?;
    ensure!(
        output.status.success(),
        "session issuer command failed with {}",
        output.status
    );
    Ok(output.stdout)
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
        .with_context(|| format!("write private file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_overlay_preserves_existing_fields_and_refuses_missing_or_repeated_groups()
    -> anyhow::Result<()> {
        let original = "runtime:\n  hostGroups:\n    - env:\n        - name: EXISTING\n          value: keep\n      volumes:\n        - name: database\n          secret:\n            secretName: database\n            items: [{key: url, path: url}]\n      volumeMounts:\n        - name: database\n          mountPath: /database\n          readOnly: true\n      replicas: 2\n";
        let rendered = adjust_host(original, "https://identity.example", "abcdefgh")?;
        let mut document: HostValues = serde_yaml::from_str(&rendered)?;
        let group = &mut document.runtime.groups[0];
        assert_eq!(group.env.len(), 4);
        assert_eq!(
            group.env[3].value.as_deref(),
            Some("/etc/host-session-ca/ca.crt")
        );
        assert_eq!(group.volumes.len(), 2);
        assert_eq!(group.mounts[1].mount_path, "/etc/host-session-ca");
        group.env.truncate(1);
        group.volumes.truncate(1);
        group.mounts.truncate(1);
        assert_eq!(
            serde_yaml::to_value(document)?,
            serde_yaml::from_str::<serde_yaml::Value>(original)?
        );
        assert!(adjust_host(&rendered, "https://identity.example", "abcdefgh").is_err());
        assert!(
            adjust_host(
                "runtime: {hostGroups: []}",
                "https://identity.example",
                "abcdefgh"
            )
            .is_err()
        );
        assert!(
            adjust_host(
                "runtime: {hostGroups: [{env: []}]}",
                "https://identity.example",
                "abcdefgh"
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn only_session_gates_accept_a_loaded_image_without_a_repository_digest() -> anyhow::Result<()>
    {
        let id = format!("sha256:{}", "a".repeat(64));
        let image = json!({"status":{"id":id,"repoDigests":[]}});
        assert!(image_tuple(&image, false).is_err());
        assert_eq!(image_tuple(&image, true)?, (id.clone(), id.clone()));
        let ambiguous = json!({"status":{"id":id,"repoDigests":[format!("repo@{id}"),format!("repo@sha256:{}", "b".repeat(64))]}});
        assert!(image_tuple(&ambiguous, true).is_err());
        Ok(())
    }
}

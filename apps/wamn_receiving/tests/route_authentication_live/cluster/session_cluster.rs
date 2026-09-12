//! Receiving session setup and checks on two deployed native hosts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::Command;
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily, workload_secret_name};
use wamn_ctl::print_release_env::ReleaseCarrier;
use wamn_ctl::provision_project_env::{
    ProvisionProjectEnvArgs, WorkloadActionVerb, WorkloadGenerationAction, WorkloadGenerationArgs,
};
use wamn_test_infrastructure::rendering::{HttpClaims, HttpWorkloadInput, render_http_workload};
use wamn_test_infrastructure::workload;

use super::super::{ENVIRONMENT, ORG, PROJECT, TENANT, sessions};
use super::resources::{checked, write_private};
use super::{ReceivingCluster, Resources, apply, kubectl};

const IDENTITY: &str = "host-session-identity";
const DEPLOYMENTS: [&str; 2] = ["flow-http-session-a", "flow-http-session-b"];

pub(super) async fn prepare(
    cluster: &ReceivingCluster,
    carrier: &ReleaseCarrier,
    fixture: &Path,
) -> anyhow::Result<String> {
    let resources = &cluster.resources;
    let fixture_path = fixture;
    let fixture: Value = serde_json::from_slice(&fs::read(fixture)?)?;
    ensure!(
        fixture["manifest_digest"] == carrier.manifest_digest.to_string(),
        "the session fixture differs from the deployed release"
    );
    let issuer = format!("https://{IDENTITY}.{}.svc.cluster.local", resources.name);
    let identity_image = resources
        .identity_image
        .as_deref()
        .context("the session identity image is built")?;
    let identity_digest = image_loaded(cluster, identity_image, "identity", false)
        .await?
        .0;
    let gates_image = resources
        .gates_image
        .as_deref()
        .context("the session gates image is built")?;
    image_loaded(cluster, gates_image, "gates", true).await?;
    let identity_secret = resources.work.join("session-identity-db.json");
    wamn_ctl::identity_issuer::run(wamn_ctl::identity_issuer::IdentityIssuerArgs {
        issuer: issuer.clone(),
        system_database_url: cluster.inputs.system_pg_url.clone(),
        prepare_generation: Some(CredentialGeneration::A),
        retire_generation: None,
        abort_generation: None,
        emit_secret: Some(identity_secret.clone()),
        namespace: resources.name.clone(),
        secret_name: "host-session-identity-db".to_owned(),
    })
    .await?;
    let target_file = resources.work.join("session-target.json");
    let target_name = workload_secret_name(
        WorkloadRoleFamily::SessionRoleReader,
        ORG,
        PROJECT,
        ENVIRONMENT,
    );
    let database = wamn_control_provision::project_env_database_name(
        ORG,
        PROJECT,
        ENVIRONMENT,
        text(&fixture, "/instance_suffix")?,
    );
    let mut target_url = reqwest::Url::parse(&cluster.inputs.system_pg_url)?;
    target_url.set_path(&format!("/{database}"));
    wamn_ctl::provision_project_env::run(ProvisionProjectEnvArgs {
        org: Some(ORG.to_owned()),
        project: Some(PROJECT.to_owned()),
        env: Some(ENVIRONMENT.to_owned()),
        tenant: Some(TENANT.to_owned()),
        disposable: false,
        system_database_url: Some(cluster.inputs.system_pg_url.clone()),
        cluster: None,
        connection_limit: None,
        app_password: None,
        app_host: None,
        app_port: 5432,
        namespace: resources.name.clone(),
        secret_namespace: None,
        target_admin_database_url: Some(target_url.to_string()),
        workload: WorkloadGenerationArgs {
            action: Some(WorkloadGenerationAction {
                family: WorkloadRoleFamily::SessionRoleReader,
                verb: WorkloadActionVerb::Prepare,
                generation: CredentialGeneration::A,
            }),
            secret: Some((WorkloadRoleFamily::SessionRoleReader, target_file.clone())),
        },
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
        pat_issuer: Default::default(),
        emit_management_author_pat_secret: None,
        emit_route_caller_pat_secret: None,
        revoke_pat_prefix: None,
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
                && document["metadata"]["namespace"] == resources.name,
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
        apply(resources, path).await?;
    }
    create_tls(cluster).await?;
    checked(
        kubectl(resources)
            .args([
                "-n",
                &resources.name,
                "create",
                "secret",
                "tls",
                "host-session-identity-tls",
            ])
            .arg(format!(
                "--cert={}",
                resources.work.join("session-tls.crt").display()
            ))
            .arg(format!(
                "--key={}",
                resources.work.join("session-tls.key").display()
            )),
    )
    .await?;
    checked(
        kubectl(resources)
            .args([
                "-n",
                &resources.name,
                "create",
                "configmap",
                "host-session-public-ca",
            ])
            .arg(format!(
                "--from-file=ca.crt={}",
                resources.work.join("session-ca.crt").display()
            )),
    )
    .await?;
    // The fixture stays in the private work directory and Kubernetes Secret.
    checked(
        kubectl(resources)
            .args([
                "-n",
                &resources.name,
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
    let chart = resources.repository.join("deploy/platform/identity");
    let args = [
        format!("issuer={issuer}"),
        "databaseSecret=host-session-identity-db".to_owned(),
        "tlsSecret=host-session-identity-tls".to_owned(),
        "image.repository=wamn-identity".to_owned(),
        format!("image.tag={}", resources.name),
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
            .arg(resources.work.join("kubeconfig"))
            .arg("--kube-context")
            .arg(format!("kind-{}", resources.name))
            .args(["--namespace", &resources.name]);
        for value in &args {
            command.arg("--set-string").arg(value);
        }
        if install {
            command.args(["--wait", "--timeout", "180s"]);
        }
        let output = checked(&mut command).await?;
        fs::write(
            resources.evidence.join(if install {
                "session-identity-install.log"
            } else {
                "session-identity-rendered.yaml"
            }),
            output,
        )?;
    }
    let deployment = read_object(
        resources,
        "session-identity-deployment",
        &["-n", &resources.name, "get", "deployment", IDENTITY],
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
        resources,
        "session-identity-pods",
        &[
            "-n",
            &resources.name,
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

async fn create_tls(cluster: &ReceivingCluster) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    let hostname = format!("{IDENTITY}.{}.svc.cluster.local", resources.name);
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
                format!("/CN={hostname}"),
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
    write_private(&resources.work.join("session-tls.ext"), format!(
        "subjectAltName=DNS:{hostname},DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n"
    ).as_bytes())?;
    for (name, args) in requests {
        let output = Command::new("openssl")
            .current_dir(&resources.work)
            .args(args)
            .output()
            .await?;
        let mut log = output.stdout;
        log.extend(output.stderr);
        fs::write(
            resources.evidence.join(format!("session-tls-{name}.log")),
            log,
        )?;
        ensure!(
            output.status.success(),
            "the session TLS {name} command failed"
        );
    }
    fs::copy(
        resources.work.join("session-ca.crt"),
        resources.evidence.join("session-ca.crt"),
    )?;
    Ok(())
}

type Preserved = BTreeMap<String, serde_yaml::Value>;

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
#[derive(Clone, Serialize, Deserialize)]
struct Env {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Serialize, Deserialize)]
struct Volume {
    name: String,
    #[serde(rename = "configMap", skip_serializing_if = "Option::is_none")]
    config_map: Option<Named>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret: Option<SecretVolume>,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Serialize, Deserialize)]
struct Named {
    name: String,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretVolume {
    secret_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_mode: Option<u32>,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Mount {
    name: String,
    mount_path: String,
    read_only: bool,
    #[serde(flatten)]
    rest: Preserved,
}

pub(super) fn adjust_host(
    overlay: &str,
    issuer: &str,
    instance_suffix: &str,
) -> anyhow::Result<String> {
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

fn ca_volume(name: &str) -> Volume {
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
fn mount(name: &str, path: &str) -> Mount {
    Mount {
        name: name.to_owned(),
        mount_path: path.to_owned(),
        read_only: true,
        rest: Preserved::new(),
    }
}

async fn image_loaded(
    cluster: &ReceivingCluster,
    image: &str,
    name: &str,
    config_fallback: bool,
) -> anyhow::Result<(String, String)> {
    let resources = &cluster.resources;
    let labels: Value = serde_json::from_slice(
        &checked(Command::new(&resources.lifecycle).args(["image-labels", image])).await?,
    )?;
    save(resources, &format!("{name}-image-labels"), &labels)?;
    ensure!(
        labels["wamn.dev/source-head"] == resources.source
            && labels["wamn.dev/build-profile"] == "release",
        "the session image carries the selected source and build profile"
    );
    let nodes = String::from_utf8(
        checked(Command::new(&resources.lifecycle).args(["nodes", &resources.name])).await?,
    )?;
    let nodes = nodes.lines().collect::<BTreeSet<_>>();
    ensure!(
        nodes.len() == 3,
        "the session image must be loaded on three distinct nodes"
    );
    let mut expected = None;
    let mut rows = Vec::new();
    for node in nodes {
        let observed: Value = serde_json::from_slice(
            &checked(Command::new(&resources.lifecycle).args(["node-image", node, image])).await?,
        )?;
        save(resources, &format!("{name}-image-{node}"), &observed)?;
        let tuple = image_tuple(&observed, config_fallback)?;
        if let Some(expected) = &expected {
            ensure!(expected == &tuple, "the session image differs across nodes");
        }
        expected = Some(tuple.clone());
        rows.push(json!({"node":node,"runtime_digest":tuple.0,"config_id":tuple.1}));
    }
    save(resources, &format!("{name}-image-nodes"), &json!(rows))?;
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

#[derive(Clone, Serialize, Deserialize)]
struct Metadata {
    name: String,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum WorkloadDocument {
    Service {
        metadata: Metadata,
        #[serde(flatten)]
        rest: Preserved,
    },
    WorkloadDeployment {
        metadata: Metadata,
        spec: DeploymentSpec,
        #[serde(flatten)]
        rest: Preserved,
    },
}
#[derive(Clone, Serialize, Deserialize)]
struct DeploymentSpec {
    replicas: u32,
    template: WorkloadTemplate,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Serialize, Deserialize)]
struct WorkloadTemplate {
    spec: WorkloadSpec,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Clone, Serialize, Deserialize)]
struct WorkloadSpec {
    #[serde(rename = "hostId", skip_serializing_if = "Option::is_none")]
    host_id: Option<String>,
    #[serde(flatten)]
    rest: Preserved,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentList<T> {
    api_version: &'static str,
    kind: &'static str,
    items: Vec<T>,
}

fn pinned_workloads(source: &str, pins: &[Value]) -> anyhow::Result<String> {
    let documents = serde_yaml::Deserializer::from_str(source)
        .map(WorkloadDocument::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    ensure!(
        documents.len() == 2,
        "the session workload source has two documents"
    );
    let service = documents.iter().find(|document| matches!(document, WorkloadDocument::Service { metadata, .. } if metadata.name == "flow-http"))
        .context("the session workload source has its flow-http Service")?;
    let template = documents.iter().find(|document| matches!(document, WorkloadDocument::WorkloadDeployment { metadata, .. } if metadata.name == "flow-http"))
        .context("the session workload source has its flow-http WorkloadDeployment")?;
    let mut items = vec![service.clone()];
    for pin in pins {
        let mut document = template.clone();
        if let WorkloadDocument::WorkloadDeployment { metadata, spec, .. } = &mut document {
            metadata.name = text(pin, "/deployment")?.to_owned();
            spec.replicas = 1;
            spec.template.spec.host_id = Some(text(pin, "/host_id")?.to_owned());
        }
        items.push(document);
    }
    Ok(serde_yaml::to_string(&DocumentList {
        api_version: "v1",
        kind: "List",
        items,
    })?)
}

pub(super) async fn assert_session(
    cluster: &ReceivingCluster,
    fixture: &Path,
    issuer: &str,
    http_image: &str,
    fresh_only: bool,
    session_client: bool,
) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    let fixture: Value = serde_json::from_slice(&fs::read(fixture)?)?;
    let gates_image = resources
        .gates_image
        .as_deref()
        .context("the session gates image is built")?;
    let gates = image_loaded(cluster, gates_image, "gates", true).await?;
    let host_digest = workload::image_ready(
        &resources.lifecycle,
        &resources.name,
        &resources.work,
        &resources.host_image,
        &resources.source,
        "release",
        &resources.evidence,
    )
    .await?;
    workload::hosts_ready(
        &resources.lifecycle,
        &resources.name,
        &resources.work,
        &resources.name,
        &resources.host_image,
        &host_digest,
        2,
        &resources.evidence,
    )
    .await?;
    let deployment = read_object(
        resources,
        "session-host-deployment",
        &[
            "-n",
            &resources.name,
            "get",
            "deployment",
            "hostgroup-default",
        ],
    )
    .await?;
    let labels = deployment
        .pointer("/spec/selector/matchLabels")
        .and_then(Value::as_object)
        .context("the host deployment has selector labels")?;
    ensure!(!labels.is_empty(), "the host selector is not empty");
    let selector = labels
        .iter()
        .map(|(key, value)| {
            Ok(format!(
                "{key}={}",
                value.as_str().context("the host selector value is text")?
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .join(",");
    let source = render_http_workload(
        &fs::read_to_string(
            resources
                .repository
                .join("deploy/platform/http-route-workload.example.yaml"),
        )?,
        &HttpWorkloadInput {
            namespace: resources.name.clone(),
            image: http_image.to_owned(),
            route_host: cluster.inputs.route_host.clone(),
            claims: HttpClaims {
                tenant: TENANT.to_owned(),
                catalog: "default".to_owned(),
                environment: resources.name.clone(),
                project: PROJECT.to_owned(),
                schema: "receiving".to_owned(),
            },
        },
    )?;
    fs::write(
        resources.evidence.join("session-workload-source.yaml"),
        &source,
    )?;
    let mut previous = Vec::new();
    for available in [true, false] {
        let stage = if session_client {
            "session-client"
        } else if available {
            "host-session-reachable"
        } else {
            "host-session-unreachable"
        };
        if !available {
            checked(kubectl(resources).args([
                "-n",
                &resources.name,
                "delete",
                "workloaddeployment",
                DEPLOYMENTS[0],
                DEPLOYMENTS[1],
                "--cascade=foreground",
                "--wait=true",
                "--timeout=120s",
            ]))
            .await?;
            checked(kubectl(resources).args([
                "-n",
                &resources.name,
                "rollout",
                "restart",
                "deployment/hostgroup-default",
            ]))
            .await?;
            checked(kubectl(resources).args([
                "-n",
                &resources.name,
                "rollout",
                "status",
                "deployment/hostgroup-default",
                "--timeout=240s",
            ]))
            .await?;
        }
        let published = key_command(resources, &format!("{stage}-published"), &["publish"]).await?;
        let kid = text(&published, "/kid")?;
        canonical_uuid(kid)?;
        let activated = key_command(
            resources,
            &format!("{stage}-activated"),
            &["activate", "--kid", kid],
        )
        .await?;
        ensure!(
            activated["activated"] == true && activated["kid"] == kid,
            "the published session key became active"
        );
        if available {
            nested_session(cluster, issuer, fresh_only, session_client).await?;
        }
        if session_client {
            break;
        }
        let pods = read_object(
            resources,
            &format!("{stage}-pin-pods"),
            &["-n", &resources.name, "get", "pods", "-l", &selector],
        )
        .await?;
        let hosts = read_object(
            resources,
            &format!("{stage}-pin-hosts"),
            &[
                "-n",
                "wamn-system",
                "get",
                "hosts",
                "-l",
                "hostgroup=default",
            ],
        )
        .await?;
        let pins = select_hosts(
            &pods,
            &hosts,
            &resources.name,
            &resources.host_image,
            &host_digest,
        )?;
        save(resources, &format!("{stage}-pins"), &json!(pins))?;
        let path = resources.evidence.join(format!("{stage}-workload.yaml"));
        fs::write(&path, pinned_workloads(&source, &pins)?)?;
        apply(resources, &path).await?;
        checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "wait",
            "--for=condition=Ready",
            "workloaddeployment/flow-http-session-a",
            "workloaddeployment/flow-http-session-b",
            "--timeout=240s",
        ]))
        .await?;
        let deployments = read_object(
            resources,
            &format!("{stage}-workload-deployments"),
            &[
                "-n",
                &resources.name,
                "get",
                "workloaddeployment",
                DEPLOYMENTS[0],
                DEPLOYMENTS[1],
            ],
        )
        .await?;
        let replicas = array(&deployments, "/items")?
            .iter()
            .map(|value| {
                ready_deployment(value)?;
                text(value, "/status/currentReplicaSet/name")
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut args = vec!["-n", &resources.name, "get", "workloadreplicaset"];
        args.extend(replicas);
        let replicasets =
            read_object(resources, &format!("{stage}-workload-replicasets"), &args).await?;
        let workloads = read_object(
            resources,
            &format!("{stage}-workloads"),
            &["-n", &resources.name, "get", "workloads"],
        )
        .await?;
        let before_pods = read_object(
            resources,
            &format!("{stage}-host-pods"),
            &["-n", &resources.name, "get", "pods", "-l", &selector],
        )
        .await?;
        let before_hosts = read_object(
            resources,
            &format!("{stage}-hosts"),
            &[
                "-n",
                "wamn-system",
                "get",
                "hosts",
                "-l",
                "hostgroup=default",
            ],
        )
        .await?;
        let service = read_object(
            resources,
            &format!("{stage}-service"),
            &["-n", &resources.name, "get", "service", "flow-http"],
        )
        .await?;
        let slices = read_object(
            resources,
            &format!("{stage}-endpointslices"),
            &[
                "-n",
                &resources.name,
                "get",
                "endpointslices",
                "-l",
                "kubernetes.io/service-name=flow-http,wasmcloud.dev/route-manager=true",
            ],
        )
        .await?;
        let endpoints = host_endpoints(
            &pins,
            &deployments,
            &replicasets,
            &workloads,
            &before_pods,
            &before_hosts,
            &service,
            &slices,
            &resources.name,
            &resources.host_image,
            &host_digest,
            http_image,
            text(&fixture, "/route_path")?,
        )?;
        save(
            resources,
            &format!("{stage}-host-endpoints"),
            &json!(endpoints),
        )?;
        if !available {
            for field in [
                "deployment_uid",
                "replicaset_uid",
                "workload_uid",
                "pod_uid",
                "host_id",
            ] {
                ensure!(
                    endpoints
                        .iter()
                        .all(|new| previous.iter().all(|old: &Value| old[field] != new[field])),
                    "the second session case must use new {field} values"
                );
            }
        }
        run_job(cluster, stage, issuer, kid, &endpoints, available, &gates).await?;
        let after_pods = read_object(
            resources,
            &format!("{stage}-post-host-pods"),
            &["-n", &resources.name, "get", "pods", "-l", &selector],
        )
        .await?;
        let after_hosts = read_object(
            resources,
            &format!("{stage}-post-hosts"),
            &[
                "-n",
                "wamn-system",
                "get",
                "hosts",
                "-l",
                "hostgroup=default",
            ],
        )
        .await?;
        let before = processes(
            &endpoints,
            &before_pods,
            &before_hosts,
            &resources.name,
            &resources.host_image,
            &host_digest,
        )?;
        let after = processes(
            &endpoints,
            &after_pods,
            &after_hosts,
            &resources.name,
            &resources.host_image,
            &host_digest,
        )?;
        ensure!(
            before == after,
            "both warm production host processes must remain unchanged"
        );
        save(
            resources,
            &format!("{stage}-process-continuity"),
            &json!({"result":"pass","hosts":2,"processes":after}),
        )?;
        previous = endpoints;
    }
    let head = checked(
        Command::new("git")
            .current_dir(&resources.repository)
            .args(["rev-parse", "HEAD"]),
    )
    .await?;
    ensure!(
        String::from_utf8(head)?.trim() == resources.source,
        "the session source commit changed"
    );
    let status = checked(
        Command::new("git")
            .current_dir(&resources.repository)
            .args([
                "status",
                "--porcelain",
                "--untracked-files=normal",
                "--",
                ".",
                ":(exclude).beads/issues.jsonl",
                ":(exclude).beads/interactions.jsonl",
            ]),
    )
    .await?;
    ensure!(status.is_empty(), "the session source tree changed");
    Ok(())
}

async fn nested_session(
    cluster: &ReceivingCluster,
    issuer: &str,
    fresh_only: bool,
    session_client: bool,
) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    let mut child = kubectl(resources)
        .args([
            "-n",
            &resources.name,
            "port-forward",
            "--address=127.0.0.1",
            "service/host-session-identity",
            ":443",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::from(fs::File::create(
            resources
                .evidence
                .join("session-nested-port-forward-errors.log"),
        )?))
        .kill_on_drop(true)
        .spawn()?;
    let result = async {
        let mut lines = BufReader::new(
            child
                .stdout
                .take()
                .context("the session port-forward has output")?,
        )
        .lines();
        let first = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await??
            .context("the session port-forward reports its endpoint")?;
        fs::write(
            resources.evidence.join("session-nested-port-forward.log"),
            format!("{first}\n"),
        )?;
        let port = first
            .strip_prefix("Forwarding from 127.0.0.1:")
            .and_then(|line| line.strip_suffix(" -> 8443"))
            .context("the session port-forward reports the declared HTTPS target")?
            .parse::<u16>()?;
        ensure!(port != 0, "the forwarded HTTPS port is assigned");
        tokio::time::timeout(
            Duration::from_secs(240),
            sessions::assert_nested_session(
                cluster.inputs.clone(),
                issuer,
                &format!("https://localhost:{port}"),
                &fs::read(resources.work.join("session-ca.crt"))?,
                fresh_only,
                session_client,
            ),
        )
        .await??;
        save(
            resources,
            if session_client {
                "session-client-result"
            } else {
                "session-nested-result"
            },
            &json!({"result":"pass","fresh_only":fresh_only,"session_client":session_client}),
        )?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let cleanup = async {
        if child.try_wait()?.is_none() {
            child.start_kill()?;
        }
        tokio::time::timeout(Duration::from_secs(5), child.wait()).await??;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    save(
        resources,
        "session-nested-port-forward-cleanup",
        &json!({"reaped":cleanup.is_ok()}),
    )?;
    result?;
    cleanup
}

fn select_hosts(
    pods: &Value,
    hosts: &Value,
    namespace: &str,
    image: &str,
    digest: &str,
) -> anyhow::Result<Vec<Value>> {
    let pods = array(pods, "/items")?
        .iter()
        .filter(|pod| pod["metadata"]["deletionTimestamp"].is_null())
        .collect::<Vec<_>>();
    ensure!(pods.len() == 2, "two production host pods are required");
    let mut pins = Vec::new();
    for pod in pods {
        ready_pod(pod, "host", image, digest)?;
        for host in array(hosts, "/items")?
            .iter()
            .filter(|host| host_matches(host, pod, namespace))
        {
            pins.push(
                json!({"host_id":text(host,"/hostId")?, "pod_uid":text(pod,"/metadata/uid")?,
                "pod_name":text(pod,"/metadata/name")?, "pod_ip":text(pod,"/status/podIP")?}),
            );
        }
    }
    distinct_pair(&pins, &["host_id", "pod_uid", "pod_ip"])?;
    pins.sort_by(|a, b| a["pod_name"].as_str().cmp(&b["pod_name"].as_str()));
    for (pin, name) in pins.iter_mut().zip(DEPLOYMENTS) {
        pin["deployment"] = json!(name);
    }
    Ok(pins)
}

fn host_matches(host: &Value, pod: &Value, namespace: &str) -> bool {
    host["metadata"]["deletionTimestamp"].is_null()
        && host["environment"] == namespace
        && host["httpPort"] == 80
        && host["hostId"].as_str().is_some_and(|id| !id.is_empty())
        && (host["hostname"] == pod["metadata"]["name"]
            || host["hostname"] == pod["status"]["podIP"])
        && condition(host, "Ready")
}

fn ready_deployment(deployment: &Value) -> anyhow::Result<()> {
    ensure!(
        deployment["spec"]["replicas"] == 1
            && deployment["status"]["currentReplicas"] == 1
            && deployment["status"]["replicas"]["expected"] == 1
            && deployment["status"]["replicas"]["current"] == 1
            && deployment["status"]["replicas"]["ready"] == 1
            && (deployment["status"]["replicas"]["unavailable"].is_null()
                || deployment["status"]["replicas"]["unavailable"] == 0)
            && condition(deployment, "Ready"),
        "each pinned deployment must have exactly one ready replica"
    );
    text(deployment, "/status/currentReplicaSet/name")?;
    Ok(())
}

fn owned_by(value: &Value, owner: &Value, kind: &str) -> bool {
    value
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .is_some_and(|owners| {
            owners.iter().any(|entry| {
                entry["kind"] == kind
                    && entry["controller"] == true
                    && entry["uid"] == owner["metadata"]["uid"]
                    && entry["name"] == owner["metadata"]["name"]
            })
        })
}

fn host_endpoints(
    pins: &[Value],
    deployments: &Value,
    replicasets: &Value,
    workloads: &Value,
    pods: &Value,
    hosts: &Value,
    service: &Value,
    slices: &Value,
    namespace: &str,
    image: &str,
    digest: &str,
    component: &str,
    route: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut addresses = Vec::new();
    for slice in array(slices, "/items")? {
        ensure!(
            slice["addressType"] == "IPv4"
                && slice
                    .pointer("/metadata/ownerReferences")
                    .and_then(Value::as_array)
                    .is_some_and(
                        |owners| owners.iter().any(|owner| owner["kind"] == "Service"
                            && owner["controller"] == true
                            && owner["uid"] == service["metadata"]["uid"])
                    ),
            "each endpoint slice belongs to the selected IPv4 Service"
        );
        let ports = array(slice, "/ports")?;
        ensure!(
            ports.len() == 1 && ports[0]["port"] == 80 && ports[0]["protocol"] == "TCP",
            "each endpoint slice has the declared HTTP port"
        );
        for endpoint in array(slice, "/endpoints")? {
            let values = array(endpoint, "/addresses")?;
            ensure!(
                endpoint["conditions"]["ready"] == true
                    && endpoint["conditions"]["serving"] == true
                    && values.len() == 1,
                "each endpoint is ready and serving one address"
            );
            addresses.push(values[0].as_str().context("the endpoint address is text")?);
        }
    }
    ensure!(
        addresses.len() == 2 && addresses.iter().collect::<BTreeSet<_>>().len() == 2,
        "the HTTP Service has exactly two distinct endpoint addresses"
    );
    ensure!(
        array(deployments, "/items")?.len() == 2,
        "exactly two pinned deployments are required"
    );
    let mut placements = Vec::new();
    for deployment in array(deployments, "/items")? {
        ready_deployment(deployment)?;
        for replicaset in array(replicasets, "/items")?.iter().filter(|replicaset| {
            replicaset["metadata"]["name"] == deployment["status"]["currentReplicaSet"]["name"]
                && owned_by(replicaset, deployment, "WorkloadDeployment")
        }) {
            for workload in array(workloads, "/items")?
                .iter()
                .filter(|workload| owned_by(workload, replicaset, "WorkloadReplicaSet"))
            {
                ensure!(
                    [deployment, replicaset, workload]
                        .into_iter()
                        .all(|value| value["metadata"]["namespace"] == namespace)
                        && workload["status"]["environment"] == namespace
                        && replicaset["spec"]["replicas"] == 1,
                    "the session placement stays in its environment with one replica"
                );
                for (value, path) in [
                    (deployment, "/spec/template/spec/components"),
                    (replicaset, "/spec/template/spec/components"),
                    (workload, "/spec/components"),
                ] {
                    let components = array(value, path)?;
                    ensure!(
                        components.len() == 1 && components[0]["image"] == component,
                        "the session placement runs only its exact HTTP component"
                    );
                }
                ensure!(
                    ["Config", "HostSelection", "Placement", "Sync", "Ready"]
                        .into_iter()
                        .all(|name| condition(workload, name)),
                    "the session workload has all five ready conditions"
                );
                placements.push((deployment, replicaset, workload));
            }
        }
    }
    ensure!(
        placements.len() == 2,
        "the two deployments each own one native workload"
    );
    let ids = placements.iter().map(|(deployment, replicaset, workload)| json!({
        "deployment_uid":deployment["metadata"]["uid"], "replicaset_uid":replicaset["metadata"]["uid"],
        "workload_uid":workload["metadata"]["uid"], "host_id":workload["status"]["hostId"],
    })).collect::<Vec<_>>();
    distinct_pair(
        &ids,
        &[
            "deployment_uid",
            "replicaset_uid",
            "workload_uid",
            "host_id",
        ],
    )?;
    let mut output = Vec::new();
    for (deployment, replicaset, workload) in placements {
        let host_id = text(workload, "/status/hostId")?;
        for pin in pins.iter().filter(|pin| {
            pin["deployment"] == deployment["metadata"]["name"] && pin["host_id"] == host_id
        }) {
            ensure!(
                deployment["spec"]["template"]["spec"]["hostId"] == host_id
                    && replicaset["spec"]["template"]["spec"]["hostId"] == host_id
                    && workload["spec"]["hostId"] == host_id
                    && workload["spec"]["kubernetes"]["service"]["name"] == "flow-http",
                "all native workload levels retain the selected host and Service"
            );
            for host in array(hosts, "/items")?
                .iter()
                .filter(|host| host["hostId"] == host_id)
            {
                for pod in array(pods, "/items")?.iter().filter(|pod| {
                    pod["metadata"]["uid"] == pin["pod_uid"] && host_matches(host, pod, namespace)
                }) {
                    ensure!(
                        pod["metadata"]["deletionTimestamp"].is_null(),
                        "the selected host pod is not being deleted"
                    );
                    ready_pod(pod, "host", image, digest)?;
                    let ip = text(pod, "/status/podIP")?;
                    ensure!(
                        addresses.contains(&ip),
                        "the selected host pod serves an HTTP endpoint"
                    );
                    output.push(json!({"deployment":pin["deployment"], "deployment_uid":deployment["metadata"]["uid"],
                        "replicaset_uid":replicaset["metadata"]["uid"], "workload_uid":workload["metadata"]["uid"],
                        "host_id":host_id, "pod_uid":pod["metadata"]["uid"], "pod_name":pod["metadata"]["name"],
                        "pod_ip":ip, "endpoint":format!("http://{ip}:80{route}")}));
                }
            }
        }
    }
    distinct_pair(&output, &["pod_uid", "pod_ip"])?;
    output.sort_by(|a, b| a["host_id"].as_str().cmp(&b["host_id"].as_str()));
    Ok(output)
}

fn processes(
    placements: &[Value],
    pods: &Value,
    hosts: &Value,
    namespace: &str,
    image: &str,
    digest: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut output = Vec::new();
    for placement in placements {
        for pod in array(pods, "/items")?.iter().filter(|pod| {
            pod["metadata"]["uid"] == placement["pod_uid"]
                && pod["metadata"]["name"] == placement["pod_name"]
                && pod["metadata"]["namespace"] == namespace
                && pod["status"]["podIP"] == placement["pod_ip"]
                && pod["metadata"]["deletionTimestamp"].is_null()
        }) {
            ready_pod(pod, "host", image, digest)?;
            ensure!(condition(pod, "Ready"), "the warm host pod stays Ready");
            for container in array(pod, "/status/containerStatuses")?
                .iter()
                .filter(|container| container["name"] == "host")
            {
                let container_id = text(container, "/containerID")?;
                let id = container_id
                    .strip_prefix("containerd://")
                    .context("the host uses its native containerd process")?;
                ensure!(
                    id.len() == 64
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                    "the host container id is canonical"
                );
                let started = text(container, "/state/running/startedAt")?;
                chrono::DateTime::parse_from_rfc3339(started)
                    .context("the host process has a valid start time")?;
                let restarts = container["restartCount"]
                    .as_u64()
                    .context("the host restart count is a nonnegative integer")?;
                for host in array(hosts, "/items")?.iter().filter(|host| {
                    host["hostId"] == placement["host_id"] && host_matches(host, pod, namespace)
                }) {
                    canonical_uuid(text(pod, "/metadata/uid")?)?;
                    canonical_uuid(text(host, "/hostId")?)?;
                    output.push(json!({"pod_uid":pod["metadata"]["uid"], "host_id":host["hostId"],
                        "container_id":container_id, "started_at":started, "restart_count":restarts}));
                }
            }
        }
    }
    distinct_pair(&output, &["pod_uid", "host_id", "container_id"])?;
    output.sort_by(|a, b| a["pod_uid"].as_str().cmp(&b["pod_uid"].as_str()));
    Ok(output)
}

fn canonical_uuid(value: &str) -> anyhow::Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value)?.hyphenated().to_string() == value,
        "the session identifier is a canonical UUID"
    );
    Ok(())
}
fn distinct_pair(values: &[Value], fields: &[&str]) -> anyhow::Result<()> {
    ensure!(
        values.len() == 2,
        "exactly two session placements are required"
    );
    for field in fields {
        let first = values[0][field]
            .as_str()
            .context("the first placement has a text field")?;
        let second = values[1][field]
            .as_str()
            .context("the second placement has a text field")?;
        ensure!(
            !first.is_empty() && !second.is_empty() && first != second,
            "the session placements have distinct {field} values"
        );
    }
    Ok(())
}
fn condition(value: &Value, name: &str) -> bool {
    value
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition["type"] == name && condition["status"] == "True")
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
fn save(resources: &Resources, name: &str, value: &Value) -> anyhow::Result<()> {
    fs::write(
        resources.evidence.join(format!("{name}.json")),
        serde_json::to_vec_pretty(value)?,
    )?;
    Ok(())
}
async fn read_object(resources: &Resources, name: &str, args: &[&str]) -> anyhow::Result<Value> {
    let bytes = checked(
        kubectl(resources)
            .arg("--request-timeout=30s")
            .args(args)
            .args(["-o", "json"]),
    )
    .await?;
    let value = serde_json::from_slice(&bytes)?;
    save(resources, name, &value)?;
    Ok(value)
}
async fn key_command(resources: &Resources, name: &str, args: &[&str]) -> anyhow::Result<Value> {
    let bytes = tokio::time::timeout(
        Duration::from_secs(30),
        checked(
            kubectl(resources)
                .args([
                    "-n",
                    &resources.name,
                    "exec",
                    "deployment/host-session-identity",
                    "-c",
                    "identity",
                    "--",
                    "/usr/local/bin/wamn-identity",
                ])
                .args(args),
        ),
    )
    .await??;
    let value = serde_json::from_slice(&bytes)?;
    save(resources, name, &value)?;
    Ok(value)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionJob {
    api_version: &'static str,
    kind: &'static str,
    metadata: JobMetadata,
    spec: JobSpec,
}
#[derive(Serialize)]
struct JobMetadata {
    name: String,
    namespace: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JobSpec {
    backoff_limit: u32,
    active_deadline_seconds: u32,
    template: PodTemplate,
}
#[derive(Serialize)]
struct PodTemplate {
    spec: PodSpec,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PodSpec {
    restart_policy: &'static str,
    automount_service_account_token: bool,
    containers: Vec<JobContainer>,
    volumes: Vec<Volume>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JobContainer {
    name: &'static str,
    image: String,
    image_pull_policy: &'static str,
    command: [&'static str; 2],
    env: Vec<Env>,
    volume_mounts: Vec<Mount>,
}

fn session_job(
    namespace: &str,
    name: &str,
    image: &str,
    issuer: &str,
    endpoints: &str,
    available: bool,
) -> SessionJob {
    let env = [
        ("WAMN_IDENTITY_ISSUER", issuer.to_owned()),
        (
            "WAMN_IDENTITY_CA_FILE",
            "/etc/host-session-ca/ca.crt".to_owned(),
        ),
        (
            "WAMN_HOST_SESSION_FIXTURE_FILE",
            "/etc/host-session-fixture/fixture.json".to_owned(),
        ),
        ("WAMN_HOST_SESSION_ENDPOINTS", endpoints.to_owned()),
        ("WAMN_HOST_SESSION_JWKS_AVAILABLE", available.to_string()),
    ]
    .into_iter()
    .map(|(name, value)| Env {
        name: name.to_owned(),
        value: Some(value),
        rest: Preserved::new(),
    })
    .collect();
    SessionJob {
        api_version: "batch/v1",
        kind: "Job",
        metadata: JobMetadata {
            name: name.to_owned(),
            namespace: namespace.to_owned(),
        },
        spec: JobSpec {
            backoff_limit: 0,
            active_deadline_seconds: 430,
            template: PodTemplate {
                spec: PodSpec {
                    restart_policy: "Never",
                    automount_service_account_token: false,
                    containers: vec![JobContainer {
                        name: "host-session",
                        image: image.to_owned(),
                        image_pull_policy: "Never",
                        command: ["/usr/local/bin/wamn-gates", "host-session-test"],
                        env,
                        volume_mounts: vec![
                            mount("ca", "/etc/host-session-ca"),
                            mount("fixture", "/etc/host-session-fixture"),
                        ],
                    }],
                    volumes: vec![
                        ca_volume("ca"),
                        Volume {
                            name: "fixture".to_owned(),
                            config_map: None,
                            secret: Some(SecretVolume {
                                secret_name: "host-session-fixture".to_owned(),
                                default_mode: Some(256),
                                rest: Preserved::new(),
                            }),
                            rest: Preserved::new(),
                        },
                    ],
                },
            },
        },
    }
}

async fn run_job(
    cluster: &ReceivingCluster,
    stage: &str,
    issuer: &str,
    kid: &str,
    endpoints: &[Value],
    available: bool,
    gates: &(String, String),
) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    let image = resources
        .gates_image
        .as_deref()
        .context("the session gates image is built")?;
    let addresses = endpoints
        .iter()
        .map(|endpoint| text(endpoint, "/endpoint"))
        .collect::<anyhow::Result<Vec<_>>>()?
        .join(",");
    let job_path = resources.evidence.join(format!("{stage}-job.json"));
    fs::write(
        &job_path,
        serde_json::to_vec_pretty(&session_job(
            &resources.name,
            stage,
            image,
            issuer,
            &addresses,
            available,
        ))?,
    )?;
    let started = chrono::Utc::now().timestamp();
    apply(resources, &job_path).await?;
    let result = async {
        let mut child = kubectl(resources)
            .args([
                "-n",
                &resources.name,
                "logs",
                "-f",
                &format!("job/{stage}"),
                "-c",
                "host-session",
                "--pod-running-timeout=120s",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::from(fs::File::create(
                resources
                    .evidence
                    .join(format!("{stage}-stream-errors.log")),
            )?))
            .kill_on_drop(true)
            .spawn()?;
        let streaming = tokio::time::timeout(Duration::from_secs(480), async {
            let mut lines = BufReader::new(
                child
                    .stdout
                    .take()
                    .context("the session Job logs have output")?,
            )
            .lines();
            let mut log = fs::File::create(resources.evidence.join(format!("{stage}-stream.log")))?;
            let mut warm = false;
            while let Some(line) = lines.next_line().await? {
                use std::io::Write as _;
                writeln!(log, "{line}")?;
                if line == "HOST_SESSION_WARM hosts=2" {
                    ensure!(!warm, "the session Job warmed exactly once");
                    warm = true;
                    let removed = key_command(
                        resources,
                        &format!("{stage}-removed"),
                        &["remove", "--kid", kid],
                    )
                    .await?;
                    ensure!(
                        removed["removed"] == true && removed["kid"] == kid,
                        "the active session key was removed"
                    );
                    if !available {
                        checked(kubectl(resources).args([
                            "-n",
                            &resources.name,
                            "scale",
                            "deployment/host-session-identity",
                            "--replicas=0",
                        ]))
                        .await?;
                    }
                }
            }
            ensure!(
                child.wait().await?.success(),
                "the session Job log process failed"
            );
            ensure!(
                warm,
                "the session Job must warm both hosts before key removal"
            );
            Ok::<(), anyhow::Error>(())
        })
        .await;
        let child_cleanup = async {
            if child.try_wait()?.is_none() {
                child.start_kill()?;
            }
            tokio::time::timeout(Duration::from_secs(5), child.wait()).await??;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        save(
            resources,
            &format!("{stage}-log-cleanup"),
            &json!({"reaped":child_cleanup.is_ok()}),
        )?;
        streaming??;
        child_cleanup?;
        checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "wait",
            "--for=condition=Complete",
            &format!("job/{stage}"),
            "--timeout=60s",
        ]))
        .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let observed = async {
        let job = read_object(
            resources,
            &format!("{stage}-state"),
            &["-n", &resources.name, "get", "job", stage],
        )
        .await?;
        let pods = read_object(
            resources,
            &format!("{stage}-pods"),
            &[
                "-n",
                &resources.name,
                "get",
                "pods",
                "-l",
                &format!("job-name={stage}"),
            ],
        )
        .await?;
        let bytes = checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "logs",
            &format!("job/{stage}"),
            "-c",
            "host-session",
        ]))
        .await?;
        fs::write(resources.evidence.join(format!("{stage}.log")), &bytes)?;
        for endpoint in endpoints {
            let pod = text(endpoint, "/pod_name")?;
            let log = checked(kubectl(resources).args([
                "--request-timeout=30s",
                "-n",
                &resources.name,
                "logs",
                pod,
                "--all-containers",
            ]))
            .await?;
            fs::write(resources.evidence.join(format!("{stage}-{pod}.log")), log)?;
        }
        assert_job(
            &job,
            &pods,
            image,
            gates,
            std::str::from_utf8(&bytes)?,
            available,
            started,
        )?;
        save(
            resources,
            &format!("{stage}-result"),
            &json!({"result":"pass","hosts":2,"jwks_available":available}),
        )?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let cleanup = checked(kubectl(resources).args([
        "-n",
        &resources.name,
        "delete",
        "job",
        stage,
        "--wait=true",
        "--timeout=120s",
    ]))
    .await;
    save(
        resources,
        &format!("{stage}-job-cleanup"),
        &json!({"deleted":cleanup.is_ok()}),
    )?;
    result?;
    observed?;
    cleanup?;
    Ok(())
}

fn assert_job(
    job: &Value,
    pods: &Value,
    image: &str,
    gates: &(String, String),
    log: &str,
    available: bool,
    started: i64,
) -> anyhow::Result<()> {
    ensure!(
        condition(job, "Complete") && job["status"]["succeeded"] == 1 && !condition(job, "Failed"),
        "the session Job must complete successfully once"
    );
    text(job, "/metadata/uid")?;
    occurred_after(text(job, "/metadata/creationTimestamp")?, started)?;
    let transition = array(job, "/status/conditions")?
        .iter()
        .filter(|condition| condition["type"] == "Complete" && condition["status"] == "True")
        .next_back()
        .context("the Job has its Complete condition")?;
    occurred_after(text(transition, "/lastTransitionTime")?, started)?;
    let pods = array(pods, "/items")?;
    ensure!(
        pods.len() == 1 && owned_by(&pods[0], job, "Job"),
        "the session Job owns one pod"
    );
    let pod = &pods[0];
    ensure!(
        pod["status"]["phase"] == "Succeeded",
        "the session Job pod succeeded"
    );
    occurred_after(text(pod, "/metadata/creationTimestamp")?, started)?;
    occurred_after(text(pod, "/status/startTime")?, started)?;
    let declared_init = pod
        .pointer("/spec/initContainers")
        .and_then(Value::as_array);
    let observed_init = pod
        .pointer("/status/initContainerStatuses")
        .and_then(Value::as_array);
    ensure!(
        declared_init.map_or(0, Vec::len) == observed_init.map_or(0, Vec::len),
        "all session Job init containers ran"
    );
    for container in observed_init.into_iter().flatten() {
        ensure!(
            container["state"]["terminated"]["exitCode"] == 0,
            "the session Job init container succeeded"
        );
        occurred_after(text(container, "/state/terminated/finishedAt")?, started)?;
    }
    let container = array(pod, "/status/containerStatuses")?
        .iter()
        .find(|container| container["name"] == "host-session")
        .context("the session container ran")?;
    occurred_after(text(container, "/state/terminated/finishedAt")?, started)?;

    ensure!(
        array(pod, "/spec/containers")?
            .iter()
            .any(|container| container["name"] == "host-session" && container["image"] == image)
            && array(pod, "/status/containerStatuses")?
                .iter()
                .any(|container| container["name"] == "host-session"
                    && container["state"]["terminated"]["exitCode"] == 0
                    && container["imageID"]
                        .as_str()
                        .is_some_and(|id| id.ends_with(&gates.0) || id == gates.1)),
        "the session Job exited zero in the exact loaded gates image"
    );
    let final_line = format!("HOST_SESSION_TEST result=pass hosts=2 jwks_available={available}");
    ensure!(
        log.lines().filter(|line| *line == final_line).count() == 1
            && log
                .lines()
                .filter(|line| *line == "HOST_SESSION_WARM hosts=2")
                .count()
                == 1,
        "the session Job reports one warm result and one successful final result"
    );
    Ok(())
}

fn occurred_after(value: &str, started: i64) -> anyhow::Result<()> {
    ensure!(
        chrono::DateTime::parse_from_rfc3339(value)?.timestamp() >= started,
        "the session Job observation must belong to this run"
    );
    Ok(())
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

    #[test]
    fn pinning_changes_only_the_two_declared_host_placements() -> anyhow::Result<()> {
        let source = "apiVersion: v1\nkind: Service\nmetadata: {name: flow-http, namespace: receiving}\nspec: {ports: [{port: 80}]}\n---\napiVersion: runtime.wasmcloud.dev/v1alpha1\nkind: WorkloadDeployment\nmetadata: {name: flow-http, namespace: receiving}\nspec:\n  replicas: 1\n  template:\n    spec:\n      environment: receiving\n      components: [{name: flow-http, image: http-image}]\n";
        let pins = vec![
            json!({"deployment":DEPLOYMENTS[0],"host_id":"host-a"}),
            json!({"deployment":DEPLOYMENTS[1],"host_id":"host-b"}),
        ];
        let actual: Value = serde_yaml::from_str(&pinned_workloads(source, &pins)?)?;
        let original = serde_yaml::Deserializer::from_str(source)
            .map(Value::deserialize)
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(actual["items"][0], original[0]);
        for (index, pin) in pins.iter().enumerate() {
            let mut expected = original[1].clone();
            expected["metadata"]["name"] = pin["deployment"].clone();
            expected["spec"]["template"]["spec"]["hostId"] = pin["host_id"].clone();
            assert_eq!(actual["items"][index + 1], expected);
        }
        Ok(())
    }
}

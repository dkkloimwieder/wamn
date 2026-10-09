//! Smoke of the `release` chart on kind (docs/plan/platform-deploy.md §9.2,
//! assumptions A1 and A2): one host, one placed workload, a clean uninstall.
//!
//! The host refuses to start without the release manifest that the chart names,
//! so the smoke runs a TLS registry with a password on the kind network and
//! pushes the canonical fixture manifest of `push_release_manifest_live.rs`.
//! A released host also binds its tenant database before its first heartbeat,
//! so the smoke provisions a stand-in Postgres the way the Receiving journey
//! does. The fixture is rewritten to that journey scope's tenant and
//! environment.
//!
//! The chart is installed with the http role, and its `<name>-http` Workload
//! must be placed on the host. Two probe WorkloadDeployments, one on the
//! release's labels and one on a wrong `wamn.release`, check placement too.
//! The checks are of placement only: no role or probe component image is in
//! the registry.
//!
//! Run:
//! `cargo build -p wamn-identity` first, then
//! `WAMN_SMOKE_HOST_IMAGE=wamn-host:<tag> cargo test -p wamn-control --test release_chart_smoke -- --ignored --nocapture`
//! The report is `report.json` in the printed work directory.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_control::dev::environment::{
    JourneyScope, connect, install_journey_platform_floor, prepare_journey_credentials,
    provision_journey_control, provision_route, reconcile_journey_run_plane,
};
use wamn_control::push_release_manifest::push_manifest_bytes;
use wamn_control::release_chart::{
    ImageSet, Role, RoleName, Target, ValuesInput, release_name, stamp, uninstall, upgrade, values,
};
use wamn_engine::release_manifest::release_label;

const CLUSTER: &str = "wamn-release-smoke";
const REGISTRY: &str = "wamn-release-smoke-registry";
const POSTGRES: &str = "wamn-release-smoke-postgres";
const NAMESPACE: &str = "wamn-system";
const OPERATOR_CHART: &str = "oci://ghcr.io/wasmcloud/charts/runtime-operator";
/// The pin of tests/conformance/tests/chart_seam_governance.rs.
const OPERATOR_VERSION: &str = "2.10.3";
const PLATFORM_DOMAIN: &str = "example.invalid";
const USER: &str = "smoke";
const PASSWORD: &str = "smoke-password";
/// The canonical fixture of `push_release_manifest_live.rs`, before its tenant
/// and environment are rewritten to the smoke scope.
const MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"component":"http-request","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"orders"}],"format-version":5,"release":{"packages":[{"package-id":"orders","package-version":"1.0.0"}]},"routes":[],"workflow":{"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"orders","wiring-id":"orders","wiring-version":1}]}}"#;
/// A component reference the host never pulls: placement needs none.
const UNPULLED: &str = "wamn-release-smoke-registry:5000/wamn/unpulled@sha256:2222222222222222222222222222222222222222222222222222222222222222";

/// The kinds that the release owns, or that its owners create.
const OWNED_KINDS: [&str; 11] = [
    "deployments",
    "pods",
    "replicasets",
    "workloaddeployments",
    "workloadreplicasets",
    "workloads",
    "services",
    "endpointslices",
    "serviceaccounts",
    "networkpolicies",
    "configmaps",
];

struct Smoke {
    work: PathBuf,
    report: Vec<Value>,
}

impl Smoke {
    /// Run one command, and fail with its output when it exits non-zero.
    async fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<String> {
        let output = Command::new(program)
            .args(args)
            .env("KUBECONFIG", self.work.join("kubeconfig"))
            .kill_on_drop(true)
            .output()
            .await
            .with_context(|| format!("start {program}"))?;
        ensure!(
            output.status.success(),
            "{program} {} exited {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?)
    }

    async fn json(&self, program: &str, args: &[&str]) -> anyhow::Result<Value> {
        serde_json::from_str(&self.run(program, args).await?).context("read command JSON")
    }

    /// Record one check with the command that observed it and what it saw.
    fn record(&mut self, check: &str, command: &str, observed: Value, pass: bool) {
        let mut row = json!({"check": check, "result": if pass {"pass"} else {"fail"},
            "command": command});
        row["output"] = observed;
        println!("{}", serde_json::to_string(&row).unwrap_or_default());
        self.report.push(row);
    }

    async fn hosts(&self, group: &str) -> anyhow::Result<Vec<Value>> {
        let hosts = self
            .json(
                "kubectl",
                &[
                    "-n",
                    NAMESPACE,
                    "get",
                    "hosts",
                    "-l",
                    &format!("hostgroup={group}"),
                    "-o",
                    "json",
                ],
            )
            .await?;
        Ok(hosts["items"].as_array().cloned().unwrap_or_default())
    }

    /// The Workloads whose name starts with `prefix`.
    async fn workloads(&self, prefix: &str) -> anyhow::Result<Vec<Value>> {
        let workloads = self
            .json(
                "kubectl",
                &["-n", NAMESPACE, "get", "workloads", "-o", "json"],
            )
            .await?;
        Ok(workloads["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|workload| {
                workload["metadata"]["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with(prefix))
            })
            .cloned()
            .collect())
    }

    /// The operator release's revision and its Deployment's resourceVersion.
    async fn operator(&self) -> anyhow::Result<Value> {
        let releases = self
            .json("helm", &["-n", NAMESPACE, "list", "-o", "json"])
            .await?;
        let revision = releases
            .as_array()
            .into_iter()
            .flatten()
            .find(|release| release["name"] == "wamn")
            .map(|release| release["revision"].clone());
        let deployment = self
            .json(
                "kubectl",
                &[
                    "-n",
                    NAMESPACE,
                    "get",
                    "deployment",
                    "runtime-operator",
                    "-o",
                    "json",
                ],
            )
            .await?;
        Ok(json!({"revision": revision,
            "resourceVersion": deployment["metadata"]["resourceVersion"]}))
    }

    /// Apply a WorkloadDeployment with one unpulled component and `selector`.
    async fn apply_probe(&self, name: &str, selector: Value) -> anyhow::Result<()> {
        let probe = json!({
            "apiVersion": "runtime.wasmcloud.dev/v1alpha1",
            "kind": "WorkloadDeployment",
            "metadata": {"name": name, "namespace": NAMESPACE},
            "spec": {"replicas": 1, "template": {"spec": {
                "hostSelector": selector,
                "environment": NAMESPACE,
                "components": [{"name": "probe", "image": UNPULLED}],
            }}},
        });
        let file = self.work.join(format!("{name}.json"));
        std::fs::write(&file, serde_json::to_vec_pretty(&probe)?)?;
        self.run("kubectl", &["apply", "-f", &file.to_string_lossy()])
            .await
            .map(drop)
    }

    async fn remove(&self) {
        let _ = Command::new("kind")
            .args(["delete", "cluster", "--name", CLUSTER])
            .output()
            .await;
        let _ = Command::new("docker")
            .args(["rm", "--force", "--volumes", REGISTRY, POSTGRES])
            .output()
            .await;
    }
}

/// The name and host of each Workload.
fn placements(workloads: &[Value]) -> Vec<Value> {
    workloads
        .iter()
        .map(|workload| {
            json!({"name": workload["metadata"]["name"],
            "hostId": workload["status"]["hostId"],
            "conditions": workload["status"]["conditions"]})
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: docker, kind, helm, kubectl, openssl, wamn-identity, WAMN_SMOKE_HOST_IMAGE"]
async fn release_chart_smoke() -> anyhow::Result<()> {
    let host_image = std::env::var("WAMN_SMOKE_HOST_IMAGE")
        .context("WAMN_SMOKE_HOST_IMAGE names a local host image, <repository>:<tag>")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let work = std::env::temp_dir().join(format!("wamn-release-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;
    println!("work directory {}", work.display());
    let mut smoke = Smoke {
        work,
        report: Vec::new(),
    };
    smoke.remove().await;
    let result = run(&mut smoke, &repository, &host_image).await;
    std::fs::write(
        smoke.work.join("report.json"),
        serde_json::to_vec_pretty(&smoke.report)?,
    )?;
    smoke.remove().await;
    result?;
    let failed: Vec<&Value> = smoke
        .report
        .iter()
        .filter(|row| row["result"] != "pass")
        .collect();
    ensure!(failed.is_empty(), "failed checks: {failed:?}");
    Ok(())
}

async fn run(smoke: &mut Smoke, repository: &Path, host_image: &str) -> anyhow::Result<()> {
    let work = smoke.work.clone();
    let path = |name: &str| work.join(name).to_string_lossy().into_owned();

    // The cluster and the operator release.
    smoke
        .run(
            "kind",
            &[
                "create",
                "cluster",
                "--name",
                CLUSTER,
                "--kubeconfig",
                &path("kubeconfig"),
            ],
        )
        .await?;
    let cert_manager = repository.join("deploy/infra/cert-manager.yaml");
    smoke
        .run("kubectl", &["apply", "-f", &cert_manager.to_string_lossy()])
        .await?;
    smoke
        .run(
            "kubectl",
            &[
                "-n",
                "cert-manager",
                "wait",
                "--for=condition=Available",
                "deployment",
                "--all",
                "--timeout=240s",
            ],
        )
        .await?;
    let operator_values = repository.join("deploy/infra/values-wamn.yaml");
    smoke
        .run(
            "helm",
            &[
                "upgrade",
                "--install",
                "wamn",
                OPERATOR_CHART,
                "--version",
                OPERATOR_VERSION,
                "-n",
                NAMESPACE,
                "--create-namespace",
                "-f",
                &operator_values.to_string_lossy(),
                "--wait",
                "--timeout",
                "5m",
            ],
        )
        .await?;

    // The registry: TLS from a private CA, and a password.
    smoke
        .run(
            "openssl",
            &[
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=wamn-release-smoke-ca",
                "-keyout",
                &path("ca.key"),
                "-out",
                &path("ca.crt"),
            ],
        )
        .await?;
    smoke
        .run(
            "openssl",
            &[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-subj",
                &format!("/CN={REGISTRY}"),
                "-keyout",
                &path("tls.key"),
                "-out",
                &path("tls.csr"),
            ],
        )
        .await?;
    std::fs::write(
        work.join("san.ext"),
        format!("subjectAltName=DNS:{REGISTRY},IP:127.0.0.1\n"),
    )?;
    smoke
        .run(
            "openssl",
            &[
                "x509",
                "-req",
                "-in",
                &path("tls.csr"),
                "-CA",
                &path("ca.crt"),
                "-CAkey",
                &path("ca.key"),
                "-CAcreateserial",
                "-days",
                "1",
                "-extfile",
                &path("san.ext"),
                "-out",
                &path("tls.crt"),
            ],
        )
        .await?;
    let htpasswd = smoke
        .run(
            "docker",
            &[
                "run",
                "--rm",
                "--entrypoint",
                "htpasswd",
                "httpd:2-alpine",
                "-Bbn",
                USER,
                PASSWORD,
            ],
        )
        .await?;
    std::fs::write(work.join("htpasswd"), htpasswd)?;
    smoke
        .run(
            "docker",
            &[
                "run",
                "--detach",
                "--name",
                REGISTRY,
                "--network",
                "kind",
                "--publish",
                "127.0.0.1::5000",
                "--volume",
                &format!("{}:/certs:ro", work.display()),
                "--env",
                "REGISTRY_HTTP_TLS_CERTIFICATE=/certs/tls.crt",
                "--env",
                "REGISTRY_HTTP_TLS_KEY=/certs/tls.key",
                "--env",
                "REGISTRY_AUTH=htpasswd",
                "--env",
                "REGISTRY_AUTH_HTPASSWD_REALM=smoke",
                "--env",
                "REGISTRY_AUTH_HTPASSWD_PATH=/certs/htpasswd",
                "registry:2",
            ],
        )
        .await?;
    let published = smoke.run("docker", &["port", REGISTRY, "5000"]).await?;
    let local = published
        .lines()
        .next()
        .context("the registry publishes port 5000")?
        .to_owned();

    // The stand-in tenant database, provisioned as the Receiving journey does.
    let init = repository.join("deploy/sql/postgres-init.sql");
    smoke
        .run(
            "docker",
            &[
                "run",
                "--detach",
                "--name",
                POSTGRES,
                "--network",
                "kind",
                "--env",
                "POSTGRES_PASSWORD=probe",
                "--volume",
                &format!(
                    "{}:/docker-entrypoint-initdb.d/postgres-init.sql:ro",
                    init.display()
                ),
                "postgres:18",
                "-c",
                "wal_level=logical",
            ],
        )
        .await?;
    let address = smoke
        .run(
            "docker",
            &[
                "inspect",
                "--format",
                "{{(index .NetworkSettings.Networks \"kind\").IPAddress}}",
                POSTGRES,
            ],
        )
        .await?
        .trim()
        .to_owned();
    let admin_url = format!("postgresql://postgres:probe@{address}:5432/postgres");
    let deadline = Instant::now() + Duration::from_secs(60);
    let (admin, admin_task) = loop {
        match connect(&admin_url).await {
            // The image restarts the server once after its init scripts.
            Ok(connected) if connected.0.simple_query("SELECT 1").await.is_ok() => break connected,
            _ if Instant::now() < deadline => tokio::time::sleep(Duration::from_secs(2)).await,
            Ok(_) => anyhow::bail!("the stand-in Postgres did not answer"),
            Err(error) => return Err(error),
        }
    };
    admin.batch_execute("CREATE DATABASE wamn_system").await?;
    admin_task.abort();
    let system_url = format!("postgresql://postgres:probe@{address}:5432/wamn_system");
    let (system, system_task) = connect(&system_url).await?;
    let scope = JourneyScope {
        org: "acme".to_owned(),
        project: "smoke".to_owned(),
        environment: "dev".to_owned(),
        tenant: "smoke-tenant".to_owned(),
    };
    provision_journey_control(&scope, &system_url, system.as_ref()).await?;
    system
        .execute(
            "UPDATE registry.meta SET platform_domain = $1",
            &[&PLATFORM_DOMAIN],
        )
        .await?;
    let secrets = work.join("secrets");
    std::fs::create_dir_all(&secrets)?;
    let route = provision_route(&scope, &system_url, system.as_ref(), &secrets, None).await?;
    system_task.abort();
    let (project, project_task) = connect(&route.database_url).await?;
    install_journey_platform_floor(project.as_ref(), &scope.tenant, PLATFORM_DOMAIN).await?;
    project_task.abort();
    reconcile_journey_run_plane(&scope, &system_url, &route.database_url).await?;
    let database = prepare_journey_credentials(
        &scope,
        &system_url,
        &route.database_url,
        &secrets,
        &secrets,
        NAMESPACE,
    )
    .await?;
    let mut urls = vec![
        "create",
        "secret",
        "generic",
        "smoke-database",
        "-n",
        NAMESPACE,
    ];
    let literals = [
        format!("--from-literal=guest={}", database.guest_sql),
        format!("--from-literal=executor={}", database.executor_platform),
    ];
    urls.extend(literals.iter().map(String::as_str));
    smoke.run("kubectl", &urls).await?;

    // The fixture release, pushed from this machine and pulled by the host.
    let manifest = std::str::from_utf8(MANIFEST)?
        .replace(
            r#""environment":"prod""#,
            &format!(r#""environment":"{}""#, scope.environment),
        )
        .replace(
            r#""tenant-id":"tenant-a""#,
            &format!(r#""tenant-id":"{}""#, scope.tenant),
        );
    let credentials = |authority: &str| {
        json!({"auths": {authority: {"username": USER, "password": PASSWORD}}}).to_string()
    };
    std::fs::write(work.join("push-auth.json"), credentials(&local))?;
    let ca = vec![work.join("ca.crt")];
    let mut pushed = None;
    for _ in 0..30 {
        match push_manifest_bytes(
            manifest.as_bytes(),
            &format!("{local}/wamn/releases"),
            false,
            &ca,
            &work.join("push-auth.json"),
        )
        .await
        {
            Ok(done) => {
                pushed = Some(done);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    let digest = pushed
        .context("push the fixture release manifest")?
        .digest
        .as_str()
        .to_owned();
    let label = release_label(&digest).context("label the fixture digest")?;
    smoke
        .run(
            "kubectl",
            &[
                "-n",
                NAMESPACE,
                "create",
                "secret",
                "generic",
                "smoke-registry",
                &format!(
                    "--from-literal=config.json={}",
                    credentials(&format!("{REGISTRY}:5000"))
                ),
                &format!("--from-file=ca.crt={}", path("ca.crt")),
            ],
        )
        .await?;

    // The host image, pinned by the digest the node reports.
    smoke
        .run(
            "kind",
            &["load", "docker-image", host_image, "--name", CLUSTER],
        )
        .await?;
    let node = format!("{CLUSTER}-control-plane");
    let inspected = smoke
        .json(
            "docker",
            &[
                "exec", &node, "crictl", "inspecti", "-o", "json", host_image,
            ],
        )
        .await?;
    let node_digest = inspected["status"]["repoDigests"][0]
        .as_str()
        .and_then(|reference| reference.rsplit_once('@'))
        .map(|(_, digest)| digest.to_owned())
        .context("the node reports a digest for the host image")?;
    let pinned_host = format!("{host_image}@{node_digest}");
    // kind load records the digest under an import name only. The kubelet
    // resolves `<repository>:<tag>@<digest>` by `<repository>@<digest>`.
    let repository_name = host_image
        .rsplit_once(':')
        .map_or(host_image, |(name, _)| name);
    smoke
        .run(
            "docker",
            &[
                "exec",
                &node,
                "ctr",
                "-n",
                "k8s.io",
                "images",
                "tag",
                &format!("docker.io/library/{host_image}"),
                &format!("docker.io/library/{repository_name}@{node_digest}"),
            ],
        )
        .await?;

    // The release, rendered by the renderer the platform uses.
    let chart = stamp(
        &repository.join("deploy/platform/release"),
        &ImageSet {
            host: pinned_host,
            http: UNPULLED.to_owned(),
            materializer: UNPULLED.to_owned(),
        },
        &work.join("chart"),
    )?;
    smoke
        .run("helm", &["dependency", "build", &chart.to_string_lossy()])
        .await?;
    let (org, project, env) = (&*scope.org, &*scope.project, &*scope.environment);
    let name = release_name(org, project, env)?;
    let host_group = serde_yaml::from_value(serde_yaml::to_value(json!({
        "replicas": 1,
        "http": {"enabled": true, "port": 80},
        "env": [
            {"name": "WAMN_REGISTRY_AUTH_FILE", "value": "/smoke-registry/config.json"},
            {"name": "WAMN_COMPONENT_ARTIFACT_BASE", "value": format!("{REGISTRY}:5000/wamn/components")},
            {"name": "WAMN_PG_URL", "valueFrom": {"secretKeyRef": {"name": "smoke-database", "key": "guest"}}},
            {"name": "WAMN_EXECUTOR_PLATFORM_PG_URL",
                "valueFrom": {"secretKeyRef": {"name": "smoke-database", "key": "executor"}}},
        ],
        "ociCaPaths": ["/smoke-registry/ca.crt"],
        "volumes": [{"name": "smoke-registry", "secret": {"secretName": "smoke-registry"}}],
        "volumeMounts": [{"name": "smoke-registry", "mountPath": "/smoke-registry", "readOnly": true}],
    }))?)?;
    let rendered = values(&ValuesInput {
        org: org.to_owned(),
        project: project.to_owned(),
        env: env.to_owned(),
        manifest_digest: digest.clone(),
        artifact_base: format!("{REGISTRY}:5000/wamn/releases"),
        route_host: "smoke.invalid".to_owned(),
        roles: vec![Role {
            name: RoleName::Http,
            config: serde_yaml::Mapping::new(),
            environment: None,
        }],
        drain_bound_seconds: 70,
        host_group,
    })?;
    std::fs::write(work.join("values.yaml"), serde_yaml::to_string(&rendered)?)?;
    let target = Target {
        kubeconfig: work.join("kubeconfig"),
        context: format!("kind-{CLUSTER}"),
        namespace: NAMESPACE.to_owned(),
    };
    let operator_before = smoke.operator().await?;
    upgrade(
        &target,
        &chart,
        &name,
        &work.join("values.yaml"),
        Duration::from_secs(300),
    )
    .await?;

    // 1. A1: the heartbeat carries the host group and the release.
    let command = format!("kubectl -n {NAMESPACE} get hosts -l hostgroup={name} -o json");
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut hosts = smoke.hosts(&name).await?;
    while hosts.len() != 1 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
        hosts = smoke.hosts(&name).await?;
    }
    let labels: Vec<Value> = hosts
        .iter()
        .map(|host| host["metadata"]["labels"].clone())
        .collect();
    let a1 = hosts.len() == 1
        && labels[0]["hostgroup"] == name.as_str()
        && labels[0]["wamn.release"] == label.as_str();
    smoke.record(
        "A1 heartbeat labels",
        &command,
        json!({"expected": {"hostgroup": name,
        "wamn.release": label}, "labels": labels}),
        a1,
    );
    let host_id = hosts
        .first()
        .and_then(|host| host["hostId"].as_str())
        .unwrap_or_default()
        .to_owned();

    // The chart's http role Workload is placed on the host.
    let role = format!("{name}-http");
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut workloads = smoke.workloads(&role).await?;
    while !workloads
        .iter()
        .any(|workload| workload["status"]["hostId"] == host_id.as_str())
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_secs(2)).await;
        workloads = smoke.workloads(&role).await?;
    }
    smoke.record(
        "http role placed on the host",
        &format!("kubectl -n {NAMESPACE} get workloads -o json (names {role}*)"),
        json!({"hostId": host_id, "workloads": placements(&workloads)}),
        !host_id.is_empty()
            && workloads
                .iter()
                .any(|workload| workload["status"]["hostId"] == host_id.as_str()),
    );

    // 2. A WorkloadDeployment that selects on both labels is placed on the host.
    let placed = format!("{name}-probe-placed");
    smoke
        .apply_probe(&placed, json!({"hostgroup": name, "wamn.release": label}))
        .await?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut workloads = smoke.workloads(&placed).await?;
    while !workloads
        .iter()
        .any(|workload| workload["status"]["hostId"] == host_id.as_str())
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_secs(2)).await;
        workloads = smoke.workloads(&placed).await?;
    }
    smoke.record(
        "placed on both labels",
        &format!("kubectl apply -f {placed}.json; kubectl -n {NAMESPACE} get workloads -o json (names {placed}*)"),
        json!({"hostSelector": {"hostgroup": name, "wamn.release": label}, "hostId": host_id,
            "workloads": placements(&workloads)}),
        !host_id.is_empty()
            && workloads.iter().any(|workload| workload["status"]["hostId"] == host_id.as_str()),
    );

    // 3. A WorkloadDeployment that selects another release is not placed.
    let probe = format!("{name}-probe-wrong");
    let mut wrong = label.clone();
    let last = wrong.pop().context("the label is not empty")?;
    wrong.push(if last == 'a' { 'b' } else { 'a' });
    smoke
        .apply_probe(&probe, json!({"hostgroup": name, "wamn.release": wrong}))
        .await?;
    tokio::time::sleep(Duration::from_secs(120)).await;
    let workloads = smoke.workloads(&probe).await?;
    let deployment = smoke
        .json(
            "kubectl",
            &[
                "-n",
                NAMESPACE,
                "get",
                "workloaddeployment",
                &probe,
                "-o",
                "json",
            ],
        )
        .await?;
    smoke.record(
        "not placed on a wrong wamn.release",
        &format!("kubectl apply -f {probe}.json; sleep 120; kubectl -n {NAMESPACE} get workloads -o json (names {probe}*)"),
        json!({"hostSelector": {"hostgroup": name, "wamn.release": wrong},
            "workloads": placements(&workloads),
            "deploymentConditions": deployment["status"]["conditions"]}),
        workloads.iter().all(|workload| {
            workload["status"]["hostId"].as_str().is_none_or(str::is_empty)
        }),
    );
    for probe in [&placed, &probe] {
        smoke
            .run(
                "kubectl",
                &[
                    "-n",
                    NAMESPACE,
                    "delete",
                    "workloaddeployment",
                    probe,
                    "--wait",
                ],
            )
            .await?;
    }

    // 5. A2: the uninstall leaves nothing of ours.
    uninstall(&target, &name).await?;
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut remaining;
    loop {
        remaining = Vec::new();
        for kind in OWNED_KINDS {
            let names = smoke
                .run("kubectl", &["-n", NAMESPACE, "get", kind, "-o", "name"])
                .await?;
            remaining.extend(
                names
                    .lines()
                    .filter(|object| object.contains(&name))
                    .map(str::to_owned),
            );
        }
        for host in smoke.hosts(&name).await? {
            remaining.push(format!(
                "host/{}",
                host["metadata"]["name"].as_str().unwrap_or_default()
            ));
        }
        if remaining.is_empty() || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let releases = smoke
        .json(
            "helm",
            &[
                "list",
                "--all-namespaces",
                "--deployed",
                "--failed",
                "--pending",
                "--superseded",
                "--uninstalling",
                "--uninstalled",
                "-o",
                "json",
            ],
        )
        .await?;
    let listed = releases
        .as_array()
        .into_iter()
        .flatten()
        .any(|release| release["name"] == name.as_str());
    smoke.record(
        "A2 uninstall leaves nothing",
        &format!("helm uninstall {name} --wait; kubectl -n {NAMESPACE} get <{}> -o name; kubectl get hosts -l hostgroup={name}; helm list --all-namespaces <every state>",
            OWNED_KINDS.join(",")),
        json!({"remaining": remaining, "helmListsRelease": listed}),
        remaining.is_empty() && !listed,
    );

    // 4. The operator release is untouched.
    let operator_after = smoke.operator().await?;
    smoke.record(
        "operator release untouched",
        &format!("helm -n {NAMESPACE} list -o json; kubectl -n {NAMESPACE} get deployment runtime-operator -o json"),
        json!({"before": operator_before, "after": operator_after}),
        operator_before == operator_after,
    );
    Ok(())
}

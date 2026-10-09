//! `wamn-ctl env apply` on kind (docs/plan/platform-deploy.md §10.1 steps 4
//! to 7, §10.2, R6, R22, §3 A6, A7 and A12; issue `wamn-snz0.3`).
//!
//! The setup is the one of `release_chart_smoke.rs`, with CloudNativePG in
//! place of the stand-in Postgres, because `apply` ensures the environment's
//! `Database` CR: a fresh kind cluster `wamn-apply-<pid>` (epic decision D9),
//! cert-manager, the operator release, the CNPG operator and one `Cluster`,
//! and a TLS registry with a password on the kind network. The system
//! database lives on the CNPG cluster and the verb reaches it through a port
//! forward. The fixture release has no http attachment and no registration,
//! so no role WorkloadDeployment renders; roles are tested by `wamn-qh1g`.
//!
//! Checks, each recorded with its command and output in `report.json`:
//! 1. The first apply creates the environment: its row, its database and
//!    one Helm revision.
//! 2. A second apply of the same document writes no Helm revision.
//! 3. A connection-only change writes no Helm revision and changes the
//!    connection's generation. The fixture's components declare no store
//!    alias, so there is no binding to change.
//! 4. `env show` output applied again writes nothing.
//! 5. A12: two environments in one namespace; every object and Secret name
//!    differs and derives from the coordinate, and each host mounts only its
//!    own Secrets.
//!
//! Checks of `wamn-snz0.4` (§10.1 steps 8 and 9, §11.2, §12.2, §12.3, R8,
//! R20, R22):
//! 6. apply B, then rollback: the live set is A, the route host and the
//!    connections are unchanged, and no revision is a `helm rollback`.
//! 7. rollback with one retained revision refuses "no retained prior
//!    release" and the history is unchanged.
//! 8. `host_exits_before_its_grace_period`: a deleted host pod with a pinned
//!    backlog exits with code 0 before `terminationGracePeriodSeconds`.
//! 9. release = none: no host pod remains, the declared instance stays
//!    enabled, no binding remains.
//! 10. delete with a run pinned to a release with no pod refuses.
//! 11. and 12. delete --data drops the database; a plain delete leaves it on
//!    the cluster and removes the row.
//!
//! A6 (kind half) and A7 (kind half) need a staged package upgrade and an
//! http role; this fixture has neither, and the report says so.
//!
//! Run (epic closeout only):
//! `WAMN_SMOKE_HOST_IMAGE=wamn-host:<tag> cargo test -p wamn-control --test environment_apply_kind -- --ignored --nocapture`

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use wamn_control::delivery::selection::{image_digest, package_set};
use wamn_control::dev::environment::{connect, reset_control_store};
use wamn_control::environment::document::{
    ConnectionDefinition, DeclaredRelease, EnvironmentDocument,
};
use wamn_control::environment::{self, Platform};
use wamn_control::package_artifact::{PackageRegistry, PushPackageRequest, push_package};
use wamn_control::pat_client::PatIssuerConfig;
use wamn_control::provision_org::{ProvisionOrgRequest, provision_org};
use wamn_control::push_release_manifest::push_manifest_bytes;
use wamn_control::release_chart::{ImageSet, Target, release_name, stamp};
use wamn_control_registry::{Template, Triple};

const NAMESPACE: &str = "wamn-system";
const OPERATOR_CHART: &str = "oci://ghcr.io/wasmcloud/charts/runtime-operator";
/// The pin of tests/conformance/tests/chart_seam_governance.rs.
const OPERATOR_VERSION: &str = "2.10.3";
const PG_CLUSTER: &str = "wamn-pg";
const PLATFORM_DOMAIN: &str = "example.invalid";
const USER: &str = "apply";
const PASSWORD: &str = "apply-password";
const ORG: &str = "acme";
const PROJECT: &str = "orders";
/// The policy provision-org stamps for the trials template.
const POLICY: &str = "dev";
/// The canonical fixture of `release_chart_smoke.rs`: one package, no http
/// attachment, no registration.
const MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"component":"http-request","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"orders"}],"format-version":5,"release":{"packages":[{"package-id":"orders","package-version":"1.0.0"}]},"routes":[],"workflow":{"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"orders","wiring-id":"orders","wiring-version":1}]}}"#;
/// The second fixture release, B: the same package, another component digest.
const MANIFEST_B: &[u8] = br#"{"attachments":{},"components":[{"component":"http-request","digest":"sha256:4444444444444444444444444444444444444444444444444444444444444444","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"orders"}],"format-version":5,"release":{"packages":[{"package-id":"orders","package-version":"1.0.0"}]},"routes":[],"workflow":{"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"orders","wiring-id":"orders","wiring-version":1}]}}"#;
/// A role image no WorkloadDeployment pulls: the fixture renders no role.
const UNPULLED: &str = "registry.invalid/wamn/unpulled@sha256:2222222222222222222222222222222222222222222222222222222222222222";
/// The package `orders@1.0.0` the fixture release names: no model, no
/// migration.
const PACKAGE_MANIFEST: &str = r#"{"package":{"id":"orders","version":"1.0.0"},"required_platform_policy_contract":{"id":"orders_data_access","state":"satisfied"},"models":{},"connections":[],"components":{}}"#;

struct Run {
    work: PathBuf,
    cluster: String,
    registry: String,
    report: Vec<Value>,
}

impl Run {
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

    fn record(&mut self, check: &str, command: &str, observed: Value, result: &str) {
        let mut row = json!({"check": check, "result": result, "command": command});
        row["output"] = observed;
        println!("{}", serde_json::to_string(&row).unwrap_or_default());
        self.report.push(row);
    }

    fn pass(&mut self, check: &str, command: &str, observed: Value, pass: bool) {
        self.record(check, command, observed, if pass { "pass" } else { "fail" });
    }

    /// The number of revisions in `helm history` of `name`.
    async fn revisions(&self, name: &str) -> anyhow::Result<usize> {
        let history = self
            .json(
                "helm",
                &[
                    "-n", NAMESPACE, "history", name, "--max", "50", "-o", "json",
                ],
            )
            .await?;
        Ok(history.as_array().map_or(0, Vec::len))
    }

    /// The release digests of the host pods of `release` that are not terminated.
    async fn live_digests(&self, release: &str) -> anyhow::Result<Vec<String>> {
        let pods = self
            .json(
                "kubectl",
                &[
                    "-n",
                    NAMESPACE,
                    "get",
                    "pods",
                    "-l",
                    &format!("wasmcloud.com/hostgroup={release}"),
                    "-o",
                    "json",
                ],
            )
            .await?;
        let mut digests: Vec<String> = pods["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|pod| {
                !matches!(
                    pod["status"]["phase"].as_str(),
                    Some("Succeeded" | "Failed")
                )
            })
            .map(|pod| {
                pod["metadata"]["annotations"]["wamn.release-digest"]
                    .as_str()
                    .unwrap_or("unlabelled")
                    .to_owned()
            })
            .collect();
        digests.sort();
        digests.dedup();
        Ok(digests)
    }

    async fn remove(&self) {
        let _ = Command::new("kind")
            .args(["delete", "cluster", "--name", &self.cluster])
            .output()
            .await;
        let _ = Command::new("docker")
            .args(["rm", "--force", "--volumes", &self.registry])
            .output()
            .await;
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: docker, kind, helm 4, kubectl, openssl, WAMN_SMOKE_HOST_IMAGE; epic closeout only"]
async fn environment_apply_kind() -> anyhow::Result<()> {
    let host_image = std::env::var("WAMN_SMOKE_HOST_IMAGE")
        .context("WAMN_SMOKE_HOST_IMAGE names a local host image, <repository>:<tag>")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let pid = std::process::id();
    let work = std::env::temp_dir().join(format!("wamn-apply-{pid}"));
    std::fs::create_dir_all(&work)?;
    println!("work directory {}", work.display());
    let mut run = Run {
        work,
        cluster: format!("wamn-apply-{pid}"),
        registry: format!("wamn-apply-{pid}-registry"),
        report: Vec::new(),
    };
    let started = Instant::now();
    run.remove().await;
    let result = Box::pin(scenario(&mut run, &repository, &host_image)).await;
    let elapsed = started.elapsed().as_secs();
    run.record("duration", "", json!({"seconds": elapsed}), "info");
    std::fs::write(
        run.work.join("report.json"),
        serde_json::to_vec_pretty(&run.report)?,
    )?;
    run.remove().await;
    result?;
    let failed: Vec<&Value> = run
        .report
        .iter()
        .filter(|row| row["result"] == "fail")
        .collect();
    ensure!(failed.is_empty(), "failed checks: {failed:?}");
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn scenario(run: &mut Run, repository: &Path, host_image: &str) -> anyhow::Result<()> {
    let work = run.work.clone();
    let path = |name: &str| work.join(name).to_string_lossy().into_owned();
    let cluster = run.cluster.clone();
    let registry = run.registry.clone();

    // The cluster, cert-manager, the operator release and CloudNativePG.
    run.run(
        "kind",
        &[
            "create",
            "cluster",
            "--name",
            &cluster,
            "--kubeconfig",
            &path("kubeconfig"),
        ],
    )
    .await?;
    for manifest in [
        "deploy/infra/cert-manager.yaml",
        "deploy/infra/cnpg-operator.yaml",
    ] {
        run.run(
            "kubectl",
            &[
                "apply",
                "--server-side",
                "-f",
                &repository.join(manifest).to_string_lossy(),
            ],
        )
        .await?;
    }
    for namespace in ["cert-manager", "cnpg-system"] {
        run.run(
            "kubectl",
            &[
                "-n",
                namespace,
                "wait",
                "--for=condition=Available",
                "deployment",
                "--all",
                "--timeout=300s",
            ],
        )
        .await?;
    }
    run.run(
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
            &repository
                .join("deploy/infra/values-wamn.yaml")
                .to_string_lossy(),
            "--wait",
            "--timeout",
            "5m",
        ],
    )
    .await?;
    run.run(
        "kubectl",
        &[
            "apply",
            "-f",
            &repository
                .join("deploy/infra/cnpg-cluster.yaml")
                .to_string_lossy(),
        ],
    )
    .await?;
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "wait",
            &format!("cluster/{PG_CLUSTER}"),
            "--for=condition=Ready",
            "--timeout=600s",
        ],
    )
    .await?;

    // The registry: TLS from a private CA, and a password.
    run.run(
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
            "/CN=wamn-apply-ca",
            "-keyout",
            &path("ca.key"),
            "-out",
            &path("ca.crt"),
        ],
    )
    .await?;
    run.run(
        "openssl",
        &[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            &format!("/CN={registry}"),
            "-keyout",
            &path("tls.key"),
            "-out",
            &path("tls.csr"),
        ],
    )
    .await?;
    std::fs::write(
        work.join("san.ext"),
        format!("subjectAltName=DNS:{registry},IP:127.0.0.1\n"),
    )?;
    run.run(
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
    let htpasswd = run
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
    run.run(
        "docker",
        &[
            "run",
            "--detach",
            "--name",
            &registry,
            "--network",
            "kind",
            "--volume",
            &format!("{}:/certs:ro", work.display()),
            "--env",
            "REGISTRY_HTTP_TLS_CERTIFICATE=/certs/tls.crt",
            "--env",
            "REGISTRY_HTTP_TLS_KEY=/certs/tls.key",
            "--env",
            "REGISTRY_AUTH=htpasswd",
            "--env",
            "REGISTRY_AUTH_HTPASSWD_REALM=apply",
            "--env",
            "REGISTRY_AUTH_HTPASSWD_PATH=/certs/htpasswd",
            "registry:2",
        ],
    )
    .await?;
    // One address for this machine and for the pods: the registry's address
    // on the kind network, in its certificate.
    let address = run
        .run(
            "docker",
            &[
                "inspect",
                "--format",
                "{{(index .NetworkSettings.Networks \"kind\").IPAddress}}",
                &registry,
            ],
        )
        .await?
        .trim()
        .to_owned();
    std::fs::write(
        work.join("san.ext"),
        format!("subjectAltName=DNS:{registry},IP:127.0.0.1,IP:{address}\n"),
    )?;
    run.run(
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
    run.run("docker", &["restart", &registry]).await?;
    let local = format!("{address}:5000");
    let credentials = |authority: &str| {
        json!({"auths": {authority: {"username": USER, "password": PASSWORD}}}).to_string()
    };
    std::fs::write(work.join("auth.json"), credentials(&local))?;
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "create",
            "secret",
            "generic",
            "apply-registry",
            &format!("--from-literal=config.json={}", credentials(&local)),
            &format!("--from-file=ca.crt={}", path("ca.crt")),
        ],
    )
    .await?;

    // The system database on the CNPG cluster, through a port forward.
    let password = run
        .run(
            "kubectl",
            &[
                "-n",
                NAMESPACE,
                "get",
                "secret",
                &format!("{PG_CLUSTER}-superuser"),
                "-o",
                "jsonpath={.data.password}",
            ],
        )
        .await?;
    let password = String::from_utf8(base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        password.trim(),
    )?)?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let _forward: Child = Command::new("kubectl")
        .env("KUBECONFIG", work.join("kubeconfig"))
        .args([
            "-n",
            NAMESPACE,
            "port-forward",
            &format!("svc/{PG_CLUSTER}-rw"),
            &format!("{port}:5432"),
        ])
        .kill_on_drop(true)
        .spawn()
        .context("start the port forward")?;
    let admin_url = format!("postgresql://postgres:{password}@127.0.0.1:{port}/postgres");
    let deadline = Instant::now() + Duration::from_secs(60);
    let (admin, admin_task) = loop {
        match connect(&admin_url).await {
            Ok(connected) => break connected,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => return Err(error),
        }
    };
    admin.batch_execute("CREATE DATABASE wamn_system").await?;
    admin_task.abort();
    let system_url = format!("postgresql://postgres:{password}@127.0.0.1:{port}/wamn_system");
    let (system, system_task) = connect(&system_url).await?;
    reset_control_store(system.as_ref()).await?;
    provision_org(ProvisionOrgRequest {
        org: ORG.to_owned(),
        template: Template::trials(),
        pool: PG_CLUSTER.to_owned(),
        system_database_url: Some(system_url.clone()),
        cluster_namespace: NAMESPACE.to_owned(),
        owner_email: None,
    })
    .await?;
    system
        .execute(
            "UPDATE registry.meta SET platform_domain = $1",
            &[&PLATFORM_DOMAIN],
        )
        .await?;

    // The package and the release, pushed from this machine.
    let packages = work.join("apps");
    let package = packages.join("orders");
    std::fs::create_dir_all(package.join("generated"))?;
    std::fs::create_dir_all(package.join("migrations"))?;
    std::fs::create_dir_all(packages.join("target"))?;
    std::fs::write(
        package.join("wamn.k"),
        "# orders@1.0.0, the apply fixture\n",
    )?;
    std::fs::write(package.join("generated/wamn.json"), PACKAGE_MANIFEST)?;
    std::fs::write(packages.join("target/components.json"), "[]")?;
    let ca = vec![work.join("ca.crt")];
    let package_base = format!("{local}/wamn/packages");
    let mut pushed = None;
    for _ in 0..30 {
        match push_package(&PushPackageRequest {
            package: package.clone(),
            registry: PackageRegistry {
                artifact_base: package_base.clone(),
                registry_auth_file: work.join("auth.json"),
                insecure_registry: false,
                oci_ca_paths: ca.clone(),
                control_database_url: system_url.clone(),
            },
            source_commit: None,
        })
        .await
        {
            Ok(done) => {
                pushed = Some(done);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    pushed.context("push the fixture package")?;
    let digest = push_manifest_bytes(
        MANIFEST,
        &format!("{local}/wamn/releases"),
        false,
        &ca,
        &work.join("auth.json"),
    )
    .await
    .context("push the fixture release manifest")?
    .digest
    .as_str()
    .to_owned();
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(MANIFEST)?;

    // The host image, pinned by the digest the node reports, as the smoke.
    run.run(
        "kind",
        &["load", "docker-image", host_image, "--name", &cluster],
    )
    .await?;
    let node = format!("{cluster}-control-plane");
    let inspected = run
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
    let repository_name = host_image
        .rsplit_once(':')
        .map_or(host_image, |(name, _)| name);
    run.run(
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
    let set = ImageSet {
        host: format!("{host_image}@{node_digest}"),
        http: UNPULLED.to_owned(),
        materializer: UNPULLED.to_owned(),
    };
    let chart = stamp(
        &repository.join("deploy/platform/release"),
        &set,
        &work.join("chart"),
    )?;
    run.run("helm", &["dependency", "build", &chart.to_string_lossy()])
        .await?;

    // The qualification of the release on the chart's host image (R12).
    system
        .execute(
            "INSERT INTO catalog.qualifications (qualification_sha256, package_set, image_digests) \
             VALUES ($1, $2::text::jsonb, $3::text::jsonb)",
            &[
                &format!("sha256:{}", "9".repeat(64)),
                &serde_json::to_string(&package_set(&manifest)?)?,
                &json!({"host": image_digest(&set.host)?}).to_string(),
            ],
        )
        .await?;
    system_task.abort();

    // The platform part of the host group: the registry mount, as the smoke.
    std::fs::write(
        work.join("host-group.yaml"),
        serde_yaml::to_string(&json!({
            "replicas": 1,
            "env": [
                {"name": "WAMN_REGISTRY_AUTH_FILE", "value": "/apply-registry/config.json"},
                {"name": "WAMN_COMPONENT_ARTIFACT_BASE", "value": format!("{local}/wamn/components")},
            ],
            "ociCaPaths": ["/apply-registry/ca.crt"],
            "volumes": [{"name": "apply-registry", "secret": {"secretName": "apply-registry"}}],
            "volumeMounts": [{"name": "apply-registry", "mountPath": "/apply-registry", "readOnly": true}],
        }))?,
    )?;
    let platform = Platform {
        system_database_url: system_url.clone(),
        target: Target {
            kubeconfig: work.join("kubeconfig"),
            context: format!("kind-{cluster}"),
            namespace: NAMESPACE.to_owned(),
        },
        chart,
        release_artifact_base: format!("{local}/wamn/releases"),
        registry_auth_file: work.join("auth.json"),
        oci_ca_paths: ca,
        package_artifact_base: Some(package_base),
        database_namespace: NAMESPACE.to_owned(),
        database_host: None,
        database_port: 5432,
        host_group: Some(work.join("host-group.yaml")),
        pat_issuer: PatIssuerConfig::default(),
        events: None,
    };
    let document = |env: &str, route_host: &str| EnvironmentDocument {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: env.to_owned(),
        release: DeclaredRelease::Digest(digest.clone()),
        route_host: route_host.to_owned(),
        policy: POLICY.to_owned(),
        connections: std::collections::BTreeMap::new(),
        floors: std::collections::BTreeMap::new(),
    };
    let apply = async |document: &EnvironmentDocument, name: &str| -> anyhow::Result<()> {
        let file = work.join(format!("{name}.k"));
        std::fs::write(&file, document.to_kcl())?;
        let plan = environment::apply(&platform, &file).await?;
        println!("{name}: {:?}", plan.plan);
        Ok(())
    };
    let name = release_name(ORG, PROJECT, "dev")?;

    // 1. The first apply creates the environment.
    let dev = document("dev", "dev.orders.example");
    apply(&dev, "dev-first").await?;
    let (system, system_task) = connect(&system_url).await?;
    system.batch_execute("SET ROLE wamn_system").await?;
    let row = system
        .query_opt(
            "SELECT instance_suffix, route_host, policy_name FROM registry.project_envs \
              WHERE org = $1 AND project = $2 AND env = 'dev'",
            &[&ORG, &PROJECT],
        )
        .await?
        .map(|row| {
            json!({"instance_suffix": row.get::<_, String>(0),
                "route_host": row.get::<_, Option<String>>(1),
                "policy_name": row.get::<_, String>(2)})
        });
    let first = run.revisions(&name).await?;
    run.pass(
        "first apply creates the environment",
        &format!("SELECT ... FROM registry.project_envs; helm -n {NAMESPACE} history {name}"),
        json!({"row": row, "revisions": first}),
        row.is_some() && first == 1,
    );

    // 2. The same document again writes no revision.
    apply(&dev, "dev-again").await?;
    let again = run.revisions(&name).await?;
    run.pass(
        "second apply writes no Helm revision",
        &format!("helm -n {NAMESPACE} history {name}"),
        json!({"before": first, "after": again}),
        again == first,
    );

    // 3. A connection-only change writes no revision and moves the generation.
    let generation = async |system: &tokio_postgres::Client| -> anyhow::Result<Option<i64>> {
        let tenant = wamn_control_registry::project_env_tenant(ORG, PROJECT, "dev");
        let row = system
            .query_opt(
                "SELECT instance_suffix FROM registry.project_envs \
                  WHERE org = $1 AND project = $2 AND env = 'dev'",
                &[&ORG, &PROJECT],
            )
            .await?
            .context("the dev row")?;
        let database = wamn_control_provision::project_env_database_name(
            ORG,
            PROJECT,
            "dev",
            &row.get::<_, String>(0),
        );
        let url = format!("postgresql://postgres:{password}@127.0.0.1:{port}/{database}");
        let (project, task) = connect(&url).await?;
        let active = project
            .query_opt(
                "SELECT active_generation FROM catalog.connection_instances \
                  WHERE tenant_id = $1 AND environment = 'dev' AND instance_id = 'labels'",
                &[&tenant],
            )
            .await?
            .and_then(|row| row.get(0));
        task.abort();
        Ok(active)
    };
    let mut connected = dev.clone();
    connected.connections.insert(
        "labels".to_owned(),
        ConnectionDefinition {
            requirement_type: wamn_catalog::RequirementType::Blobstore,
            definition: json!({"endpoint": "http://store.invalid", "container": "labels", "prefix": "a/"}),
        },
    );
    apply(&connected, "dev-connection").await?;
    let generation_one = generation(system.as_ref()).await?;
    connected
        .connections
        .get_mut("labels")
        .context("the labels connection")?
        .definition =
        json!({"endpoint": "http://store.invalid", "container": "labels", "prefix": "b/"});
    apply(&connected, "dev-connection-changed").await?;
    let generation_two = generation(system.as_ref()).await?;
    let after_connection = run.revisions(&name).await?;
    run.pass(
        "connection-only change writes no Helm revision",
        &format!("helm -n {NAMESPACE} history {name}; SELECT active_generation FROM catalog.connection_instances"),
        json!({"revisions": after_connection, "generations": [generation_one, generation_two],
            "binding": "the fixture's components declare no store alias, so no binding exists"}),
        after_connection == first && generation_one == Some(1) && generation_two == Some(2),
    );

    // 4. The output of env show, applied, writes nothing.
    let shown = environment::show::show(&platform, &Triple::new(ORG, PROJECT, "dev")).await?;
    apply(&shown, "dev-shown").await?;
    let after_show = run.revisions(&name).await?;
    let generation_three = generation(system.as_ref()).await?;
    run.pass(
        "env show output applied writes nothing",
        &format!("wamn-ctl env show {ORG}/{PROJECT}/dev | env apply; helm -n {NAMESPACE} history {name}"),
        json!({"document": shown.to_kcl(), "revisions": after_show, "generation": generation_three}),
        after_show == first && generation_three == generation_two,
    );
    system_task.abort();

    // 5. A12: a second environment in the same namespace.
    let qa = document("qa", "qa.orders.example");
    apply(&qa, "qa-first").await?;
    let qa_name = release_name(ORG, PROJECT, "qa")?;
    let mut names = std::collections::BTreeMap::<String, Vec<String>>::new();
    for kind in [
        "deployments",
        "services",
        "workloaddeployments",
        "secrets",
        "configmaps",
    ] {
        for object in run
            .run("kubectl", &["-n", NAMESPACE, "get", kind, "-o", "name"])
            .await?
            .lines()
        {
            for (env, release) in [("dev", &name), ("qa", &qa_name)] {
                let coordinate = format!("{ORG}--{PROJECT}--{env}");
                if object.contains(release.as_str()) || object.contains(&coordinate) {
                    names
                        .entry(env.to_owned())
                        .or_default()
                        .push(object.to_owned());
                }
            }
        }
    }
    let dev_names = names.get("dev").cloned().unwrap_or_default();
    let qa_names = names.get("qa").cloned().unwrap_or_default();
    let shared: Vec<&String> = dev_names
        .iter()
        .filter(|name| qa_names.contains(name))
        .collect();
    let mut mounts = serde_json::Map::new();
    let mut own = true;
    for (env, release) in [("dev", &name), ("qa", &qa_name)] {
        let pods = run
            .json(
                "kubectl",
                &[
                    "-n",
                    NAMESPACE,
                    "get",
                    "pods",
                    "-l",
                    &format!("wasmcloud.com/hostgroup={release}"),
                    "-o",
                    "json",
                ],
            )
            .await?;
        let other = if env == "dev" { "qa" } else { "dev" };
        let referenced: Vec<String> = pods["items"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|pod| {
                let text = pod["spec"].to_string();
                text.match_indices(&format!("--{other}"))
                    .map(|(at, _)| text[at.saturating_sub(40)..at + 4].to_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        own &= referenced.is_empty();
        mounts.insert(
            env.to_owned(),
            json!({"other environment references": referenced}),
        );
    }
    run.pass(
        "A12 two environments in one namespace",
        &format!("kubectl -n {NAMESPACE} get deployments,services,workloaddeployments,secrets,configmaps -o name; kubectl get pods -l wasmcloud.com/hostgroup=<release> -o json"),
        json!({"dev": dev_names, "qa": qa_names, "shared": shared, "mounts": mounts,
            "review": "Shared in the namespace, by design: the operator release's TLS Secrets \
                (wasmcloud-runtime-tls, wasmcloud-data-tls, the CA) and ServiceAccount, which \
                every host mounts by fixed name (R6); the registry Secret apply-registry of this \
                test. Every environment Secret is <family prefix><org>--<project>--<env>, every \
                chart object is named after r-<slug>-<hash>."}),
        !dev_names.is_empty() && !qa_names.is_empty() && shared.is_empty() && own,
    );

    // The checks of wamn-snz0.4: rollback, release none, the host's one
    // shutdown budget, and delete.
    let digest_b = push_manifest_bytes(
        MANIFEST_B,
        &format!("{local}/wamn/releases"),
        false,
        &platform.oci_ca_paths,
        &work.join("auth.json"),
    )
    .await
    .context("push the second fixture release manifest")?
    .digest
    .as_str()
    .to_owned();
    let dev_triple = Triple::new(ORG, PROJECT, "dev");
    let qa_triple = Triple::new(ORG, PROJECT, "qa");
    let project_database = async |env: &str| -> anyhow::Result<(String, String)> {
        let (system, task) = connect(&system_url).await?;
        let suffix: String = system
            .query_one(
                "SELECT instance_suffix FROM registry.project_envs \
                  WHERE org = $1 AND project = $2 AND env = $3",
                &[&ORG, &PROJECT, &env],
            )
            .await?
            .get(0);
        task.abort();
        let database =
            wamn_control_provision::project_env_database_name(ORG, PROJECT, env, &suffix);
        let url = format!("postgresql://postgres:{password}@127.0.0.1:{port}/{database}");
        Ok((database, url))
    };

    // 6. R8: apply B, then rollback. The live set is A again, the route host
    // and the connections are unchanged, and no `helm rollback` ran: every
    // revision is an apply's.
    let mut dev_b = shown.clone();
    dev_b.release = DeclaredRelease::Digest(digest_b.clone());
    apply(&dev_b, "dev-b").await?;
    let after_b = run.live_digests(&name).await?;
    let rolled = environment::rollback::rollback(&platform, &dev_triple, "kind check").await?;
    let after_rollback = run.live_digests(&name).await?;
    let shown_after = environment::show::show(&platform, &dev_triple).await?;
    let history = run
        .json(
            "helm",
            &[
                "-n", NAMESPACE, "history", &name, "--max", "50", "-o", "json",
            ],
        )
        .await?;
    let descriptions: Vec<String> = history
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["description"].as_str().map(str::to_owned))
        .collect();
    let no_helm_rollback = descriptions
        .iter()
        .all(|description| !description.starts_with("Rollback to"));
    run.pass(
        "rollback is an apply of the previous intended release",
        &format!("wamn-ctl env rollback {dev_triple} --reason 'kind check'; helm -n {NAMESPACE} history {name}"),
        json!({"after_b": after_b, "after_rollback": after_rollback, "plan": rolled.plan,
            "descriptions": descriptions, "shown_before": shown.to_kcl(), "shown_after": shown_after.to_kcl()}),
        after_b == [digest_b.clone()]
            && after_rollback == [digest.clone()]
            && shown_after == shown
            && no_helm_rollback
            && descriptions
                .last()
                .is_some_and(|description| description.starts_with("rollback by")),
    );

    // 7. R8 (3): one retained revision refuses and writes nothing.
    let qa_before = run.revisions(&qa_name).await?;
    let refused = environment::rollback::rollback(&platform, &qa_triple, "kind check").await;
    let qa_after = run.revisions(&qa_name).await?;
    let message = refused.as_ref().err().map(|error| format!("{error:#}"));
    run.pass(
        "rollback with one retained revision refuses",
        &format!("wamn-ctl env rollback {qa_triple} --reason 'kind check'"),
        json!({"error": message, "revisions": [qa_before, qa_after]}),
        message.as_deref().is_some_and(|message| {
            message.contains(environment::rollback::NO_RETAINED_PRIOR_RELEASE)
        }) && qa_before == qa_after,
    );

    // 8. host_exits_before_its_grace_period: a host pod with a pinned backlog
    // it never empties exits with code 0 before its grace period.
    let (system, system_task) = connect(&system_url).await?;
    system.batch_execute("SET ROLE wamn_system").await?;
    system
        .execute(
            "UPDATE registry.env_policies SET drain_bound_seconds = 30 WHERE org = $1 AND name = $2",
            &[&ORG, &POLICY],
        )
        .await?;
    system_task.abort();
    apply(&shown, "dev-short-drain").await?;
    let (_, dev_url) = project_database("dev").await?;
    let (project, project_task) = connect(&dev_url).await?;
    let tenant = wamn_control_registry::project_env_tenant(ORG, PROJECT, "dev");
    project
        .execute(
            "INSERT INTO wamn_run.runs \
               (tenant_id, run_id, flow_id, flow_version, package_id, manifest_digest, environment, \
                wiring_id, wiring_version, status, trigger_source, input_json, service_principal_id) \
             VALUES ($1, 'pinned-backlog', 'orders', 1, 'orders', $2, 'dev', 'orders', 1, \
                     'dispatched', 'automation', '{}', '00000000-0000-0000-0000-000000000001')",
            &[&tenant, &digest],
        )
        .await?;
    project
        .execute(
            "INSERT INTO wamn_run.run_queue (tenant_id, run_id, available_at) \
             VALUES ($1, 'pinned-backlog', 'infinity')",
            &[&tenant],
        )
        .await?;
    project_task.abort();
    let pods = run
        .json(
            "kubectl",
            &[
                "-n",
                NAMESPACE,
                "get",
                "pods",
                "-l",
                &format!("wasmcloud.com/hostgroup={name}"),
                "-o",
                "json",
            ],
        )
        .await?;
    let pod = pods["items"][0]["metadata"]["name"]
        .as_str()
        .context("a dev host pod")?
        .to_owned();
    let grace = pods["items"][0]["spec"]["terminationGracePeriodSeconds"]
        .as_u64()
        .context("the pod's grace period")?;
    let deleted = Instant::now();
    run.run(
        "kubectl",
        &["-n", NAMESPACE, "delete", "pod", &pod, "--wait=false"],
    )
    .await?;
    let mut exit_code = None;
    let mut exited_after = None;
    while deleted.elapsed() < Duration::from_secs(grace + 30) {
        let text = run
            .run(
                "kubectl",
                &[
                    "-n",
                    NAMESPACE,
                    "get",
                    "pod",
                    &pod,
                    "--ignore-not-found",
                    "-o",
                    "json",
                ],
            )
            .await?;
        if text.trim().is_empty() {
            exited_after.get_or_insert(deleted.elapsed().as_secs());
            break;
        }
        let observed: Value = serde_json::from_str(&text)?;
        if let Some(code) =
            observed["status"]["containerStatuses"][0]["state"]["terminated"]["exitCode"].as_i64()
        {
            exit_code = Some(code);
            exited_after = Some(deleted.elapsed().as_secs());
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    run.pass(
        "host_exits_before_its_grace_period",
        &format!("kubectl -n {NAMESPACE} delete pod {pod}; kubectl get pod {pod} -o json"),
        json!({"grace_seconds": grace, "exit_code": exit_code, "exited_after_seconds": exited_after}),
        exit_code == Some(0) && exited_after.is_some_and(|seconds| seconds < grace),
    );

    // 9. release = none: no host pod remains, the declared instances stay
    // enabled, and the release's bindings are gone.
    let mut qa_none = environment::show::show(&platform, &qa_triple).await?;
    qa_none.release = DeclaredRelease::None;
    qa_none.connections.insert(
        "labels".to_owned(),
        ConnectionDefinition {
            requirement_type: wamn_catalog::RequirementType::Blobstore,
            definition: json!({"endpoint": "http://store.invalid", "container": "labels", "prefix": "qa/"}),
        },
    );
    apply(&qa_none, "qa-none").await?;
    let qa_live = run.live_digests(&qa_name).await?;
    let (_, qa_url) = project_database("qa").await?;
    let (project, project_task) = connect(&qa_url).await?;
    let qa_tenant = wamn_control_registry::project_env_tenant(ORG, PROJECT, "qa");
    let enabled: Vec<String> = project
        .query(
            "SELECT instance_id FROM catalog.connection_instances \
              WHERE tenant_id = $1 AND environment = 'qa' AND lifecycle_status = 'enabled'",
            &[&qa_tenant],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let bindings: i64 = project
        .query_one(
            "SELECT count(*) FROM catalog.connection_bindings WHERE tenant_id = $1",
            &[&qa_tenant],
        )
        .await?
        .get(0);
    project_task.abort();
    run.pass(
        "release none uninstalls and keeps the declared instances",
        &format!(
            "wamn-ctl env apply qa-none.k; kubectl get pods -l wasmcloud.com/hostgroup={qa_name}"
        ),
        json!({"live": qa_live, "enabled": enabled, "bindings": bindings,
            "note": "the fixture's components declare no store alias, so A had no binding"}),
        qa_live.is_empty() && enabled == ["labels"] && bindings == 0,
    );

    // 10. delete refuses on the run pinned to A, which no pod serves now.
    let refused = environment::delete::delete(&platform, &dev_triple, false).await;
    let message = refused.as_ref().err().map(|error| format!("{error:#}"));
    run.pass(
        "delete refuses on a stranded run",
        &format!("wamn-ctl env delete {dev_triple}"),
        json!({"error": message}),
        message
            .as_deref()
            .is_some_and(|message| message.contains("pinned-backlog")),
    );

    // 11. delete --data drops the database; 12. a plain delete keeps it.
    let (dev_database, _) = project_database("dev").await?;
    let (qa_database, _) = project_database("qa").await?;
    let dev_lines = environment::delete::delete(&platform, &dev_triple, true).await?;
    let qa_lines = environment::delete::delete(&platform, &qa_triple, false).await?;
    let (admin, admin_task) = connect(&admin_url).await?;
    let exists = async |database: &str| -> anyhow::Result<bool> {
        Ok(admin
            .query_opt("SELECT 1 FROM pg_database WHERE datname = $1", &[&database])
            .await?
            .is_some())
    };
    let dev_kept = exists(&dev_database).await?;
    let qa_kept = exists(&qa_database).await?;
    admin_task.abort();
    let (system, system_task) = connect(&system_url).await?;
    system.batch_execute("SET ROLE wamn_system").await?;
    let rows: i64 = system
        .query_one(
            "SELECT count(*) FROM registry.project_envs WHERE org = $1 AND project = $2",
            &[&ORG, &PROJECT],
        )
        .await?
        .get(0);
    system_task.abort();
    run.pass(
        "delete --data drops the database; a plain delete keeps it",
        &format!("wamn-ctl env delete {dev_triple} --data; wamn-ctl env delete {qa_triple}"),
        json!({"dev": dev_lines, "qa": qa_lines, "dev_database_kept": dev_kept,
            "qa_database_kept": qa_kept, "rows": rows}),
        !dev_kept && qa_kept && rows == 0,
    );

    run.record(
        "A6 kind half",
        "",
        json!({"reason": "the fixture release has no staged package upgrade, so no old release serves on an expanded schema between steps 5 and 6"}),
        "not-run",
    );
    run.record(
        "A7 kind half",
        "",
        json!({"reason": "the fixture release has no http attachment, so no route host is served"}),
        "not-run",
    );
    Ok(())
}

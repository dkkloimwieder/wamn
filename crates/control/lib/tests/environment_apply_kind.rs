//! `wamn-ctl env apply` on kind (docs/plan/platform-deploy.md §10.1 steps 4
//! to 7, §10.2, R6, R22, §3 A6, A7 and A12; issue `wamn-snz0.3`).
//!
//! This is the one kind suite of the platform. `apply` ensures the
//! environment's `Database` CR, so the suite runs CloudNativePG: a fresh kind
//! cluster `wamn-apply-<pid>` (epic decision D9),
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
//!    backlog it can claim finishes the run and exits with code 0 before
//!    `terminationGracePeriodSeconds` (R20 (2)). The fixture has no
//!    executable component, so the run is one the claim itself finishes: a
//!    `durable` run whose lease expired after an effect attempt, which the
//!    claim terminalizes as `effect-uncertain`.
//! 9. release = none: no host pod remains, the declared instance stays
//!    enabled, no binding remains.
//! 10. delete with a run pinned to a release with no pod refuses.
//! 11. and 12. delete --data drops the database; a plain delete leaves it on
//!    the cluster and removes the row.
//!
//!
//! Check of `wamn-snz0.5` (§3 A7, R2):
//! 13. A7: two environments serve one release with an http attachment, so each
//!    has the http role. A request to each environment's http Service with
//!    each route host reaches the route guest only under the Service's own
//!    route host; the other host is not routed.
//!
//! Checks of the release chart (§9.2, A1), after check 1:
//! 14. A1: the dev host's heartbeat carries `hostgroup` and `wamn.release`.
//! 15. A WorkloadDeployment that selects another `wamn.release` is not
//!     placed.
//! 16. After every check, the operator release has its revision and its
//!     Deployment's resourceVersion of before the first apply.
//!
//! A6 (kind half) needs a staged package upgrade; this fixture has none, and
//! the report says so.
//!
//! Run (epic closeout only):
//! `WAMN_SMOKE_HOST_IMAGE=wamn-host:<tag> cargo test -p wamn-control --test environment_apply_kind -- --ignored --nocapture`

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use wamn_control::delivery::selection::{image_set_digests, package_set};
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
use wamn_engine::release_manifest::release_label;

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
/// The canonical fixture release, A: one package, no http attachment, no
/// registration.
const MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"component":"http-request","descriptor":{"component":"http-request","component-digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"orders"}],"format-version":6,"release":{"packages":[{"package-id":"orders","package-version":"1.0.0"}]},"routes":[],"workflow":{"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"orders","wiring-id":"orders","wiring-version":1}]}}"#;
/// The second fixture release, B: the same package, another component digest.
const MANIFEST_B: &[u8] = br#"{"attachments":{},"components":[{"component":"http-request","descriptor":{"component":"http-request","component-digest":"sha256:4444444444444444444444444444444444444444444444444444444444444444","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"digest":"sha256:4444444444444444444444444444444444444444444444444444444444444444","interface-version":"0.1","operations":{"wamn:node/handler@0.1.0":{}},"package-id":"orders"}],"format-version":6,"release":{"packages":[{"package-id":"orders","package-version":"1.0.0"}]},"routes":[],"workflow":{"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"orders","wiring-id":"orders","wiring-version":1}]}}"#;
/// The third fixture release, H: the same package with one http attachment,
/// served with no authentication, so its environments render the http role.
const MANIFEST_H: &[u8] = br#"{"attachments":{"orders-read-http":{"auth-policy":{"modes":["none"]},"component":"http-request","definition":{"id":"orders-read-http","route":{"method":"GET","path":"/orders"},"run-deadline-ms":30000,"type":"http"},"definition-hash":"sha256:5555555555555555555555555555555555555555555555555555555555555555","operation":"orders:order/read@1.0.0","package-id":"orders","type":"http"}},"components":[{"component":"http-request","descriptor":{"component":"http-request","component-digest":"sha256:6666666666666666666666666666666666666666666666666666666666666666","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"digest":"sha256:6666666666666666666666666666666666666666666666666666666666666666","interface-version":"0.1","operations":{"orders:order/read@1.0.0":{}},"package-id":"orders"}],"format-version":6,"release":{"packages":[{"package-id":"orders","package-version":"1.0.0"}]},"routes":[{"component":"http-request","operation":"orders:order/read@1.0.0","package-id":"orders","type":"get"}],"workflow":{"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"orders","wiring-id":"orders","wiring-version":1}]}}"#;
/// The route guest's refusal of a path the release does not serve.
const ROUTE_NOT_FOUND: &str = r#"{"error":{"code":"route-not-found"}}"#;
/// A role image no WorkloadDeployment pulls: no fixture release has a
/// registration, so no materializer renders.
const UNPULLED: &str = "registry.invalid/wamn/unpulled@sha256:2222222222222222222222222222222222222222222222222222222222222222";
/// The package `orders@1.0.0` the fixture release names: no model, no
/// migration, no SQL, and one stateless command in its one component, the
/// shape of `apps/edge_device`.
const PACKAGE_MANIFEST: &str = r#"{"package":{"id":"orders","version":"1.0.0"},"required_platform_policy_contract":{"id":"orders_data_access","state":"satisfied"},"models":{},"custom_operations":{"order.read":{"type":"command","visibility":"public","permission":"order.read","input":{"fields":[{"path":"request_id","type":"text","nullable":false},{"path":"value.order","type":"text","nullable":false}]},"result":{"class":"one","fields":[{"path":"order","type":"text","nullable":false}]},"errors":["invalid_input","permission_denied","internal_error"],"error_details":{},"idempotent_by":"stateless","label":"Read an order","description":"The one operation of the apply fixture. It declares no SQL."}},"connections":[],"components":{"orders":{"connections":[]}}}"#;
/// The data access policy of [`PACKAGE_MANIFEST`]: no relation, no schema.
/// `manifest_sha256` is the SHA-256 of the manifest bytes.
const DATA_ACCESS: &str = r#"{"contract":"orders_data_access","manifest_sha256":"sha256:0bd4cf516c32a2db01711d915fb6ff947c649460eddc5031c3a9d4bd0950c0c2","package":"orders@1.0.0","relations":[],"role":"wamn_app","schemas":[]}"#;

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

    /// The Host objects whose heartbeat names `group`.
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

    /// The name and host of each Workload whose name starts with `prefix`.
    async fn placements(&self, prefix: &str) -> anyhow::Result<Vec<Value>> {
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
            .map(|workload| {
                json!({"name": workload["metadata"]["name"],
                    "hostId": workload["status"]["hostId"],
                    "conditions": workload["status"]["conditions"]})
            })
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
    let output = wamn_schema_generator::output_root(&package);
    std::fs::create_dir_all(output.join("platform-policy"))?;
    std::fs::create_dir_all(package.join("migrations"))?;
    std::fs::create_dir_all(packages.join("target"))?;
    std::fs::write(
        package.join("wamn.k"),
        "# orders@1.0.0, the apply fixture\n",
    )?;
    std::fs::write(output.join("wamn.json"), PACKAGE_MANIFEST)?;
    std::fs::write(output.join("platform-policy/data-access.json"), DATA_ACCESS)?;
    // The build index entry of the component: push-package lists it, and no
    // step pulls it.
    let component = packages.join("target/orders.wasm");
    std::fs::write(&component, b"orders")?;
    let recorded_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        + 1;
    std::fs::write(
        packages.join("target/components.json"),
        serde_json::to_vec(
            &json!([{"name": "orders", "crate": "orders", "file": component,
            "sha256": "1c168adb00d208e42f93314529f1fa9c0427eb63233ceda95a5db52b7012a719",
            "recorded_at": recorded_at}]),
        )?,
    )?;
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
            component_artifact_base: format!("{local}/wamn/components"),
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

    // The http role image: the route guest, built as the Dockerfile builds it
    // and pushed to the registry as a wasm component.
    let flags = run
        .run(
            &repository.join("tools/guest-rustflags").to_string_lossy(),
            &[],
        )
        .await?;
    let built = Command::new(env!("CARGO"))
        .current_dir(repository.join("apps"))
        .env("RUSTFLAGS", flags.trim())
        .args(["build", "--locked", "--offline", "--release"])
        .args(["--target", "wasm32-wasip2", "-p", "http-route"])
        .status()
        .await
        .context("start the route guest build")?;
    ensure!(built.success(), "the route guest build exited {built}");
    let guest = std::fs::read(repository.join("apps/target/wasm32-wasip2/release/http_route.wasm"))
        .context("read the route guest")?;
    wash_runtime::oci::set_extra_ca_certificates(&[work.join("ca.crt")])?;
    let http_digest = wash_runtime::oci::push_component(
        &format!("{local}/wamn/flow-http:apply"),
        &guest,
        wash_runtime::oci::OciConfig::new_with_credentials(USER, PASSWORD),
        None,
    )
    .await
    .context("push the http role image")?;
    ensure!(
        http_digest.starts_with("sha256:"),
        "the http role push returned {http_digest}"
    );
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
        http: format!("{local}/wamn/flow-http@{http_digest}"),
        materializer: UNPULLED.to_owned(),
    };
    let chart = stamp(
        &repository.join("deploy/platform/release"),
        &set,
        &work.join("chart"),
    )?;
    run.run("helm", &["dependency", "build", &chart.to_string_lossy()])
        .await?;

    // The qualification of each fixture release on the chart's image set (R12).
    for (fill, bytes) in [("9", MANIFEST), ("8", MANIFEST_B), ("7", MANIFEST_H)] {
        let (release, _) = wamn_catalog::ServingManifest::from_canonical_bytes(bytes)?;
        system
            .execute(
                "INSERT INTO catalog.qualifications (qualification_sha256, package_set, image_digests) \
                 VALUES ($1, $2::text::jsonb, $3::text::jsonb)",
                &[
                    &format!("sha256:{}", fill.repeat(64)),
                    &serde_json::to_string(&package_set(&release)?)?,
                    &serde_json::to_string(&image_set_digests(&set)?)?,
                ],
            )
            .await?;
    }
    system_task.abort();

    // The platform part of the host group: the registry mount.
    std::fs::write(
        work.join("host-group.yaml"),
        serde_yaml::to_string(&json!({
            "replicas": 1,
            "env": [
                {"name": "WAMN_REGISTRY_AUTH_FILE", "value": "/apply-registry/config.json"},
                {"name": "DOCKER_CONFIG", "value": "/apply-registry"},
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

    let operator_before = run.operator().await?;

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

    // 14. A1: the heartbeat carries the host group and the release.
    let label = release_label(&digest).context("label the fixture digest")?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut hosts = run.hosts(&name).await?;
    while hosts.len() != 1 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
        hosts = run.hosts(&name).await?;
    }
    let labels: Vec<Value> = hosts
        .iter()
        .map(|host| host["metadata"]["labels"].clone())
        .collect();
    run.pass(
        "A1 heartbeat labels",
        &format!("kubectl -n {NAMESPACE} get hosts -l hostgroup={name} -o json"),
        json!({"expected": {"hostgroup": name, "wamn.release": label}, "labels": labels}),
        labels.len() == 1
            && labels[0]["hostgroup"] == name.as_str()
            && labels[0]["wamn.release"] == label.as_str(),
    );

    // 15. A WorkloadDeployment that selects another release is not placed.
    let probe = format!("{name}-probe-wrong");
    let mut wrong = label.clone();
    let last = wrong.pop().context("the label is not empty")?;
    wrong.push(if last == 'a' { 'b' } else { 'a' });
    let selector = json!({"hostgroup": name, "wamn.release": wrong});
    std::fs::write(
        work.join("probe-wrong.json"),
        serde_json::to_vec_pretty(&json!({
            "apiVersion": "runtime.wasmcloud.dev/v1alpha1",
            "kind": "WorkloadDeployment",
            "metadata": {"name": probe, "namespace": NAMESPACE},
            "spec": {"replicas": 1, "template": {"spec": {
                "hostSelector": selector,
                "environment": NAMESPACE,
                "components": [{"name": "probe", "image": UNPULLED}],
            }}},
        }))?,
    )?;
    run.run("kubectl", &["apply", "-f", &path("probe-wrong.json")])
        .await?;
    tokio::time::sleep(Duration::from_secs(120)).await;
    let workloads = run.placements(&probe).await?;
    run.pass(
        "not placed on a wrong wamn.release",
        &format!("kubectl apply -f probe-wrong.json; sleep 120; kubectl -n {NAMESPACE} get workloads -o json (names {probe}*)"),
        json!({"hostSelector": selector, "workloads": workloads}),
        workloads
            .iter()
            .all(|workload| workload["hostId"].as_str().is_none_or(str::is_empty)),
    );
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "delete",
            "workloaddeployment",
            &probe,
            "--wait",
        ],
    )
    .await?;

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
    // it can claim finishes the run and exits with code 0 before its grace
    // period (R20 (2)). The run turns claimable ten seconds after it is
    // seeded, once the pod is draining. It is `durable`, its prior lease
    // expired after an effect attempt, so the claim terminalizes it as
    // `effect-uncertain` with no component to execute.
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
                wiring_id, wiring_version, status, trigger_source, input_json, service_principal_id, \
                durability_class) \
             VALUES ($1, 'drain-claimable', 'orders', 1, 'orders', $2, 'dev', 'orders', 1, \
                     'running', 'automation', '{}', '00000000-0000-0000-0000-000000000001', \
                     'durable')",
            &[&tenant, &digest],
        )
        .await?;
    project
        .execute(
            "INSERT INTO wamn_run.run_queue \
               (tenant_id, run_id, available_at, lease_owner, lease_expires_at, lease_generation) \
             VALUES ($1, 'drain-claimable', now() + interval '10 seconds', 'gone-replica', \
                     '2000-01-01', 1)",
            &[&tenant],
        )
        .await?;
    let hash = format!("sha256:{}", "5".repeat(64));
    project
        .execute(
            "INSERT INTO wamn_run.effect_attempts \
               (tenant_id, run_id, root_plan_hash, current_plan_hash, frame_id, local_node_id, \
                source_artifact_hash, requirement_name, occurrence, seq, generation_fact_type, \
                attempt_deadline_at, attempt_input_ref) \
             VALUES ($1, 'drain-claimable', $2, $2, 0, 'effect-node', $2, 'manager', 0, 1, \
                     'not-required', '2099-01-01T00:00:00Z', 'sha256:drain-effect-input')",
            &[&tenant, &hash],
        )
        .await?;
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
    // The host's own log of its drain, kept in the report.
    let host_log = work.join("drain-host.log");
    let mut follow = Command::new("kubectl")
        .args(["-n", NAMESPACE, "logs", "--follow", &pod])
        .env("KUBECONFIG", work.join("kubeconfig"))
        .stdout(std::fs::File::create(&host_log)?)
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("follow the host log")?;
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
    let finished = project
        .query_one(
            "SELECT r.status, EXISTS (SELECT 1 FROM wamn_run.run_queue q \
                                      WHERE q.tenant_id = r.tenant_id AND q.run_id = r.run_id) \
               FROM wamn_run.runs r WHERE r.tenant_id = $1 AND r.run_id = 'drain-claimable'",
            &[&tenant],
        )
        .await?;
    let (status, queued): (String, bool) = (finished.get(0), finished.get(1));
    let _ = follow.kill().await;
    let log = std::fs::read_to_string(&host_log).unwrap_or_default();
    let log_tail: Vec<&str> = log
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    run.pass(
        "host_exits_before_its_grace_period",
        &format!("kubectl -n {NAMESPACE} delete pod {pod}; kubectl get pod {pod} -o json"),
        json!({"grace_seconds": grace, "exit_code": exit_code, "exited_after_seconds": exited_after,
            "run_status": status, "run_queued": queued, "host_log_tail": log_tail}),
        exit_code == Some(0)
            && exited_after.is_some_and(|seconds| seconds < grace)
            && status == "effect-uncertain"
            && !queued,
    );

    // The run that check 10 finds stranded: pinned to A and never claimable.
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
    // 13. A7: each route host reaches only its own environment.
    let digest_h = push_manifest_bytes(
        MANIFEST_H,
        &format!("{local}/wamn/releases"),
        false,
        &platform.oci_ca_paths,
        &work.join("auth.json"),
    )
    .await
    .context("push the http fixture release manifest")?
    .digest
    .as_str()
    .to_owned();
    let served = [
        ("east", "east.orders.example"),
        ("west", "west.orders.example"),
    ];
    for (env, route_host) in served {
        let mut http = document(env, route_host);
        http.release = DeclaredRelease::Digest(digest_h.clone());
        apply(&http, &format!("{env}-http")).await?;
    }
    let mut answers = Vec::new();
    let mut own_only = true;
    for (service_env, _) in served {
        let service = format!(
            "http://{}-http.{NAMESPACE}.svc.cluster.local/no-such-route",
            release_name(ORG, PROJECT, service_env)?
        );
        for (host_env, route_host) in served {
            let (status, body) = route_probe(run, &service, route_host).await?;
            let reached = status == 404 && body == ROUTE_NOT_FOUND;
            own_only &= reached == (service_env == host_env);
            answers.push(json!({"service": service_env, "host": route_host,
                "status": status, "body": body, "reached_the_route_guest": reached}));
        }
    }
    run.pass(
        "A7 each route host reaches only its own environment",
        "wamn-ctl env apply east-http.k west-http.k; curl -H 'Host: <route host>' http://<release>-http/no-such-route",
        json!({"release": digest_h, "answers": answers}),
        own_only,
    );

    // 16. The operator release is untouched.
    let operator_after = run.operator().await?;
    run.pass(
        "operator release untouched",
        &format!("helm -n {NAMESPACE} list -o json; kubectl -n {NAMESPACE} get deployment runtime-operator -o json"),
        json!({"before": operator_before, "after": operator_after}),
        operator_before == operator_after,
    );
    Ok(())
}

/// GET `url` from a pod in the namespace with `Host: route_host`, and return
/// the status and the body.
async fn route_probe(run: &Run, url: &str, route_host: &str) -> anyhow::Result<(u16, String)> {
    let pod = format!("route-probe-{}", route_host.replace('.', "-"));
    let script = format!(
        "curl --silent --show-error --connect-timeout 5 --max-time 15 --output /tmp/body \
         --write-out '%{{http_code}}\\n' --header 'Host: {route_host}' '{url}'; cat /tmp/body"
    );
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "run",
            &pod,
            "--restart=Never",
            &format!(
                "--image={}",
                wamn_test_infrastructure::workload::HTTP_PROBE_IMAGE
            ),
            "--command",
            "--",
            "/bin/sh",
            "-ec",
            &script,
        ],
    )
    .await?;
    let waited = run
        .run(
            "kubectl",
            &[
                "-n",
                NAMESPACE,
                "wait",
                "--for=jsonpath={.status.phase}=Succeeded",
                &format!("pod/{pod}"),
                "--timeout=120s",
            ],
        )
        .await;
    let logs = run
        .run("kubectl", &["-n", NAMESPACE, "logs", &format!("pod/{pod}")])
        .await;
    run.run(
        "kubectl",
        &["-n", NAMESPACE, "delete", "pod", &pod, "--wait=true"],
    )
    .await?;
    waited?;
    let logs = logs?;
    let (status, body) = logs
        .split_once('\n')
        .with_context(|| format!("the route probe printed {logs:?}"))?;
    Ok((
        status.trim().parse().context("the route probe status")?,
        body.to_owned(),
    ))
}

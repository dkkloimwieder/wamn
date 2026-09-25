//! Receiving through the kind edge: one HTTPS host serves the built web files
//! and forwards `/password` and `/api` (docs/plan/web-deployment.md).

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use reqwest::StatusCode;
use reqwest::header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, ETAG, IF_NONE_MATCH, SET_COOKIE};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_control::print_release_env::lookup_release_carrier;
use wamn_control::project_env_membership::{self, ProjectEnvMembershipRequest};
use wamn_test_infrastructure::rendering::{HttpClaims, HttpWorkloadInput, render_http_workload};
use wamn_test_infrastructure::workload;

use super::super::{ENVIRONMENT, ORG, PROJECT, RELEASE_ID, ROUTE_CALLER_ROLE, TENANT};
use super::resources::{self, checked, write_private};
use super::{ReceivingCluster, apply, deployment, install_host, kubectl, provision, route_cases};
use super::{session_cluster, start};

const EDGE: &str = "wamn-edge";
const IDENTITY: &str = "host-session-identity";
const EMAIL: &str = "edge-operator@example.test";
const PASSWORD: &str = "edge-disposable-fixture-password";
/// The build that `pnpm run build` writes in `apps/wamn_receiving/web`.
const WEB_DIST: &str = "WAMN_EDGE_WEB_DIST";
/// When set, the case keeps the cluster for a browser run until `edge.done`
/// appears in its results directory.
const BY_HAND: &str = "WAMN_EDGE_BY_HAND";

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl, openssl, WAMN_EDGE_WEB_DIST"]
async fn receiving_through_the_edge() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "docker", "kind", "kubectl", "helm", "jq", "curl", "openssl",
    ]);
    let dist = PathBuf::from(std::env::var(WEB_DIST).with_context(|| {
        format!("{WEB_DIST} names the output of pnpm run build in apps/wamn_receiving/web")
    })?);
    ensure!(
        dist.is_absolute() && dist.join("index.html").is_file(),
        "{WEB_DIST} must be an absolute build directory with index.html"
    );
    let evidence = super::evidence_directory()?;
    Box::pin(super::with_signals(&evidence, run(&evidence, &dist))).await
}

async fn run(evidence: &Path, dist: &Path) -> anyhow::Result<()> {
    let mut cluster = start(evidence, true).await?;
    let result = async {
        // The session case setup: the session release, identity and two
        // session hosts, then the route ingress on them.
        let (route, _) = provision(&cluster.inputs, &cluster.artifacts).await?;
        let fixture = cluster.resources.work.join("session-host-fixture.json");
        super::super::sessions::prepare_session_host_fixture(&cluster.inputs, &fixture).await?;
        let carrier = lookup_release_carrier(
            &route.database_url,
            TENANT,
            RELEASE_ID + 1,
            &cluster.inputs.release_artifact_base,
        )
        .await?;
        let issuer = session_cluster::prepare(&cluster, &carrier, &fixture).await?;
        let document: Value = serde_json::from_slice(&fs::read(&fixture)?)?;
        let suffix = document["instance_suffix"]
            .as_str()
            .context("the session fixture has its instance suffix")?;
        let audience = document["audience"]
            .as_str()
            .context("the session fixture has its audience")?
            .to_owned();
        let secrets = deployment::native_secrets(&cluster)?;
        install_host(
            &cluster.resources,
            &cluster.inputs,
            &carrier,
            &super::HostBinding {
                replicas: 2,
                nats_url: &cluster.nats_url,
                native_nats_secrets: &secrets,
                source: &cluster.source,
                session: Some((&issuer, suffix)),
            },
        )
        .await?;
        let resources = &cluster.resources;
        let digest = workload::image_ready(
            &resources.lifecycle,
            &resources.name,
            &resources.work,
            &resources.host_image,
            &resources.source,
            "release",
            evidence,
        )
        .await?;
        workload::hosts_ready(&workload::HostsReadyInput {
            lifecycle: &resources.lifecycle,
            cluster: &resources.name,
            work: &resources.work,
            namespace: &resources.name,
            image: &resources.host_image,
            runtime_digest: &digest,
            replicas: 2,
            evidence,
        })
        .await?;
        let http = deployment::publish_http(&cluster).await?;
        route_ingress(&cluster, &http).await?;
        activate_session_key(&cluster, &issuer).await?;
        serve_passwords(&cluster).await?;
        let account = account(&cluster, &route.database_url).await?;
        let edge = install_edge(&cluster, dist).await?;
        check_edge(&cluster, &edge, &account, &audience).await?;
        if std::env::var_os(BY_HAND).is_some() {
            hold(&cluster, &edge, &audience).await?;
        }
        super::assert_source_unchanged(&cluster.resources).await
    }
    .await;
    route_cases::finish(&mut cluster, result).await
}

/// The route ingress on the session hosts, from the deployment example.
async fn route_ingress(cluster: &ReceivingCluster, http_image: &str) -> anyhow::Result<()> {
    let resources = &cluster.resources;
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
    let path = resources.evidence.join("edge-route-workload.yaml");
    fs::write(&path, source)?;
    apply(resources, &path).await?;
    checked(kubectl(resources).args([
        "-n",
        &resources.name,
        "wait",
        "--for=condition=Ready",
        "workloaddeployment/flow-http",
        "--timeout=240s",
    ]))
    .await?;
    Ok(())
}

/// The hosts verify a session against the issuer's active key.
async fn activate_session_key(cluster: &ReceivingCluster, issuer: &str) -> anyhow::Result<()> {
    let database = wamn_control::provision_project_env::secret_value(
        &cluster.resources.work.join("session-identity-db.json"),
        "url",
    )?;
    let (mut client, connection) =
        tokio_postgres::connect(&database, tokio_postgres::NoTls).await?;
    let driver = tokio::spawn(connection);
    let result = async {
        let key =
            wamn_platform_identity::session_keys::publish_session_key(&mut client, issuer).await?;
        wamn_platform_identity::session_keys::activate_session_key(&mut client, issuer, &key.kid)
            .await
    }
    .await;
    driver.abort();
    result?;
    Ok(())
}

/// Identity serves `/password` only with a mail configuration. The account
/// below takes its invitation from the database, so no mail is sent.
async fn serve_passwords(cluster: &ReceivingCluster) -> anyhow::Result<()> {
    let resources = &cluster.resources;
    checked(kubectl(resources).args([
        "-n",
        &resources.name,
        "create",
        "secret",
        "generic",
        "edge-mail",
        "--from-literal=api-key=unused-edge-fixture",
    ]))
    .await?;
    let output = checked(
        Command::new("helm")
            .args(["upgrade", IDENTITY])
            .arg(resources.repository.join("deploy/platform/identity"))
            .arg("--kubeconfig")
            .arg(resources.work.join("kubeconfig"))
            .arg("--kube-context")
            .arg(format!("kind-{}", resources.name))
            .args(["--namespace", &resources.name, "--reuse-values"])
            .args(["--set-string", "resendSecret=edge-mail"])
            .args(["--set-string", "resendFrom=WAMN <fixture@example.invalid>"])
            .args(["--wait", "--timeout", "180s"]),
    )
    .await?;
    fs::write(resources.evidence.join("edge-identity-upgrade.log"), output)?;
    Ok(())
}

struct Account {
    human: String,
    invitation: String,
}

/// One person with the route-caller role and an unused invitation.
async fn account(cluster: &ReceivingCluster, project_url: &str) -> anyhow::Result<Account> {
    let system = &cluster.inputs.system_pg_url;
    let (admin, admin_task) = super::super::connect(system).await?;
    let actor = wamn_control_provision::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    admin
        .execute("SELECT set_config('app.user_id', $1, false)", &[&actor])
        .await?;
    let human =
        wamn_platform_identity::create_human(admin.as_ref(), EMAIL, EMAIL, "Edge operator").await?;
    project_env_membership::grant(ProjectEnvMembershipRequest {
        org: ORG.to_owned(),
        project: PROJECT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        principal_id: human.id().to_string(),
        system_database_url: system.clone(),
    })
    .await?;
    // An invitation secret is the prefix and 64 lowercase hexadecimal digits.
    let invitation = format!(
        "wamn_inv_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let hash = <sha2::Sha256 as sha2::Digest>::digest(invitation.as_bytes()).to_vec();
    admin
        .execute(
            "SELECT set_config('app.operation', 'admin:seed-identity-fixture', false), \
             set_config('app.tenant_id', $1, false)",
            &[&TENANT],
        )
        .await?;
    admin
        .execute(
            "INSERT INTO identity.password_tokens (token_hash, principal_id, purpose, expires_at) \
             VALUES ($1, $2::text::uuid, 'invitation', now() + interval '2 hours')",
            &[&hash, &human.id().as_str()],
        )
        .await?;
    admin_task.abort();
    let (project, project_task) = super::super::connect(project_url).await?;
    super::super::bind_fixture_principal(project.as_ref(), TENANT).await?;
    project
        .execute(
            "INSERT INTO app_system.users (tenant_id, id, type, email, status) \
             VALUES ($1, $2::text::uuid, 'person', $3, 'active')",
            &[&TENANT, &human.id().as_str(), &EMAIL],
        )
        .await?;
    project
        .execute(
            "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) \
             VALUES ($1, $2::text::uuid, $3)",
            &[&TENANT, &human.id().as_str(), &ROUTE_CALLER_ROLE],
        )
        .await?;
    project_task.abort();
    Ok(Account {
        human: human.id().to_string(),
        invitation,
    })
}

struct Edge {
    /// The node address that reaches the edge Service.
    address: SocketAddr,
    /// The certificate authority of the edge certificate.
    ca: PathBuf,
}

/// The build in a bucket, and the edge chart in front of it.
async fn install_edge(cluster: &ReceivingCluster, dist: &Path) -> anyhow::Result<Edge> {
    let resources = &cluster.resources;
    write_private(
        &resources.work.join("minio.env"),
        format!(
            "MINIO_ROOT_USER=edge\nMINIO_ROOT_PASSWORD={}\n",
            uuid::Uuid::new_v4().simple()
        )
        .as_bytes(),
    )?;
    checked(
        Command::new(&resources.lifecycle)
            .arg("edge-bucket")
            .arg(&resources.name)
            .arg(&resources.work)
            .arg(dist),
    )
    .await?;
    let minio = resources::kind_address(&resources::inspect(resources, "minio").await?)?;
    let namespace = &resources.name;
    let values = [
        format!("host={}", cluster.inputs.route_host),
        "tlsSecret=wamn-edge-tls".to_owned(),
        "issuer=wasmcloud-ca".to_owned(),
        format!("api=http://flow-http.{namespace}.svc.cluster.local"),
        format!("identity=https://{IDENTITY}.{namespace}.svc.cluster.local"),
        "identityCaConfigMap=host-session-public-ca".to_owned(),
        format!("bucket.endpoint=http://{minio}:9000"),
        "bucket.name=web".to_owned(),
        "service.type=NodePort".to_owned(),
    ];
    let mut command = Command::new("helm");
    command
        .args(["upgrade", "--install", EDGE])
        .arg(resources.repository.join("deploy/platform/edge"))
        .arg("--kubeconfig")
        .arg(resources.work.join("kubeconfig"))
        .arg("--kube-context")
        .arg(format!("kind-{namespace}"))
        .args(["--namespace", namespace]);
    for value in &values {
        command.arg("--set-string").arg(value);
    }
    let output = checked(command.args(["--wait", "--timeout", "180s"])).await?;
    fs::write(resources.evidence.join("edge-install.log"), output)?;
    let service: Value = serde_json::from_slice(
        &checked(kubectl(resources).args(["-n", namespace, "get", "service", EDGE, "-o", "json"]))
            .await?,
    )?;
    let port = service
        .pointer("/spec/ports/0/nodePort")
        .and_then(Value::as_u64)
        .context("the edge Service has a node port")?;
    let node = resources::kind_address(&resources::inspect(resources, "control-plane").await?)?;
    let ca = checked(kubectl(resources).args([
        "-n",
        namespace,
        "get",
        "secret",
        "wamn-edge-tls",
        "-o",
        "jsonpath={.data.ca\\.crt}",
    ]))
    .await?;
    let ca = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, ca.trim_ascii())?;
    let path = resources.work.join("edge-ca.crt");
    write_private(&path, &ca)?;
    Ok(Edge {
        address: SocketAddr::new(node.into(), u16::try_from(port)?),
        ca: path,
    })
}

/// The edge rules, through the node port, as a browser meets them.
async fn check_edge(
    cluster: &ReceivingCluster,
    edge: &Edge,
    account: &Account,
    audience: &str,
) -> anyhow::Result<()> {
    let host = &cluster.inputs.route_host;
    let http = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .tls_certs_only(reqwest::Certificate::from_pem_bundle(&fs::read(&edge.ca)?)?)
        .resolve(host, edge.address)
        .build()?;
    let base = format!("https://{host}:{}", edge.address.port());
    let mut results = serde_json::Map::new();

    // A page path answers with the index, which the browser revalidates. A
    // new node port refuses until kube-proxy programs it.
    let deadline = Instant::now() + Duration::from_secs(60);
    let page = loop {
        match http.get(format!("{base}/purchase-orders")).send().await {
            Ok(page) => break page,
            Err(error) if error.is_connect() && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    ensure!(page.status() == StatusCode::OK, "the edge refused a page");
    ensure!(
        header(&page, CACHE_CONTROL) == "no-cache"
            && header(&page, CONTENT_TYPE).starts_with("text/html"),
        "a page is html with no-cache"
    );
    let index = page.text().await?;
    let asset = index
        .split('"')
        .find(|part| {
            part.starts_with("/assets/")
                && Path::new(part)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("js"))
        })
        .context("the index names its script")?
        .to_owned();
    let script = http.get(format!("{base}{asset}")).send().await?;
    ensure!(
        script.status() == StatusCode::OK
            && header(&script, CACHE_CONTROL) == "public, max-age=31536000, immutable",
        "a built file is immutable"
    );
    results.insert("page".into(), json!({"cache_control": "no-cache"}));
    results.insert(
        "asset".into(),
        json!({"path": asset, "cache_control": "public, max-age=31536000, immutable"}),
    );

    let enroll = http
        .post(format!("{base}/password/enroll"))
        .json(&json!({"principal_id": account.human, "invitation": account.invitation, "password": PASSWORD}))
        .send()
        .await?;
    ensure!(
        enroll.status().is_success(),
        "enrollment through the edge answered {}",
        enroll.status()
    );
    let login = http
        .post(format!("{base}/password/session"))
        .json(&json!({"email": EMAIL, "password": PASSWORD, "aud": audience, "carrier": "cookie"}))
        .send()
        .await?;
    ensure!(
        login.status() == StatusCode::OK,
        "cookie login through the edge answered {}",
        login.status()
    );
    let cookies = login
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .map(|value| value.to_str().map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    let pair = |name: &str| {
        cookies
            .iter()
            .find(|cookie| cookie.starts_with(&format!("{name}=")))
            .and_then(|cookie| cookie.split(';').next())
            .map(str::to_owned)
            .with_context(|| format!("the login set {name}"))
    };
    let session = pair("__Host-wamn-session")?;
    let csrf = pair("__Host-wamn-csrf")?;
    pair("__Secure-wamn-renewal")?;
    ensure!(
        cookies
            .iter()
            .all(|cookie| cookie.contains("Secure") && !cookie.contains("Domain")),
        "every session cookie is Secure with no Domain"
    );
    let jar = format!("{session}; {csrf}");
    let csrf = csrf
        .split_once('=')
        .map(|(_, value)| value.to_owned())
        .context("the CSRF cookie has a value")?;
    results.insert("login".into(), json!({"cookies": cookies.len()}));

    // A list read, then the same read with its tag.
    let orders_url = format!("{base}/api/purchase_order/query?limit=5");
    let orders = http.get(&orders_url).header(COOKIE, &jar).send().await?;
    ensure!(
        orders.status() == StatusCode::OK,
        "the order list through the edge answered {}",
        orders.status()
    );
    let tag = header(&orders, ETAG).to_owned();
    let cache = header(&orders, CACHE_CONTROL).to_owned();
    ensure!(
        tag.starts_with("W/") && cache.starts_with("private"),
        "a list has a weak tag and is private"
    );
    let orders: Value = orders.json().await?;
    let revalidated = http
        .get(&orders_url)
        .header(COOKIE, &jar)
        .header(IF_NONE_MATCH, &tag)
        .send()
        .await?;
    ensure!(
        revalidated.status() == StatusCode::NOT_MODIFIED,
        "the same list with its tag answered {}",
        revalidated.status()
    );
    results.insert(
        "list".into(),
        json!({"etag": tag, "cache_control": cache, "revalidated": 304}),
    );

    // A supplier change, with the CSRF header a cookie write carries.
    let order = first_with(&orders, "row_version").context("the list has an order")?;
    let suppliers: Value = http
        .get(format!("{base}/api/supplier/query?limit=5"))
        .header(COOKIE, &jar)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let supplier = rows(&suppliers)
        .into_iter()
        .find(|row| row["id"] != order["supplier_id"])
        .context("a second supplier exists")?;
    let revision = order["row_version"]
        .as_i64()
        .or_else(|| {
            order["row_version"]
                .as_str()
                .and_then(|text| text.parse().ok())
        })
        .context("the order has its revision")?;
    let change = http
        .post(format!("{base}/api/purchase_order/update"))
        .header(COOKIE, &jar)
        .header("x-wamn-csrf", &csrf)
        .json(&json!([{
            "request_id": uuid::Uuid::new_v4().to_string(),
            "id": order["id"],
            "expected_row_version": revision,
            "change": {"supplier_id": supplier["id"]},
        }]))
        .send()
        .await?;
    ensure!(
        change.status() == StatusCode::OK,
        "the supplier change through the edge answered {}",
        change.status()
    );
    let changed: Value = change.json().await?;
    ensure!(
        first_with(&changed, "row_version").is_some_and(|row| row["supplier_id"] == supplier["id"]),
        "the change returns the order with its new supplier: {changed}"
    );
    results.insert("write".into(), json!({"status": 200}));
    fs::write(
        cluster.resources.evidence.join("edge-results.json"),
        serde_json::to_vec_pretty(&results)?,
    )?;
    Ok(())
}

/// Keep the cluster for a browser run, then return to teardown.
async fn hold(cluster: &ReceivingCluster, edge: &Edge, audience: &str) -> anyhow::Result<()> {
    let evidence = &cluster.resources.evidence;
    fs::write(
        evidence.join("edge.json"),
        serde_json::to_vec_pretty(&json!({
            "host": cluster.inputs.route_host,
            "address": edge.address.to_string(),
            "ca": edge.ca,
            "email": EMAIL,
            "password": PASSWORD,
            "audience": audience,
        }))?,
    )?;
    let done = evidence.join("edge.done");
    println!(
        "EDGE_BY_HAND edge={} host={} results={}; create {} to tear down",
        edge.address,
        cluster.inputs.route_host,
        evidence.display(),
        done.display()
    );
    let deadline = Instant::now() + Duration::from_hours(3);
    while !done.exists() {
        ensure!(Instant::now() < deadline, "the browser run did not finish");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Ok(())
}

fn header(response: &reqwest::Response, name: reqwest::header::HeaderName) -> &str {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// Every object in a response that carries an `id`.
fn rows(value: &Value) -> Vec<&Value> {
    let mut found = Vec::new();
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(object) => {
                if object.contains_key("id") {
                    found.push(value);
                } else {
                    pending.extend(object.values());
                }
            }
            Value::Array(items) => pending.extend(items.iter().rev()),
            _ => {}
        }
    }
    found
}

fn first_with<'a>(value: &'a Value, field: &str) -> Option<&'a Value> {
    rows(value).into_iter().find(|row| row.get(field).is_some())
}

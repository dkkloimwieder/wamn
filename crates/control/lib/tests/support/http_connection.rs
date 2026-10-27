//! The real HTTP connection guest and its in-cluster recording peer.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use chrono::Utc;
use serde_json::{Value, json};

use super::{NAMESPACE, ORG, PACKAGE, PASSWORD, PROJECT, Run, USER};
use wamn_catalog::{
    ComponentDescriptor, ComponentPackageScope, ServingComponent, ServingManifest, WiringDocument,
};

pub const SERVICE: &str = "http-fixture";
pub const ORIGIN: &str = "http://http-fixture:8080";
pub const INBOUND_HANDLE: &str = "inbound-signature";
pub const INBOUND_SECRET: &str = "fixture-inbound-key";
pub const OUTBOUND_HANDLE: &str = "signed-upstream";
pub const OUTBOUND_SECRET: &str = "fixture-outbound-key";
pub const SCHEDULE: &str = "http-minute";
pub const OAUTH2_HANDLE: &str = "oauth2-upstream";
pub const OAUTH1_HANDLE: &str = "oauth1-upstream";

pub async fn deploy(run: &Run, repository: &Path, system_url: &str) -> anyhow::Result<()> {
    let consumer_key = std::env::var("WAMN_HTTP_FIXTURE_CONSUMER_KEY")?;
    let consumer_secret = std::env::var("WAMN_HTTP_FIXTURE_CONSUMER_SECRET")?;
    let token = std::env::var("WAMN_HTTP_FIXTURE_TOKEN")?;
    let token_secret = std::env::var("WAMN_HTTP_FIXTURE_TOKEN_SECRET")?;
    let realm = std::env::var("WAMN_HTTP_FIXTURE_REALM")?;
    let oauth1 = json!({"oauth1": {
        "consumer-key": consumer_key, "consumer-secret": consumer_secret,
        "token": token, "token-secret": token_secret, "realm": realm
    }})
    .to_string();
    let image = format!("{}-http-fixture:debug", run.cluster);
    run.run(
        "docker",
        &[
            "build",
            "-t",
            &image,
            &repository
                .join("crates/control/lib/tests/support/http-fixture")
                .to_string_lossy(),
        ],
    )
    .await?;
    run.run(
        "kind",
        &["load", "docker-image", "--name", &run.cluster, &image],
    )
    .await?;
    let file = run.work.join("http-fixture.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&json!({
            "apiVersion": "v1", "kind": "List", "items": [
                {
                    "apiVersion": "v1", "kind": "Secret",
                    "metadata": {"name": "import-source", "namespace": NAMESPACE},
                    "stringData": {"credentials.json": json!({"orgs": {ORG: {"netsuite": oauth1}}, "projects": {
                        PROJECT: {
                            "upstream": json!({
                                "headers": {"authorization": "Bearer fixture-token"}
                            }).to_string(),
                            OAUTH2_HANDLE: json!({"oauth2": {
                                "token-url": format!("{ORIGIN}/oauth/token"),
                                "client-id": "fixture-client", "client-secret": "fixture-client-secret"
                            }}).to_string(),
                            OAUTH1_HANDLE: oauth1,
                            INBOUND_HANDLE: json!({"hmac": {
                                "algorithm": "sha256", "secret": INBOUND_SECRET
                            }}).to_string(),
                            OUTBOUND_HANDLE: json!({"hmac": {
                                "algorithm": "sha256", "secret": OUTBOUND_SECRET,
                                "header": "x-wamn-signature"
                            }}).to_string()
                        }
                    }}).to_string()}
                },
                {
                    "apiVersion": "apps/v1", "kind": "Deployment",
                    "metadata": {"name": SERVICE, "namespace": NAMESPACE},
                    "spec": {"replicas": 1, "selector": {"matchLabels": {"app": SERVICE}},
                        "template": {"metadata": {"labels": {"app": SERVICE}},
                            "spec": {"containers": [{
                                "name": SERVICE, "image": image, "imagePullPolicy": "Never",
                                "ports": [{"containerPort": 8080}],
                                "env": [{"name": "WAMN_HTTP_FIXTURE_HMAC_SECRET",
                                    "value": OUTBOUND_SECRET},
                                    {"name":"WAMN_HTTP_FIXTURE_CLIENT_ID", "value":"fixture-client"},
                                    {"name":"WAMN_HTTP_FIXTURE_CLIENT_SECRET", "value":"fixture-client-secret"},
                                    {"name":"WAMN_HTTP_FIXTURE_CONSUMER_KEY", "value":consumer_key},
                                    {"name":"WAMN_HTTP_FIXTURE_CONSUMER_SECRET", "value":consumer_secret},
                                    {"name":"WAMN_HTTP_FIXTURE_TOKEN", "value":token},
                                    {"name":"WAMN_HTTP_FIXTURE_TOKEN_SECRET", "value":token_secret},
                                    {"name":"WAMN_HTTP_FIXTURE_ACCESS_TOKEN", "value":"fixture-issued-token"},
                                    {"name":"WAMN_HTTP_FIXTURE_LIMIT", "value":"2"},
                                    {"name":"WAMN_HTTP_FIXTURE_REALM", "value":realm}],
                                "readinessProbe": {"tcpSocket": {"port": 8080}}
                            }]}}}
                },
                {
                    "apiVersion": "v1", "kind": "Service",
                    "metadata": {"name": SERVICE, "namespace": NAMESPACE},
                    "spec": {"selector": {"app": SERVICE},
                        "ports": [{"port": 8080, "targetPort": 8080}]}
                }
            ]
        }))?,
    )?;
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&file)?)?;
    let secret = manifest["items"].as_array_mut().unwrap().remove(0);
    wamn_control::org_connection::declare(
        &wamn_control::org_connection::DeclareConnectionRequest {
            system_database_url: system_url.to_owned(),
            org: ORG.into(),
            name: "netsuite".into(),
            requirement_type: wamn_catalog::RequirementType::Http,
            definition: json!({"primary-authority":ORIGIN, "tls-verification":"disabled"}),
        },
        &wamn_platform_identity::auth_changes::AuthChanges::default(),
    )
    .await?;
    wamn_control::connection_configuration::import(
        system_url,
        ORG,
        secret["stringData"]["credentials.json"].as_str().unwrap(),
    )
    .await?;
    std::fs::write(&file, serde_json::to_vec(&manifest)?)?;
    run.run("kubectl", &["apply", "-f", &file.to_string_lossy()])
        .await?;
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "rollout",
            "status",
            &format!("deployment/{SERVICE}"),
            "--timeout=120s",
        ],
    )
    .await?;
    Ok(())
}

/// Admit the actual debug guest bytes and upload the production artifact layout.
pub async fn component(
    run: &Run,
    repository: &Path,
    registry: &str,
    version: &str,
    package: &str,
    name: &str,
) -> anyhow::Result<ComponentDescriptor> {
    use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, Config, ImageLayer};
    use oci_client::manifest::OciImageManifest;
    use oci_client::secrets::RegistryAuth;
    use wamn_engine::component_artifact::{
        component_artifact_config_bytes, component_artifact_layout,
    };

    let target =
        repository.join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()));
    run.run(
        "cargo",
        &[
            "build",
            "--locked",
            "--offline",
            "--manifest-path",
            &repository
                .join("apps/platform/no-std/Cargo.toml")
                .to_string_lossy(),
            "--target-dir",
            &target.to_string_lossy(),
            "--target",
            "wasm32-wasip2",
            "-p",
            name,
        ],
    )
    .await?;
    let bytes = std::fs::read(
        target
            .join("wasm32-wasip2/debug")
            .join(format!("{}.wasm", name.replace('-', "_"))),
    )?;
    let admitted = admit_guest(repository, version, package, name, &bytes)?;
    let reference = wamn_catalog::component_artifact_reference(
        &format!("{registry}/wamn/components"),
        &admitted.component.component_digest,
    )?;
    let reference = oci_client::Reference::with_tag(
        reference.registry().to_owned(),
        reference.repository().to_owned(),
        reference.tag().to_owned(),
    );
    let config_bytes = component_artifact_config_bytes(&admitted.component);
    let layout = component_artifact_layout(&bytes, &config_bytes);
    let layer = ImageLayer::new(bytes.clone(), layout.layer_media_type().to_owned(), None);
    let config = Config::new(
        config_bytes.clone(),
        layout.config_media_type().to_owned(),
        None,
    );
    let manifest = OciImageManifest::build(std::slice::from_ref(&layer), &config, None);
    oci_client::Client::new(ClientConfig {
        extra_root_certificates: vec![Certificate {
            encoding: CertificateEncoding::Pem,
            data: std::fs::read(run.work.join("ca.crt"))?,
        }],
        read_timeout: Some(Duration::from_secs(30)),
        connect_timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    })
    .push(
        &reference,
        &[layer],
        config,
        &RegistryAuth::Basic(USER.to_owned(), PASSWORD.to_owned()),
        Some(manifest),
    )
    .await?;
    Ok(ComponentDescriptor::new(
        admitted.component,
        admitted.connections,
    ))
}

/// Admit the built bytes with the declarations used by the kind fixture.
pub fn admit_guest(
    repository: &Path,
    version: &str,
    package: &str,
    name: &str,
    bytes: &[u8],
) -> anyhow::Result<wamn_catalog::AdmittedComponentFacts> {
    let template = std::fs::read_to_string(
        repository.join(format!("apps/platform/no-std/{name}/declaration.json.in")),
    )?;
    let declaration = serde_json::from_value(declaration(
        &template
            .replace(
                "__TENANT_ID__",
                &wamn_control_registry::project_env_tenant(ORG, PROJECT, "http"),
            )
            .replace("__PACKAGE_ID__", package)
            .replace("__PACKAGE_VERSION__", version),
    )?)?;
    let engine = wamn_engine::build_engine(&[])?;
    let admitted = wamn_engine::component_admission::validate_component_admission(
        &engine,
        bytes,
        wamn_engine::component_admission::ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages: ["wamn:node".to_owned(), "wamn:connection".to_owned()]
                .into(),
            base_digests: BTreeSet::new(),
        },
    )?;
    Ok(admitted)
}

/// Declare the outbound handles before admission for ordinary binding.
pub fn declaration(template: &str) -> anyhow::Result<Value> {
    let mut declaration: Value = serde_json::from_str(template)?;
    if declaration["component"] == "http-request" {
        for handle in [OUTBOUND_HANDLE, OAUTH2_HANDLE, OAUTH1_HANDLE] {
            declaration["connections"]
                .as_array_mut()
                .expect("HTTP requirements")
                .push(json!({"store-alias": handle, "requirement-type": "http"}));
        }
    }
    Ok(declaration)
}

pub fn wiring() -> anyhow::Result<WiringDocument> {
    Ok(serde_json::from_value(json!({
        "format-version": "0.1", "wiring-id": "http-send", "version": 1,
        "entry": "input",
        "nodes": {
            "input": {"component": "transform", "interface-version": "0.1.0",
                "operation": "wamn:node/handler@0.1.0", "params": {"pointer": ""}},
            "send": {"component": "http-request", "interface-version": "0.1.0",
                "operation": "wamn:node/async-handler@0.1.0",
                "params": {"requirement": "upstream", "method": "POST", "path-and-query": "/reuse"}}
        },
        "edges": [{"from": "input", "to": "send"}]
    }))?)
}

pub fn signed_wiring() -> anyhow::Result<WiringDocument> {
    let mut graph = serde_json::to_value(wiring()?)?;
    graph["wiring-id"] = json!("http-signed-send");
    graph["nodes"]["send"]["params"]["requirement"] = json!(OUTBOUND_HANDLE);
    graph["nodes"]["send"]["params"]["path-and-query"] = json!("/signed");
    Ok(serde_json::from_value(graph)?)
}

pub fn oauth_wirings() -> anyhow::Result<Vec<WiringDocument>> {
    [
        ("http-oauth2", OAUTH2_HANDLE, "/items"),
        (
            "http-oauth1",
            OAUTH1_HANDLE,
            "/oauth1?z=a%2Bb&x=hello%20world&x=two",
        ),
    ]
    .into_iter()
    .map(|(id, handle, path)| {
        let mut graph = serde_json::to_value(wiring()?)?;
        graph["wiring-id"] = json!(id);
        graph["nodes"]["send"]["params"] = json!({
            "requirement":handle, "method":"GET", "path-and-query":path
        });
        Ok(serde_json::from_value(graph)?)
    })
    .collect()
}

pub fn paged_wiring() -> anyhow::Result<WiringDocument> {
    let mut graph = serde_json::to_value(wiring()?)?;
    graph["wiring-id"] = json!("http-pages");
    graph["nodes"]["send"]["component"] = json!("http-page");
    graph["nodes"]["send"]["params"] = json!({
        "requirement": "upstream", "path-and-query": "/pages?limit=2",
        "page": {"style": "offset", "limit": "limit", "offset": "offset"},
        "items": "/items"
    });
    Ok(serde_json::from_value(graph)?)
}

pub fn scheduled_wiring() -> anyhow::Result<WiringDocument> {
    let mut graph = serde_json::to_value(wiring()?)?;
    graph["wiring-id"] = json!(SCHEDULE);
    graph["nodes"]["send"]["params"]["path-and-query"] = json!("/scheduled");
    Ok(serde_json::from_value(graph)?)
}

pub fn release(
    h: &[u8],
    version: &str,
    components: Vec<ComponentDescriptor>,
) -> anyhow::Result<Vec<u8>> {
    let scope = ComponentPackageScope {
        tenant_id: wamn_control_registry::project_env_tenant(ORG, PROJECT, "http"),
        package_id: PACKAGE.to_owned(),
        package_version: version.to_owned(),
    };
    let facts: Vec<_> = components
        .iter()
        .cloned()
        .map(|descriptor| descriptor.into_admitted(scope.clone()).component)
        .collect();
    let (mut manifest, _) = ServingManifest::from_canonical_bytes(h)?;
    manifest
        .release
        .packages
        .retain(|package| package.package_id() != "label_fixture");
    manifest
        .release
        .packages
        .insert(wamn_catalog::PackageCoordinate::new(
            "http_fixture",
            "1.0.0",
        )?);
    for (descriptor, fact) in components.into_iter().zip(&facts) {
        manifest
            .components
            .insert(ServingComponent::project(fact, descriptor.connections)?);
    }
    let bytes = manifest.canonical_bytes();
    ServingManifest::from_canonical_bytes(&bytes)?;
    Ok(bytes)
}

pub fn assert_observed_request(status: u16, body: &str, log: &str) -> anyhow::Result<()> {
    ensure!(status == 200, "route returned {status}: {body}");
    let response: Value = serde_json::from_str(body)?;
    ensure!(response["status"] == 200, "guest response: {response}");
    ensure!(
        response["body"] == json!({"id": 1}),
        "guest body: {response}"
    );
    let records = route_requests(log)?;
    let requests: Vec<_> = records
        .iter()
        .map(|record| record["head"].as_str().unwrap_or_default())
        .collect();
    ensure!(
        requests.len() == 1,
        "fixture saw {} requests",
        requests.len()
    );
    ensure!(
        requests[0].lines().next() == Some("POST /reuse HTTP/1.1"),
        "fixture received the wrong target or protocol"
    );
    ensure!(
        records[0]["body"] == json!(br#"{"id":1}"#.as_slice()),
        "fixture received the wrong body"
    );
    let headers: Vec<_> = requests[0]
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .collect();
    let authorization: Vec<_> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.trim())
        .collect();
    ensure!(
        authorization == ["Bearer fixture-token"],
        "wrong credential reached the wire"
    );
    ensure!(
        headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("idempotency-key")
                && value.trim().ends_with(":send:0")),
        "effect omitted its delivery idempotency key"
    );
    Ok(())
}

pub fn request_records(log: &str) -> anyhow::Result<Vec<Value>> {
    log.lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn route_requests(log: &str) -> anyhow::Result<Vec<Value>> {
    Ok(request_records(log)?
        .into_iter()
        .filter(|record| {
            // The schedule has its own target, so its traffic cannot count as a route request.
            record["head"].as_str().is_some_and(|head| {
                head.starts_with("POST /reuse HTTP/1.1\r\n")
                    || head.starts_with("POST /signed HTTP/1.1\r\n")
            })
        })
        .collect())
}

pub fn bindings_match(rows: &Value, document: &super::EnvironmentDocument, digest: &str) -> bool {
    ["instances", "generations", "bindings"].iter().all(|name| {
        rows[name]
            .as_array()
            .is_some_and(|rows| rows.len() == if *name == "bindings" { 5 } else { 4 })
    }) && ["upstream", OUTBOUND_HANDLE, OAUTH2_HANDLE, OAUTH1_HANDLE]
        .iter()
        .all(|handle| {
            let find = |table: &str| {
                rows[table]
                    .as_array()
                    .and_then(|rows| rows.iter().find(|row| row["instance_id"] == format!("{PACKAGE}/{handle}")))
            };
            let (Some(instance), Some(generation)) = (find("instances"), find("generations"))
            else {
                return false;
            };
            let bindings: Vec<_> = rows["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["instance_id"] == format!("{PACKAGE}/{handle}"))
                .collect();
            instance["requirement_type"] == "http"
                && instance["active_generation"] == 1
                && generation["definition_json"]
                    == json!({"primary-authority":format!("{}/{}", ORIGIN, if *handle == OAUTH2_HANDLE {"oauth/"} else {""}),"tls-verification":"disabled"})
                && generation["credential_set_handle"].as_str().is_some_and(|value| value.starts_with(&format!("org:{ORG}:{}@", document.applications[PACKAGE].connections[*handle])))
                && bindings.len() == if *handle == "upstream" { 2 } else { 1 }
                && bindings.iter().all(|binding| {
                    binding["manifest_digest"] == digest
                        && binding["store_alias"] == *handle
                        && binding["binding_status"] == "active"
                        && binding["validation_status"] == "valid"
                })
        })
}

pub async fn request_log(run: &Run) -> anyhow::Result<String> {
    let (status, body) = super::route_probe(run, &format!("{ORIGIN}/recorded"), SERVICE).await?;
    ensure!(
        status == 200,
        "recorded requests endpoint returned {status}"
    );
    let requests: Vec<Value> = serde_json::from_str(&body)?;
    Ok(requests
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n"))
}

pub async fn assert_triggers(
    run: &mut Run,
    database: &tokio_postgres::Client,
    environment: &super::integration::WorkflowEnvironment<'_>,
) -> anyhow::Result<()> {
    let work = run.work.clone();
    wamn_gate_harness::run_check::check(
        &work,
        "assert_triggers",
        database,
        assert_triggers_inner(run, database, environment),
    )
    .await
}

async fn assert_triggers_inner(
    run: &mut Run,
    database: &tokio_postgres::Client,
    environment: &super::integration::WorkflowEnvironment<'_>,
) -> anyhow::Result<()> {
    for document in [wiring()?, signed_wiring()?, paged_wiring()?]
        .into_iter()
        .chain(oauth_wirings()?)
    {
        super::integration::enable_trigger(
            environment,
            PACKAGE,
            &document,
            &document.wiring_id,
            &[],
            wamn_control::author_workflow::WorkflowTrigger::Manual,
            None,
        )
        .await?;
    }
    let (unsigned_status, unsigned_body) =
        run_manual(&run.work, database, environment, "http-send", "unsigned").await?;
    let observed = request_log(run).await?;
    let result =
        wamn_gate_harness::run_check::check(&run.work, "unsigned_request", database, async {
            assert_observed_request(unsigned_status, &unsigned_body, &observed)
        })
        .await;
    run.collect("4 HTTP unsigned request", result);
    let work = run.work.clone();
    let result = wamn_gate_harness::run_check::check(&work, "signed_request", database, async {
        let (status, body) = run_manual(
            &run.work,
            database,
            environment,
            "http-signed-send",
            "signed",
        )
        .await?;
        let observed = request_log(run).await?;
        let records = request_records(&observed)?;
        let signed: Vec<_> = records
            .iter()
            .filter(|record| {
                record["head"]
                    .as_str()
                    .is_some_and(|head| head.starts_with("POST /signed HTTP/1.1\r\n"))
            })
            .collect();
        let passed = status == 200
            && serde_json::from_str::<Value>(&body)?
                == serde_json::from_str::<Value>(&unsigned_body)?
            && signed.len() == 1
            && signed[0]["signature_verified"] == true
            && signed[0]["body"] == json!(br#"{"id":1}"#.as_slice());
        run.pass(
            "HTTP guest sends once with a verified host signature",
            "POST /http/signed-send; GET /recorded",
            json!({"status": status, "body": body, "requests": signed}),
            passed,
        );
        ensure!(
            passed,
            "the fixture did not verify one signed outbound request"
        );

        Ok(())
    })
    .await;
    run.collect("4 HTTP signed request", result);
    let result = assert_oauth(run, database, environment).await;
    run.collect("4 HTTP OAuth", result);
    let result = assert_paged(run, database, environment).await;
    run.collect("4 HTTP paging", result);

    // The release declares no schedule. The scheduled wiring runs as an
    // environment workflow. Its nodes call no operation that needs a grant,
    // so its execution role holds no permission.
    let document = scheduled_wiring()?;
    super::integration::enable_schedule(environment, PACKAGE, &document, SCHEDULE, &[]).await?;
    let first_due = super::integration::next_minute(Utc::now());
    let result = scheduled_runs(&run.work, database, environment.tenant, first_due).await;
    run.pass("an enabled schedule workflow starts exactly one run per due key as the workflow user with its role",
        "author and enable the schedule workflow; SELECT schedule runs from wamn_run.runs",
        json!({"rows": result.as_ref().ok(), "error": result.as_ref().err().map(ToString::to_string)}),
        result.is_ok());
    result?;
    Ok(())
}

async fn run_manual(
    work: &std::path::Path,
    database: &tokio_postgres::Client,
    environment: &super::integration::WorkflowEnvironment<'_>,
    workflow: &str,
    key: &str,
) -> anyhow::Result<(u16, String)> {
    wamn_gate_harness::run_check::check(
        work,
        "run_manual",
        database,
        run_manual_inner(work, database, environment, workflow, key),
    )
    .await
}

async fn run_manual_inner(
    work: &std::path::Path,
    database: &tokio_postgres::Client,
    environment: &super::integration::WorkflowEnvironment<'_>,
    workflow: &str,
    key: &str,
) -> anyhow::Result<(u16, String)> {
    let id = super::integration::admit_workflow(
        work,
        environment,
        PACKAGE,
        workflow,
        &json!({"id":1}),
        key,
    )
    .await?;
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    loop {
        let row = database
            .query_one(
                "SELECT status, result_json, fail_type FROM wamn_run.runs \
                 WHERE tenant_id=$1 AND run_id=$2",
                &[&environment.tenant, &id],
            )
            .await?;
        let status: String = row.get(0);
        if status == "completed" {
            return Ok((200, row.get::<_, Value>(1).to_string()));
        }
        ensure!(
            !matches!(
                status.as_str(),
                "failed" | "infrastructure-failure" | "effect-uncertain"
            ),
            "workflow {workflow} failed: {status}, fail_type {:?}, result {:?}",
            row.get::<_, Option<String>>(2),
            row.get::<_, Option<Value>>(1)
        );
        ensure!(
            std::time::Instant::now() < deadline,
            "workflow {workflow} did not complete"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The runs of the schedule workflow at its first two due moments from
/// `first_due`, once both completed.
async fn scheduled_runs(
    work: &std::path::Path,
    database: &tokio_postgres::Client,
    tenant: &str,
    first_due: chrono::DateTime<Utc>,
) -> anyhow::Result<Value> {
    wamn_gate_harness::run_check::check(work, "scheduled_runs", database, async {
        tokio::time::timeout(
            Duration::from_secs(240),
            scheduled_runs_inner(database, tenant, first_due),
        )
        .await
        .context("schedule did not complete two due moments within 240 seconds")?
    })
    .await
}

async fn scheduled_runs_inner(
    database: &tokio_postgres::Client,
    tenant: &str,
    first_due: chrono::DateTime<Utc>,
) -> anyhow::Result<Value> {
    let workflow_user = wamn_project_state::workflow_user_id().to_string();
    let dues = [first_due, first_due + chrono::Duration::minutes(1)]
        .map(|due| due.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let keys = dues
        .clone()
        .map(|due| format!("schedule:{PACKAGE}/{SCHEDULE}:{due}"));
    loop {
        let rows: Value = database.query_one(
            "SELECT COALESCE(jsonb_agg(jsonb_build_object(\
                'idempotency_key', idempotency_key, 'trigger_source', trigger_source, \
                'execution_principal_id', execution_principal_id, 'execution_role', execution_role, \
                'invoked_by', invoked_by, 'input_json', input_json, 'status', status, \
                'package_id', package_id, 'wiring_id', wiring_id, 'wiring_version', wiring_version, \
                'fail_type', fail_type, 'result_json', result_json) \
                ORDER BY idempotency_key), '[]'::jsonb) FROM wamn_run.runs \
                WHERE tenant_id = $1 AND idempotency_key = ANY($2)",
            &[&tenant, &keys.as_slice()],
        ).await?.get(0);
        let runs = rows.as_array().context("schedule run rows")?;
        for (key, due) in keys.iter().zip(&dues) {
            let matching: Vec<_> = runs
                .iter()
                .filter(|row| row["idempotency_key"] == *key)
                .collect();
            ensure!(
                matching.len() <= 1,
                "multiple schedule runs for {key}: {rows}"
            );
            if let Some(row) = matching.first() {
                ensure!(
                    row["trigger_source"] == "schedule"
                        && row["invoked_by"] == "schedule"
                        && row["execution_principal_id"] == workflow_user
                        && row["execution_role"] == SCHEDULE
                        && row["package_id"] == PACKAGE
                        && row["wiring_id"] == SCHEDULE
                        && row["wiring_version"] == 1
                        && row["input_json"] == json!({"due": due}),
                    "schedule run has wrong identity, input or principal: {row}"
                );
                ensure!(
                    !matches!(
                        row["status"].as_str(),
                        Some("failed" | "infrastructure-failure" | "effect-uncertain")
                    ),
                    "scheduled workflow failed: {row}"
                );
            }
        }
        if runs.len() == 2 && runs.iter().all(|row| row["status"] == "completed") {
            return Ok(rows);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn assert_oauth(
    run: &mut Run,
    database: &tokio_postgres::Client,
    environment: &super::integration::WorkflowEnvironment<'_>,
) -> anyhow::Result<()> {
    let work = run.work.clone();
    wamn_gate_harness::run_check::check(
        &work,
        "assert_oauth",
        database,
        assert_oauth_inner(run, database, environment),
    )
    .await
}

async fn assert_oauth_inner(
    run: &mut Run,
    database: &tokio_postgres::Client,
    environment: &super::integration::WorkflowEnvironment<'_>,
) -> anyhow::Result<()> {
    for (index, route) in ["oauth2", "oauth2", "oauth1", "oauth1"]
        .into_iter()
        .enumerate()
    {
        let (status, body) = run_manual(
            &run.work,
            database,
            environment,
            &format!("http-{route}"),
            &format!("{route}-{index}"),
        )
        .await?;
        ensure!(
            status == 200 && serde_json::from_str::<Value>(&body)?["status"] == 200,
            "{route} refused: {status}: {body}"
        );
    }
    let records = request_records(&request_log(run).await?)?;
    let matching = |prefix: &str| {
        records
            .iter()
            .filter(|record| {
                record["head"]
                    .as_str()
                    .is_some_and(|head| head.starts_with(prefix))
            })
            .collect::<Vec<_>>()
    };
    let tokens = matching("POST /oauth/token ");
    let bearer = matching("GET /oauth/items ");
    let oauth1 = matching("GET /oauth1?");
    let passed = tokens.len() == 1
        && tokens[0]["token_verified"] == true
        && bearer.len() == 2
        && bearer
            .iter()
            .all(|record| record["bearer_verified"] == true && record["token_fetches"] == 1)
        && oauth1.len() == 2
        && oauth1
            .iter()
            .all(|record| record["oauth1_verified"] == true)
        && oauth1[0]["oauth_nonce"]
            .as_str()
            .is_some_and(|nonce| !nonce.is_empty())
        && oauth1[0]["oauth_nonce"] != oauth1[1]["oauth_nonce"];
    run.pass(
        "OAuth2 reuses its issued bearer and OAuth1 signatures verify",
        "POST /http/oauth2 twice; POST /http/oauth1 twice; GET /recorded",
        json!({"tokens":tokens,"bearer":bearer,"oauth1":oauth1}),
        passed,
    );
    ensure!(
        passed,
        "OAuth fixture evidence differs from the credential contract"
    );
    Ok(())
}

async fn assert_paged(
    run: &mut Run,
    database: &tokio_postgres::Client,
    environment: &super::integration::WorkflowEnvironment<'_>,
) -> anyhow::Result<()> {
    let work = run.work.clone();
    wamn_gate_harness::run_check::check(
        &work,
        "assert_paged",
        database,
        assert_paged_inner(run, database, environment),
    )
    .await
}

async fn assert_paged_inner(
    run: &mut Run,
    database: &tokio_postgres::Client,
    environment: &super::integration::WorkflowEnvironment<'_>,
) -> anyhow::Result<()> {
    let (status, body) =
        run_manual(&run.work, database, environment, "http-pages", "pages").await?;
    let items: Value = serde_json::from_str(&body)?;
    let pages: Vec<_> = request_records(&request_log(run).await?)?
        .into_iter()
        .filter(|record| record["page"].is_number())
        .collect();
    let attempts: Vec<_> = pages
        .iter()
        .map(|record| {
            json!([
                record["page"],
                record["page_request_count"],
                record["status"]
            ])
        })
        .collect();
    let passed = status == 200
        && items
            == json!([
                {"id":"one","name":"First"}, {"id":"two","name":"Second"},
                {"id":"three","name":"Third"}, {"id":"four","name":"Fourth"},
                {"id":"five","name":"Fifth"}
            ])
        && json!(attempts)
            == json!([
                [1, 1, "200 OK"],
                [2, 1, "429 Too Many Requests"],
                [1, 2, "200 OK"],
                [2, 2, "200 OK"],
                [3, 1, "200 OK"]
            ]);
    run.pass(
        "HTTP paging retries once and emits every fixture item exactly once",
        "POST /http/pages; GET /recorded",
        json!({"status": status, "items": items, "pages": pages}),
        passed,
    );
    ensure!(
        passed,
        "HTTP paging retry or emitted items differ from the fixture contract"
    );
    Ok(())
}

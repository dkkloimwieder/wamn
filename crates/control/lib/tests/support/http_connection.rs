//! The real HTTP connection guest and its in-cluster recording peer.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use chrono::Utc;
use serde_json::{Value, json};

pub use super::release_documents::http::{
    API_KEY_HANDLE, AWS_HANDLE, INBOUND_HANDLE, INBOUND_SECRET, ORIGIN, OUTBOUND_HANDLE,
    OUTBOUND_SECRET, SCHEDULE, SERVICE, admit_guest, assert_observed_request,
    authentication_wirings, declaration, paged_wiring, release, request_records, scheduled_wiring,
    signed_wiring, wiring,
};

use super::{NAMESPACE, ORG, PACKAGE, PASSWORD, PROJECT, Run, USER};
use wamn_catalog::ComponentDescriptor;

pub(super) const NETSUITE_PRIVATE_KEY: &str = include_str!("http-fixture/fixtures/p256.pem");

pub(super) fn netsuite_auth() -> Value {
    json!({
        "client_id":"fixture-client",
        "token_url":format!("{ORIGIN}/services/rest/auth/oauth2/v1/token"),
        "scopes":["rest_webservices","restlets"],
        "key_id":"certificate-id", "algorithm":"ES256", "scope_separator":",",
    })
}

pub async fn deploy(run: &mut Run, repository: &Path, system_url: &str) -> anyhow::Result<()> {
    let image = format!("{}-http-fixture:debug", run.cluster);
    prepare_image(run, repository).await?;
    let file = run.work.join("http-fixture.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&json!({
            "apiVersion": "v1", "kind": "List", "items": [
                {
                    "apiVersion": "v1", "kind": "Secret",
                    "metadata": {"name": "import-source", "namespace": NAMESPACE},
                    "stringData": {"upstream":json!({"username":"fixture","password":"fixture-password"}).to_string(),"rest-fixture":json!({"username":"fixture","password":"fixture-password"}).to_string(),"netsuite":json!({"private_key":NETSUITE_PRIVATE_KEY}).to_string(),OUTBOUND_HANDLE:json!({"key":OUTBOUND_SECRET}).to_string(),API_KEY_HANDLE:json!({"key":"fixture-api-key"}).to_string(),AWS_HANDLE:json!({"access_key_id":"fixture-access","secret_access_key":"fixture-private"}).to_string(),INBOUND_HANDLE:json!({"hmac":{"algorithm":"sha256","secret":INBOUND_SECRET}}).to_string()}

                },
                {
                    "apiVersion": "apps/v1", "kind": "Deployment",
                    "metadata": {"name": SERVICE, "namespace": NAMESPACE},
                    "spec": {"replicas": 1, "selector": {"matchLabels": {"app": SERVICE}},
                        "template": {"metadata": {"labels": {"app": SERVICE}},
                            "spec": {"containers": [{
                                "name": SERVICE, "image": image, "imagePullPolicy": "Never",
                                "ports": [{"containerPort": 8080}],
                                "env": [{"name":"WAMN_HTTP_FIXTURE_LIMIT","value":"2"}],
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
    let credentials = secret["stringData"]
        .as_object()
        .context("fixture credential values")?
        .clone();
    let _ = system_url;
    let mut references = serde_json::Map::new();
    let secret_name = wamn_credential_store::kubernetes::secret_name(ORG, PROJECT, "http");
    let mut data = serde_json::Map::new();
    for (name, value) in &credentials {
        let version = uuid::Uuid::new_v4().to_string();
        let value = value.as_str().context("credential value")?;
        data.insert(version.clone(), value.into());
        references.insert(
            name.clone(),
            json!({"store":"k8s","reference":secret_name,"version":version}),
        );
    }
    std::fs::write(
        run.work.join("http-credential-reference.json"),
        serde_json::to_vec(&references)?,
    )?;
    manifest["items"].as_array_mut().unwrap().push(json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":secret_name,"namespace":NAMESPACE,"labels":{"wamn.org":ORG,"wamn.project":PROJECT,"wamn.env":"http"}},"stringData":data}));
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

pub use super::release_documents::http::bindings_match;
/// Admit the actual debug guest bytes and upload the production artifact layout.
use super::release_documents::http::credential_secret;

pub async fn configure(run: &Run, system_url: &str, environment: &str) -> anyhow::Result<()> {
    let mut references: serde_json::Map<String, Value> = serde_json::from_slice(&std::fs::read(
        run.work.join("http-credential-reference.json"),
    )?)?;
    let values: Value =
        serde_json::from_slice(&std::fs::read(run.work.join("http-fixture.json"))?)?;
    let secret = credential_secret(environment, &values, &mut references)?;
    let file = run
        .work
        .join(format!("http-credentials-{environment}.json"));
    std::fs::write(&file, serde_json::to_vec(&secret)?)?;
    run.run("kubectl", &["apply", "-f", &file.to_string_lossy()])
        .await?;
    let (mut system, driver) = tokio_postgres::connect(system_url, tokio_postgres::NoTls).await?;
    let driver = tokio::spawn(driver);
    system.batch_execute("SET ROLE wamn_system; INSERT INTO registry.integration_type_auth VALUES ('http','custom') ON CONFLICT DO NOTHING").await?;
    let tx = system.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_control").await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    for (name, reference) in references {
        let stored = wamn_credential_store::StoreVersion {
            store: "k8s",
            reference: reference["reference"]
                .as_str()
                .context("fixture reference")?
                .into(),
            version: reference["version"]
                .as_str()
                .context("fixture version")?
                .into(),
        };
        let (auth_type, auth) = if name == "netsuite" {
            ("oauth2_client_credentials_jwt", netsuite_auth())
        } else if name == API_KEY_HANDLE {
            ("api_key", json!({"in":"query","name":"key"}))
        } else if name == AWS_HANDLE {
            (
                "aws_sigv4",
                json!({"region":"us-east-1","service":"execute-api"}),
            )
        } else if name == OUTBOUND_HANDLE {
            ("api_key", json!({"in":"header","name":"x-wamn-key"}))
        } else if name == INBOUND_HANDLE {
            ("custom", json!({}))
        } else {
            ("http_basic", json!({}))
        };
        let material = secret["stringData"]
            .get(&stored.version)
            .context("fixture credential reference")?
            .as_str()
            .context("fixture material")?;
        wamn_test_infrastructure::credentials::define(&tx,ORG,&json!({"project":PROJECT,"env":environment,"name":name,"integration_type":"http","auth_type":auth_type,"external_system":name,"parameters":{"primary-authority":ORIGIN,"tls-verification":"disabled","auth":auth},"credential":material}),&wamn_test_infrastructure::credentials::ReferencedCredentials(&stored)).await?;
    }
    tx.commit().await?;
    driver.abort();
    Ok(())
}

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
            data: std::fs::read(run.kept.secret_work.join("ca.crt"))?,
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
        .chain(authentication_wirings()?)
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
            && signed[0]["key_verified"] == true
            && signed[0]["body"] == json!(br#"{"id":1}"#.as_slice());
        run.pass(
            "HTTP guest sends once with a verified API key",
            "POST /http/signed-send; GET /recorded",
            json!({"status": status, "body": body, "requests": signed}),
            passed,
        );
        ensure!(
            passed,
            "the fixture did not verify one authenticated outbound request"
        );

        Ok(())
    })
    .await;
    run.collect("4 HTTP signed request", result);
    let result = assert_authentication(run, database, environment).await;
    run.collect("4 HTTP authentication", result);
    let result = assert_paged(run, database, environment).await;
    run.collect("4 HTTP paging", result);

    // The release declares no schedule. The scheduled wiring runs as an
    // environment workflow. Its nodes call no operation that needs a grant,
    // so its execution role holds no permission.
    let document = scheduled_wiring()?;
    super::integration::enable_schedule(environment, PACKAGE, &document, SCHEDULE, &[]).await?;
    let first_due = super::integration::next_minute(Utc::now());
    let result = scheduled_runs(&run.work, database, environment.env_key, first_due).await;
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
                 WHERE env_key=$1 AND run_id=$2",
                &[&environment.env_key, &id],
            )
            .await?;
        let status: String = row.get(0);
        if status == "completed" {
            return Ok((200, row.get::<_, Value>(1).to_string()));
        }
        ensure!(
            !matches!(status.as_str(), "failed" | "infrastructure-failure"),
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
    env_key: &str,
    first_due: chrono::DateTime<Utc>,
) -> anyhow::Result<Value> {
    wamn_gate_harness::run_check::check(work, "scheduled_runs", database, async {
        tokio::time::timeout(
            Duration::from_secs(240),
            scheduled_runs_inner(database, env_key, first_due),
        )
        .await
        .context("schedule did not complete two due moments within 240 seconds")?
    })
    .await
}

async fn scheduled_runs_inner(
    database: &tokio_postgres::Client,
    env_key: &str,
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
                WHERE env_key = $1 AND idempotency_key = ANY($2)",
            &[&env_key, &keys.as_slice()],
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
                        Some("failed" | "infrastructure-failure")
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

async fn assert_authentication(
    run: &mut Run,
    database: &tokio_postgres::Client,
    environment: &super::integration::WorkflowEnvironment<'_>,
) -> anyhow::Result<()> {
    let work = run.work.clone();
    wamn_gate_harness::run_check::check(
        &work,
        "assert_authentication",
        database,
        assert_authentication_inner(run, database, environment),
    )
    .await
}

async fn assert_authentication_inner(
    run: &mut Run,
    database: &tokio_postgres::Client,
    environment: &super::integration::WorkflowEnvironment<'_>,
) -> anyhow::Result<()> {
    for (route, field) in [("api-key", "key_verified"), ("aws", "aws_verified")] {
        let (status, body) = run_manual(
            &run.work,
            database,
            environment,
            &format!("http-{route}"),
            route,
        )
        .await?;
        ensure!(
            status == 200 && serde_json::from_str::<Value>(&body)?["status"] == 200,
            "HTTP authentication refused: {route}"
        );
        let records = request_records(&request_log(run).await?)?;
        ensure!(
            records.iter().any(|record| record[field] == true),
            "HTTP authentication was not verified: {route}"
        );
    }
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
        && items["items"]
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

pub async fn prepare_image(run: &mut Run, repository: &Path) -> anyhow::Result<()> {
    let image = format!("{}-http-fixture:debug", run.cluster);
    let source = repository.join("crates/control/lib/tests/support/http-fixture");
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    for file in [
        "Dockerfile",
        "Cargo.toml",
        "Cargo.lock",
        "server.rs",
        "netsuite.rs",
        "oauth.rs",
        "fixtures/p256.pem",
        "fixtures/p256-public.sec1",
    ] {
        hash.update(&std::fs::read(source.join(file))?);
    }
    let digest = hex::encode(hash.finish().as_ref());
    if !run.reuse || run.kept.fixture_source != digest {
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
        run.kept.fixture_source = digest;
    }
    run.load_image(&image).await
}

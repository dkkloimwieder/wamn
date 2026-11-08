//! Application deployment uses product commands; Kubernetes and SQL are observers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_environment_control::environment::document::EnvironmentDocument;
use wamn_environment_control::platform::resources;

use super::{POLL_LIMIT, ProcessGroup, Run, execute, get, kubectl, success};

struct Journey<'a> {
    run: &'a Run,
    sources: &'a [PathBuf; 2],
    file: PathBuf,
    variables: BTreeMap<String, String>,
    token: String,
    client: reqwest::Client,
    edge: String,
}

impl Journey<'_> {
    fn command(&self, ctl: bool, args: &[&str]) -> Command {
        let mut command = if ctl {
            Command::new(self.run.binary.with_file_name("wamn-ctl"))
        } else {
            Command::new(&self.run.binary)
        };
        command
            .current_dir(&self.run.directory)
            .env_remove("WAMN_RELEASE_HOST_GROUP")
            .env_remove("WAMN_DATABASE_HOST")
            .env_remove("WAMN_DATABASE_PORT")
            .env_remove("WAMN_IDENTITY_URL")
            .env_remove("WAMN_NATS_TLS_SERVER_NAME")
            .env("WAMN_DATABASE_NAMESPACE", "platform")
            .env("WAMN_RELEASE_NAMESPACE", "hosts")
            .envs(&self.variables)
            .env("KUBECONFIG", &self.run.target.kubeconfig)
            .env("WAMN_KCL", &self.run.kcl)
            .env("WAMN_RELEASE_CHART", &self.run.chart)
            .args(args);
        command
    }

    async fn run(
        &self,
        label: &str,
        command: &mut Command,
    ) -> anyhow::Result<std::process::Output> {
        let started = Instant::now();
        let output = execute(command).await?;
        std::fs::write(
            self.run.directory.join(format!("{label}.stdout")),
            &output.stdout,
        )?;
        std::fs::write(
            self.run.directory.join(format!("{label}.stderr")),
            &output.stderr,
        )?;
        eprintln!("{label}: {:.2}s", started.elapsed().as_secs_f64());
        Ok(output)
    }

    async fn apply(&self, label: &str, command: &mut Command) -> anyhow::Result<()> {
        let output = self.run(label, command).await?;
        ensure!(
            output.status.success(),
            "{label} failed; see {}",
            self.run.directory.display()
        );
        Ok(())
    }

    fn deploy(&self, digest: &str) -> Command {
        let mut command = self.command(false, &["deploy", "--release", digest, "--env"]);
        command.arg(&self.file);
        command
    }

    fn reapply(&self) -> Command {
        let mut command = self.command(true, &["env", "apply"]);
        command.arg(&self.file).args(["--application", "alpha"]);
        command
    }

    async fn status(&self) -> anyhow::Result<Value> {
        let output = success(self.command(false, &["status", "--env"]).arg(&self.file)).await?;
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    async fn route(&self, app: &str, route: &str, body: &Value) -> anyhow::Result<Value> {
        let endpoint = format!(
            "{}/api/{app}/{route}",
            self.edge
                .replace("alpha.wamn.localhost", &format!("{app}.wamn.localhost"))
        );
        let request = if route.ends_with("/create") {
            self.client.post(endpoint).json(body)
        } else {
            let query = wamn_execution_contract::encode_read_query(
                body[0].as_object().context("read request")?,
            );
            self.client.get(format!("{endpoint}?{query}"))
        };
        let response = request.bearer_auth(&self.token).send().await?;
        ensure!(
            response.status().is_success(),
            "{app}/{route} returned {}",
            response.status()
        );
        let mut response: Value = response.json().await?;
        ensure!(
            response[0].get("value").is_some(),
            "{app}/{route} refused: {response}"
        );
        Ok(response[0]["value"].take())
    }

    async fn publish(&self, app: &str, version: &str, root: &Path) -> anyhow::Result<String> {
        self.apply(
            &format!("build-{app}-{version}"),
            self.command(false, &["build"]).arg(root),
        )
        .await?;
        self.apply(
            &format!("push-{app}-{version}"),
            self.command(true, &["push-package", "--package"])
                .arg(root)
                .args([
                    "--artifact-base",
                    &self.variables["WAMN_PACKAGE_ARTIFACT_BASE"],
                    "--component-artifact-base",
                    &self.variables["WAMN_COMPONENT_ARTIFACT_BASE"],
                    "--insecure-registry",
                ]),
        )
        .await?;
        let output = self
            .run(
                &format!("publish-{app}-{version}"),
                self.command(true, &["publish-release"])
                    .arg(&self.file)
                    .args([
                        "--application",
                        app,
                        "--package",
                        &format!("{app}@{version}"),
                        "--artifact-base",
                        &self.variables["WAMN_PACKAGE_ARTIFACT_BASE"],
                        "--insecure-registry",
                    ]),
            )
            .await?;
        ensure!(output.status.success(), "publish {app}@{version} failed");
        let digest = String::from_utf8(output.stdout)?.trim().to_owned();
        wamn_package_artifact::ensure_digest(&digest)?;
        Ok(digest)
    }
}

pub(super) async fn exercise(run: &Run, password: &str) -> anyhow::Result<()> {
    let sources = sources(&run.repository, &run.checkout).await?;
    let postgres =
        resources::port_forward(&run.target, "platform", "service/wamn-pg-rw", 5432).await?;
    let nats = resources::port_forward(&run.target, "platform", "service/wamn-nats", 4222).await?;
    let identity =
        resources::port_forward(&run.target, "identity", "service/identity", 443).await?;
    let control = resources::port_forward(
        &run.target,
        "hosts",
        &format!("service/c-{}-http", run.document.org),
        80,
    )
    .await?;
    let edge = resources::port_forward(&run.target, "hosts", "service/wamn-edge", 443).await?;
    let identity_tls = get(run, "identity", "secret", "identity-tls").await?;
    let operator = get(
        run,
        "identity",
        "secret",
        &format!("operator-{}", run.document.org),
    )
    .await?;
    let mut pem = secret(&operator, "tls.crt")?;
    pem.extend(secret(&operator, "tls.key")?);
    let identity_host = "identity.identity.svc.cluster.local";
    let identity_client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .add_root_certificate(reqwest::Certificate::from_pem(&secret(
            &identity_tls,
            "ca.crt",
        )?)?)
        .identity(reqwest::Identity::from_pem(&pem)?)
        .resolve(identity_host, ([127, 0, 0, 1], identity.port).into())
        .build()?;
    let endpoint = format!("https://{identity_host}:{}", identity.port);
    let login: Value = identity_client.post(format!("{endpoint}/password/session"))
        .json(&json!({"email":run.document.owner_email,"password":password,"aud":format!("urn:wamn:control:{}",run.document.org)}))
        .send().await?.error_for_status()?.json().await?;
    let access = login["access_token"].as_str().context("owner session")?;
    let claims: Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(access.split('.').nth(1).context("session claims")?)?,
    )?;
    let principal = claims["sub"]
        .as_str()
        .context("owner principal")?
        .to_owned();
    let issued: Value = identity_client.post(format!("{endpoint}/pats"))
        .json(&json!({"principal_id":principal,"label":"deployment journey","lifetime_seconds":14400}))
        .send().await?.error_for_status()?.json().await?;
    let token = issued["token"].as_str().context("owner PAT")?.to_owned();
    let created: Value = identity_client
        .post(format!(
            "http://127.0.0.1:{}/wamn_control/project/create",
            control.port
        ))
        .header(reqwest::header::HOST, &run.document.route_hosts.control)
        .bearer_auth(access)
        .json(
            &json!([{"request_id":uuid::Uuid::new_v4().to_string(),"value":{"project":"journey"}}]),
        )
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    ensure!(
        created[0].get("value").is_some(),
        "project creation refused: {created}"
    );

    let apply = get(run, "platform", "secret", "wamn-apply").await?;
    let admin = get(run, "platform", "secret", "wamn-pg-superuser").await?;
    let apply_url = forwarded(&String::from_utf8(secret(&apply, "url")?)?, postgres.port)?;
    let mut admin_url = url::Url::parse(&format!(
        "postgresql://postgres@127.0.0.1:{}/wamn_system",
        postgres.port
    ))?;
    admin_url
        .set_password(Some(&String::from_utf8(secret(&admin, "password")?)?))
        .map_err(|()| anyhow::anyhow!("administrator password"))?;
    let mut variables = BTreeMap::from([
        ("WAMN_APPLY_DATABASE_URL".into(), apply_url.clone()),
        ("WAMN_CONTROL_DATABASE_URL".into(), apply_url),
        ("WAMN_SYSTEM_ADMIN_URL".into(), admin_url.to_string()),
        (
            "WAMN_NATS_URL".into(),
            format!("tls://127.0.0.1:{}", nats.port),
        ),
    ]);
    for name in [
        "WAMN_PACKAGE_ARTIFACT_BASE",
        "WAMN_COMPONENT_ARTIFACT_BASE",
        "WAMN_RELEASE_ARTIFACT_BASE",
        "WAMN_REGISTRY_AUTH_FILE",
        "DATABASE_URL",
        "WAMN_FLOW_HTTP_COMPONENT",
    ] {
        variables.insert(
            name.into(),
            std::env::var(name).with_context(|| format!("set {name}"))?,
        );
    }
    let certificate = get(run, "platform", "secret", "wamn-ctl-nats").await?;
    for (name, key) in [
        ("WAMN_NATS_TLS_CA", "ca.crt"),
        ("WAMN_NATS_TLS_CERT", "tls.crt"),
        ("WAMN_NATS_TLS_KEY", "tls.key"),
    ] {
        let path = run.directory.join(key);
        std::fs::write(&path, secret(&certificate, key)?)?;
        variables.insert(name.into(), path.to_string_lossy().into_owned());
    }
    let file = run.directory.join("environments/environment.k");
    let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":run.document.org,"project":"journey","env":"dev","target":"cloud","policy":"dev",
        "integrations":{"probe":{"version":1}},
        "applications":{
            "alpha":{"current":"none","hostname":"alpha.wamn.localhost","packages":["alpha"],"connections":{"upstream":"probe"}},
            "beta":{"current":"none","hostname":"beta.wamn.localhost","packages":["beta"]}
        }
    }))?)?;
    std::fs::write(&file, document.to_kcl())?;
    let edge_tls = get(run, "hosts", "secret", "wamn-edge-tls").await?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .add_root_certificate(reqwest::Certificate::from_pem(&secret(
            &edge_tls, "ca.crt",
        )?)?)
        .resolve("alpha.wamn.localhost", ([127, 0, 0, 1], edge.port).into())
        .resolve("beta.wamn.localhost", ([127, 0, 0, 1], edge.port).into())
        .build()?;
    let journey = Journey {
        run,
        sources: &sources,
        file,
        variables,
        token,
        client,
        edge: format!("https://alpha.wamn.localhost:{}", edge.port),
    };
    // Publication precedes destination creation. Every artifact remains in the registry.
    let a0 = journey.publish("alpha", "1.0.0", &sources[0]).await?;
    let b0 = journey.publish("beta", "1.0.0", &sources[1]).await?;
    version(&sources[0], "1.0.0", "1.0.1")?;
    version(&sources[1], "1.0.0", "1.0.1")?;
    let a1 = journey.publish("alpha", "1.0.1", &sources[0]).await?;
    let b1 = journey.publish("beta", "1.0.1", &sources[1]).await?;
    super::database_state(run).await?;
    let output = first_install(&journey, &b0).await?;
    ensure!(output.status.success(), "initial beta deployment failed");
    ensure!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|notice| notice["observed"]["kind"] == "not-checked"),
        "unqualified deployment omitted its notice"
    );
    let login: Value = identity_client.post(format!("{endpoint}/password/session"))
        .json(&json!({"email":run.document.owner_email,"password":password,"aud":format!("urn:wamn:control:{}",run.document.org)}))
        .send().await?.error_for_status()?.json().await?;
    let access = login["access_token"]
        .as_str()
        .context("refreshed owner session")?;
    // B creates the destination. Control routes then prepare A's external connection.
    for (route, value) in [
        (
            "integration/write",
            json!({"project":"journey","name":"probe","integration_type":"http","auth_type":"none","external_system":null}),
        ),
        (
            "connection/write",
            json!({"project":"journey","env":"dev","name":"probe","parameters":{"primary-authority":format!("http://c-{}-http.hosts.svc.cluster.local",run.document.org),"tls-verification":"disabled"}}),
        ),
        (
            "connection/enable",
            json!({"project":"journey","env":"dev","name":"probe"}),
        ),
    ] {
        let answer: Value = journey
            .client
            .post(format!(
                "http://127.0.0.1:{}/wamn_control/{route}",
                control.port
            ))
            .header(reqwest::header::HOST, &run.document.route_hosts.control)
            .bearer_auth(access)
            .json(&json!([{"request_id":uuid::Uuid::new_v4().to_string(),"value":value}]))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure!(
            answer[0].get("value").is_some(),
            "{route} refused: {answer}"
        );
    }
    journey
        .apply("install-alpha", &mut journey.deploy(&a0))
        .await?;
    ensure!(
        EnvironmentDocument::compile(&journey.file)?
            .applications
            .values()
            .all(|selection| selection.previous.is_none()),
        "first installation retained a previous selection"
    );
    let mut rows = BTreeMap::new();
    for app in ["alpha", "beta"] {
        let counter = journey
            .route(
                app,
                "counter/create",
                &json!([{"request_id":uuid::Uuid::new_v4().to_string(),"idempotency_key":uuid::Uuid::new_v4().to_string(),"label":app}]),
            )
            .await?;
        let widget = journey.route(app,"widget/create",&json!([{"request_id":uuid::Uuid::new_v4().to_string(),"idempotency_key":uuid::Uuid::new_v4().to_string(),"id":uuid::Uuid::new_v4().to_string(),"code":"standard","maker_id":null,"note":app}])).await?;
        rows.insert(app, (counter, widget));
    }
    let database = observer(&journey).await?;
    let baseline = stored(&journey, &database).await?;
    state(
        &journey,
        &database,
        &baseline,
        &rows,
        [(&a0, "v0"), (&b0, "v0")],
    )
    .await?;
    let before = runtime(&journey, "beta", &database).await?;
    journey
        .apply(
            "source-alpha",
            journey
                .command(false, &["deploy"])
                .arg(&sources[0])
                .arg("--env")
                .arg(&journey.file),
        )
        .await?;
    ensure!(
        runtime(&journey, "beta", &database).await? == before,
        "alpha update changed beta"
    );
    state(
        &journey,
        &database,
        &baseline,
        &rows,
        [(&a1, "v1"), (&b0, "v0")],
    )
    .await?;
    ensure!(
        EnvironmentDocument::compile(&journey.file)?.applications["alpha"]
            .previous
            .as_deref()
            == Some(a0.as_str()),
        "source deploy lost A0"
    );
    let before = runtime(&journey, "alpha", &database).await?;
    journey
        .apply("update-beta", &mut journey.deploy(&b1))
        .await?;
    ensure!(
        runtime(&journey, "alpha", &database).await? == before,
        "beta update changed alpha"
    );
    state(
        &journey,
        &database,
        &baseline,
        &rows,
        [(&a1, "v1"), (&b1, "v1")],
    )
    .await?;
    let before = runtime(&journey, "beta", &database).await?;
    let selections = EnvironmentDocument::compile(&journey.file)?;
    ensure!(
        selections.applications["beta"].previous.as_deref() == Some(b0.as_str()),
        "beta update lost B0"
    );
    journey
        .apply("reapply-alpha", &mut journey.reapply())
        .await?;
    ensure!(
        EnvironmentDocument::compile(&journey.file)? == selections,
        "reapply changed selections"
    );
    ensure!(
        runtime(&journey, "beta", &database).await? == before,
        "reapply changed beta"
    );
    state(
        &journey,
        &database,
        &baseline,
        &rows,
        [(&a1, "v1"), (&b1, "v1")],
    )
    .await?;
    journey
        .apply(
            "rollback-alpha",
            journey
                .command(false, &["rollback", "alpha", "--env"])
                .arg(&journey.file),
        )
        .await?;
    ensure!(
        runtime(&journey, "beta", &database).await? == before,
        "rollback changed beta"
    );
    state(
        &journey,
        &database,
        &baseline,
        &rows,
        [(&a0, "v0"), (&b1, "v1")],
    )
    .await?;
    let selections = EnvironmentDocument::compile(&journey.file)?;
    let alpha = runtime(&journey, "alpha", &database).await?;
    let absent = format!(
        "sha256:{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    ensure!(
        !journey
            .run("missing-artifact", &mut journey.deploy(&absent))
            .await?
            .status
            .success(),
        "missing artifact was accepted"
    );
    ensure!(
        EnvironmentDocument::compile(&journey.file)? == selections,
        "missing artifact changed selections"
    );
    ensure!(
        runtime(&journey, "alpha", &database).await? == alpha,
        "missing artifact changed alpha"
    );
    ensure!(
        runtime(&journey, "beta", &database).await? == before,
        "missing artifact changed beta"
    );
    recovery(&journey, &a1, &a0, &database, &baseline).await?;
    ensure!(
        runtime(&journey, "beta", &database).await? == before,
        "recovery changed beta"
    );
    state(
        &journey,
        &database,
        &baseline,
        &rows,
        [(&a0, "v0"), (&b1, "v1")],
    )
    .await?;
    Ok(())
}

async fn first_install(
    journey: &Journey<'_>,
    digest: &str,
) -> anyhow::Result<std::process::Output> {
    let system =
        wamn_package_control::database::connect(&journey.variables["WAMN_SYSTEM_ADMIN_URL"])
            .await?;
    system
        .batch_execute("SET default_transaction_read_only = on")
        .await?;
    let environment_sql = "SELECT jsonb_build_object('instance_suffix',instance_suffix,'created_at',created_at) FROM registry.project_envs WHERE org=$1 AND project='journey' AND env='dev'";
    let org = &journey.run.document.org;
    ensure!(
        system.query_opt(environment_sql, &[org]).await?.is_none(),
        "interrupted first install requires an absent environment"
    );
    let name = wamn_control_provision::environment_state::application_release_name(
        org, "journey", "dev", "beta",
    )?;
    let shared = wamn_control_provision::environment_state::environment_resource_name(
        org, "journey", "dev",
    )?;
    let mut environment = Value::Null;
    for after_sql in [false, true] {
        let label = if after_sql {
            "interrupt-beta-after-sql"
        } else {
            "interrupt-beta-provisioning"
        };
        let started = Instant::now();
        let mut command = journey.deploy(digest);
        command
            .process_group(0)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(
                journey.run.directory.join(format!("{label}.stdout")),
            )?)
            .stderr(std::fs::File::create(
                journey.run.directory.join(format!("{label}.stderr")),
            )?);
        let mut child = command.spawn()?;
        let group = ProcessGroup(
            rustix::process::Pid::from_raw(i32::try_from(child.id().context("deploy PID")?)?)
                .context("positive PID")?,
        );
        let observed = tokio::time::timeout(super::COMMAND_LIMIT, async {
            loop {
                ensure!(
                    child.try_wait()?.is_none(),
                    "{label}: deploy exited before the interruption point"
                );
                if after_sql {
                    // Helm creates this record only after the SQL Job returned successfully.
                    let releases = get(
                        journey.run,
                        "hosts",
                        "secrets",
                        &format!("-l=owner=helm,name={name},status=pending-install"),
                    )
                    .await?;
                    if let Some(release) = releases["items"]
                        .as_array()
                        .context("Helm release records")?
                        .first()
                    {
                        break Ok::<_, anyhow::Error>(release["metadata"].clone());
                    }
                } else if let Some(row) = system.query_opt(environment_sql, &[org]).await? {
                    break Ok(row.get::<_, Value>(0));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        // Reap the owned process even when the observation times out or fails.
        let killed = rustix::process::kill_process_group(group.0, rustix::process::Signal::KILL);
        let exited = child.wait().await;
        drop(group);
        let observed = observed.context("first-install interruption point was not observed")??;
        killed?;
        ensure!(!exited?.success(), "interrupted deploy returned success");
        eprintln!("{label}: {:.2}s", started.elapsed().as_secs_f64());
        std::fs::write(
            journey.run.directory.join(format!("{label}.json")),
            serde_json::to_vec_pretty(&observed)?,
        )?;
        let jobs = get(journey.run, "hosts", "jobs", "").await?;
        ensure!(
            jobs["items"]
                .as_array()
                .context("SQL Jobs")?
                .iter()
                .all(|job| job["metadata"]["name"] != format!("{name}-initialize")),
            "{label}: SQL initialization Job still exists"
        );
        if after_sql {
            let pods = get(
                journey.run,
                "hosts",
                "pods",
                &format!("-l=wasmcloud.com/hostgroup={name}"),
            )
            .await?;
            ensure!(
                pods["items"]
                    .as_array()
                    .context("beta pods")?
                    .iter()
                    .all(|pod| !pod["status"]["conditions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|condition| condition["type"] == "Ready"
                            && condition["status"] == "True")),
                "missed the interruption window: beta was already ready"
            );
            ensure!(
                helm_history(journey.run, &name).await?["status"] == "pending-install",
                "missed the interruption window: Helm was no longer pending"
            );
        } else {
            environment = observed;
            let workloads = get(journey.run, "hosts", "workloaddeployments", "").await?;
            ensure!(
                workloads["items"]
                    .as_array()
                    .context("shared workloads")?
                    .iter()
                    .all(
                        |workload| workload["metadata"]["name"] != format!("{shared}-materializer")
                    ),
                "missed the interruption window: provisioning already reached the materializer"
            );
        }
    }
    let database = observer(journey).await?;
    let baseline = stored(journey, &database).await?;
    let packages = baseline["catalog.packages"]
        .as_array()
        .context("installed package baselines")?;
    ensure!(
        packages
            .iter()
            .filter(|package| package["package_id"] == "beta")
            .count()
            == 1
            && packages
                .iter()
                .any(|package| package["package_id"] == "beta"
                    && package["package_version"] == "1.0.0"),
        "B0 must have exactly one committed package baseline before retry"
    );
    let records = initialization_records(&database).await?;
    let output = journey
        .run("install-beta", &mut journey.deploy(digest))
        .await?;
    ensure!(
        output.status.success(),
        "interrupted first install did not recover"
    );
    ensure!(
        system
            .query_one(environment_sql, &[org])
            .await?
            .get::<_, Value>(0)
            == environment,
        "first-install retry replaced the environment instance"
    );
    ensure!(
        stored(journey, &database).await? == baseline
            && initialization_records(&database).await? == records,
        "first-install retry replayed SQL initialization"
    );
    let status = journey.status().await?;
    let beta = &status["applications"]["beta"];
    let live = beta["observed"]["live"]
        .as_object()
        .context("beta live releases")?;
    ensure!(
        beta["current"] == digest
            && live.len() == 1
            && live.contains_key(digest)
            && helm_history(journey.run, &name).await?["status"] == "deployed",
        "first-install retry did not serve B0"
    );
    journey
        .route(
            "beta",
            "widget/list",
            &json!([{"selector":{},"maker_id":null}]),
        )
        .await?;
    Ok(output)
}

async fn initialization_records(database: &tokio_postgres::Client) -> anyhow::Result<Value> {
    let mut records = serde_json::Map::new();
    for table in [
        "platform.schema_baseline",
        "catalog.packages",
        "catalog.package_migrations",
    ] {
        // xmin identifies the inserting/updating transaction, even if bytes are unchanged.
        records.insert(table.into(), query(database, &format!("SELECT coalesce(jsonb_agg(jsonb_build_array(to_jsonb(t),t.xmin::text) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM {table} t")).await?);
    }
    Ok(Value::Object(records))
}

fn secret(object: &Value, key: &str) -> anyhow::Result<Vec<u8>> {
    Ok(base64::engine::general_purpose::STANDARD.decode(
        object["data"][key]
            .as_str()
            .with_context(|| format!("Secret lacks {key}"))?,
    )?)
}

fn forwarded(value: &str, port: u16) -> anyhow::Result<String> {
    let mut url = url::Url::parse(value)?;
    url.set_host(Some("127.0.0.1"))?;
    url.set_port(Some(port))
        .map_err(|()| anyhow::anyhow!("database port"))?;
    Ok(url.to_string())
}

async fn sources(repository: &Path, checkout: &Path) -> anyhow::Result<[PathBuf; 2]> {
    let head = success(
        Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(["rev-parse", "HEAD"]),
    )
    .await?;
    let head = String::from_utf8(head.stdout)?;
    success(
        Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(["worktree", "add", "--detach"])
            .arg(checkout)
            .arg(head.trim()),
    )
    .await?;
    success(Command::new("git").arg("-C").arg(checkout).args([
        "submodule",
        "update",
        "--init",
        "--recursive",
    ]))
    .await?;
    let apps = checkout.join("apps");
    let roots = [apps.join("alpha"), apps.join("beta")];
    for (app, root) in ["alpha", "beta"].into_iter().zip(&roots) {
        copy_source(&apps.join("platform_fixture"), root, app)?;
        let manifest = root.join("wamn.k");
        let text = std::fs::read_to_string(&manifest)?
            .replace("version = \"2.1.1\"", "version = \"1.0.0\"")
            .replace(
                "routes = {",
                &format!("routes = {{path_prefix = \"/{app}\", "),
            );
        std::fs::write(manifest, text)?;
        let file = root.join("publication/attachments.json");
        let mut attachments: BTreeMap<String, Value> =
            serde_json::from_slice(&std::fs::read(&file)?)?;
        for attachment in attachments.values_mut() {
            let path = attachment["definition"]["route"]["path"]
                .as_str()
                .context("fixture route path")?;
            attachment["definition"]["route"]["path"] = format!("/{app}{path}").into();
            attachment["definition-hash"] =
                wamn_execution_contract::canonical_json_sha256(&attachment["definition"]).into();
        }
        std::fs::write(file, serde_json::to_vec_pretty(&attachments)?)?;
        let query = root.join("query/widget_list.sql");
        std::fs::write(
            &query,
            std::fs::read_to_string(&query)?.replace(
                "to_jsonb(widget)",
                "to_jsonb(widget) || '{\"behavior\":\"v0\"}'::jsonb",
            ),
        )?;
        if app == "alpha" {
            require_connection(root)?;
        }
    }
    let manifest = apps.join("Cargo.toml");
    let workspace = std::fs::read_to_string(&manifest)?.replace(
        "\"platform_fixture/component\",",
        "\"platform_fixture/component\",\n    \"alpha/component\",\n    \"beta/component\",",
    );
    std::fs::write(&manifest, workspace)?;
    // Register the new local crates without updating locked registry dependencies.
    success(
        Command::new("cargo")
            .current_dir(checkout)
            .args(["update", "--offline", "--workspace", "--manifest-path"])
            .arg(&manifest),
    )
    .await?;
    Ok(roots)
}

fn require_connection(root: &Path) -> anyhow::Result<()> {
    let file = root.join("wamn.k");
    std::fs::write(
        &file,
        std::fs::read_to_string(&file)?.replace(
            "connections = {}",
            "connections = {upstream = {interface = \"wamn:connection/http@0.1.0\", mode = \"read\"}}",
        ),
    )?;
    // Like rest_connector, declare HTTP authority on the platform palette component.
    // The package-owned component keeps only its PostgreSQL connection.
    let directory = root.join("publication/wirings");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("probe.json"),
        serde_json::to_vec_pretty(&json!({
            "format-version": "0.1",
            "wiring-id": "probe",
            "version": 1,
            "entry": "request",
            "nodes": {
                "request": {
                    "component": "http-request",
                    "interface-version": "0.1.0",
                    "operation": "wamn:node/async-handler@0.1.0",
                    "params": {
                        "requirement": "upstream",
                        "method": "GET",
                        "path-and-query": "/"
                    }
                }
            },
            "edges": []
        }))?,
    )?;
    Ok(())
}

fn copy_source(source: &Path, target: &Path, app: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .context("fixture source path is not UTF-8")?
            .replace("platform_fixture", app)
            .replace("platform-fixture", app)
            .replace("fixture", app);
        let destination = target.join(name);
        if entry.file_type()?.is_dir() {
            copy_source(&entry.path(), &destination, app)?;
        } else {
            let text = std::fs::read_to_string(entry.path())?;
            std::fs::write(
                destination,
                text.replace("platform_fixture", app)
                    .replace("platform-fixture", app)
                    .replace("inventory", &format!("{app}_data"))
                    .replace("\"fixture\"", &format!("\"{app}\""))
                    .replace(
                        "components = {fixture =",
                        &format!("components = {{{app} ="),
                    ),
            )?;
        }
    }
    Ok(())
}

fn version(root: &Path, previous: &str, next: &str) -> anyhow::Result<()> {
    let file = root.join("wamn.k");
    std::fs::write(
        &file,
        std::fs::read_to_string(&file)?.replace(
            &format!("version = \"{previous}\""),
            &format!("version = \"{next}\""),
        ),
    )?;
    let query = root.join("query/widget_list.sql");
    std::fs::write(&query, std::fs::read_to_string(&query)?.replace("v0", "v1"))?;
    Ok(())
}

async fn observer(journey: &Journey<'_>) -> anyhow::Result<tokio_postgres::Client> {
    let system =
        wamn_package_control::database::connect(&journey.variables["WAMN_APPLY_DATABASE_URL"])
            .await?;
    system
        .batch_execute("SET default_transaction_read_only = on")
        .await?;
    let row = system.query_one("SELECT instance_suffix FROM registry.project_envs WHERE org=$1 AND project='journey' AND env='dev'", &[&journey.run.document.org]).await?;
    let suffix: String = row.get(0);
    let name = wamn_control_provision::project_env_database_name(
        &journey.run.document.org,
        "journey",
        "dev",
        &suffix,
    );
    let mut url = url::Url::parse(&journey.variables["WAMN_APPLY_DATABASE_URL"])?;
    url.set_path(&name);
    let client = wamn_package_control::database::connect(url.as_str()).await?;
    let owner: String = client
        .query_one(
            "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname=current_database()",
            &[],
        )
        .await?
        .get(0);
    client
        .batch_execute(&format!("SET ROLE {}", wamn_pg_core::quote_ident(&owner)))
        .await?;
    client
        .batch_execute("SET default_transaction_read_only = on")
        .await?;
    Ok(client)
}

async fn query(client: &tokio_postgres::Client, sql: &str) -> anyhow::Result<Value> {
    Ok(client
        .query_one(sql, &[])
        .await
        .with_context(|| format!("observe {sql}"))?
        .get(0))
}

async fn stored(journey: &Journey<'_>, client: &tokio_postgres::Client) -> anyhow::Result<Value> {
    let mut state = serde_json::Map::new();
    state.insert(
        "cluster".into(),
        get(
            journey.run,
            "platform",
            "cluster.postgresql.cnpg.io",
            "wamn-pg",
        )
        .await?["metadata"]["uid"]
            .clone(),
    );
    state.insert("database".into(),query(client,"SELECT jsonb_build_array(oid,datname) FROM pg_database WHERE datname=current_database()").await?);
    // The database owns this roster. Include every installed package and SQL record.
    for table in [
        "catalog.packages",
        "catalog.package_migrations",
        "catalog.package_definition_owners",
        "platform.schema_baseline",
        "platform.number_sequence",
        "platform.number_counter",
    ] {
        state.insert(table.into(),query(client,&format!("SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM {table} t")).await?);
    }
    let mut schemas: std::collections::BTreeSet<String> = client.query("SELECT DISTINCT schema_name FROM catalog.package_definition_owners ORDER BY schema_name",&[]).await?.iter().map(|row|row.get(0)).collect();
    ensure!(
        !schemas.is_empty(),
        "no installed application SQL ownership"
    );
    for root in journey.sources {
        let artifact = wamn_schema_generator::package_manifest_path(root);
        let manifest =
            wamn_catalog::manifest::PackageManifest::from_slice(&std::fs::read(artifact)?)?;
        schemas.extend(manifest.models.into_values().map(|model| model.schema));
    }
    for schema in schemas {
        let functions: Value = client.query_one("SELECT coalesce(jsonb_agg(pg_get_functiondef(p.oid) ORDER BY p.oid),'[]'::jsonb) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname=$1 AND p.prokind IN ('f','p')", &[&schema]).await?.get(0);
        state.insert(format!("{schema}.functions"), functions);
        let relations = client.query("SELECT c.oid::bigint,c.relname,c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relkind IN ('r','p','S','v','m') ORDER BY c.relname",&[&schema]).await?;
        for row in relations {
            let oid: i64 = row.get(0);
            let name: String = row.get(1);
            let kind: String = row.get(2);
            let relation = format!(
                "\"{}\".\"{}\"",
                schema.replace('"', "\"\""),
                name.replace('"', "\"\"")
            );
            let definitions = query(client,&format!(r"SELECT jsonb_build_object(
                'relation',(SELECT jsonb_build_array(relkind,relpersistence,relrowsecurity,relforcerowsecurity,relreplident,reloptions) FROM pg_class WHERE oid={oid}),
                'fields',(SELECT jsonb_agg(jsonb_build_array(a.attname,format_type(a.atttypid,a.atttypmod),a.attnotnull,a.attidentity,a.attgenerated,pg_get_expr(d.adbin,d.adrelid)) ORDER BY a.attnum) FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE a.attrelid={oid} AND a.attnum>0 AND NOT a.attisdropped),
                'constraints',(SELECT jsonb_agg(jsonb_build_array(conname,pg_get_constraintdef(oid)) ORDER BY conname) FROM pg_constraint WHERE conrelid={oid}),
                'indexes',(SELECT jsonb_agg(pg_get_indexdef(indexrelid) ORDER BY indexrelid) FROM pg_index WHERE indrelid={oid}),
                'triggers',(SELECT jsonb_agg(pg_get_triggerdef(oid) ORDER BY tgname) FROM pg_trigger WHERE tgrelid={oid} AND NOT tgisinternal),
                'policies',(SELECT jsonb_agg(jsonb_build_array(polname,polcmd,polroles,polpermissive,pg_get_expr(polqual,polrelid),pg_get_expr(polwithcheck,polrelid)) ORDER BY polname) FROM pg_policy WHERE polrelid={oid}),
                'sequence',(SELECT to_jsonb(s) FROM pg_sequence s WHERE seqrelid={oid}))")).await?;
            state.insert(format!("{relation}.definition"), definitions);
            let data = if kind == "S" {
                query(
                    client,
                    &format!("SELECT jsonb_build_array(last_value,is_called) FROM {relation}"),
                )
                .await?
            } else {
                query(client,&format!("SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM {relation} t")).await?
            };
            state.insert(format!("{relation}.data"), data);
        }
    }
    Ok(Value::Object(state))
}

async fn runtime(
    journey: &Journey<'_>,
    app: &str,
    database: &tokio_postgres::Client,
) -> anyhow::Result<Value> {
    let status = journey.status().await?;
    let selected = &status["applications"][app];
    let observed = &selected["observed"];
    let name = observed["release_name"]
        .as_str()
        .context("observed application release")?;
    let pods = get(
        journey.run,
        "hosts",
        "pods",
        &format!("-l=wasmcloud.com/hostgroup={name}"),
    )
    .await?;
    let mut uids = pods["items"]
        .as_array()
        .context("application pods")?
        .iter()
        .map(|pod| {
            pod["metadata"]["uid"]
                .as_str()
                .context("pod UID")
                .map(str::to_owned)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    uids.sort();
    ensure!(!uids.is_empty(), "{app} has no pods");
    let deployment = get(
        journey.run,
        "hosts",
        "deployment",
        &format!("hostgroup-{name}"),
    )
    .await?;
    let mut workloads = BTreeMap::new();
    for workload in observed["workloads"]
        .as_object()
        .context("observed workloads")?
        .keys()
    {
        let object = get(journey.run, "hosts", "workloaddeployment", workload).await?;
        workloads.insert(workload, object["spec"].clone());
    }
    ensure!(!workloads.is_empty(), "{app} has no workload");
    let service = get(journey.run, "hosts", "service", &format!("{name}-http")).await?;
    let bindings: Value = database.query_one("SELECT coalesce(jsonb_agg(to_jsonb(b) ORDER BY to_jsonb(b)::text),'[]'::jsonb) FROM catalog.connection_binding b WHERE package_id=$1",&[&app]).await?.get(0);
    if app == "alpha" {
        ensure!(
            bindings.as_array().is_some_and(|rows| !rows.is_empty()),
            "alpha has no installed connection binding"
        );
    }
    let sql_permissions: Value = database.query_one("SELECT coalesce(jsonb_agg(jsonb_build_array(r.rolname,c.oid,has_table_privilege(r.oid,c.oid,'SELECT'),has_table_privilege(r.oid,c.oid,'INSERT'),has_table_privilege(r.oid,c.oid,'UPDATE'),has_table_privilege(r.oid,c.oid,'DELETE')) ORDER BY r.rolname,c.oid),'[]'::jsonb) FROM pg_roles r CROSS JOIN pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE c.relkind IN ('r','p','v','m') AND EXISTS (SELECT FROM catalog.package_definition_owners o WHERE o.owner_package_id=$1 AND o.schema_name=n.nspname AND o.relation_name=c.relname)",&[&app]).await?.get(0);
    let permissions: Value = database.query_one("SELECT coalesce(jsonb_agg(to_jsonb(p) ORDER BY to_jsonb(p)::text),'[]'::jsonb) FROM platform.permission p WHERE role_name=$1 OR permission LIKE $2",&[&format!("application-{app}"),&format!("{app}:%")]).await?.get(0);
    let roles: Value = database.query_one("SELECT coalesce(jsonb_agg(to_jsonb(r) ORDER BY to_jsonb(r)::text),'[]'::jsonb) FROM platform.user_role r WHERE role_name=$1 OR role_name='admin'",&[&format!("application-{app}")]).await?.get(0);
    Ok(
        json!({"current":selected["current"],"previous":selected["previous"],"hostname":selected["hostname"],"revision":observed["revision"],"live":observed["live"],"pods":uids,"deployment":deployment["spec"],"workloads":workloads,"service":service["spec"],"bindings":bindings,"sql_permissions":sql_permissions,"permissions":permissions,"roles":roles}),
    )
}

async fn state(
    journey: &Journey<'_>,
    database: &tokio_postgres::Client,
    baseline: &Value,
    rows: &BTreeMap<&str, (Value, Value)>,
    expected: [(&str, &str); 2],
) -> anyhow::Result<()> {
    ensure!(
        stored(journey, database).await? == *baseline,
        "application SQL, initialization records, sequences or data changed"
    );
    let status = journey.status().await?;
    for ((app, (counter, widget)), (digest, behavior)) in rows.iter().zip(expected) {
        ensure!(
            status["applications"][app]["current"] == digest,
            "{app} selected the wrong release"
        );
        let live = status["applications"][app]["observed"]["live"]
            .as_object()
            .context("live releases")?;
        ensure!(
            live.len() == 1 && live.contains_key(digest),
            "{app} does not exclusively serve {digest}"
        );
        let found = journey
            .route(app, "counter/get", &json!([{"id":counter["id"]}]))
            .await?;
        ensure!(found == *counter, "{app} lost its counter row");
        let found = journey
            .route(app, "widget/get", &json!([{"id":widget["id"]}]))
            .await?;
        ensure!(found == *widget, "{app} lost its widget row");
        let list = journey
            .route(
                app,
                "widget/list",
                &json!([{"selector":{},"maker_id":null}]),
            )
            .await?;
        let found = list["rows"]
            .as_array()
            .context("widget list")?
            .iter()
            .find(|row| row["id"] == widget["id"])
            .context("original widget in list")?;
        ensure!(
            found["attributes"]["behavior"] == behavior,
            "{app} served the wrong executable behavior"
        );
    }
    ensure!(
        status["applications"]["alpha"]["observed"]["release_name"]
            != status["applications"]["beta"]["observed"]["release_name"],
        "applications share a Helm release"
    );
    Ok(())
}

async fn recovery(
    journey: &Journey<'_>,
    candidate: &str,
    running: &str,
    database: &tokio_postgres::Client,
    baseline: &Value,
) -> anyhow::Result<()> {
    // Exhausting pod quota prevents a replacement while preserving the running pod.
    // This is a cluster failure, after product control preparation and a real Helm write.
    let pods = get(journey.run, "hosts", "pods", "").await?;
    let count = pods["items"]
        .as_array()
        .context("host pods")?
        .iter()
        .filter(|pod| {
            !matches!(
                pod["status"]["phase"].as_str(),
                Some("Succeeded" | "Failed")
            )
        })
        .count();
    success(kubectl(journey.run).args([
        "-n",
        "hosts",
        "create",
        "quota",
        "journey-readiness",
        "--hard",
        &format!("pods={count}"),
    ]))
    .await?;
    let result = async {
        let before = runtime(journey, "beta", database).await?;
        let alpha_before = runtime(journey, "alpha", database).await?;
        let output = journey
            .run("failed-alpha", &mut journey.deploy(candidate))
            .await?;
        ensure!(
            !output.status.success(),
            "candidate unexpectedly passed readiness"
        );
        let status = journey.status().await?;
        let alpha = &status["applications"]["alpha"];
        ensure!(
            alpha["current"] == candidate && alpha["previous"] == running,
            "failed selection was not retained"
        );
        ensure!(
            alpha["observed"]["live"][running]
                .as_u64()
                .is_some_and(|n| n > 0),
            "failed replacement lost the running release"
        );
        journey
            .route(
                "alpha",
                "widget/list",
                &json!([{"selector":{},"maker_id":null}]),
            )
            .await?;
        ensure!(
            runtime(journey, "beta", database).await? == before,
            "failed replacement changed beta"
        );
        let name = alpha["observed"]["release_name"]
            .as_str()
            .context("alpha release")?;
        let revision = helm_history(journey.run, name).await?;
        ensure!(
            revision["revision"].as_u64() > alpha_before["revision"]["number"].as_u64(),
            "failed apply made no real Helm write"
        );
        ensure!(
            stored(journey, database).await? == *baseline,
            "failed replacement changed application SQL or data"
        );
        journey
            .route(
                "beta",
                "widget/list",
                &json!([{"selector":{},"maker_id":null}]),
            )
            .await?;
        let mut command = journey.reapply();
        command
            .process_group(0)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(
                journey.run.directory.join("interrupt-alpha.stdout"),
            )?)
            .stderr(std::fs::File::create(
                journey.run.directory.join("interrupt-alpha.stderr"),
            )?);
        let mut child = command.spawn()?;
        let group = ProcessGroup(
            rustix::process::Pid::from_raw(i32::try_from(child.id().context("apply PID")?)?)
                .context("positive PID")?,
        );
        tokio::time::timeout(POLL_LIMIT, async {
            loop {
                ensure!(
                    child.try_wait()?.is_none(),
                    "apply exited before a pending Helm write"
                );
                let history = helm_history(journey.run, name).await?;
                if history["revision"].as_u64() > revision["revision"].as_u64()
                    && history["status"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("pending-"))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("no pending application Helm write")??;
        rustix::process::kill_process_group(group.0, rustix::process::Signal::KILL)?;
        child.wait().await?;
        drop(group);
        ensure!(
            stored(journey, database).await? == *baseline,
            "interrupted replacement changed application SQL or data"
        );
        ensure!(
            runtime(journey, "beta", database).await? == before,
            "interrupted replacement changed beta"
        );
        journey
            .route(
                "alpha",
                "widget/list",
                &json!([{"selector":{},"maker_id":null}]),
            )
            .await?;
        ensure!(
            EnvironmentDocument::compile(&journey.file)?.applications["alpha"]
                .previous
                .as_deref()
                == Some(running),
            "retry replaced previous selection"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let cleanup =
        success(kubectl(journey.run).args(["-n", "hosts", "delete", "quota", "journey-readiness"]))
            .await;
    result?;
    cleanup?;
    journey
        .apply("recover-alpha", &mut journey.reapply())
        .await?;
    let status = journey.status().await?;
    let selected = &status["applications"]["alpha"];
    ensure!(
        selected["current"] == candidate && selected["previous"] == running,
        "recovery changed the persisted selection"
    );
    let live = selected["observed"]["live"]
        .as_object()
        .context("recovered live releases")?;
    ensure!(
        live.len() == 1 && live.contains_key(candidate),
        "recovery did not serve the pending release"
    );
    let name = status["applications"]["alpha"]["observed"]["release_name"]
        .as_str()
        .context("alpha release")?;
    ensure!(
        helm_history(journey.run, name).await?["status"] == "deployed",
        "recovery left Helm pending"
    );
    let served = journey
        .route(
            "alpha",
            "widget/list",
            &json!([{"selector":{},"maker_id":null}]),
        )
        .await?;
    ensure!(
        served["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["attributes"]["behavior"] == "v1")),
        "recovery served the wrong executable behavior"
    );
    journey
        .apply(
            "restore-alpha",
            journey
                .command(false, &["rollback", "alpha", "--env"])
                .arg(&journey.file),
        )
        .await
}

async fn helm_history(run: &Run, name: &str) -> anyhow::Result<Value> {
    let output = success(
        Command::new("helm")
            .arg("--kubeconfig")
            .arg(&run.target.kubeconfig)
            .arg("--kube-context")
            .arg(&run.target.context)
            .args(["history", name, "-n", "hosts", "-o", "json"]),
    )
    .await?;
    let mut history: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    history.pop().context("Helm history is empty")
}

//! Application deployment uses product commands; Kubernetes and SQL are observers.

use sqlx::{Connection as _, Row as _};
#[path = "../../../../../services/ctl/src/read_query.rs"]
mod read_query;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_environment::environment::document::{EnvironmentDocument, SchemaMode};
use wamn_environment::platform::document::Provider;

use super::{POLL_LIMIT, ProcessGroup, Run, execute, get, kubectl, success};

#[path = "control_operations.rs"]
mod control_operations;

#[path = "platform_upgrade.rs"]
mod platform_upgrade;

#[path = "platform_schema.rs"]
mod platform_schema;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Case {
    Recovery,
    Reset,
    PlatformUpgrade,
    PlatformSchema,
    ControlOperations,
}

struct Journey<'a> {
    run: &'a Run,
    file: PathBuf,
    project: &'a str,
    environment: &'a str,
    isolated_host: &'a str,
    variables: &'a BTreeMap<String, String>,
    token: &'a str,
    control_url: &'a str,
    control_token: &'a str,
    client: &'a reqwest::Client,
    edge: &'a str,
}

impl Journey<'_> {
    fn command(&self, args: &[&str]) -> Command {
        self.cli(&self.run.binary, args)
    }

    fn cli(&self, binary: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(binary);
        command
            .current_dir(&self.run.directory)
            .env_remove("WAMN_TEST_OWNER_PASSWORD")
            .env_remove("WAMN_CONTROL_URL")
            .env_remove("WAMN_TOKEN")
            .env_remove("WAMN_RELEASE_HOST_GROUP")
            .env_remove("WAMN_DATABASE_HOST")
            .env_remove("WAMN_DATABASE_PORT")
            .env_remove("WAMN_IDENTITY_URL")
            .env_remove("WAMN_NATS_TLS_SERVER_NAME")
            .env("WAMN_DATABASE_NAMESPACE", "platform")
            .env("WAMN_RELEASE_NAMESPACE", "hosts")
            .envs(self.variables)
            .env("WAMN_CONTROL_URL", self.control_url)
            .env("WAMN_TOKEN", self.control_token)
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
            "{label} failed; see {}: {}",
            self.run.directory.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    fn deploy(&self, application: &str, digest: &str) -> Command {
        let mut command = self.command(&[
            "deploy",
            "--application",
            application,
            "--release",
            digest,
            "--env",
        ]);
        command.arg(&self.file);
        command
    }

    async fn reapply(&self) -> anyhow::Result<Command> {
        let status = self.status().await?;
        let release = status["applications"]["alpha"]["current"]
            .as_str()
            .context("canonical alpha selection")?;
        Ok(self.deploy("alpha", release))
    }

    async fn status(&self) -> anyhow::Result<Value> {
        let output = self
            .run("status", self.command(&["status", "--env"]).arg(&self.file))
            .await?;
        ensure!(
            output.status.success(),
            "status failed; see {}",
            self.run.directory.display()
        );
        let status: Value = serde_json::from_slice(&output.stdout)?;
        eprintln!("observed status: {status}");
        Ok(status)
    }

    async fn route(&self, app: &str, route: &str, body: &Value) -> anyhow::Result<Value> {
        let status = self.status().await?;
        let hostname = status["applications"][app]["hostname"]
            .as_str()
            .context("canonical application hostname")?;
        let mut base = url::Url::parse(self.edge)?;
        base.set_host(Some(hostname))?;
        let endpoint = format!("{}/api/{app}/{route}", base.as_str().trim_end_matches('/'));
        let request = if route.ends_with("/create") {
            self.client.post(endpoint).json(body)
        } else {
            let query = read_query::encode_read_query(body[0].as_object().context("read request")?);
            self.client.get(format!("{endpoint}?{query}"))
        };
        let response = request.bearer_auth(self.token).send().await?;
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
        let output = self
            .run(
                &format!("publish-{app}-{version}"),
                self.command(&["schema", "publish"])
                    .arg(root)
                    .arg("--env")
                    .arg(&self.file),
            )
            .await?;
        ensure!(
            output.status.success(),
            "publish {app}@{version} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let published: Value = serde_json::from_slice(&output.stdout)?;
        let digest = published["release"]
            .as_str()
            .context("published release digest")?
            .to_owned();
        wamn_package_artifact::ensure_digest(&digest)?;
        eprintln!("published {app}@{version}: {digest}");
        Ok(digest)
    }
    async fn delete_environment(&self) -> anyhow::Result<()> {
        let coordinate = format!(
            "{}/{}/{}",
            self.run.document.org, self.environment, self.project
        );
        let binary = self.run.binary.clone();
        let binary = binary.to_str().context("CLI path is not UTF-8")?;
        let deletion = format!(
            "exec '{}' env delete '{}' --data",
            binary.replace('\'', "'\\''"),
            coordinate.replace('\'', "'\\''"),
        );
        let input = self.run.directory.join("delete-environment.input");
        std::fs::write(&input, format!("{coordinate}\n"))?;
        // script supplies the terminal that the product's destructive confirmation requires.
        self.apply(
            "delete-environment",
            self.cli(
                Path::new("sh"),
                &[
                    "-c",
                    "exec script -qec \"$1\" /dev/null < \"$2\"",
                    "cleanup",
                    &deletion,
                ],
            )
            .arg(input),
        )
        .await?;
        eprintln!(
            "Deleted environment {coordinate} and its data. Project deletion has no product operation."
        );
        Ok(())
    }
}

pub(super) async fn exercise(run: &Run, cases: &[Case], password: &str) -> anyhow::Result<()> {
    let result = exercise_inner(run, cases, password).await;
    if run.checkout.exists() {
        success(
            Command::new("git")
                .args(["-c", "core.hooksPath=/dev/null"])
                .arg("-C")
                .arg(&run.repository)
                .args(["worktree", "remove", "--force"])
                .arg(&run.checkout),
        )
        .await?;
    }
    result
}

async fn exercise_inner(run: &Run, cases: &[Case], password: &str) -> anyhow::Result<()> {
    if cases.contains(&Case::PlatformUpgrade) {
        platform_upgrade::inputs(run)?;
    }
    if cases.contains(&Case::PlatformSchema) {
        platform_schema::inputs(run)?;
    }
    success(Command::new("script").arg("--version")).await?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let project = match run.document.provider {
        Provider::Kind => format!("journey-{}", &id[..8]),
        // External IAM grants name these service accounts before the approved run.
        Provider::Gcp => "journey".to_owned(),
    };
    let environment = "dev".to_owned();
    let (_, domain) = run
        .document
        .route_hosts
        .control
        .split_once('.')
        .context("control domain")?;
    let alpha_host = format!("alpha-{}.{domain}", &id[..12]);
    let beta_host = format!("beta-{}.{domain}", &id[..12]);
    let isolated_host = format!("isolated-{}.{domain}", &id[..12]);
    let sources = sources(&run.repository, &run.checkout, &id, cases).await?;
    let postgres =
        wamn_kubernetes::forward::port_forward(&run.target, "platform", "service/wamn-pg-rw", 5432)
            .await?;
    let nats =
        wamn_kubernetes::forward::port_forward(&run.target, "platform", "service/wamn-nats", 4222)
            .await?;
    let identity =
        wamn_kubernetes::forward::port_forward(&run.target, "identity", "service/identity", 443)
            .await?;
    let control = match run.document.provider {
        Provider::Kind => Some(
            wamn_kubernetes::forward::port_forward(
                &run.target,
                "hosts",
                &format!("service/c-{}-http", run.document.org),
                80,
            )
            .await?,
        ),
        Provider::Gcp => None,
    };
    let edge =
        wamn_kubernetes::forward::port_forward(&run.target, "hosts", "service/wamn-edge", 80)
            .await?;
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
    let apply = get(run, "platform", "secret", "wamn-apply").await?;
    let mut admin = get(run, "platform", "secret", "wamn-pg-superuser").await?;
    let apply_url = forwarded(&String::from_utf8(secret(&apply, "url")?)?, postgres.port)?;
    let mut admin_url = url::Url::parse(&format!(
        "postgresql://postgres@127.0.0.1:{}/wamn_system",
        postgres.port
    ))?;
    admin_url
        .set_password(Some(&String::from_utf8(secret(&mut admin, "password")?)?))
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
    for (name, repository) in [
        ("WAMN_PACKAGE_ARTIFACT_BASE", "packages"),
        ("WAMN_COMPONENT_ARTIFACT_BASE", "components"),
        ("WAMN_RELEASE_ARTIFACT_BASE", "releases"),
    ] {
        let value = match run.document.provider {
            Provider::Kind => std::env::var(name).with_context(|| format!("set {name}"))?,
            Provider::Gcp => format!("{}/wamn/{repository}", run.document.registry),
        };
        variables.insert(name.into(), value);
    }
    for name in [
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
    let mut system = wamn_postgres::connect(&variables["WAMN_SYSTEM_ADMIN_URL"]).await?;
    sqlx::raw_sql("SET default_transaction_read_only = on")
        .execute(&mut system)
        .await
        .map(drop)?;
    let enrolled: bool = sqlx::query(
        "SELECT EXISTS(SELECT FROM identity.password_credentials c WHERE c.principal_id=p.id) \
             FROM identity.principals p WHERE p.email=$1",
    )
    .bind(&run.document.owner_email)
    .fetch_one(&mut system)
    .await?
    .get(0);
    if run.document.provider == Provider::Gcp {
        let exists: bool =
            sqlx::query("SELECT EXISTS(SELECT FROM registry.projects WHERE org=$1 AND id=$2)")
                .bind(&run.document.org)
                .bind(&project)
                .fetch_one(&mut system)
                .await?
                .get(0);
        ensure!(
            !exists,
            "GKE journey project already exists; use a fresh disposable platform"
        );
    }
    drop(system);
    ensure!(
        enrolled || run.document.provider == Provider::Kind,
        "enroll the GCP owner through the hosted UI before this run"
    );
    let login = super::owner_session(run, &identity_client, &endpoint, password, !enrolled).await?;
    let principal = login["sub"].as_str().context("owner session principal")?;
    let access = login["access_token"]
        .as_str()
        .context("owner control session")?;
    let file = run.directory.join("environments/environment.k");
    let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":run.document.org,"project":project,"env":environment,"target":"cloud","policy":"dev","revocation_seconds":60,
        "applications":{
            "alpha":{"revocation_seconds":5,"hostname":alpha_host},
            "beta":{"revocation_seconds":5,"hostname":beta_host}
        }
    }))?)?;
    std::fs::write(&file, document.to_kcl())?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .resolve(&alpha_host, ([127, 0, 0, 1], edge.port).into())
        .resolve(&beta_host, ([127, 0, 0, 1], edge.port).into())
        .resolve(&isolated_host, ([127, 0, 0, 1], edge.port).into())
        .build()?;
    let edge_url = format!("http://{alpha_host}:{}", edge.port);
    let issued: Value = identity_client.post(format!("{endpoint}/pats"))
        .json(&json!({"principal_id":principal,"label":"deployment journey","lifetime_seconds":14400}))
        .send().await?.error_for_status()?.json().await?;
    let token = issued["token"].as_str().context("owner PAT")?.to_owned();
    let control_url = match run.document.provider {
        Provider::Kind => format!(
            "http://127.0.0.1:{}",
            control.as_ref().context("type control forward")?.port
        ),
        // The public edge serves control routes under /api.
        Provider::Gcp => format!("https://{}/api", run.document.route_hosts.control),
    };
    let journey = Journey {
        run,
        file,
        project: &project,
        environment: &environment,
        isolated_host: &isolated_host,
        variables: &variables,
        token: &token,
        control_url: &control_url,
        control_token: access,
        client: &client,
        edge: &edge_url,
    };
    let operations = async {
        let create = identity_client.post(format!("{control_url}/wamn_control/project/create"));
        let create = match run.document.provider {
            Provider::Kind => {
                create.header(reqwest::header::HOST, &run.document.route_hosts.control)
            }
            Provider::Gcp => create,
        };
        let created: Value = create
        .bearer_auth(access)
        .json(&json!([{"request_id":uuid::Uuid::new_v4().to_string(),"value":{"project":project}}]))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
        ensure!(
            created[0].get("value").is_some(),
            "project creation refused: {created}"
        );

        let artifacts = prepare_artifacts(&journey, &sources, cases).await?;
        for &case in cases {
            let document = case_document(&document, case);
            std::fs::write(&journey.file, document.to_kcl())?;
            let result = match case {
                Case::ControlOperations => {
                    control_operations::exercise(
                        &journey,
                        &artifacts,
                        &identity_client,
                        &endpoint,
                        &control_url,
                        password,
                    )
                    .await
                }
                Case::Reset => reset(&journey, &artifacts).await,
                Case::Recovery => application_recovery(&journey, &artifacts).await,
                Case::PlatformUpgrade => platform_upgrade::exercise(&journey, &artifacts).await,
                Case::PlatformSchema => platform_schema::exercise(&journey, &artifacts["a0"]).await,
            };
            result?;
            journey.delete_environment().await?;
        }
        Ok(())
    };
    let result = match run.document.provider {
        Provider::Kind => operations.await,
        // Return from a stalled case in time to run product cleanup within the GKE bound.
        Provider::Gcp => tokio::time::timeout(Duration::from_mins(45), operations)
            .await
            .context("GKE application operations exceeded 45 minutes")
            .and_then(|result| result),
    };
    // Publication and provisioning can fail after creating part of the environment.
    if result.is_err()
        && let Err(error) = journey.delete_environment().await
    {
        eprintln!("environment cleanup after failure failed: {error:#}");
    }
    let prefix = issued["token_prefix"]
        .as_str()
        .context("owner PAT prefix")?;
    let revoked = journey
        .apply(
            "revoke-journey-pat",
            &mut journey.cli(&run.binary, &["revoke-pat", "--prefix", prefix]),
        )
        .await;
    if let Err(error) = &revoked {
        eprintln!("journey PAT cleanup failed: {error:#}");
    }
    result.and(revoked)
}

fn case_document(document: &EnvironmentDocument, case: Case) -> EnvironmentDocument {
    let mut document = document.clone();
    if case == Case::PlatformSchema {
        document.applications.retain(|name, _| name == "alpha");
    }
    if case == Case::ControlOperations {
        document.schema.mode = Some(SchemaMode::Migrate);
    }
    document
}

// Produce immutable inputs once, before any focused body. This uses the same
// build and publication operation as source deploy, without installing code.
async fn prepare_artifacts(
    journey: &Journey<'_>,
    sources: &BTreeMap<&str, PathBuf>,
    cases: &[Case],
) -> anyhow::Result<BTreeMap<&'static str, String>> {
    let document = EnvironmentDocument::compile(&journey.file)?;
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        "a0",
        journey.publish("alpha", "1.0.0", &sources["alpha"]).await?,
    );
    if cases.iter().any(|case| *case != Case::PlatformSchema) {
        artifacts.insert(
            "b0",
            journey.publish("beta", "1.0.0", &sources["beta"]).await?,
        );
    }
    if cases.contains(&Case::Recovery) {
        version(&sources["alpha"], "1.0.0", "1.0.1")?;
        artifacts.insert(
            "a1",
            journey.publish("alpha", "1.0.1", &sources["alpha"]).await?,
        );
    }
    if cases.contains(&Case::ControlOperations) {
        version(&sources["beta"], "1.0.0", "1.0.1")?;
        artifacts.insert(
            "b1",
            journey.publish("beta", "1.0.1", &sources["beta"]).await?,
        );
        version(&sources["alpha"], "1.0.0", "1.1.0")?;
        std::fs::write(
            sources["alpha"].join("migrations/0002_counter_data.sql"),
            "UPDATE alpha_data.counter SET label = label || '-migrated';\n",
        )?;
        artifacts.insert(
            "schema",
            journey.publish("alpha", "1.1.0", &sources["alpha"]).await?,
        );
    }
    if cases.contains(&Case::Reset) {
        let root = &sources["alpha"];
        let manifest = root.join("wamn.k");
        let initial = root.join("migrations/0001_initial.sql");
        let code = std::fs::read_to_string(&manifest)?;
        let sql = std::fs::read_to_string(&initial)?;
        let previous = if cases.contains(&Case::Recovery) {
            "1.0.1"
        } else {
            "1.0.0"
        };
        std::fs::write(
            &manifest,
            code.replace(&format!("version = \"{previous}"), "version = \"1.1.0"),
        )?;
        std::fs::write(
            &initial,
            format!("{sql}\nCREATE INDEX widget_note_idx ON alpha_data.widget (note);\n"),
        )?;
        artifacts.insert("reset", journey.publish("alpha", "1.1.0", root).await?);
        std::fs::write(manifest, code)?;
        std::fs::write(initial, sql)?;
        let mut isolated = document.clone();
        isolated.env = format!("isolated-{}", journey.environment);
        isolated.applications.retain(|name, _| name == "beta");
        std::fs::write(&journey.file, isolated.to_kcl())?;
        artifacts.insert(
            "isolation",
            journey
                .publish("beta", "isolation", &sources["beta"])
                .await?,
        );
        let root = &sources["beta"];
        let manifest = root.join("wamn.k");
        let code = std::fs::read_to_string(&manifest)?;
        let start = code
            .find("        counter = manifest.Model")
            .context("counter model")?;
        let end = code[start..]
            .find("        numbered_label =")
            .context("next model")?
            + start;
        // A package has one runtime schema, and its relations need distinct table names.
        let counter = code[start..end]
            .replace(
                "counter = manifest.Model(\"counter\", _schema)",
                "capture_counter = manifest.Model(\"capture_counter\", _schema)",
            )
            .replace("table = \"counter\"", "table = \"capture_counter\"");
        std::fs::write(
            &manifest,
            code.replacen("    models = {", &format!("    models = {{\n{counter}"), 1)
                .replace("version = \"1.0.0", "version = \"1.1.0"),
        )?;
        std::fs::write(
            root.join("migrations/0002_capture.sql"),
            "CREATE TABLE beta_data.capture_counter (\n\
             id bigint GENERATED ALWAYS AS IDENTITY CONSTRAINT capture_counter_id_pkey PRIMARY KEY,\n\
             label text NOT NULL, edit_version int4 NOT NULL DEFAULT 1,\n\
             created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP);\n",
        )?;
        artifacts.insert(
            "isolation_capture",
            journey.publish("beta", "isolation-capture", root).await?,
        );
    }
    std::fs::write(&journey.file, document.to_kcl())?;
    Ok(artifacts)
}

async fn data(journey: &Journey<'_>) -> anyhow::Result<BTreeMap<String, (Value, Value)>> {
    let mut rows = BTreeMap::new();
    let mut status = journey.status().await?;
    let applications: BTreeMap<String, Value> =
        serde_json::from_value(status["applications"].take())?;
    for app in applications.into_keys() {
        let counter = journey
            .route(
                &app,
                "counter/create",
                &json!([{
                    "request_id":uuid::Uuid::new_v4().to_string(),
                    "idempotency_key":uuid::Uuid::new_v4().to_string(), "label":app
                }]),
            )
            .await?;
        let widget = journey.route(&app, "widget/create", &json!([{
            "request_id":uuid::Uuid::new_v4().to_string(),
            "idempotency_key":uuid::Uuid::new_v4().to_string(),
            "id":uuid::Uuid::new_v4().to_string(), "code":"standard", "maker_id":null,"note":app
        }])).await?;
        rows.insert(app, (counter, widget));
    }
    Ok(rows)
}

async fn reset(journey: &Journey<'_>, artifacts: &BTreeMap<&str, String>) -> anyhow::Result<()> {
    journey
        .apply(
            "provision-reset-environment",
            journey
                .cli(&journey.run.binary, &["env", "apply"])
                .arg(&journey.file)
                .args(["--application", "beta"]),
        )
        .await?;
    journey
        .apply(
            "designate-disposable",
            journey
                .command(&["env", "apply"])
                .arg(&journey.file)
                .arg("--disposable"),
        )
        .await?;
    for (application, key) in [("beta", "b0"), ("alpha", "a0")] {
        journey
            .apply(
                &format!("install-{application}-schema"),
                journey
                    .command(&[
                        "schema",
                        "apply",
                        application,
                        "--release",
                        &artifacts[key],
                        "--env",
                    ])
                    .arg(&journey.file),
            )
            .await?;
        journey
            .apply(
                &format!("install-{application}"),
                &mut journey.deploy(application, &artifacts[key]),
            )
            .await?;
    }
    let rows = data(journey).await?;
    let selections = journey.status().await?;
    let mut previous = BTreeMap::new();
    let mut writers = Vec::new();
    for app in rows.keys() {
        let running = runtime(journey, app).await?;
        let name = selections["applications"][app]["observed"]["release_name"]
            .as_str()
            .context("application release")?;
        writers.push(wamn_registry::name::resource_suffix("hostgroup", name));
        previous.insert(app, running);
    }
    let document = EnvironmentDocument::compile(&journey.file)?;
    let shared = wamn_control_provision::environment_names::environment_resource_name(
        &document.org,
        &document.project,
        &document.env,
    )?;
    writers.push(wamn_registry::name::resource_suffix("hostgroup", &shared));
    writers.push(wamn_control_provision::project_env_cdc_secret_name(
        &document.org,
        &document.project,
        &document.env,
    ));
    let mut database = observer(journey).await?;
    // Hold the application-DDL lock so the stopped state is observable before reset writes.
    let mut blocked = database.begin().await?;
    sqlx::raw_sql(wamn_environment::apply_package::LOCK_SQL)
        .execute(&mut *blocked)
        .await?;
    let stopped = async {
        tokio::time::timeout(POLL_LIMIT, async {
            loop {
                let mut all_stopped = true;
                for name in &writers {
                    let deployment = get(journey.run, "hosts", "deployment", name).await?;
                    all_stopped &= deployment["spec"]["replicas"] == 0
                        && deployment["status"]["readyReplicas"].as_u64().unwrap_or(0) == 0;
                }
                if all_stopped {
                    break Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .context("reset did not stop application hosts and the reader")??;
        blocked.rollback().await?;
        Ok::<_, anyhow::Error>(())
    };
    let mut command = schema_command(journey, "apply", Some(&artifacts["reset"]));
    let (applied, observed) = tokio::join!(journey.apply("reset-schema", &mut command), stopped);
    observed?;
    applied?;
    let status = journey.status().await?;
    let indexed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT FROM pg_index WHERE indexrelid=to_regclass('alpha_data.widget_note_idx') AND indisvalid)")
        .fetch_one(&mut database).await?;
    ensure!(indexed, "reset omitted the selected package's index");
    for (app, (_, widget)) in &rows {
        ensure!(
            status["applications"][app]["current"] == selections["applications"][app]["current"],
            "reset changed {app}'s code selections"
        );
        let removed: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT NOT EXISTS(SELECT FROM {}.widget WHERE id=$1::text::uuid)",
            postgres_protocol::escape::escape_identifier(&format!("{app}_data")),
        )))
        .bind(widget["id"].as_str().context("seeded widget")?)
        .fetch_one(&mut database)
        .await?;
        ensure!(removed, "reset retained {app}'s data");
        let resumed = runtime(journey, app).await?;
        ensure!(
            resumed["pods"]
                .as_array()
                .context("resumed pods")?
                .iter()
                .all(|pod| !previous[app]["pods"]
                    .as_array()
                    .is_some_and(|pods| pods.contains(pod))),
            "reset did not replace {app}'s stopped pods"
        );
        let listed = journey
            .route(
                app,
                "widget/list",
                &json!([{"selector":{},"maker_id":null}]),
            )
            .await?;
        ensure!(
            listed["rows"].as_array().is_some_and(Vec::is_empty),
            "{app} did not resume with empty data"
        );
    }
    Ok(())
}

async fn application_recovery(
    journey: &Journey<'_>,
    artifacts: &BTreeMap<&str, String>,
) -> anyhow::Result<()> {
    first_install(journey, &artifacts["b0"]).await?;
    journey
        .apply(
            "initialize-alpha",
            &mut schema_command(journey, "apply", Some(&artifacts["a0"])),
        )
        .await?;
    journey
        .apply(
            "install-alpha",
            &mut journey.deploy("alpha", &artifacts["a0"]),
        )
        .await?;
    let rows = data(journey).await?;
    let mut database = observer(journey).await?;
    let baseline = stored(journey, &mut database).await?;
    recovery(
        journey,
        &artifacts["a1"],
        &artifacts["a0"],
        &mut database,
        &baseline,
    )
    .await?;
    for (app, (counter, widget)) in &rows {
        ensure!(
            journey
                .route(app, "counter/get", &json!([{"id":counter["id"]}]))
                .await?
                == *counter
                && journey
                    .route(app, "widget/get", &json!([{"id":widget["id"]}]))
                    .await?
                    == *widget,
            "{app} lost data during recovery"
        );
    }
    Ok(())
}

fn schema_command(journey: &Journey<'_>, verb: &str, release: Option<&str>) -> Command {
    let mut command = journey.command(&["schema", verb, "alpha", "--env"]);
    command.arg(&journey.file);
    if let Some(release) = release {
        command.args(["--release", release]);
    }
    command
}

async fn first_install(
    journey: &Journey<'_>,
    digest: &str,
) -> anyhow::Result<std::process::Output> {
    let mut system = wamn_postgres::connect(&journey.variables["WAMN_SYSTEM_ADMIN_URL"]).await?;
    sqlx::raw_sql("SET default_transaction_read_only = on")
        .execute(&mut system)
        .await
        .map(drop)?;
    let environment_sql = "SELECT jsonb_build_object('instance_suffix',instance_suffix,'created_at',created_at) FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3";
    let org = &journey.run.document.org;
    ensure!(
        sqlx::query(sqlx::AssertSqlSafe((environment_sql).to_string()))
            .bind(org)
            .bind(&journey.project)
            .bind(&journey.environment)
            .fetch_optional(&mut system)
            .await?
            .is_none(),
        "interrupted first install requires an absent environment"
    );
    let name = wamn_control_provision::environment_names::application_release_name(
        org,
        journey.project,
        journey.environment,
        "beta",
    )?;
    let shared = wamn_control_provision::environment_names::environment_resource_name(
        org,
        journey.project,
        journey.environment,
    )?;
    let mut environment = Value::Null;
    for after_sql in [false, true] {
        let label = if after_sql {
            "interrupt-beta-after-sql"
        } else {
            "interrupt-beta-provisioning"
        };
        let started = Instant::now();
        if after_sql {
            journey
                .apply(
                    "retry-provisioning",
                    journey
                        .cli(&journey.run.binary, &["env", "apply"])
                        .arg(&journey.file)
                        .args(["--application", "beta"]),
                )
                .await?;
            journey
                .apply(
                    "initialize-beta",
                    journey
                        .command(&["schema", "apply", "beta", "--release", digest, "--env"])
                        .arg(&journey.file),
                )
                .await?;
        }
        let mut command = if after_sql {
            journey.deploy("beta", digest)
        } else {
            let mut command = journey.cli(&journey.run.binary, &["env", "apply"]);
            command.arg(&journey.file).args(["--application", "beta"]);
            command
        };
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
                    "{label}: command exited before the interruption point"
                );
                if after_sql {
                    // Schema initialization completed explicitly before this code deployment.
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
                } else if let Some(row) =
                    sqlx::query(sqlx::AssertSqlSafe((environment_sql).to_string()))
                        .bind(org)
                        .bind(&journey.project)
                        .bind(&journey.environment)
                        .fetch_optional(&mut system)
                        .await?
                {
                    break Ok(row.get::<Value, _>(0));
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
        ensure!(!exited?.success(), "interrupted command returned success");
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
        }
    }
    let mut database = observer(journey).await?;
    let baseline = stored(journey, &mut database).await?;
    let output = journey
        .run("install-beta", &mut journey.deploy("beta", digest))
        .await?;
    ensure!(
        output.status.success(),
        "interrupted first install did not recover"
    );
    ensure!(
        sqlx::query(sqlx::AssertSqlSafe((environment_sql).to_string()))
            .bind(org)
            .bind(&journey.project)
            .bind(&journey.environment)
            .fetch_one(&mut system)
            .await?
            .get::<Value, _>(0)
            == environment,
        "first-install retry replaced the environment instance"
    );
    ensure!(
        stored(journey, &mut database).await? == baseline,
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

async fn sources(
    repository: &Path,
    checkout: &Path,
    id: &str,
    cases: &[Case],
) -> anyhow::Result<BTreeMap<&'static str, PathBuf>> {
    let head = success(
        Command::new("git")
            .args(["-c", "core.hooksPath=/dev/null"])
            .arg("-C")
            .arg(repository)
            .args(["rev-parse", "HEAD"]),
    )
    .await?;
    let head = String::from_utf8(head.stdout)?;
    success(
        Command::new("git")
            .args(["-c", "core.hooksPath=/dev/null"])
            .arg("-C")
            .arg(repository)
            .args(["worktree", "add", "--detach"])
            .arg(checkout)
            .arg(head.trim()),
    )
    .await?;
    success(
        Command::new("git")
            .args(["-c", "core.hooksPath=/dev/null"])
            .arg("-C")
            .arg(checkout)
            .args(["submodule", "update", "--init", "--recursive"]),
    )
    .await?;
    let apps = checkout.join("apps");
    let mut roots = BTreeMap::new();
    roots.insert("alpha", apps.join("alpha"));
    if cases.iter().any(|case| *case != Case::PlatformSchema) {
        roots.insert("beta", apps.join("beta"));
    }
    for (&app, root) in &roots {
        copy_source(&apps.join("platform_fixture"), root, app)?;
        let manifest = root.join("wamn.k");
        let text = std::fs::read_to_string(&manifest)?.replace(
            "version = \"2.1.1\"",
            &format!("version = \"1.0.0-test-{id}\""),
        );
        let text = text
            .replace(
                "routes = {",
                &format!("routes = {{path_prefix = \"/{app}\", "),
            )
            .replace("path = \"/", &format!("path = \"/{app}/"));
        std::fs::write(manifest, text)?;
        let query = root.join("query/widget_list.sql");
        std::fs::write(
            &query,
            std::fs::read_to_string(&query)?.replace(
                "to_jsonb(widget)",
                "to_jsonb(widget) || '{\"behavior\":\"v0\"}'::jsonb",
            ),
        )?;
    }
    let manifest = apps.join("Cargo.toml");
    let mut members = String::new();
    for app in roots.keys() {
        writeln!(members, "    \"{app}/component\",")?;
    }
    let workspace = std::fs::read_to_string(&manifest)?.replace(
        "\"platform_fixture/component\",",
        &format!("\"platform_fixture/component\",\n{members}"),
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
            &format!("version = \"{previous}"),
            &format!("version = \"{next}"),
        ),
    )?;
    let query = root.join("query/widget_list.sql");
    std::fs::write(&query, std::fs::read_to_string(&query)?.replace("v0", "v1"))?;
    Ok(())
}

async fn observer(journey: &Journey<'_>) -> anyhow::Result<sqlx::PgConnection> {
    let mut system = wamn_postgres::connect(&journey.variables["WAMN_SYSTEM_ADMIN_URL"]).await?;
    sqlx::raw_sql("SET default_transaction_read_only = on")
        .execute(&mut system)
        .await
        .map(drop)?;
    let document = EnvironmentDocument::compile(&journey.file)?;
    let row = sqlx::query("SELECT instance_suffix FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3").bind(&document.org).bind(&document.project).bind(&document.env).fetch_one(&mut system).await?;
    let suffix: String = row.get(0);
    let name = wamn_control_provision::project_env_database_name(
        &document.org,
        &document.project,
        &document.env,
        &suffix,
    );
    let mut url = url::Url::parse(&journey.variables["WAMN_SYSTEM_ADMIN_URL"])?;
    url.set_path(&name);
    let mut client = wamn_postgres::connect(url.as_str()).await?;
    let owner: String = sqlx::query(
        "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname=current_database()",
    )
    .fetch_one(&mut client)
    .await?
    .get(0);
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "SET ROLE {}",
            postgres_protocol::escape::escape_identifier(&owner)
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)?;
    sqlx::raw_sql("SET default_transaction_read_only = on")
        .execute(&mut client)
        .await
        .map(drop)?;
    Ok(client)
}

async fn query(client: &mut sqlx::PgConnection, sql: &str) -> anyhow::Result<Value> {
    Ok(sqlx::query(sqlx::AssertSqlSafe((sql).to_string()))
        .fetch_one(&mut *client)
        .await
        .with_context(|| format!("observe {sql}"))?
        .get(0))
}

async fn stored(journey: &Journey<'_>, client: &mut sqlx::PgConnection) -> anyhow::Result<Value> {
    let status = journey.status().await?;
    let identity = json!({
        "cluster": get(journey.run, "platform", "cluster.postgresql.cnpg.io", "wamn-pg").await?["metadata"]["uid"],
        "database": query(client, "SELECT jsonb_build_array(oid,datname) FROM pg_database WHERE datname=current_database()").await?,
        "packages": status["schema"]["installed"],
    });
    eprintln!("database identity: {identity}");
    Ok(identity)
}

async fn runtime(journey: &Journey<'_>, app: &str) -> anyhow::Result<Value> {
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
        &wamn_registry::name::resource_suffix("hostgroup", name),
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
    let runtime = json!({"current":selected["current"],"hostname":selected["hostname"],"revision":observed["revision"],"live":observed["live"],"pods":uids,"deployment":deployment["spec"],"workloads":workloads,"service":service["spec"]});
    eprintln!("{app} runtime: {runtime}");
    Ok(runtime)
}

async fn recovery(
    journey: &Journey<'_>,
    candidate: &str,
    running: &str,
    database: &mut sqlx::PgConnection,
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
        journey.project,
        "--hard",
        &format!("pods={count}"),
    ]))
    .await?;
    let result = async {
        let before = runtime(journey, "beta").await?;
        let alpha_before = runtime(journey, "alpha").await?;
        let output = journey
            .run("failed-alpha", &mut journey.deploy("alpha", candidate))
            .await?;
        ensure!(
            !output.status.success(),
            "candidate unexpectedly passed readiness"
        );
        let status = journey.status().await?;
        let alpha = &status["applications"]["alpha"];
        ensure!(
            alpha["current"] == running,
            "failed rollout replaced the successful release"
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
            runtime(journey, "beta").await? == before,
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
            "failed replacement changed database or schema identity"
        );
        journey
            .route(
                "beta",
                "widget/list",
                &json!([{"selector":{},"maker_id":null}]),
            )
            .await?;
        let mut command = journey.reapply().await?;
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
            "interrupted replacement changed database or schema identity"
        );
        ensure!(
            runtime(journey, "beta").await? == before,
            "interrupted replacement changed beta"
        );
        journey
            .route(
                "alpha",
                "widget/list",
                &json!([{"selector":{},"maker_id":null}]),
            )
            .await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let cleanup =
        success(kubectl(journey.run).args(["-n", "hosts", "delete", "quota", journey.project]))
            .await;
    result?;
    cleanup?;
    journey
        .apply("recover-alpha", &mut journey.deploy("alpha", candidate))
        .await?;
    let status = journey.status().await?;
    let selected = &status["applications"]["alpha"];
    ensure!(
        selected["current"] == candidate,
        "recovery did not apply the requested release"
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
                .command(&["rollback", "alpha", "--env"])
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

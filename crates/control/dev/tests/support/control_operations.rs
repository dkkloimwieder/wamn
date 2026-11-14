//! DEP6 observes authenticated workers and selection isolation on one platform.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_catalog::HostRouteSet;
use wamn_environment::platform::{artifact_plan, document::Provider};

use super::{Journey, get, route_request, success};

struct Control<'a> {
    journey: &'a Journey<'a>,
    client: &'a reqwest::Client,
    url: &'a str,
    identity_url: &'a str,
    email: &'a str,
    password: &'a str,
    session: tokio::sync::Mutex<Value>,
}

impl Control<'_> {
    async fn request(&self, operation: &str, mut value: Value) -> anyhow::Result<Value> {
        let (_, attachment) = HostRouteSet::Control
            .attachments()
            .find(|(_, attachment)| attachment.route.operation == operation)
            .context("control operation contract")?;
        if attachment.route.authority == wamn_catalog::HostRouteAuthority::ProjectAdmin {
            value["project"] = self.journey.project.into();
            value["env"] = self.journey.environment.into();
        }
        let mut session = self.session.lock().await;
        if session["expires_at"].as_i64().context("session expiry")?
            <= chrono::Utc::now().timestamp() + 30
        {
            *session = super::super::password_session(
                self.journey.run,
                self.client,
                self.identity_url,
                self.email,
                self.password,
                false,
            )
            .await?;
        }
        let mut request = route_request::request(
            self.client,
            self.url,
            session["access_token"]
                .as_str()
                .context("session access token")?,
            attachment,
            &value,
        )?;
        drop(session);
        // Only the type port-forward URL needs the route host. The GKE URL
        // already names it, and the load balancer refuses a second Host over HTTP/2.
        if self.journey.run.document.provider == Provider::Kind {
            request.headers_mut().insert(
                reqwest::header::HOST,
                self.journey.run.document.route_hosts.control.parse()?,
            );
        }
        let response = self.client.execute(request).await?.error_for_status()?;
        let [answer]: [Value; 1] = response.json().await?;
        Ok(answer)
    }

    async fn call(&self, operation: &str, value: Value) -> anyhow::Result<Value> {
        let mut answer = self.request(operation, value).await?;
        ensure!(
            answer.get("error").is_none(),
            "{operation} refused: {answer}"
        );
        answer
            .as_object_mut()
            .context("control answer")?
            .remove("value")
            .context("control value")
    }

    async fn operation(&self, operation: &str, value: Value) -> anyhow::Result<Value> {
        let started = self.call(operation, value).await?;
        let name = started["job"].as_str().context("operation Job")?;
        tokio::time::timeout(Duration::from_mins(16), async {
            loop {
                let status = self.call("operation.status", json!({"job":name})).await?;
                match status["state"].as_str() {
                    Some("succeeded") => break,
                    Some("failed") => anyhow::bail!("{operation} failed: {}", status["log"]),
                    Some("pending" | "running") => tokio::time::sleep(Duration::from_secs(1)).await,
                    _ => anyhow::bail!("invalid operation status: {status}"),
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("operation Job deadline")??;
        let pods = get(
            self.journey.run,
            "platform",
            "pods",
            &format!("-l=job-name={name}"),
        )
        .await?;
        ensure!(
            !pods["items"].as_array().context("Job pods")?.is_empty(),
            "Job has no pod"
        );
        Ok(pods)
    }
}

pub(super) async fn exercise(
    journey: &Journey<'_>,
    artifacts: &BTreeMap<&str, String>,
    identity: &reqwest::Client,
    identity_url: &str,
    control_url: &str,
    password: &str,
) -> anyhow::Result<()> {
    // Initial environment declaration and provisioning remain trusted product verbs.
    journey
        .apply(
            "provision-environment",
            journey
                .cli(
                    &journey.run.binary.with_file_name("wamn-ctl"),
                    &["env", "apply"],
                )
                .arg(&journey.file)
                .args(["--application", "alpha"]),
        )
        .await?;
    for (application, key) in [("alpha", "a0"), ("beta", "b0")] {
        journey
            .apply(
                &format!("bootstrap-{application}-schema"),
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
                &format!("bootstrap-{application}-code"),
                &mut journey.deploy(application, &artifacts[key]),
            )
            .await?;
    }
    let row = journey
        .route(
            "alpha",
            "counter/create",
            &json!([{
                "request_id":uuid::Uuid::new_v4().to_string(),
                "idempotency_key":uuid::Uuid::new_v4().to_string(),"label":"before"
            }]),
        )
        .await?;
    // Published execution must succeed after the submitting checkout is gone.
    success(
        Command::new("git")
            .args(["-c", "core.hooksPath=/dev/null"])
            .arg("-C")
            .arg(&journey.run.repository)
            .args(["worktree", "remove", "--force"])
            .arg(&journey.run.checkout),
    )
    .await?;
    let session = super::super::password_session(
        journey.run,
        identity,
        identity_url,
        &journey.run.document.owner_email,
        password,
        false,
    )
    .await?;
    let control = Control {
        journey,
        client: identity,
        url: control_url,
        identity_url,
        email: &journey.run.document.owner_email,
        password,
        session: tokio::sync::Mutex::new(session),
    };
    let coordinate = wamn_control_provision::environment_names::environment_resource_name(
        &journey.run.document.org,
        journey.project,
        journey.environment,
    )?;
    // This case enrolls its outsider from kind's local mail outbox.
    if journey.run.document.provider == Provider::Kind {
        denial(&control, identity_url, &artifacts["b1"]).await?;
    }
    let before = document(journey, &coordinate).await?;
    let alpha_release = application_release(journey, "alpha").await?;
    let deployed = control
        .operation(
            "application.deploy",
            json!({"application":"beta","release":artifacts["b1"]}),
        )
        .await?;
    let after = document(journey, &coordinate).await?;
    ensure!(
        after["applications"]["alpha"] == before["applications"]["alpha"]
            && application_release(journey, "alpha").await? == alpha_release,
        "deploying beta changed alpha's selection or release ConfigMap"
    );
    ensure!(
        after["applications"]["beta"]["current"] == artifacts["b1"],
        "beta selection did not advance"
    );
    worker(journey, &deployed).await?;
    control
        .operation("application.rollback", json!({"application":"beta"}))
        .await?;
    let rolled_back = document(journey, &coordinate).await?;
    ensure!(
        rolled_back["applications"]["beta"]["current"] == artifacts["b0"]
            && rolled_back["applications"]["alpha"] == before["applications"]["alpha"],
        "route rollback did not restore only beta"
    );
    let applied = control
        .operation(
            "schema.apply",
            json!({"application":"alpha","release":artifacts["schema"]}),
        )
        .await?;
    job_credentials(journey, &coordinate, &deployed, &applied).await?;
    let found = journey
        .route("alpha", "counter/get", &json!([{"id":row["id"]}]))
        .await?;
    ensure!(
        found["label"] == "before-migrated",
        "schema.apply did not mutate the served data"
    );
    ensure!(
        document(journey, &coordinate).await?["applications"] == rolled_back["applications"],
        "schema.apply changed code selections"
    );
    Ok(())
}

async fn document(journey: &Journey<'_>, coordinate: &str) -> anyhow::Result<Value> {
    let object = get(journey.run, "hosts", "configmap", coordinate).await?;
    Ok(serde_json::from_str(
        object["data"]["environment.json"]
            .as_str()
            .context("environment document")?,
    )?)
}

async fn application_release(journey: &Journey<'_>, application: &str) -> anyhow::Result<Value> {
    let name = wamn_control_provision::environment_names::application_release_name(
        &journey.run.document.org,
        journey.project,
        journey.environment,
        application,
    )?;
    Ok(get(journey.run, "hosts", "configmap", &name).await?["data"].take())
}

async fn jobs(control: &Control<'_>) -> anyhow::Result<BTreeSet<String>> {
    let objects = get(control.journey.run, "platform", "jobs", "").await?;
    objects["items"]
        .as_array()
        .context("Jobs")?
        .iter()
        .map(|job| {
            Ok(job["metadata"]["uid"]
                .as_str()
                .context("Job UID")?
                .to_owned())
        })
        .collect()
}

async fn denial(control: &Control<'_>, identity_url: &str, release: &str) -> anyhow::Result<()> {
    let other = format!("other-{}", uuid::Uuid::new_v4().simple());
    control
        .call("project.create", json!({"project":other}))
        .await?;
    let email = format!("{other}@example.invalid");
    control.call("user.invite", json!({"email":email,"display_name":"Other project administrator","project_admins":[other]})).await?;
    let password = uuid::Uuid::new_v4().to_string();
    let login = super::super::password_session(
        control.journey.run,
        control.client,
        identity_url,
        &email,
        &password,
        true,
    )
    .await?;
    let outsider = Control {
        journey: control.journey,
        client: control.client,
        url: control.url,
        identity_url,
        email: &email,
        password: &password,
        session: tokio::sync::Mutex::new(login),
    };
    // A valid control session for another project reaches authorization, rather than failing authentication.
    outsider.call("control.mine", json!({})).await?;
    let before = jobs(control).await?;
    // A control route denial is the documented 403 `permission-denied` delivery answer.
    let answer = outsider
        .request(
            "application.deploy",
            json!({"application":"alpha","release":release}),
        )
        .await;
    ensure!(
        answer
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<reqwest::Error>()?.status())
            == Some(reqwest::StatusCode::FORBIDDEN),
        "cross-project deploy was not denied: {answer:?}"
    );
    ensure!(
        jobs(control).await? == before,
        "denied deploy created a Job"
    );
    Ok(())
}

fn secret_names<'a>(value: &'a Value, names: &mut BTreeSet<&'a str>) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let name = match key.as_str() {
                    "secretKeyRef" | "secretRef" => value["name"].as_str(),
                    "secret" => value["secretName"]
                        .as_str()
                        .or_else(|| value["name"].as_str()),
                    _ => None,
                };
                if let Some(name) = name {
                    names.insert(name);
                }
                secret_names(value, names);
            }
        }
        Value::Array(values) => {
            for value in values {
                secret_names(value, names);
            }
        }
        _ => {}
    }
}

async fn job_credentials(
    journey: &Journey<'_>,
    coordinate: &str,
    code: &Value,
    schema: &Value,
) -> anyhow::Result<()> {
    let target = get(
        journey.run,
        "platform",
        "configmap",
        &format!("{coordinate}-operations"),
    )
    .await?;
    let cluster = target["data"]["cluster"]
        .as_str()
        .context("target CNPG cluster")?;
    let administrator = format!("{cluster}-superuser");
    // Schema mutation needs the target administrator in addition to the code worker's credentials.
    // Derive shared credentials from the deployed code pod; no credential roster is pinned here.
    let mut common = BTreeSet::new();
    for pod in code["items"].as_array().context("code pods")? {
        let mut actual = BTreeSet::new();
        secret_names(&pod["spec"], &mut actual);
        ensure!(
            !actual.contains(administrator.as_str()),
            "deploy pod has administrator Secret"
        );
        for container in ["containers", "initContainers"]
            .into_iter()
            .flat_map(|field| pod["spec"][field].as_array().into_iter().flatten())
        {
            ensure!(
                !container["env"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|env| env["name"] == "WAMN_SYSTEM_ADMIN_URL"
                        || env["value"]
                            .as_str()
                            .is_some_and(|value| value.contains("/credentials/administrator"))),
                "deploy pod has administrator environment"
            );
        }
        common.extend(actual);
    }
    let mut expected = common;
    expected.insert(&administrator);
    for pod in schema["items"].as_array().context("schema pods")? {
        let mut actual = BTreeSet::new();
        secret_names(&pod["spec"], &mut actual);
        ensure!(
            actual == expected,
            "schema pod credentials differ from its operation needs"
        );
    }
    Ok(())
}

async fn worker(journey: &Journey<'_>, pods: &Value) -> anyhow::Result<()> {
    let selected = artifact_plan::select(&journey.run.chart)?;
    let selector = get(
        journey.run,
        "platform",
        "configmap",
        &wamn_registry::name::operation(&journey.run.document.org),
    )
    .await?;
    let inputs = selector["data"]["inputs"]
        .as_str()
        .context("worker input selection")?;
    let configuration = get(journey.run, "platform", "configmap", inputs).await?;
    ensure!(
        configuration["immutable"] == true,
        "worker inputs are mutable"
    );
    for file in ["Chart.yaml", "values.yaml"] {
        ensure!(
            configuration["data"][file].as_str()
                == Some(std::fs::read_to_string(journey.run.chart.join(file))?.as_str()),
            "worker chart differs from the published selection"
        );
    }
    for pod in pods["items"].as_array().context("worker pods")? {
        let spec = &pod["spec"];
        for volume in spec["volumes"].as_array().context("volumes")? {
            ensure!(
                volume.get("hostPath").is_none() && volume.get("persistentVolumeClaim").is_none(),
                "worker uses machine storage"
            );
        }
        ensure!(
            spec["volumes"]
                .as_array()
                .context("volumes")?
                .iter()
                .any(|volume| volume["configMap"]["name"] == inputs),
            "worker did not mount selected chart inputs"
        );
        let encoded = serde_json::to_string(spec)?;
        for path in [
            &journey.run.repository,
            &journey.run.checkout,
            &journey.run.chart,
        ] {
            ensure!(
                !encoded.contains(path.to_str().context("checkout path")?),
                "worker refers to submitting machine"
            );
        }
        let container = spec["containers"]
            .as_array()
            .context("containers")?
            .iter()
            .find(|container| container["name"] == "operation")
            .context("operation container")?;
        ensure!(
            container["image"] == selected.image("ctl")? && pod["status"]["phase"] == "Succeeded",
            "worker did not execute the published image"
        );
    }
    Ok(())
}

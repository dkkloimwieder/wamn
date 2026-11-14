//! DEP6 observes authenticated workers and selection isolation on one platform.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_catalog::HostRouteSet;
use wamn_environment::platform::{artifact_plan, document::Provider};

use super::{Journey, get, read_query, success};

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
        let endpoint = format!(
            "{}{}",
            self.url.trim_end_matches('/'),
            attachment.definition["route"]["path"]
                .as_str()
                .context("route path")?
        );
        let request = if attachment.route.type_.is_read() {
            self.client.get(format!(
                "{endpoint}?{}",
                read_query::encode_read_query(value.as_object().context("read request")?)
            ))
        } else {
            self.client
                .post(endpoint)
                .json(&json!([{"request_id":uuid::Uuid::new_v4().to_string(),"value":value}]))
        };
        let mut request = request
            .bearer_auth(
                session["access_token"]
                    .as_str()
                    .context("session access token")?,
            )
            .build()?;
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
            &self.journey.run.document.org,
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
    // Provision through the authenticated product route.
    journey
        .apply(
            "provision-environment",
            journey
                .command(&["env", "apply"])
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
    let before = journey.status().await?;
    let alpha_revision = &before["applications"]["alpha"]["observed"]["revision"]["number"];
    let deployed = control
        .operation(
            "application.deploy",
            json!({"application":"beta","release":artifacts["b1"]}),
        )
        .await?;
    let after = journey.status().await?;
    ensure!(
        after["applications"]["alpha"]["current"] == before["applications"]["alpha"]["current"]
            && after["applications"]["alpha"]["observed"]["revision"]["number"] == *alpha_revision,
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
    let rolled_back = journey.status().await?;
    ensure!(
        rolled_back["applications"]["beta"]["current"] == artifacts["b0"]
            && rolled_back["applications"]["alpha"]["current"]
                == before["applications"]["alpha"]["current"],
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
    let after_schema = journey.status().await?;
    for (application, selected) in rolled_back["applications"]
        .as_object()
        .context("applications")?
    {
        ensure!(
            after_schema["applications"][application]["current"] == selected["current"],
            "schema.apply changed a code selection"
        );
    }
    Ok(())
}

async fn jobs(control: &Control<'_>) -> anyhow::Result<BTreeSet<String>> {
    let objects = get(
        control.journey.run,
        &control.journey.run.document.org,
        "jobs",
        "",
    )
    .await?;
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
    let org = &journey.run.document.org;
    let target = get(
        journey.run,
        org,
        "configmap",
        &format!("{coordinate}-operations"),
    )
    .await?;
    let cluster = target["data"]["cluster"]
        .as_str()
        .context("target CNPG cluster")?;
    let administrator = format!("{cluster}-superuser");
    let administration = get(
        journey.run,
        org,
        "configmap",
        &wamn_registry::name::control_administration_secret_name(org),
    )
    .await?;
    let key =
        wamn_control_provision::control_administration_key(journey.project, journey.environment);
    let connection: wamn_postgres::ConnectionConfig = serde_json::from_str(
        administration["data"][key]
            .as_str()
            .context("public project connection")?,
    )?;
    let owner = wamn_control_provision::migrations::MigrationDatabase::Project
        .managed_role(&connection.database);
    let migration = wamn_control_provision::workload_secret_name(
        wamn_control_provision::WorkloadRoleFamily::Migration,
        org,
        journey.project,
        journey.environment,
    );
    let capture = wamn_control_provision::project_env_cdc_secret_name(
        org,
        journey.project,
        journey.environment,
    );
    let mut common = BTreeSet::new();
    for pod in code["items"].as_array().context("code pods")? {
        secret_names(&pod["spec"], &mut common);
    }
    ensure!(
        !common.contains(administrator.as_str())
            && !common.contains(owner.password_secret.name.as_str())
            && !common.contains(migration.as_str())
            && !common.contains(capture.as_str()),
        "code worker received SQL or capture authority"
    );
    let mut expected = common;
    expected.extend([
        owner.password_secret.name.as_str(),
        migration.as_str(),
        capture.as_str(),
    ]);
    for pod in schema["items"].as_array().context("schema pods")? {
        let mut actual = BTreeSet::new();
        secret_names(&pod["spec"], &mut actual);
        ensure!(
            !actual.contains(administrator.as_str()) && actual == expected,
            "schema worker credentials differ from the owning roles"
        );
    }
    Ok(())
}

async fn worker(journey: &Journey<'_>, pods: &Value) -> anyhow::Result<()> {
    let selected = artifact_plan::select(&journey.run.chart)?;
    for pod in pods["items"].as_array().context("worker pods")? {
        let spec = &pod["spec"];
        for volume in spec["volumes"].as_array().context("volumes")? {
            ensure!(
                volume.get("hostPath").is_none() && volume.get("persistentVolumeClaim").is_none(),
                "worker uses machine storage"
            );
        }
        let container = spec["containers"]
            .as_array()
            .context("containers")?
            .iter()
            .find(|container| container["name"] == "operation")
            .context("operation container")?;
        ensure!(
            container["image"] == selected.image("control")?
                && pod["status"]["phase"] == "Succeeded",
            "worker did not execute the published image"
        );
    }
    Ok(())
}

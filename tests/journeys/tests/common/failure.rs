//! Capture failed journeys before running their deferred environment deletions.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt::Display;
use std::io::{BufWriter, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use futures_util::io::AsyncBufReadExt as _;
use futures_util::{FutureExt as _, StreamExt as _, stream};
use k8s_openapi::api::core::v1::{Event, Namespace, Pod};
use kube::api::{DynamicObject, ListParams, LogParams};
use kube::{Api, ResourceExt as _};
use serde_json::{Value, json};
use wamn_identity_protocol::PAT_TOKEN_PREFIX;
use wamn_registry::{OrgId, Triple, name};

use super::{Capacity, Cluster, Org, PLATFORM, host, postgres_clusters};

// Storage namespaces named by wamn_kubernetes::helm::org_release_namespace.
const HELM_NAMESPACE_PREFIX: &str = "wamn-helm-";

tokio::task_local! {
    static CAPTURE: RefCell<Capture>;
}

#[derive(Default)]
struct Capture {
    orgs: BTreeSet<String>,
    envs: BTreeSet<String>,
    project_stems: BTreeSet<String>,
    deletes: Vec<(Command, String)>,
    capacity: Vec<Capacity>,
}

/// Run a journey and collect artifacts on either an error return or a panic.
pub async fn capture<F>(test: &str, journey: impl FnOnce() -> F) -> anyhow::Result<()>
where
    F: Future<Output = anyhow::Result<()>>,
{
    let journey = Box::pin(journey());
    let Some(results) = std::env::var_os("WAMN_RESULTS").filter(|path| !path.is_empty()) else {
        return journey.await;
    };
    let (outcome, capture) = CAPTURE
        .scope(RefCell::default(), async {
            let outcome = AssertUnwindSafe(journey).catch_unwind().await;
            (outcome, CAPTURE.with(RefCell::take))
        })
        .await;
    if !matches!(outcome, Ok(Ok(()))) {
        let save =
            AssertUnwindSafe(Box::pin(capture.save(Path::new(&results), test))).catch_unwind();
        match tokio::time::timeout(Duration::from_secs(60), save).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => note(error),
            Ok(Err(_)) => note("collection panicked"),
            Err(_) => note("collection exceeded 60 seconds"),
        }
    }
    for (command, coordinate) in capture.deletes {
        delete(command, &coordinate);
    }
    match outcome {
        Ok(result) => result,
        Err(payload) => resume_unwind(payload),
    }
}

pub(super) fn hold_capacity(capacity: Capacity) -> Option<Capacity> {
    let mut capacity = Some(capacity);
    let _ = CAPTURE.try_with(|capture| capture.borrow_mut().capacity.extend(capacity.take()));
    capacity
}

pub(super) fn org(org: &OrgId) {
    let _ = CAPTURE.try_with(|capture| {
        capture.borrow_mut().orgs.insert(org.to_string());
    });
}

pub(super) fn environment(triple: &Triple) {
    let _ = CAPTURE.try_with(|capture| {
        let mut capture = capture.borrow_mut();
        capture
            .envs
            .insert(name::environment_key(&triple.org, &triple.env));
        capture
            .project_stems
            .insert(name::project_env_namespace_stem(
                &triple.org,
                &triple.project,
                &triple.env,
            ));
    });
}

pub(super) fn delete_environment(org: &Org, triple: &Triple) {
    let mut command = org.command();
    let coordinate = triple.to_string();
    command.args(["env", "delete", &coordinate]);
    let mut pending = Some((command, coordinate));
    let _ = CAPTURE.try_with(|capture| capture.borrow_mut().deletes.extend(pending.take()));
    if let Some((command, coordinate)) = pending {
        delete(command, &coordinate);
    }
}

fn delete(mut command: Command, coordinate: &str) {
    match command.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => eprintln!(
            "wamn env delete {coordinate} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => eprintln!("wamn env delete {coordinate}: {error}"),
    }
}

fn note(action: impl Display) {
    eprintln!("journey evidence incomplete: {action}");
}

impl Capture {
    async fn save(&self, results: &Path, test: &str) -> anyhow::Result<()> {
        let root = results.join(format!("{test}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .with_context(|| format!("create {}", root.display()))?;
        eprintln!("journey evidence: {}", root.display());
        let cluster = Cluster::load()
            .await
            .map_err(|_| anyhow::anyhow!("load the journey cluster inputs"))?;

        // Current environment, project and application charts deploy in the org namespace.
        // Also capture any existing coordinate-named namespaces.
        let mut namespaces: BTreeSet<_> = self.orgs.iter().cloned().collect();
        namespaces.insert(PLATFORM.to_owned());
        let mut release_namespaces: BTreeSet<_> = self
            .orgs
            .iter()
            .map(|org| format!("{HELM_NAMESPACE_PREFIX}{org}"))
            .collect();
        match Api::<Namespace>::all(cluster.client.clone())
            .list_metadata(&ListParams::default())
            .await
        {
            Ok(list) => namespaces.extend(list.items.into_iter().filter_map(|namespace| {
                let name = namespace.metadata.name?;
                if name
                    .strip_prefix(HELM_NAMESPACE_PREFIX)
                    .is_some_and(|env| self.envs.contains(env))
                {
                    release_namespaces.insert(name);
                    return None;
                }
                let project = self.project_stems.iter().any(|stem| {
                    name == *stem
                        || name
                            .strip_prefix(stem)
                            .and_then(|suffix| suffix.strip_prefix(name::INSTANCE_SUFFIX_SEPARATOR))
                            .is_some_and(|suffix| {
                                suffix
                                    .parse::<wamn_registry::instance_suffix::InstanceSuffix>()
                                    .is_ok()
                            })
                });
                (project || self.envs.contains(&name)).then_some(name)
            })),
            Err(_) => note("list namespaces"),
        }
        let collect_namespaces = stream::iter(&namespaces)
            .for_each_concurrent(3, |namespace| capture_namespace(&cluster, &root, namespace));
        let collect_releases = stream::iter(&release_namespaces)
            .for_each_concurrent(2, |namespace| collect_releases(&cluster, &root, namespace));
        tokio::join!(collect_namespaces, collect_releases);
        Ok(())
    }
}

async fn capture_namespace(cluster: &Cluster, root: &Path, namespace: &str) {
    let client = &cluster.client;
    let directory = root.join(namespace);
    if std::fs::create_dir(&directory).is_err() {
        note(format_args!("create {}", directory.display()));
        return;
    }

    let pods = Api::<Pod>::namespaced(client.clone(), namespace);
    let events = Api::<Event>::namespaced(client.clone(), namespace);
    let password = &cluster.outputs.observer_password;
    tokio::join!(
        capture_logs(&pods, &directory, password),
        capture_events(&events, &directory, password),
        async {
            for (kind, plural) in [
                ("WorkloadDeployment", "workloaddeployments"),
                ("WorkloadReplicaSet", "workloadreplicasets"),
            ] {
                capture_status(
                    &host::runtime(client, namespace, kind, plural),
                    &directory.join(format!("{plural}.json")),
                    password,
                )
                .await;
            }
        },
        async {
            capture_status(
                &postgres_clusters(client, namespace),
                &directory.join("clusters.json"),
                password,
            )
            .await;
        },
    );
}

async fn capture_logs(api: &Api<Pod>, directory: &Path, password: &str) {
    let Ok(list) = api.list(&ListParams::default()).await else {
        note(format_args!("list pods in {}", directory.display()));
        return;
    };
    stream::iter(list.items)
        .for_each_concurrent(8, |pod| async move {
            let name = pod.name_any();
            let directory = directory.join("pods").join(&name);
            if std::fs::create_dir_all(&directory).is_err() {
                note(format_args!("create {}", directory.display()));
                return;
            }
            let Some(spec) = pod.spec else {
                return;
            };
            let containers = spec
                .containers
                .iter()
                .map(|container| &container.name)
                .chain(
                    spec.init_containers
                        .iter()
                        .flatten()
                        .map(|container| &container.name),
                )
                .chain(
                    spec.ephemeral_containers
                        .iter()
                        .flatten()
                        .map(|container| &container.name),
                );
            let restarted: BTreeSet<_> = pod
                .status
                .iter()
                .flat_map(|status| {
                    status
                        .container_statuses
                        .iter()
                        .flatten()
                        .chain(status.init_container_statuses.iter().flatten())
                        .chain(status.ephemeral_container_statuses.iter().flatten())
                })
                .filter(|status| status.restart_count > 0)
                .map(|status| &status.name)
                .collect();
            for container in containers {
                for previous in [false, true] {
                    if previous && !restarted.contains(container) {
                        continue;
                    }
                    let path = directory.join(if previous {
                        format!("{container}.previous.log")
                    } else {
                        format!("{container}.log")
                    });
                    let params = LogParams {
                        container: Some(container.clone()),
                        previous,
                        timestamps: true,
                        ..LogParams::default()
                    };
                    if save_logs(api, &name, &params, &path, password)
                        .await
                        .is_err()
                    {
                        note(format_args!("save logs to {}", path.display()));
                    }
                }
            }
        })
        .await;
}

async fn save_logs(
    api: &Api<Pod>,
    pod: &str,
    params: &LogParams,
    path: &Path,
    password: &str,
) -> anyhow::Result<()> {
    let mut logs = std::pin::pin!(api.log_stream(pod, params).await?);
    let mut file = BufWriter::new(std::fs::File::create(path)?);
    let mut line = String::new();
    let mut pem = false;
    while logs.read_line(&mut line).await? != 0 {
        file.write_all(redact_line(&line, password, &mut pem).as_bytes())?;
        line.clear();
    }
    Ok(file.flush()?)
}

async fn capture_events(api: &Api<Event>, directory: &Path, password: &str) {
    let Ok(list) = api.list(&ListParams::default()).await else {
        note(format_args!("list events in {}", directory.display()));
        return;
    };
    let events: Vec<_> = list
        .items
        .into_iter()
        .map(|event| {
            json!({
                "name": event.metadata.name,
                "object": {"kind": event.involved_object.kind, "name": event.involved_object.name},
                "type": event.type_,
                "reason": event.reason,
                "message": event.message.map(|message| redact(&message, password)),
                "count": event.count,
                "start": event.first_timestamp,
                "end": event.last_timestamp,
                "eventTime": event.event_time,
                "series": event.series,
            })
        })
        .collect();
    write_json(&directory.join("events.json"), &events);
}

async fn capture_status(api: &Api<DynamicObject>, path: &Path, password: &str) {
    let Ok(list) = api.list(&ListParams::default()).await else {
        note(format_args!("list resources for {}", path.display()));
        return;
    };
    let statuses: Vec<_> = list
        .items
        .into_iter()
        .map(|mut object| {
            let mut status = object.data["status"].take();
            scrub_status(&mut status, password);
            json!({"name": object.metadata.name, "status": status})
        })
        .collect();
    write_json(path, &statuses);
}

fn scrub_status(status: &mut Value, password: &str) {
    match status {
        Value::Object(fields) => {
            fields.retain(|key, _| !credential_marker(key));
            for value in fields.values_mut() {
                scrub_status(value, password);
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub_status(item, password);
            }
        }
        Value::String(value) => *value = redact(value, password),
        _ => {}
    }
}

fn credential_marker(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "authorization",
        "bearer ",
        "token",
        "credential",
        "invitation code",
        "private key",
        "secret",
        PAT_TOKEN_PREFIX,
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || text.contains("eyJ")
        || (text.contains("://") && text.contains('@'))
}

fn redact(text: &str, password: &str) -> String {
    let mut pem = false;
    text.split_inclusive('\n')
        .map(|line| redact_line(line, password, &mut pem))
        .collect()
}

fn redact_line<'a>(line: &'a str, password: &str, pem: &mut bool) -> &'a str {
    *pem |= line.contains("-----BEGIN ");
    let redact =
        *pem || credential_marker(line) || (!password.is_empty() && line.contains(password));
    *pem &= !line.contains("-----END ");
    match (redact, line.ends_with('\n')) {
        (true, true) => "[redacted]\n",
        (true, false) => "[redacted]",
        (false, _) => line,
    }
}

async fn collect_releases(cluster: &Cluster, root: &Path, namespace: &str) {
    let Ok(Value::Array(releases)) = helm(cluster, namespace, &["list"]).await else {
        note(format_args!("helm list in {namespace}"));
        return;
    };
    for release in releases {
        let Some(name) = release["name"]
            .as_str()
            .filter(|name| name::valid_resource_segment(name))
        else {
            note(format_args!("read Helm release name in {namespace}"));
            continue;
        };
        let path = root.join("helm").join(namespace).join(name);
        if std::fs::create_dir_all(&path).is_err() {
            note(format_args!("create {}", path.display()));
            continue;
        }
        let history_args = ["history", name];
        let status_args = ["status", name];
        let (history, status) = tokio::join!(
            helm(cluster, namespace, &history_args),
            helm(cluster, namespace, &status_args),
        );
        match history {
            Ok(Value::Array(rows)) => {
                let revisions: Vec<_> = rows.into_iter().map(|row| json!({
                    "revision": row["revision"], "updated": row["updated"], "status": row["status"],
                    "chart": row["chart"], "app_version": row["app_version"],
                })).collect();
                write_json(&path.join("history.json"), &revisions);
            }
            _ => note(format_args!("helm history for {namespace}/{name}")),
        }
        match status {
            Ok(Value::Object(status)) => write_json(
                &path.join("status.json"),
                &json!({
                    "name": name, "namespace": namespace, "version": status["version"],
                    "status": status["info"]["status"], "first_deployed": status["info"]["first_deployed"],
                    "last_deployed": status["info"]["last_deployed"],
                }),
            ),
            _ => note(format_args!("helm status for {namespace}/{name}")),
        }
    }
}

async fn helm(cluster: &Cluster, namespace: &str, args: &[&str]) -> anyhow::Result<Value> {
    let output = tokio::process::Command::new("helm")
        .args(["--kubeconfig"])
        .arg(&cluster.outputs.kubeconfig)
        .args([
            "--kube-context",
            &cluster.outputs.context,
            "--namespace",
            namespace,
        ])
        .args(args)
        .args(["--output", "json"])
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(output.status.success(), "Helm exited {}", output.status);
    // The raw Helm status document can contain release values and manifests.
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn write_json(path: &Path, value: &impl serde::Serialize) {
    let written = serde_json::to_vec_pretty(value)
        .context("encode JSON")
        .and_then(|json| std::fs::write(path, json).context("write JSON"));
    if written.is_err() {
        note(format_args!("write {}", path.display()));
    }
}

//! C10 checks rollout order and the state at each successful product command.
//!
//! The platform is already installed here. Its startup waits are covered by
//! `xtask/src/journey_cluster.rs::check_startup`.

mod common;

use std::time::Duration;

use anyhow::{Context as _, ensure};
use common::{Environment, Org, active_project_cluster, host, request};
use futures_util::TryStreamExt as _;
use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet},
    core::v1::Pod,
};
use kube::api::{
    ApiResource, DynamicObject, GroupVersionKind, ListParams, WatchEvent, WatchParams,
};
use kube::{Api, ResourceExt as _, core::Selector};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use wamn_registry::name;

const APPLICATION: &str = "client_acme_receiving";
const COMMAND: Duration = Duration::from_mins(20);
const OBSERVE: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: cluster, bin:helm"]
async fn commands_wait_for_their_resources() -> anyhow::Result<()> {
    common::capture(
        "commands_wait_for_their_resources",
        commands_wait_for_their_resources_case,
    )
    .await
}

async fn commands_wait_for_their_resources_case() -> anyhow::Result<()> {
    let source = request::fixture(APPLICATION);
    ensure!(source.is_dir(), "missing fixture {}", source.display());
    let org = timeout(COMMAND, Org::create())
        .await
        .context("create the waits org")??;
    timeout(OBSERVE, check_org(&org))
        .await
        .context("read the control rollout after org create")??;
    let mut watches = timeout(OBSERVE, Watches::start(&org))
        .await
        .context("start the rollout watches")??;

    let environment = timeout(COMMAND, org.apply(APPLICATION))
        .await
        .context("initial env apply")??;
    watches.check(&environment, Stage::Apply).await?;

    timeout(COMMAND, environment.deploy(&source, APPLICATION))
        .await
        .context("deploy the capture fixture")??;
    watches.check(&environment, Stage::Deploy).await?;

    timeout(COMMAND, host::reapply(&environment, |_| {}))
        .await
        .context("reapply the environment")??;
    watches.check(&environment, Stage::Reapply).await
}

async fn check_org(org: &Org) -> anyhow::Result<()> {
    const REVISION: &str = "deployment.kubernetes.io/revision";

    let client = &org.cluster.client;
    let namespace = org.name.as_str();
    let name = name::control_resource(&org.name);
    let mut deployment = Api::<Deployment>::namespaced(client.clone(), namespace)
        .get(&name)
        .await
        .with_context(|| format!("read Deployment {namespace}/{name}"))?;
    let spec = deployment
        .spec
        .take()
        .context("control Deployment has no spec")?;
    let uid = deployment
        .metadata
        .uid
        .as_deref()
        .context("control Deployment has no UID")?;
    let revision = deployment
        .annotations()
        .get(REVISION)
        .context("control Deployment has no revision")?;
    ensure!(
        spec.template
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.annotations.as_ref())
            .and_then(|annotations| annotations.get("wamn.dev/control-credential-job"))
            .is_some_and(|job| !job.is_empty()),
        "control Deployment {name} has no credential Job annotation on its template"
    );
    let desired = spec
        .replicas
        .context("control Deployment has no replica count")?;
    let status = deployment
        .status
        .as_ref()
        .context("control Deployment has no status")?;
    ensure!(
        deployment
            .metadata
            .generation
            .zip(status.observed_generation)
            .is_some_and(|(generation, observed)| observed >= generation)
            && status.updated_replicas == Some(desired)
            && status.replicas == Some(desired)
            && status.available_replicas == Some(desired),
        "control Deployment {name} had an incomplete rollout after org create: {status:?}"
    );

    let selector =
        Selector::try_from(spec.selector).context("read the control Deployment selector")?;
    let params = ListParams::default().labels_from(&selector);
    let replicas = Api::<ReplicaSet>::namespaced(client.clone(), namespace);
    let pods = Api::<Pod>::namespaced(client.clone(), namespace);
    let (replicas, pods) = tokio::try_join!(replicas.list(&params), pods.list(&params))
        .context("list control ReplicaSets and Pods")?;
    let current = replicas
        .items
        .iter()
        .find(|replica| {
            replica.annotations().get(REVISION) == Some(revision)
                && replica
                    .owner_references()
                    .iter()
                    .any(|owner| owner.controller == Some(true) && owner.uid == uid)
        })
        .with_context(|| {
            format!("control Deployment {name} has no ReplicaSet for revision {revision}")
        })?;
    let current_uid = current
        .metadata
        .uid
        .as_deref()
        .context("current control ReplicaSet has no UID")?;
    ensure!(
        pods.items.len() == usize::try_from(desired)?,
        "control Deployment {name} has {} Pods after org create, expected {desired}",
        pods.items.len()
    );
    for pod in &pods.items {
        ensure!(
            pod.owner_references()
                .iter()
                .any(|owner| owner.controller == Some(true) && owner.uid == current_uid),
            "control Pod {} does not belong to the current ReplicaSet",
            pod.name_any()
        );
        ensure!(
            pod.metadata.deletion_timestamp.is_none()
                && pod
                    .status
                    .as_ref()
                    .and_then(|status| status.conditions.as_ref())
                    .is_some_and(|conditions| conditions
                        .iter()
                        .any(|condition| condition.type_ == "Ready" && condition.status == "True")),
            "control Pod {} is not Ready after org create",
            pod.name_any()
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Apply,
    Deploy,
    Reapply,
}

struct Watches {
    clusters: Watch,
    databases: Watch,
    certificates: Watch,
    maps: Watch,
    deployments: Watch,
    workloads: Watch,
    replicas: Watch,
}

impl Watches {
    async fn start(org: &Org) -> anyhow::Result<Self> {
        let client = &org.cluster.client;
        let namespace = org.name.as_str();
        let api = |group, version, resource, plural| {
            let resource = ApiResource::from_gvk_with_plural(
                &GroupVersionKind::gvk(group, version, resource),
                plural,
            );
            Api::namespaced_with(client.clone(), namespace, &resource)
        };
        let (clusters, databases, certificates, maps, deployments, workloads, replicas) = tokio::try_join!(
            Watch::start(api("postgresql.cnpg.io", "v1", "Cluster", "clusters")),
            Watch::start(api("postgresql.cnpg.io", "v1", "Database", "databases")),
            Watch::start(api("cert-manager.io", "v1", "Certificate", "certificates")),
            Watch::start(api("", "v1", "ConfigMap", "configmaps")),
            Watch::start(api("apps", "v1", "Deployment", "deployments")),
            Watch::start(host::runtime(
                client,
                namespace,
                "WorkloadDeployment",
                "workloaddeployments"
            )),
            Watch::start(host::runtime(
                client,
                namespace,
                "WorkloadReplicaSet",
                "workloadreplicasets"
            )),
        )?;
        Ok(Self {
            clusters,
            databases,
            certificates,
            maps,
            deployments,
            workloads,
            replicas,
        })
    }

    async fn check(&mut self, environment: &Environment, stage: Stage) -> anyhow::Result<()> {
        let returned = Instant::now();
        timeout(OBSERVE, self.check_project(environment, stage, returned))
            .await
            .with_context(|| format!("{stage:?} end-state reads"))?
            .with_context(|| format!("{stage:?} rollout"))
    }

    async fn check_project(
        &mut self,
        environment: &Environment,
        stage: Stage,
        returned: Instant,
    ) -> anyhow::Result<()> {
        for watch in [
            &mut self.clusters,
            &mut self.databases,
            &mut self.certificates,
            &mut self.maps,
            &mut self.deployments,
            &mut self.workloads,
            &mut self.replicas,
        ] {
            watch.drain()?;
        }
        let org = &environment.org;
        let triple = &environment.triple;
        let cluster_name = active_project_cluster(&org.cluster, triple).await?;
        let cluster = self.clusters.get(&cluster_name).await?;
        let database_name = cluster.data["spec"]["bootstrap"]["initdb"]["database"]
            .as_str()
            .context("the project Cluster does not name its database")?;
        let ca_name = name::project_server_ca_copy(&cluster_name);
        let host_name =
            name::host_deployment_name(&name::environment_host_name(&triple.org, &triple.env));
        let (database, ca, host) = tokio::try_join!(
            self.databases.get(database_name),
            self.maps.get(&ca_name),
            self.deployments.get(&host_name),
        )?;
        let owns_ca = |copy: &DynamicObject| {
            copy.metadata
                .owner_references
                .iter()
                .flatten()
                .any(|owner| {
                    owner.api_version == "postgresql.cnpg.io/v1"
                        && owner.name == cluster_name
                        && cluster.metadata.uid.as_deref() == Some(&owner.uid)
                })
        };
        ensure!(
            owns_ca(&ca)
                && ca.data["data"]["ca.crt"]
                    .as_str()
                    .is_some_and(|pem| !pem.is_empty()),
            "{ca_name} must hold ca.crt and belong to {cluster_name}"
        );

        let fresh = stage == Stage::Apply;
        self.clusters.check(&cluster, returned, fresh, ready)?;
        self.databases
            .check(&database, returned, fresh, |database| {
                database.data["status"]["applied"] == true && observed_generation(database)
            })?;
        self.deployments.check(&host, returned, fresh, host_ready)?;
        if fresh {
            let ready_change = self
                .clusters
                .history
                .iter()
                .find(|change| {
                    change.matches(&cluster, returned)
                        && change.action != Action::Deleted
                        && ready(&change.object)
                })
                .context("missing the project Cluster Ready watch event")?;
            let created = self.maps.added(&ca, returned)?;
            ensure!(
                owns_ca(&created.object),
                "the CA copy initially had a different owner"
            );
            // The local cluster uses one etcd. Its revisions order writes across both watches.
            let ready_revision = ready_change.revision()?;
            let copy_revision = created.revision()?;
            ensure!(
                ready_revision < copy_revision,
                "{cluster_name} became Ready at revision {ready_revision}, CA copy {ca_name} was created at {copy_revision}"
            );
        }

        let migration = database.data["spec"]["owner"]
            .as_str()
            .context("project Database owner")?;
        let capture = cluster.data["spec"]["managed"]["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|role| role["replication"] == true)
            .and_then(|role| role["name"].as_str())
            .context("project Cluster capture login")?;
        for login in [migration, capture] {
            let cert = self
                .certificates
                .get(&name::project_login_certificate(database_name, login))
                .await?;
            self.certificates.check(&cert, returned, fresh, ready)?;
        }

        if !fresh {
            let reader_name = name::cdc_reader_name(&triple.org, &triple.project, &triple.env);
            let reader = self.deployments.get(&reader_name).await?;
            let deploying = stage == Stage::Deploy;
            // Epic 16 Open item: the reader has no readiness probe. Availability shows the pod runs.
            self.deployments
                .check(&reader, returned, deploying, deployment_available)?;
            self.check_workloads(returned, deploying).await?;
        }
        Ok(())
    }

    async fn check_workloads(&self, returned: Instant, fresh: bool) -> anyhow::Result<()> {
        let workloads = self.workloads.api.list(&ListParams::default()).await?;
        let mut application = workloads
            .items
            .iter()
            .filter(|object| {
                object
                    .annotations()
                    .get("wamn.application")
                    .is_some_and(|name| name == APPLICATION)
            })
            .peekable();
        ensure!(
            application.peek().is_some(),
            "{APPLICATION} has no WorkloadDeployment"
        );
        for workload in application {
            let current = current_replica_set(workload).with_context(|| {
                format!("{} has no current WorkloadReplicaSet", workload.name_any())
            })?;
            self.workloads.check(workload, returned, fresh, |object| {
                ready(object) && current_replica_set(object) == Some(current)
            })?;
            let replicas = self.replicas.get(current).await?;
            // The replica set exposes the generation that reported Ready.
            self.replicas.check(&replicas, returned, fresh, |object| {
                ready_condition(object).is_some_and(|condition| {
                    object.metadata.generation.is_some_and(|generation| {
                        condition["observedGeneration"].as_i64() == Some(generation)
                    })
                })
            })?;
        }
        Ok(())
    }
}

fn ready_condition(object: &DynamicObject) -> Option<&serde_json::Value> {
    object.data["status"]["conditions"]
        .as_array()?
        .iter()
        .find(|condition| condition["type"] == "Ready" && condition["status"] == "True")
}

fn ready(object: &DynamicObject) -> bool {
    ready_condition(object).is_some()
}

fn current_replica_set(workload: &DynamicObject) -> Option<&str> {
    workload.data["status"]["currentReplicaSet"]["name"]
        .as_str()
        .filter(|name| !name.is_empty())
}

fn observed_generation(object: &DynamicObject) -> bool {
    object.metadata.generation.is_some_and(|generation| {
        object.data["status"]["observedGeneration"].as_i64() == Some(generation)
    })
}

fn deployment_available(object: &DynamicObject) -> bool {
    observed_generation(object)
        && object.data["status"]["availableReplicas"]
            .as_i64()
            .is_some_and(|count| count > 0)
        && host::condition(object, "Available") == Some("True")
}

fn host_ready(object: &DynamicObject) -> bool {
    let desired = object.data["spec"]["replicas"].as_i64().unwrap_or(1);
    let status = &object.data["status"];
    deployment_available(object)
        && status["updatedReplicas"].as_i64() == Some(desired)
        && status["readyReplicas"].as_i64() == Some(desired)
        && status["availableReplicas"].as_i64() == Some(desired)
        && status["replicas"].as_i64() == Some(desired)
}

struct Watch {
    api: Api<DynamicObject>,
    stream: JoinHandle<()>,
    changes: mpsc::UnboundedReceiver<anyhow::Result<Change>>,
    history: Vec<Change>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Listed,
    Added,
    Modified,
    Deleted,
}

struct Change {
    time: Instant,
    action: Action,
    object: DynamicObject,
}

impl Change {
    fn record(action: Action, mut object: DynamicObject) -> Self {
        object.metadata.managed_fields = None;
        object.data = serde_json::json!({
            "status": object.data["status"].take(),
            "spec": {"replicas": object.data["spec"]["replicas"].take()},
        });
        Self {
            time: Instant::now(),
            action,
            object,
        }
    }

    fn matches(&self, object: &DynamicObject, returned: Instant) -> bool {
        self.time <= returned && self.object.metadata.uid == object.metadata.uid
    }

    fn revision(&self) -> anyhow::Result<u64> {
        self.object
            .resource_version()
            .context("watch event resource version")?
            .parse()
            .context("local cluster resource version is not an etcd revision")
    }
}

impl Watch {
    async fn start(api: Api<DynamicObject>) -> anyhow::Result<Self> {
        let path = api.resource_url().to_owned();
        let list = api
            .list(&ListParams::default())
            .await
            .with_context(|| format!("list {path}"))?;
        let mut revision = list
            .metadata
            .resource_version
            .context("list resource version")?;
        let params = WatchParams::default().timeout(120);
        let mut events = api
            .watch(&params, &revision)
            .await
            .with_context(|| format!("watch {path}"))?;
        let history = list
            .items
            .into_iter()
            .map(|object| Change::record(Action::Listed, object))
            .collect();
        let (send, changes) = mpsc::unbounded_channel();
        let stream = tokio::spawn({
            let api = api.clone();
            async move {
                let result: anyhow::Result<()> = async {
                    loop {
                        let mut current = std::pin::pin!(events);
                        while let Some(event) = current.try_next().await? {
                            let (action, object) = match event {
                                WatchEvent::Added(object) => (Action::Added, object),
                                WatchEvent::Modified(object) => (Action::Modified, object),
                                WatchEvent::Deleted(object) => (Action::Deleted, object),
                                WatchEvent::Bookmark(mark) => {
                                    revision = mark.metadata.resource_version;
                                    continue;
                                }
                                WatchEvent::Error(error) => {
                                    return Err(kube::Error::Api(error).into());
                                }
                            };
                            revision = object
                                .resource_version()
                                .context("watch event resource version")?;
                            send.send(Ok(Change::record(action, object)))
                                .context("rollout observer closed")?;
                        }
                        events = api.watch(&params, &revision).await?;
                    }
                }
                .await;
                if let Err(error) = result {
                    let _ = send.send(Err(error.context(format!("watch {path}"))));
                }
            }
        });
        Ok(Self {
            api,
            stream,
            changes,
            history,
        })
    }

    async fn get(&self, name: &str) -> anyhow::Result<DynamicObject> {
        self.api
            .get(name)
            .await
            .with_context(|| format!("read {}/{name}", self.api.resource_url()))
    }

    fn drain(&mut self) -> anyhow::Result<()> {
        while let Ok(change) = self.changes.try_recv() {
            self.history.push(change?);
        }
        ensure!(
            !self.changes.is_closed(),
            "watch {} closed",
            self.api.resource_url()
        );
        Ok(())
    }

    fn added(&self, object: &DynamicObject, returned: Instant) -> anyhow::Result<&Change> {
        self.history
            .iter()
            .find(|change| change.action == Action::Added && change.matches(object, returned))
            .with_context(|| {
                format!(
                    "{}/{} was not observed being created before the command returned",
                    self.api.resource_url(),
                    object.name_any()
                )
            })
    }

    fn check(
        &self,
        object: &DynamicObject,
        returned: Instant,
        fresh: bool,
        is_ready: impl Fn(&DynamicObject) -> bool,
    ) -> anyhow::Result<()> {
        let resource = || format!("{}/{}", self.api.resource_url(), object.name_any());
        let uid = object
            .metadata
            .uid
            .as_deref()
            .with_context(|| format!("{} has no UID", resource()))?;
        let last = self
            .history
            .iter()
            .rev()
            .find(|change| change.matches(object, returned))
            .with_context(|| {
                format!(
                    "{} ({uid}) had no watch event before command return",
                    resource()
                )
            })?;
        ensure!(
            last.action != Action::Deleted
                && last.object.metadata.generation == object.metadata.generation
                && is_ready(&last.object),
            "{} was not ready when the command returned: {}",
            resource(),
            last.object.data["status"]
        );
        ensure!(
            is_ready(object),
            "{} was not ready in the end-state read: {}",
            resource(),
            object.data["status"]
        );
        if fresh {
            ensure!(
                !is_ready(&self.added(object, returned)?.object),
                "{} first appeared already ready",
                resource()
            );
        }
        Ok(())
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stream.abort();
    }
}

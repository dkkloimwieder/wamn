//! Analysis: §10.1 steps 2 and 3 of docs/plan/platform-deploy.md.
//!
//! Analysis compiles nothing new and writes nothing. It reads each authority
//! once (§5): the policy and the environment row in the system database, the
//! release manifest from OCI, the image set from the release chart, the
//! qualification, the installed state in the project database, the newest
//! successful Helm revision and the live set. It then checks the document
//! against them and returns the plan, or the refusals.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use tokio::process::Command;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::ServingManifest;
use wamn_runtime::release_manifest_source::ReleaseManifestSource;

use super::Platform;
use super::document::{DeclaredRelease, EnvironmentDocument};
use crate::delivery::selection::{image_digest, package_set};
use crate::release_chart;

/// The policy facts `apply` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub name: String,
    pub readiness_budget_seconds: i32,
    pub drain_bound_seconds: i32,
}

/// The environment row of the system database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentRow {
    pub instance_suffix: String,
    pub route_host: Option<String>,
    pub policy_name: String,
}

/// One installed package in the project database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Installed {
    /// The installed version: the registered version that no other succeeds.
    pub current: String,
    /// Every registered version to its predecessor version.
    pub lineage: BTreeMap<String, Option<String>>,
    /// The registered versions with stage evidence
    /// (`catalog.package_upgrade_qualifications`).
    pub evidence: BTreeSet<String>,
}

/// One connection instance in the project database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub requirement_type: String,
    pub enabled: bool,
    pub definition: Option<Value>,
}

/// What the project database holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectState {
    pub tenant: String,
    pub installed: BTreeMap<String, Installed>,
    pub instances: BTreeMap<String, Instance>,
    /// `(manifest_digest, component_digest, store_alias, instance_id)`.
    pub bindings: BTreeSet<(String, String, String, String)>,
    /// Package id to recorded floor version.
    pub floors: BTreeMap<String, String>,
}

/// The release the document names, as OCI and the chart hold it.
#[derive(Debug, Clone)]
pub struct ReleaseFacts {
    pub digest: String,
    pub manifest: ServingManifest,
    pub image_set: release_chart::ImageSet,
    pub qualification: String,
    /// The store aliases the release's components require.
    pub required_connections: BTreeSet<String>,
}

/// The newest successful revision of the release chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Revision {
    pub number: u32,
    pub chart: String,
    pub values: Value,
}

impl Revision {
    /// The release digest the revision installed.
    pub fn manifest_digest(&self) -> Option<&str> {
        self.values["release"]["manifestDigest"].as_str()
    }
}

/// Everything analysis read.
#[derive(Debug, Clone)]
pub struct Authorities {
    pub release_name: String,
    pub policy: Policy,
    pub row: Option<EnvironmentRow>,
    pub project: Option<ProjectState>,
    pub release: Option<ReleaseFacts>,
    pub revision: Option<Revision>,
    /// Release digest to the count of host pods that are not terminated.
    pub live: BTreeMap<String, usize>,
}

/// The result of analysis: the authorities and the differences to apply.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub authorities: Authorities,
    pub actor: String,
    pub plan: Vec<String>,
}

/// Read every authority and check the document against it (§10.1 steps 2 and 3).
///
/// # Errors
///
/// When an authority cannot be read, or when the document is refused. A
/// refusal names each reason.
pub async fn analyse(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<Analysis> {
    let release_name =
        release_chart::release_name(&document.org, &document.project, &document.env)?;
    let system = connect(&platform.system_database_url).await?;
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let policy = read_policy(&system, &document.org, &document.policy).await?;
    let row = read_row(&system, document).await?;
    let project = match &row {
        Some(row) => read_project(platform, document, row).await?,
        None => None,
    };
    let release = match &document.release {
        DeclaredRelease::None => None,
        DeclaredRelease::Digest(declared) => {
            let tenant = wamn_control_provision::project_env_tenant(
                &document.org,
                &document.project,
                &document.env,
            );
            Some(read_release(platform, &system, declared, &tenant).await?)
        }
    };
    let revision = read_revision(platform, &release_name).await?;
    let live = read_live_set(platform, &release_name).await?;
    let authorities = Authorities {
        release_name,
        policy,
        row,
        project,
        release,
        revision,
        live,
    };
    let actor = actor();
    let refusals = refusals(document, &authorities);
    if !refusals.is_empty() {
        bail!("refused:\n- {}", refusals.join("\n- "));
    }
    let plan = plan(document, &authorities);
    Ok(Analysis {
        authorities,
        actor,
        plan,
    })
}

/// The actor: the identity principal the CLI authenticates as, or `$USER`
/// when it has none (owner ruling 2026-10-09). `wamn-ctl` has none today.
fn actor() -> String {
    std::env::var("USER").unwrap_or_else(|_| "unknown".to_owned())
}

pub(crate) async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to the database")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

pub(crate) async fn read_policy(system: &Client, org: &str, name: &str) -> anyhow::Result<Policy> {
    let row = system
        .query_opt(
            "SELECT readiness_budget_seconds, drain_bound_seconds \
               FROM registry.env_policies WHERE org = $1 AND name = $2",
            &[&org, &name],
        )
        .await
        .context("read the environment policy; apply system migration 0017 with upgrade-schema")?
        .with_context(|| format!("refused: org {org} has no environment policy {name}"))?;
    Ok(Policy {
        name: name.to_owned(),
        readiness_budget_seconds: row.get(0),
        drain_bound_seconds: row.get(1),
    })
}

pub(crate) async fn read_row(
    system: &Client,
    document: &EnvironmentDocument,
) -> anyhow::Result<Option<EnvironmentRow>> {
    Ok(system
        .query_opt(
            "SELECT instance_suffix, route_host, policy_name FROM registry.project_envs \
              WHERE org = $1 AND project = $2 AND env = $3",
            &[&document.org, &document.project, &document.env],
        )
        .await
        .context("read the environment row")?
        .map(|row| EnvironmentRow {
            instance_suffix: row.get(0),
            route_host: row.get(1),
            policy_name: row.get(2),
        }))
}

/// The project database URL: the system URL with the environment's database.
pub(crate) fn project_url(
    platform: &Platform,
    document: &EnvironmentDocument,
    row: &EnvironmentRow,
) -> anyhow::Result<String> {
    let mut url = url::Url::parse(&platform.system_database_url).context("parse the system URL")?;
    url.set_path(&format!(
        "/{}",
        wamn_control_provision::project_env_database_name(
            &document.org,
            &document.project,
            &document.env,
            &row.instance_suffix
        )
    ));
    Ok(url.to_string())
}

pub(crate) async fn read_project(
    platform: &Platform,
    document: &EnvironmentDocument,
    row: &EnvironmentRow,
) -> anyhow::Result<Option<ProjectState>> {
    let Ok(mut client) = connect(&project_url(platform, document, row)?).await else {
        return Ok(None);
    };
    let tenant =
        wamn_control_provision::project_env_tenant(&document.org, &document.project, &document.env);
    let transaction = client
        .transaction()
        .await
        .context("begin the project read")?;
    transaction
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&tenant])
        .await
        .context("claim the tenant")?;
    let mut installed = BTreeMap::<String, Installed>::new();
    for row in transaction
        .query(
            "SELECT package_id, package_version, predecessor_version FROM catalog.packages \
              WHERE tenant_id = $1",
            &[&tenant],
        )
        .await
        .context("read the installed packages")?
    {
        installed
            .entry(row.get(0))
            .or_default()
            .lineage
            .insert(row.get(1), row.get(2));
    }
    for package in installed.values_mut() {
        let succeeded: BTreeSet<&String> = package.lineage.values().flatten().collect();
        package.current = package
            .lineage
            .keys()
            .find(|version| !succeeded.contains(version))
            .cloned()
            .unwrap_or_default();
    }
    for row in transaction
        .query(
            "SELECT package_id, candidate_package_version \
               FROM catalog.package_upgrade_qualifications WHERE tenant_id = $1",
            &[&tenant],
        )
        .await
        .context("read the stage evidence")?
    {
        if let Some(package) = installed.get_mut(row.get::<_, &str>(0)) {
            package.evidence.insert(row.get(1));
        }
    }
    let mut instances = BTreeMap::new();
    for row in transaction
        .query(
            "SELECT i.instance_id, i.requirement_type, i.lifecycle_status = 'enabled', \
                    g.definition_json \
               FROM catalog.connection_instances i \
               LEFT JOIN catalog.connection_generations g \
                 ON g.tenant_id = i.tenant_id AND g.environment = i.environment \
                AND g.instance_id = i.instance_id AND g.generation = i.active_generation \
              WHERE i.tenant_id = $1 AND i.environment = $2",
            &[&tenant, &document.env],
        )
        .await
        .context("read the connection instances")?
    {
        instances.insert(
            row.get(0),
            Instance {
                requirement_type: row.get(1),
                enabled: row.get(2),
                definition: row.get(3),
            },
        );
    }
    let bindings = transaction
        .query(
            "SELECT manifest_digest, component_digest, store_alias, instance_id \
               FROM catalog.connection_bindings WHERE tenant_id = $1 AND environment = $2",
            &[&tenant, &document.env],
        )
        .await
        .context("read the connection bindings")?
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect();
    let floors_installed: bool = transaction
        .query_one(
            "SELECT to_regclass('catalog.package_floors') IS NOT NULL",
            &[],
        )
        .await
        .context("probe the floor table")?
        .get(0);
    let floors = if floors_installed {
        transaction
            .query(
                "SELECT package_id, version FROM catalog.package_floors WHERE tenant_id = $1",
                &[&tenant],
            )
            .await
            .context("read the recorded floors")?
            .into_iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect()
    } else {
        BTreeMap::new()
    };
    transaction.commit().await.context("end the project read")?;
    Ok(Some(ProjectState {
        tenant,
        installed,
        instances,
        bindings,
        floors,
    }))
}

async fn read_release(
    platform: &Platform,
    system: &Client,
    digest: &str,
    tenant: &str,
) -> anyhow::Result<ReleaseFacts> {
    let source = ReleaseManifestSource::new(
        &platform.release_artifact_base,
        false,
        &platform.registry_auth_file,
    )
    .context("open the release manifest source")?
    .with_ca_paths(&platform.oci_ca_paths)
    .context("read the registry CA")?;
    let bytes = source
        .pull_verified(digest)
        .await
        .with_context(|| format!("pull release manifest {digest}"))?;
    let (manifest, _) = ServingManifest::from_canonical_bytes(&bytes)
        .with_context(|| format!("read release manifest {digest}"))?;
    let image_set = release_chart::image_set(&platform.chart).await?;
    system
        .query_one("SELECT set_config('app.tenant', $1, false)", &[&tenant])
        .await
        .context("claim the release's tenant")?;
    let qualification: Option<String> = system
        .query_opt(
            "SELECT qualification_sha256 FROM catalog.qualifications \
              WHERE package_set = $1::text::jsonb AND image_digests ->> 'host' = $2 \
              ORDER BY recorded_at DESC LIMIT 1",
            &[
                &serde_json::to_string(&package_set(&manifest)?)?,
                &image_digest(&image_set.host)?,
            ],
        )
        .await
        .context("look up the release's qualification")?
        .map(|row| row.get(0));
    let qualification = qualification.with_context(|| {
        format!(
            "refused: release {digest} has no qualification on the chart's host image {}",
            image_set.host
        )
    })?;
    let components: Vec<String> = manifest
        .components
        .iter()
        .map(|component| component.digest.as_str().to_owned())
        .collect();
    let required_connections = system
        .query(
            "SELECT DISTINCT store_alias FROM catalog.connection_requirements \
              WHERE tenant_id = $1 AND component_digest = ANY($2)",
            &[&tenant, &components],
        )
        .await
        .context("read the release's connection requirements")?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    Ok(ReleaseFacts {
        digest: digest.to_owned(),
        manifest,
        image_set,
        qualification,
        required_connections,
    })
}

/// Run `helm` with the platform's kubeconfig and context.
pub(crate) async fn helm(
    platform: &Platform,
    arguments: &[&str],
) -> anyhow::Result<std::process::Output> {
    Command::new("helm")
        .args(arguments)
        .arg("--kubeconfig")
        .arg(&platform.target.kubeconfig)
        .args(["--kube-context", &platform.target.context])
        .args(["--namespace", &platform.target.namespace])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("start helm")
}

/// The newest revision whose status is `deployed` or `superseded`, with its
/// values. A failed or pending revision is not a predecessor for anything.
pub(crate) async fn read_revision(
    platform: &Platform,
    release_name: &str,
) -> anyhow::Result<Option<Revision>> {
    let output = helm(
        platform,
        &["history", release_name, "--max", "50", "-o", "json"],
    )
    .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            stderr.contains("not found"),
            "helm history {release_name} exited {}: {stderr}",
            output.status
        );
        return Ok(None);
    }
    let history: Vec<Value> =
        serde_json::from_slice(&output.stdout).context("decode helm history")?;
    let Some(entry) = newest_successful(&history) else {
        return Ok(None);
    };
    let number = entry["revision"]
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .context("helm history gave a revision that is not a number")?;
    let revision = number.to_string();
    let output = helm(
        platform,
        &[
            "get",
            "values",
            release_name,
            "--revision",
            &revision,
            "-o",
            "json",
        ],
    )
    .await?;
    ensure!(
        output.status.success(),
        "helm get values {release_name} --revision {revision} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(Some(Revision {
        number,
        chart: entry["chart"].as_str().unwrap_or_default().to_owned(),
        values: serde_json::from_slice(&output.stdout).context("decode helm values")?,
    }))
}

/// Whether Helm holds the release chart of the environment, in any status.
pub(crate) async fn release_present(
    platform: &Platform,
    release_name: &str,
) -> anyhow::Result<bool> {
    let output = helm(platform, &["status", release_name]).await?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        stderr.contains("not found"),
        "helm status {release_name} exited {}: {stderr}",
        output.status
    );
    Ok(false)
}

/// The newest entry of `helm history` with status `deployed` or `superseded`.
pub(crate) fn newest_successful(history: &[Value]) -> Option<&Value> {
    history
        .iter()
        .filter(|entry| matches!(entry["status"].as_str(), Some("deployed" | "superseded")))
        .max_by_key(|entry| entry["revision"].as_u64())
}

/// The live set: release digest to the count of host pods of the host group
/// that are not terminated. A terminating pod still counts: it is draining.
pub(crate) async fn read_live_set(
    platform: &Platform,
    release_name: &str,
) -> anyhow::Result<BTreeMap<String, usize>> {
    Ok(live_set(&read_pods(platform, release_name).await?))
}

/// The host pods of the environment's host group, as `kubectl get pods -o json`.
pub(crate) async fn read_pods(platform: &Platform, release_name: &str) -> anyhow::Result<Value> {
    let selector = format!("wasmcloud.com/hostgroup={release_name}");
    let output = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(&platform.target.kubeconfig)
        .args(["--context", &platform.target.context])
        .args(["--namespace", &platform.target.namespace])
        .args(["get", "pods", "-l", &selector, "-o", "json"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("start kubectl")?;
    ensure!(
        output.status.success(),
        "kubectl get pods -l {selector} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("decode the pod list")
}

pub(crate) fn live_set(pods: &Value) -> BTreeMap<String, usize> {
    let mut live = BTreeMap::new();
    for pod in pods["items"].as_array().into_iter().flatten() {
        if matches!(
            pod["status"]["phase"].as_str(),
            Some("Succeeded" | "Failed")
        ) {
            continue;
        }
        let digest = pod["metadata"]["annotations"]["wamn.release-digest"]
            .as_str()
            .unwrap_or("unlabelled");
        *live.entry(digest.to_owned()).or_insert(0) += 1;
    }
    live
}

/// Whether `version` is `ancestor` or descends from it through
/// `predecessor_version` (R16). No ordering of versions is used.
pub fn descends(lineage: &BTreeMap<String, Option<String>>, ancestor: &str, version: &str) -> bool {
    let mut at = Some(version);
    let mut seen = BTreeSet::new();
    while let Some(current) = at {
        if current == ancestor {
            return true;
        }
        if !seen.insert(current) {
            return false;
        }
        at = lineage.get(current).and_then(Option::as_deref);
    }
    false
}

/// The R17 predicate for one package the release names that is installed:
/// the installed version, a staged successor with stage evidence, or the
/// tested predecessor that the installed version's stage evidence names.
/// `apply` and `rollback` use this one function.
pub fn package_compatible(installed: &Installed, version: &str) -> Result<(), String> {
    if version == installed.current {
        return Ok(());
    }
    let staged_successor = installed.lineage.get(version).and_then(Option::as_deref)
        == Some(installed.current.as_str())
        && installed.evidence.contains(version);
    let tested_predecessor = installed.evidence.contains(&installed.current)
        && installed
            .lineage
            .get(&installed.current)
            .and_then(Option::as_deref)
            == Some(version);
    if staged_successor || tested_predecessor {
        return Ok(());
    }
    Err(format!(
        "is neither the installed version {}, a staged successor with stage evidence, \
         nor the tested predecessor of the installed version",
        installed.current
    ))
}

/// Every reason the document is refused against the authorities (§10.1 step 3).
pub fn refusals(document: &EnvironmentDocument, authorities: &Authorities) -> Vec<String> {
    let mut refusals = Vec::new();
    let empty = ProjectState::default();
    let project = authorities.project.as_ref().unwrap_or(&empty);
    for (package, recorded) in &project.floors {
        let lineage = project
            .installed
            .get(package)
            .map(|installed| &installed.lineage);
        match document.floors.get(package) {
            None => refusals.push(format!(
                "floor {package}@{recorded} is recorded and not declared"
            )),
            Some(declared) if document.release == DeclaredRelease::None && declared != recorded => {
                refusals.push(format!(
                    "floor {package}@{declared}: with release none no floor advances from {recorded}"
                ));
            }
            // A version not registered yet is a successor step 9 applies from
            // its artifact; it cannot be an ancestor of a registered floor.
            Some(declared)
                if lineage.is_some_and(|lineage| {
                    lineage.contains_key(declared) && !descends(lineage, recorded, declared)
                }) =>
            {
                refusals.push(format!(
                    "floor {package}@{declared} does not descend from the recorded floor {recorded}"
                ));
            }
            Some(_) => {}
        }
    }
    for (package, declared) in &document.floors {
        match project.installed.get(package) {
            None => refusals.push(format!(
                "floor {package}@{declared} names a package not installed"
            )),
            Some(_)
                if document.release == DeclaredRelease::None
                    && !project.floors.contains_key(package) =>
            {
                refusals.push(format!(
                    "floor {package}@{declared}: with release none no floor advances"
                ));
            }
            // Step 9 applies a version not registered yet from its verified
            // artifact, when its predecessor is the installed version.
            Some(_) => {}
        }
    }
    if let Some(release) = &authorities.release {
        for package in &release.manifest.release.packages {
            let (id, version) = (package.package_id(), package.package_version());
            if let Some(installed) = project.installed.get(id) {
                if let Err(reason) = package_compatible(installed, version) {
                    refusals.push(format!("package {id}@{version} {reason}"));
                }
                if let Some(floor) = project.floors.get(id)
                    && !descends(&installed.lineage, floor, version)
                {
                    refusals.push(format!("package {id}@{version} is below the floor {floor}"));
                }
            }
        }
        for alias in &release.required_connections {
            if !document.connections.contains_key(alias) {
                refusals.push(format!(
                    "connection {alias} is required by release {} and not declared",
                    release.digest
                ));
            }
        }
    }
    refusals
}

/// The differences from each authority (§10.1 step 3, the plan).
fn plan(document: &EnvironmentDocument, authorities: &Authorities) -> Vec<String> {
    let mut plan = Vec::new();
    match &authorities.row {
        None => {
            plan.push("create the environment row, database, credentials and run plane".to_owned());
        }
        Some(row) if row.route_host.as_deref() != Some(document.route_host.as_str()) => {
            plan.push(format!("set the route host to {}", document.route_host));
        }
        Some(_) => {}
    }
    if let Some(row) = &authorities.row
        && row.policy_name != document.policy
    {
        plan.push(format!("set the policy to {}", document.policy));
    }
    let instances = authorities
        .project
        .as_ref()
        .map(|project| &project.instances);
    for (instance, connection) in &document.connections {
        match instances.and_then(|instances| instances.get(instance)) {
            None => plan.push(format!("add connection {instance}")),
            Some(existing)
                if !existing.enabled
                    || existing.definition.as_ref() != Some(&connection.definition) =>
            {
                plan.push(format!("update connection {instance}"));
            }
            Some(_) => {}
        }
    }
    for (instance, existing) in instances.into_iter().flatten() {
        if existing.enabled && !document.connections.contains_key(instance) {
            plan.push(format!("retire connection {instance} after the drain"));
        }
    }
    let current = authorities
        .revision
        .as_ref()
        .and_then(Revision::manifest_digest);
    match (&authorities.release, current) {
        (Some(release), Some(current)) if release.digest == current => {
            plan.push(format!(
                "release {current} is installed; Helm writes only if the values differ"
            ));
        }
        (Some(release), _) => plan.push(format!(
            "stage and roll out release {} (qualification {})",
            release.digest, release.qualification
        )),
        (None, Some(current)) => plan.push(format!("uninstall release {current} and drain")),
        (None, None) => {}
    }
    if let Some(project) = &authorities.project {
        for (package, declared) in &document.floors {
            if project.floors.get(package) != Some(declared) {
                plan.push(format!("contract {package} to floor {declared} when safe"));
            }
        }
    }
    if plan.is_empty() {
        plan.push("nothing differs".to_owned());
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lineage(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(version, predecessor)| ((*version).to_owned(), predecessor.map(str::to_owned)))
            .collect()
    }

    #[test]
    fn descends_through_predecessors_only() {
        let lineage = lineage(&[
            ("2.2.0", None),
            ("2.3.0", Some("2.2.0")),
            ("2.4.0", Some("2.3.0")),
        ]);
        assert!(descends(&lineage, "2.2.0", "2.4.0"));
        assert!(descends(&lineage, "2.4.0", "2.4.0"));
        assert!(!descends(&lineage, "2.4.0", "2.2.0"));
        assert!(!descends(&lineage, "2.2.0", "9.9.9"));
    }

    #[test]
    fn the_compatibility_predicate_holds_its_table() {
        let installed = Installed {
            current: "2.3.0".to_owned(),
            lineage: lineage(&[
                ("2.2.0", None),
                ("2.3.0", Some("2.2.0")),
                ("2.4.0", Some("2.3.0")),
            ]),
            evidence: BTreeSet::from(["2.3.0".to_owned()]),
        };
        // installed, tested predecessor, staged successor without evidence, unrelated.
        assert!(package_compatible(&installed, "2.3.0").is_ok());
        assert!(package_compatible(&installed, "2.2.0").is_ok());
        assert!(package_compatible(&installed, "2.4.0").is_err());
        assert!(package_compatible(&installed, "1.0.0").is_err());
        let staged = Installed {
            evidence: BTreeSet::from(["2.4.0".to_owned()]),
            ..installed
        };
        assert!(package_compatible(&staged, "2.4.0").is_ok());
        assert!(package_compatible(&staged, "2.2.0").is_err());
    }

    #[test]
    fn the_newest_successful_revision_skips_failed_and_pending() {
        let history = vec![
            json!({"revision": 1, "status": "superseded"}),
            json!({"revision": 2, "status": "deployed"}),
            json!({"revision": 3, "status": "failed"}),
            json!({"revision": 4, "status": "pending-upgrade"}),
        ];
        assert_eq!(newest_successful(&history).unwrap()["revision"], 2);
        assert!(newest_successful(&history[2..]).is_none());
    }

    #[test]
    fn the_live_set_counts_pods_that_are_not_terminated() {
        let pods = json!({"items": [
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:a"}}, "status": {"phase": "Running"}},
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:a"}, "deletionTimestamp": "t"}, "status": {"phase": "Running"}},
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}, "status": {"phase": "Pending"}},
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:c"}}, "status": {"phase": "Succeeded"}}
        ]});
        assert_eq!(
            live_set(&pods),
            BTreeMap::from([("sha256:a".to_owned(), 2), ("sha256:b".to_owned(), 1)])
        );
    }

    fn authorities(floors: &[(&str, &str)]) -> Authorities {
        Authorities {
            release_name: "r-prod-0".to_owned(),
            policy: Policy {
                name: "prod".to_owned(),
                readiness_budget_seconds: 600,
                drain_bound_seconds: 300,
            },
            row: None,
            project: Some(ProjectState {
                tenant: "acme".to_owned(),
                installed: BTreeMap::from([(
                    "wamn_wms".to_owned(),
                    Installed {
                        current: "2.4.0".to_owned(),
                        lineage: lineage(&[
                            ("2.2.0", None),
                            ("2.3.0", Some("2.2.0")),
                            ("2.4.0", Some("2.3.0")),
                        ]),
                        evidence: BTreeSet::new(),
                    },
                )]),
                floors: floors
                    .iter()
                    .map(|(package, version)| ((*package).to_owned(), (*version).to_owned()))
                    .collect(),
                ..ProjectState::default()
            }),
            release: None,
            revision: None,
            live: BTreeMap::new(),
        }
    }

    fn document(release: DeclaredRelease, floors: &[(&str, &str)]) -> EnvironmentDocument {
        EnvironmentDocument {
            org: "acme".to_owned(),
            project: "wms".to_owned(),
            env: "prod".to_owned(),
            release,
            route_host: "wms.acme.example".to_owned(),
            policy: "prod".to_owned(),
            connections: BTreeMap::new(),
            floors: floors
                .iter()
                .map(|(package, version)| ((*package).to_owned(), (*version).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn floors_are_declared_and_monotonic() {
        let digest = DeclaredRelease::Digest(format!("sha256:{}", "a".repeat(64)));
        let recorded = authorities(&[("wamn_wms", "2.3.0")]);
        assert!(
            refusals(
                &document(digest.clone(), &[("wamn_wms", "2.4.0")]),
                &recorded
            )
            .is_empty()
        );
        assert!(
            refusals(
                &document(digest.clone(), &[("wamn_wms", "2.3.0")]),
                &recorded
            )
            .is_empty()
        );
        let omitted = refusals(&document(digest.clone(), &[]), &recorded);
        assert!(
            omitted[0].contains("recorded and not declared"),
            "{omitted:?}"
        );
        let ancestor = refusals(
            &document(digest.clone(), &[("wamn_wms", "2.2.0")]),
            &recorded,
        );
        assert!(ancestor[0].contains("does not descend"), "{ancestor:?}");
        let advanced = refusals(
            &document(DeclaredRelease::None, &[("wamn_wms", "2.4.0")]),
            &recorded,
        );
        assert!(advanced[0].contains("no floor advances"), "{advanced:?}");
        let first = refusals(
            &document(digest.clone(), &[("wamn_wms", "2.4.0")]),
            &authorities(&[]),
        );
        assert!(first.is_empty(), "{first:?}");
        // A version not registered yet is the contract-phase successor that
        // step 9 applies from its artifact.
        let successor = refusals(&document(digest, &[("wamn_wms", "2.5.0")]), &recorded);
        assert!(successor.is_empty(), "{successor:?}");
        let none_first = refusals(
            &document(DeclaredRelease::None, &[("wamn_wms", "2.4.0")]),
            &authorities(&[]),
        );
        assert!(
            none_first[0].contains("no floor advances"),
            "{none_first:?}"
        );
    }
}

//! Independent environment provisioning and absent-application initialization.

pub mod reset;
pub mod schema;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::Value;

use super::document::{DeclaredRelease, EnvironmentDocument};
use super::{Platform, analyse, ensure, stage};
use crate::platform::{bootstrap, identity, job};

/// Database identities resolved before a Job starts, compared under its own lock.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ExpectedDatabase {
    active: Option<String>,
    pub pending: Option<String>,
    pub initialization_identity: Option<String>,
}

impl ExpectedDatabase {
    pub async fn read(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<Self> {
        let client = wamn_package_control::database::connect(&platform.system_database_url).await?;
        let row = client.query_opt(
            "SELECT instance_suffix, pending_database->>'initialization_identity' FROM registry.project_envs \
             WHERE org=$1 AND project=$2 AND env=$3",
            &[&document.org, &document.project, &document.env],
        ).await?;
        Ok(match row {
            Some(row) => Self {
                active: Some(row.get(0)),
                pending: row.get(1),
                initialization_identity: None,
            },
            None => Self {
                active: None,
                pending: None,
                initialization_identity: None,
            },
        })
    }

    /// The Job owns this session until SQL finishes. An expired CLI cannot release it.
    pub async fn lock(
        &self,
        platform: &Platform,
        document: &EnvironmentDocument,
        exclusive: bool,
    ) -> anyhow::Result<super::lock::LifecycleLock> {
        let lock = if exclusive {
            super::lock::LifecycleLock::acquire_wait(
                &platform.system_database_url,
                &document.triple(),
            )
            .await?
        } else {
            super::lock::LifecycleLock::acquire_shared(
                &platform.system_database_url,
                &document.triple(),
            )
            .await?
        };
        let actual = Self::read(platform, document).await?;
        ensure!(
            actual.active == self.active && actual.pending == self.pending,
            "SQL Job database instance changed before execution"
        );
        if let Some(expected) = &self.initialization_identity {
            let state = schema::state(platform, document).await?;
            ensure!(
                state.initialization_identity.as_ref() == Some(expected),
                "SQL Job database initialization changed before execution"
            );
        }
        if !exclusive {
            crate::provision_project_env::registry::require_connected(
                lock.client(),
                &document.triple(),
            )
            .await?;
        }
        Ok(lock)
    }
}

/// Report whether a selected application has any absent package baseline.
/// Version and schema differences never authorize SQL during code deployment.
pub async fn needs_initialization(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<bool> {
    let installed = stage::installed(platform, document).await?;
    Ok(document.applications.values().any(|application| {
        application
            .packages
            .iter()
            .any(|id| !installed.contains_key(id))
    }))
}

/// Initialize project state and absent package baselines after CNPG creates the database.
/// Committed package records survive a later failure and prevent SQL replay.
pub async fn initialize(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin_url: &str,
    application: Option<&str>,
) -> anyhow::Result<()> {
    let document = analyse::select_application(document, application)?;
    let DeclaredRelease::Digest(digest) = document.legacy_release() else {
        anyhow::bail!("initialization requires an application release");
    };
    let manifest = analyse::pull_release_manifest(platform, digest).await?;
    let refusals = analyse::application_refusals(
        &document.membership(),
        manifest
            .release
            .packages
            .iter()
            .map(wamn_catalog::PackageCoordinate::package_id),
    );
    ensure!(refusals.is_empty(), "{}", refusals.join("; "));
    let (packages, _) = analyse::read_release_packages(platform, &manifest).await?;
    let project_url = ensure::project_url(platform, &document.triple()).await?;
    let database = url::Url::parse(&project_url)?;
    let admin = wamn_package_control::database::connect(admin_url).await?;
    admin
        .batch_execute(&crate::provision_project_env::privilege_sql(
            database.path().trim_start_matches('/'),
        ))
        .await
        .context("set the database privilege floor after CNPG")?;
    ensure::ensure_project_database(platform, &document, &packages, &admin, admin_url).await?;
    let drops =
        super::bindings::project_authority(&platform.system_database_url, &project_url, &document)
            .await?;
    platform.auth_changes().await?.publish(drops).await;
    let installed = stage::installed(platform, &document).await?;
    stage::initialize(platform, &document, &installed, &manifest, &project_url).await?;
    Ok(())
}

/// Execute independent application SQL using the installed platform's ctl image.
pub async fn apply_deployed(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: &str,
    release: &str,
    resolution: Option<&wamn_package_control::apply_package::MigrationResolution>,
) -> anyhow::Result<()> {
    run_schema_deployed(
        platform,
        document,
        Operation::ApplySchema {
            application,
            release,
            resolution,
        },
    )
    .await
}

/// Record an explicit assertion through the installed platform SQL Job.
pub async fn adopt_deployed(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<()> {
    run_schema_deployed(platform, document, Operation::AdoptSchema).await
}

async fn run_schema_deployed(
    platform: &Platform,
    document: &EnvironmentDocument,
    operation: Operation<'_>,
) -> anyhow::Result<()> {
    let directory = ensure::PrivateDirectory::create()?;
    let chart = crate::platform::artifact_plan::select(&platform.chart)?;
    let cluster = reset::recorded_cluster(platform, document).await?;
    run_deployed(
        platform,
        document,
        operation,
        chart.image("ctl")?,
        &cluster,
        &directory.path("schema"),
    )
    .await?;
    Ok(())
}

/// SQL operations that run in the selected ctl image.
#[derive(Debug, Clone, Copy)]
pub enum Operation<'a> {
    /// Record the environment and return its CNPG Database resource.
    Provision,
    /// Resolve reset inputs, or restore the recorded inputs after CNPG recreation.
    Reset {
        candidate: Option<(&'a str, &'a str)>,
    },
    /// Initialize the selected application's absent baselines after CNPG is ready.
    Initialize,
    /// Assert selected SQL state without executing package SQL.
    AdoptSchema,
    /// Advance explicitly selected SQL without activating application code.
    ApplySchema {
        application: &'a str,
        release: &'a str,
        resolution: Option<&'a wamn_package_control::apply_package::MigrationResolution>,
    },
}

/// Run the SQL operation in a short-lived Job using protected credential inputs.
/// Provision returns a Database resource for the caller to apply and wait for.
/// The input document must already select one application for initialization.
pub async fn run_deployed(
    platform: &Platform,
    document: &EnvironmentDocument,
    operation: Operation<'_>,
    image: &str,
    cluster: &str,
    directory: &Path,
) -> anyhow::Result<Option<Value>> {
    if matches!(operation, Operation::Initialize) {
        ensure!(
            document.applications.len() == 1,
            "select one application before initialization"
        );
    }
    let verb = match operation {
        Operation::Provision => "provision",
        Operation::Reset { .. } => "reset",
        Operation::Initialize => "initialize",
        Operation::ApplySchema { .. } => "schema-apply",
        Operation::AdoptSchema => "schema-adopt",
    };
    let coordinate = match operation {
        Operation::Provision | Operation::Reset { .. } | Operation::AdoptSchema => {
            wamn_control_provision::environment_state::environment_resource_name(
                &document.org,
                &document.project,
                &document.env,
            )?
        }
        Operation::ApplySchema { application, .. } => {
            analyse::application_release_name(document, application)?
        }
        Operation::Initialize => analyse::application_release_name(
            document,
            document
                .applications
                .keys()
                .next()
                .expect("one selected application"),
        )?,
    };
    let name = format!("{coordinate}-{verb}");
    let mut arguments = vec![
        "env",
        verb,
        "/inputs/environment.json",
        "/inputs/database.json",
    ];
    if matches!(operation, Operation::Provision) {
        arguments.extend(["--emit-secret", "/out/secret.json"]);
    }
    let candidate = match operation {
        Operation::ApplySchema {
            application,
            release,
            ..
        } => Some((application, release)),
        Operation::Reset { candidate } => candidate,
        _ => None,
    };
    if let Some((application, release)) = candidate {
        arguments.extend(["--application", application, "--release", release]);
    }
    let resolution = match operation {
        Operation::ApplySchema {
            resolution: Some(choice),
            ..
        } => Some(choice.to_string()),
        _ => None,
    };
    if let Some(resolution) = &resolution {
        arguments.extend(["--resolve", resolution]);
    }
    let mut job = job::manifest(&name, image, &["/usr/local/bin/wamn-ctl"], &arguments);
    let mut files = BTreeMap::from([(
        "environment.json".into(),
        String::from_utf8(document.canonical_json()?)?,
    )]);
    let mut expected = ExpectedDatabase::read(platform, document).await?;
    if matches!(
        operation,
        Operation::AdoptSchema | Operation::ApplySchema { .. }
    ) {
        expected.initialization_identity = Some(
            schema::state(platform, document)
                .await?
                .initialization_identity
                .context("schema mutation requires a known initialization identity")?,
        );
    }
    files.insert("database.json".into(), serde_json::to_string(&expected)?);
    let mut secrets = Vec::new();
    let admin: bootstrap::SecretData = crate::kubectl::get(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.database_namespace,
        "secret",
        &format!("{cluster}-superuser"),
    )
    .await?
    .context("missing CNPG superuser Secret")?;
    // SQL Jobs use the CNPG Service and PostgreSQL's default port, never a native forward.
    let host =
        crate::provision_project_env::database_host(None, cluster, &platform.database_namespace);
    let mut admin_url = url::Url::parse("postgresql://postgres@localhost/wamn_system")?;
    admin_url.set_host(Some(&host))?;
    admin_url
        .set_password(Some(
            &bootstrap::secret_text(&admin, "password")?.replace('%', "%25"),
        ))
        .map_err(|()| anyhow::anyhow!("encode the administrator password"))?;
    secrets.push(job::credential(
        &mut job,
        &format!("{name}-admin"),
        "WAMN_SYSTEM_ADMIN_URL",
        "url",
        admin_url.as_str(),
    ));
    let apply: bootstrap::SecretData = crate::kubectl::get(
        &platform.target.kubeconfig,
        &platform.target.context,
        "platform",
        "secret",
        "wamn-apply",
    )
    .await?
    .context("missing Secret platform/wamn-apply")?;
    secrets.push(job::credential(
        &mut job,
        &format!("{name}-apply"),
        "WAMN_APPLY_DATABASE_URL",
        "url",
        &bootstrap::secret_text(&apply, "url")?,
    ));
    let release_base = super::runtime::pod_artifact_base(
        platform.insecure_registry,
        &platform.release_artifact_base,
    )?;
    job::environment(
        &mut job,
        &[
            ("WAMN_RELEASE_NAMESPACE", &platform.target.namespace),
            ("WAMN_DATABASE_NAMESPACE", &platform.database_namespace),
            ("WAMN_DATABASE_HOST", &host),
            ("WAMN_RELEASE_ARTIFACT_BASE", &release_base),
        ],
    );
    for (variable, value) in [
        (
            "WAMN_PACKAGE_ARTIFACT_BASE",
            platform.package_artifact_base.as_deref(),
        ),
        (
            "WAMN_COMPONENT_ARTIFACT_BASE",
            platform.component_artifact_base.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            job::environment(
                &mut job,
                &[(
                    variable,
                    &super::runtime::pod_artifact_base(platform.insecure_registry, value)?,
                )],
            );
        }
    }
    // The existing host overlay carries the selected host's transport arguments.
    files.insert(
        "host.json".into(),
        serde_json::to_string(&serde_json::json!({
            "extraArgs": if platform.insecure_registry {
                vec!["--allow-insecure-registries"]
            } else { Vec::new() }
        }))?,
    );
    job::environment(
        &mut job,
        &[("WAMN_RELEASE_HOST_GROUP", "/inputs/host.json")],
    );
    let registry = pod_registry_credentials(
        platform.insecure_registry,
        serde_json::from_slice(&std::fs::read(&platform.registry_auth_file)?)?,
    )?;
    files.insert("registry.json".into(), serde_json::to_string(&registry)?);
    job::environment(
        &mut job,
        &[("WAMN_REGISTRY_AUTH_FILE", "/inputs/registry.json")],
    );
    let mut ca_paths = Vec::new();
    for (index, path) in platform.oci_ca_paths.iter().enumerate() {
        let name = format!("registry-ca-{index}.crt");
        files.insert(name.clone(), std::fs::read_to_string(path)?);
        ca_paths.push(format!("/inputs/{name}"));
    }
    job::environment(&mut job, &[("WAMN_OCI_CA_PATHS", &ca_paths.join(":"))]);
    nats_inputs(
        &mut job,
        &mut files,
        platform
            .auth_nats
            .as_ref()
            .context("SQL Jobs require the authorization change publisher")?,
    )?;
    secrets.push(job::files(&mut job, files));
    if matches!(operation, Operation::Provision) {
        job::secret_output(&mut job);
    }
    let output = job::run(&platform.target, directory, &mut job, &mut secrets).await?;
    match operation {
        Operation::Provision | Operation::Reset { .. } => Ok(Some(
            serde_json::from_str(&output).context("read the provisioned Database resource")?,
        )),
        Operation::ApplySchema { .. } | Operation::AdoptSchema => {
            print!("{output}");
            Ok(None)
        }
        Operation::Initialize => {
            ensure::ensure_cdc(platform, document).await?;
            Ok(None)
        }
    }
}

fn pod_registry_credentials(insecure_registry: bool, mut registry: Value) -> anyhow::Result<Value> {
    let auths = registry["auths"]
        .as_object_mut()
        .context("registry credentials auths")?;
    let local = crate::platform::resources::LOCAL_REGISTRY;
    let pod = super::runtime::pod_registry(insecure_registry, local);
    if pod != local
        && let Some(credentials) = auths.remove(local)
    {
        auths.insert(pod.into(), credentials);
    }
    Ok(registry)
}

fn nats_inputs(
    job: &mut Value,
    files: &mut BTreeMap<String, String>,
    nats: &wamn_platform_identity::nats::PlatformNats,
) -> anyhow::Result<()> {
    // The Service hostname matches the server certificate without the native TLS override.
    job::environment(job, &[("WAMN_NATS_URL", identity::NATS_URL)]);
    for (variable, name, path) in [
        ("WAMN_NATS_TLS_CA", "nats-ca.crt", &nats.nats_tls_ca),
        ("WAMN_NATS_TLS_CERT", "nats.crt", &nats.nats_tls_cert),
        ("WAMN_NATS_TLS_KEY", "nats.key", &nats.nats_tls_key),
    ] {
        add_file(job, files, variable, name, path)?;
    }
    Ok(())
}

fn add_file(
    job: &mut Value,
    files: &mut BTreeMap<String, String>,
    variable: &str,
    name: &str,
    path: &Path,
) -> anyhow::Result<()> {
    files.insert(
        name.to_owned(),
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    );
    job::environment(job, &[(variable, &format!("/inputs/{name}"))]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize as _;
    use wamn_platform_identity::nats::PlatformNats;

    #[test]
    fn sql_job_keeps_credentials_for_other_registries() -> anyhow::Result<()> {
        use crate::platform::resources::{LOCAL_REGISTRY, POD_REGISTRY};
        let credentials = serde_json::json!({"auths": {
            LOCAL_REGISTRY: {"username":"local", "password":"local-password"},
            "registry.example": {"username":"remote", "password":"remote-password"}
        }});
        assert_eq!(
            pod_registry_credentials(false, credentials.clone())?,
            credentials
        );
        let rewritten = pod_registry_credentials(true, credentials.clone())?;
        assert_eq!(
            rewritten["auths"][POD_REGISTRY],
            credentials["auths"][LOCAL_REGISTRY]
        );
        assert!(rewritten["auths"].get(LOCAL_REGISTRY).is_none());
        assert_eq!(
            rewritten["auths"]["registry.example"],
            credentials["auths"]["registry.example"]
        );
        Ok(())
    }

    #[test]
    fn sql_job_uses_the_cluster_broker_and_its_certificate_name() -> anyhow::Result<()> {
        let private = ensure::PrivateDirectory::create()?;
        let credential = private.path("native-credential");
        std::fs::write(&credential, "credential bytes")?;
        let nats = PlatformNats {
            nats_url: "nats://127.0.0.1:54321".into(),
            nats_tls_ca: credential.clone(),
            nats_tls_cert: credential.clone(),
            nats_tls_key: credential,
            nats_tls_server_name: Some("native-forward.invalid".into()),
        };
        let mut job = job::manifest("initialize", "ctl", &["wamn-ctl"], &["env", "initialize"]);
        let mut files = BTreeMap::new();
        nats_inputs(&mut job, &mut files, &nats)?;
        let variables = job::container(&mut job)["env"].as_array().unwrap();
        let endpoint = variables
            .iter()
            .find(|entry| entry["name"] == "WAMN_NATS_URL")
            .unwrap()["value"]
            .as_str()
            .unwrap();
        assert_eq!(endpoint, identity::NATS_URL);
        assert_ne!(endpoint, nats.nats_url);
        assert!(
            !variables
                .iter()
                .any(|entry| entry["name"] == "WAMN_NATS_TLS_SERVER_NAME")
        );
        let host = url::Url::parse(endpoint)?.host_str().unwrap().to_owned();
        let certificate = serde_yaml::Deserializer::from_str(include_str!(
            "../../../../../deploy/infra/nats-certificates.yaml"
        ))
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|object| {
            object["kind"] == "Certificate" && object["metadata"]["name"] == "wasmcloud-nats-tls"
        })
        .unwrap();
        assert!(
            certificate["spec"]["dnsNames"]
                .as_array()
                .unwrap()
                .contains(&Value::String(host))
        );
        for variable in variables
            .iter()
            .filter(|entry| entry["name"] != "WAMN_NATS_URL")
        {
            let path = variable["value"].as_str().unwrap();
            let file = path.strip_prefix("/inputs/").unwrap();
            assert_eq!(files[file], "credential bytes");
        }
        Ok(())
    }
}

//! Independent environment provisioning and absent-application initialization.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::Value;

use super::document::{DeclaredRelease, EnvironmentDocument};
use super::{Platform, analyse, ensure, stage};
use crate::platform::{bootstrap, job};

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

/// SQL operations that run in the selected ctl image.
#[derive(Debug, Clone, Copy)]
pub enum Operation {
    /// Record the environment and return its CNPG Database resource.
    Provision,
    /// Initialize the selected application's absent baselines after CNPG is ready.
    Initialize,
}

/// Run the SQL operation in a short-lived Job using protected credential inputs.
/// Provision returns a Database resource for the caller to apply and wait for.
/// The input document must already select one application for initialization.
pub async fn run_deployed(
    platform: &Platform,
    document: &EnvironmentDocument,
    operation: Operation,
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
        Operation::Initialize => "initialize",
    };
    let coordinate = match operation {
        Operation::Provision => wamn_release_control::release_chart::release_name(
            &document.org,
            &document.project,
            &document.env,
        )?,
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
    let mut arguments = vec!["env", verb, "/inputs/environment.json"];
    if matches!(operation, Operation::Provision) {
        arguments.extend(["--emit-secret", "/out/secret.json"]);
    }
    let mut job = job::manifest(&name, image, &["/usr/local/bin/wamn-ctl"], &arguments);
    let mut files = BTreeMap::from([(
        "environment.json".into(),
        String::from_utf8(document.canonical_json()?)?,
    )]);
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
    let host = platform.database_host.clone().unwrap_or_else(|| {
        format!(
            "{cluster}-rw.{}.svc.cluster.local",
            platform.database_namespace
        )
    });
    let mut admin_url = url::Url::parse("postgresql://postgres@localhost/wamn_system")?;
    admin_url.set_host(Some(&host))?;
    admin_url
        .set_port(Some(platform.database_port))
        .map_err(|()| anyhow::anyhow!("encode the administrator port"))?;
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
    job::environment(
        &mut job,
        &[
            ("WAMN_RELEASE_NAMESPACE", &platform.target.namespace),
            ("WAMN_DATABASE_NAMESPACE", &platform.database_namespace),
            ("WAMN_DATABASE_HOST", &host),
            ("WAMN_DATABASE_PORT", &platform.database_port.to_string()),
            (
                "WAMN_RELEASE_ARTIFACT_BASE",
                &platform.release_artifact_base,
            ),
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
            job::environment(&mut job, &[(variable, value)]);
        }
    }
    add_file(
        &mut job,
        &mut files,
        "WAMN_REGISTRY_AUTH_FILE",
        "registry.json",
        &platform.registry_auth_file,
    )?;
    let mut ca_paths = Vec::new();
    for (index, path) in platform.oci_ca_paths.iter().enumerate() {
        let name = format!("registry-ca-{index}.crt");
        files.insert(name.clone(), std::fs::read_to_string(path)?);
        ca_paths.push(format!("/inputs/{name}"));
    }
    job::environment(&mut job, &[("WAMN_OCI_CA_PATHS", &ca_paths.join(":"))]);
    let nats = platform
        .auth_nats
        .as_ref()
        .context("SQL Jobs require the authorization change publisher")?;
    job::environment(&mut job, &[("WAMN_NATS_URL", &nats.nats_url)]);
    for (variable, name, path) in [
        ("WAMN_NATS_TLS_CA", "nats-ca.crt", &nats.nats_tls_ca),
        ("WAMN_NATS_TLS_CERT", "nats.crt", &nats.nats_tls_cert),
        ("WAMN_NATS_TLS_KEY", "nats.key", &nats.nats_tls_key),
    ] {
        add_file(&mut job, &mut files, variable, name, path)?;
    }
    if let Some(server) = &nats.nats_tls_server_name {
        job::environment(&mut job, &[("WAMN_NATS_TLS_SERVER_NAME", server)]);
    }
    secrets.push(job::files(&mut job, files));
    if matches!(operation, Operation::Provision) {
        job::secret_output(&mut job);
    }
    let output = job::run(&platform.target, directory, &mut job, &mut secrets).await?;
    match operation {
        Operation::Provision => Ok(Some(
            serde_json::from_str(&output).context("read the provisioned Database resource")?,
        )),
        Operation::Initialize => {
            ensure::ensure_cdc(platform, document).await?;
            Ok(None)
        }
    }
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

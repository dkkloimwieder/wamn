//! Worker execution of one server-selected application operation.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

use super::{Platform, document::EnvironmentDocument, sql};

/// Run one published schema operation through the existing SQL owners.
///
/// # Errors
/// Returns preparation, SQL, or reset failures without changing code selections.
pub async fn schema(
    platform: &Platform,
    file: &Path,
    application: &str,
    release: Option<&str>,
    apply: bool,
    resolution: Option<&wamn_package_control::apply_package::MigrationResolution>,
    administrator: Option<&str>,
) -> anyhow::Result<sql::schema::Diff> {
    let document = EnvironmentDocument::read_deployed(platform, file, Some(application)).await?;
    let (release, candidates) =
        sql::schema::release_candidates(platform, &document, application, release).await?;
    let state = sql::schema::state(platform, &document).await?;
    let diff = sql::schema::plan(&document, application, Some(release), &candidates, &state)?;
    if resolution.is_none() && diff.requires_reset() && diff.disposable == Some(true) {
        sql::schema::reset_candidates(platform, &diff, candidates).await?;
    }
    if apply {
        let release = diff
            .release
            .as_deref()
            .context("schema apply needs a published release")?;
        wamn_package_control::apply_package::resolution_package(
            resolution,
            diff.packages
                .keys()
                .map(|id| (id.as_str(), state.migration_records(id))),
        )?;
        if resolution.is_none() && diff.requires_reset() {
            if let Some(admin) = administrator {
                Box::pin(sql::reset::reset_deployed_with_admin(
                    platform,
                    file,
                    Some((application, release)),
                    admin,
                ))
                .await?;
            } else {
                Box::pin(sql::reset::reset_deployed(
                    platform,
                    file,
                    Some((application, release)),
                ))
                .await?;
            }
        } else if let Some(admin) = administrator {
            let expected = sql::ExpectedDatabase::read(platform, &document).await?;
            let lock = expected.lock(platform, &document, false).await?;
            sql::schema::apply(platform, &document, application, release, admin, resolution)
                .await?;
            super::ensure::ensure_cdc_inputs(platform, &document, Some(admin)).await?;
            lock.release().await;
        } else {
            sql::apply_deployed(platform, &document, application, release, resolution).await?;
        }
    }
    Ok(diff)
}

/// Execute one server-built command in the product ctl image.
/// Kubernetes supplies the mounted inputs. No request selects a path or credential.
#[derive(Debug)]
pub struct Request<'a> {
    pub operation: &'a str,
    pub org: &'a str,
    pub project: &'a str,
    pub env: &'a str,
    pub application: Option<&'a str>,
    pub release: Option<&'a str>,
    pub resolution: Option<&'a wamn_package_control::apply_package::MigrationResolution>,
}

/// Prepare in-cluster inputs, then invoke the existing operation owner.
///
/// # Errors
/// Returns invalid target, input, Kubernetes, or operation failures.
pub async fn run(request: Request<'_>) -> anyhow::Result<Value> {
    let actor = std::env::var("WAMN_DEPLOY_ACTOR")
        .context("authenticated deploy actor is absent")?
        .parse::<wamn_platform_identity::PrincipalId>()
        .context("authenticated deploy actor is not a principal id")?;
    ensure!(
        matches!(
            request.operation,
            "deploy" | "rollback" | "diff" | "apply" | "reset" | "adopt"
        ),
        "unknown application operation"
    );
    let coordinate = wamn_control_provision::environment_names::environment_resource_name(
        request.org,
        request.project,
        request.env,
    )?;
    prepare_kubeconfig(Path::new("/work/kubeconfig"))?;
    crate::platform::operations::worker_chart(
        Path::new("/usr/local/share/wamn"),
        Path::new("/operation-inputs"),
        Path::new("/work/chart"),
    )?;
    let mut platform = Platform::apply_from_env()?;
    let configuration: Value =
        serde_json::from_slice(&std::fs::read("/operation-inputs/configuration.json")?)?;
    platform.insecure_registry = configuration["insecure_registry"]
        .as_bool()
        .context("registry transport is absent")?;
    platform.package_artifact_base = Some(
        configuration["package_base"]
            .as_str()
            .context("package registry is absent")?
            .into(),
    );
    platform.component_artifact_base = Some(
        configuration["component_base"]
            .as_str()
            .context("component registry is absent")?
            .into(),
    );
    platform.target.namespace = "hosts".into();
    let object: Value = wamn_kubernetes::kubectl::get(
        &platform.target.kubeconfig,
        &platform.target.context,
        "hosts",
        "configmap",
        &coordinate,
    )
    .await?
    .context("canonical environment is absent")?;
    let document = EnvironmentDocument::from_json(
        object["data"]["environment.json"]
            .as_str()
            .context("canonical environment document is absent")?
            .as_bytes(),
    )?;
    ensure!(
        document.org == request.org
            && document.project == request.project
            && document.env == request.env,
        "canonical environment target differs"
    );
    let file = Path::new("/work/environment.json");
    std::fs::write(file, serde_json::to_vec(&document)?)?;
    super::runtime::resolve_platform(&mut platform, request.org).await?;
    let administrator = if matches!(request.operation, "apply" | "reset" | "adopt") {
        let cluster = sql::reset::recorded_cluster(&platform, &document).await?;
        Some(crate::platform::operations::administrator(&cluster)?)
    } else {
        None
    };
    match request.operation {
        "deploy" => {
            let analysis = super::deploy_as(
                &platform,
                file,
                request.release.context("deploy requires a release")?,
                Some(actor.as_str()),
            )
            .await?;
            Ok(json!({"notices":analysis.notices,"plan":analysis.plan}))
        }
        "rollback" => {
            let analysis = super::apply_selection(
                &platform,
                file,
                request
                    .application
                    .context("rollback requires an application")?,
                super::document::SelectionChange::Rollback(request.release),
                Some(actor.as_str()),
            )
            .await?;
            Ok(json!({"notices":analysis.notices,"plan":analysis.plan}))
        }
        "diff" | "apply" => {
            let diff = schema(
                &platform,
                file,
                request
                    .application
                    .context("schema operation requires an application")?,
                request.release,
                request.operation == "apply",
                request.resolution,
                administrator.as_deref(),
            )
            .await?;
            Ok(serde_json::to_value([diff])?)
        }
        "reset" => {
            Box::pin(sql::reset::reset_deployed_with_admin(
                &platform,
                file,
                None,
                administrator.as_deref().context("reset administrator")?,
            ))
            .await
        }
        "adopt" => {
            let expected = sql::ExpectedDatabase::read(&platform, &document).await?;
            let lock = expected.lock(&platform, &document, false).await?;
            let identity = sql::schema::state(&platform, &document)
                .await?
                .initialization_identity
                .context("adoption requires an initialization identity")?;
            let adopted = sql::schema::adopt_application(
                &platform,
                &document,
                &identity,
                request.application,
            )
            .await?;
            lock.release().await;
            Ok(json!({"adopted":adopted,"sql_executed":false,"live_verified":false}))
        }
        _ => unreachable!("operation was checked before execution"),
    }
}

fn prepare_kubeconfig(path: &Path) -> anyhow::Result<()> {
    const ACCOUNT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
    let host = std::env::var("KUBERNETES_SERVICE_HOST").context("in-cluster API host is absent")?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
    let configuration = json!({"apiVersion":"v1","kind":"Config","current-context":"operation",
        "clusters":[{"name":"cluster","cluster":{"server":format!("https://{host}:{port}"),"certificate-authority":format!("{ACCOUNT}/ca.crt")}}],
        "users":[{"name":"worker","user":{"tokenFile":format!("{ACCOUNT}/token")}}],
        "contexts":[{"name":"operation","context":{"cluster":"cluster","user":"worker","namespace":"hosts"}}]});
    std::fs::write(path, serde_json::to_vec(&configuration)?)?;
    Ok(())
}

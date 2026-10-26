//! Receiving routes tests and helpers.

use super::*;

/// Copy the package at `source` to `destination`, with its build output.
///
/// The build output lies outside the package, in
/// `<package>/../target/wamn/<package>`, so the copy takes it from the
/// source's output root to the destination's.
///
/// The build's input digest covers the lock file of the enclosing workspace,
/// `<package>/../Cargo.lock`, so the copy takes it too. Without it the copy's
/// tree differs from its `build.json` and push-package refuses it as stale.
pub(super) fn copy_package(source: &Path, destination: &Path) -> anyhow::Result<()> {
    copy_package_tree(source, destination)?;
    let lock = |root: &Path| root.parent().map(|parent| parent.join("Cargo.lock"));
    if let (Some(from), Some(to)) = (lock(source), lock(destination))
        && from.is_file()
        && !to.exists()
    {
        std::fs::copy(from, to)?;
    }
    let output = wamn_schema_generator::output_root(source);
    if output.is_dir() {
        let copied = wamn_schema_generator::output_root(destination);
        std::fs::create_dir_all(copied.parent().context("an output root has a parent")?)?;
        copy_package_tree(&output, &copied)?;
    }
    Ok(())
}

fn copy_package_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        // Installed web packages are not package sources, and pnpm links them.
        if kind.is_dir() && entry.file_name() == "node_modules" {
            continue;
        }
        if kind.is_dir() {
            copy_package_tree(&entry.path(), &target)?;
        } else {
            anyhow::ensure!(kind.is_file(), "package sources must be regular files");
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Provision the journey control store, the environment and its route in
/// the owned PostgreSQL server, and copy the operator Secret to
/// `inputs.operator_secret_output`.
async fn provision_environment(
    inputs: &JourneyDocument,
    admin: &Client,
    root: &Path,
) -> anyhow::Result<wamn_control::provision_project_env::ProvisionedRoute> {
    let system_url = &inputs.system_pg_url;
    let version: i32 = admin
        .query_one("SHOW server_version_num", &[])
        .await
        .context("read PostgreSQL version")?
        .get::<_, String>(0)
        .parse()
        .context("parse PostgreSQL version")?;
    anyhow::ensure!(
        version >= 180_000,
        "journey requires PostgreSQL 18 or newer"
    );
    provision_journey_control(&scope(), system_url, admin).await?;
    admin
        .execute(
            "UPDATE registry.meta SET platform_domain = $1",
            &[&PLATFORM_DOMAIN],
        )
        .await
        .context("set the disposable journey platform domain")?;
    let management_secret = root.join("management-author-pat.json");
    let route =
        provision_route(&scope(), system_url, admin, root, Some(&management_secret)).await?;
    let operator_secret = root.join("operator-pat.json");
    std::fs::copy(&operator_secret, &inputs.operator_secret_output).with_context(|| {
        format!(
            "copy production-minted operator Secret from {} to {}",
            operator_secret.display(),
            inputs.operator_secret_output.display()
        )
    })?;
    std::fs::set_permissions(
        &inputs.operator_secret_output,
        Permissions::from_mode(0o600),
    )
    .with_context(|| {
        format!(
            "set operator Secret mode on {}",
            inputs.operator_secret_output.display()
        )
    })?;
    Ok(route)
}

/// Provision the environment and install the candidate's release in it as
/// `apply` does: its packages through the package stage from their
/// artifacts. It publishes nothing (docs/plan/platform-deploy.md §8.2,
/// §10.2). Returns the route of the environment.
pub(super) async fn install_candidate_release(
    inputs: &JourneyDocument,
    candidate: &Candidate,
    evidence: &Path,
) -> anyhow::Result<wamn_control::provision_project_env::ProvisionedRoute> {
    let system_url = inputs.system_pg_url.clone();
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    let identity_log = evidence.join("identity.log");
    std::fs::File::create(&identity_log).context("create retained identity log")?;
    std::os::unix::fs::symlink(&identity_log, root.join("identity.log"))
        .context("retain identity output outside the private scratch directory")?;
    let (admin, admin_task) = connect(&system_url).await?;
    let route = provision_environment(inputs, admin.as_ref(), root).await;
    admin_task.abort();
    let route = route?;
    let (project, project_task) = connect(&route.database_url).await?;
    let floor = install_journey_platform_floor(project.as_ref(), PLATFORM_DOMAIN).await;
    project_task.abort();
    floor?;
    reconcile_journey_run_plane(&scope(), &system_url, &route.database_url).await?;
    prepare_journey_credentials(
        &scope(),
        &system_url,
        &route.database_url,
        root,
        &inputs.host_secret_directory,
        &inputs.host_secret_namespace,
    )
    .await?;
    wamn_gate_harness::case_release::install_candidate(
        candidate,
        &system_url,
        &route.database_url,
        &wamn_control_registry::Triple::new(
            identity().org.as_str(),
            identity().project.as_str(),
            identity().environment.as_str(),
        ),
        "receiving",
    )
    .await?;
    wamn_control::dev::environment::grant_operator_admin_role(
        &route.database_url,
        identity().org.as_str(),
        identity().project.as_str(),
        identity().environment.as_str(),
        identity().tenant().as_str(),
        PLATFORM_DOMAIN,
    )
    .await?;
    Ok(route)
}

/// Publish the release of this checkout through the authoring path, and
/// return the route and the digest of the release. Only the delivery case,
/// which mints its own release, publishes.
pub(super) async fn publish_receiving_release(
    inputs: &JourneyDocument,
    scenario_worker: &Path,
) -> anyhow::Result<(
    wamn_control::provision_project_env::ProvisionedRoute,
    String,
)> {
    let system_url = inputs.system_pg_url.clone();
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    let (admin, admin_task) = connect(&system_url).await?;
    let route = provision_environment(inputs, admin.as_ref(), root).await?;
    let (project, project_task) = connect(&route.database_url).await?;
    // The gate and authorship read component facts from the verified package
    // artifacts, and apply-package installs them, so the packages are pushed
    // first.
    let packages = super::environment::push_journey_packages(inputs).await?;
    install_journey_project(&packages, project.as_ref(), &route.database_url).await?;
    verify_journey_operation_grants(project.as_ref()).await?;
    reconcile_journey_run_plane(&scope(), &system_url, &route.database_url).await?;
    wamn_control::dev::environment::grant_operator_admin_role(
        &route.database_url,
        identity().org.as_str(),
        identity().project.as_str(),
        identity().environment.as_str(),
        identity().tenant().as_str(),
        PLATFORM_DOMAIN,
    )
    .await?;
    let credentials = prepare_journey_credentials(
        &scope(),
        &system_url,
        &route.database_url,
        root,
        &inputs.host_secret_directory,
        &inputs.host_secret_namespace,
    )
    .await?;
    reconcile_journey_data_access(&packages, &route.database_url).await?;
    let declarations = render_component_declarations(root, &inputs.component_directory)?;
    let admitted_components = admit_journey_components(inputs, &declarations)?;
    verify_journey_components_are_effectful(&admitted_components)?;

    // One gate-launch path for both live gates: the Gate is a spawned
    // `wamn-scenario-worker serve`, never a task in this process
    // (wamn-10yt.10.32). The in-process exemption this test used to hold died
    // with that ruling.
    let mut management_server = spawn_journey_management_gate(
        &scope(),
        scenario_worker,
        &credentials,
        &credentials.management_admitter,
        &super::environment::journey_package_registry(inputs)?,
        ROUTE_JOURNEY_GATE_BIND,
    )
    .await?;
    let gate_reports = gate_journey_wirings(
        management_server.bind(),
        route
            .management_token
            .as_deref()
            .context("project provisioning emitted no management-author PAT")?,
    )
    .await?;
    verify_zero_case_gate_reports(admin.as_ref(), &gate_reports).await?;
    reconcile_journey_run_plane(&scope(), &system_url, &route.database_url).await?;
    let target = JourneyReleaseTarget {
        project_url: &route.database_url,
        system_url: &system_url,
        publisher: route
            .management_principal_subject
            .as_deref()
            .context("project provisioning emitted no management-author principal")?,
    };
    let digest = super::environment::publish_journey_release(inputs, target).await?;
    let stopped = management_server.shutdown().await;
    project_task.abort();
    admin_task.abort();
    stopped?;
    Ok((route, digest))
}

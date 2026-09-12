//! Receiving dev tests and helpers.

use super::*;

pub(super) const JOURNEY_URL_ENV: &str = "WAMN_ROUTE_PG18_URL";
pub(super) const DEV_COMMAND_TIMEOUT: Duration = Duration::from_secs(12 * 60);
pub(super) const DEV_EXPECTED_MIGRATIONS: [(&str, &str, i32, &str); 3] = [
    (
        OVERLAY_PACKAGE_ID,
        OVERLAY_PACKAGE_VERSION,
        1,
        "migrations/0001_add_inspection_required.sql",
    ),
    (
        OVERLAY_PACKAGE_ID,
        OVERLAY_PACKAGE_VERSION,
        2,
        "migrations/0002_quality_inspection.sql",
    ),
    (
        BASE_PACKAGE_ID,
        BASE_PACKAGE_VERSION,
        1,
        "migrations/0001_initial.sql",
    ),
];

pub(super) struct DevJourneyInputs {
    pub(super) wamn_binary: PathBuf,
    pub(super) environment: DevEnvironmentInputs,
}

impl DevJourneyInputs {
    fn required() -> anyhow::Result<Self> {
        let inputs = Self {
            wamn_binary: required_journey_path("WAMN_RECEIVING_DEV_BIN")?,
            environment: DevEnvironmentInputs {
                host_binary: required_journey_path("WAMN_RECEIVING_DEV_HOST_BIN")?,
                nats_url: required_journey("WAMN_RECEIVING_DEV_NATS_URL")?,
                event_nats_url: required_journey("WAMN_EVT_NATS_URL")?,
                event_nats_username: required_journey("WAMN_EVT_NATS_USERNAME")?,
                event_nats_password_file: required_journey_path("WAMN_EVT_NATS_PASSWORD_FILE")?,
                stream_replicas: required_journey("WAMN_EVT_STREAM_REPLICAS")?.parse()
                    .context("the dev test requires its declared NATS stream replica count")?,
                dup_window_secs: required_journey("WAMN_EVT_DUP_WINDOW_SECS")?.parse()
                    .context("the dev test requires its declared NATS duplicate window")?,
                tempo_query_url: required_journey("WAMN_RECEIVING_DEV_TEMPO_QUERY_URL")?,
                otel_exporter_otlp_endpoint: required_journey(
                    "WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT",
                )?,
                flow_http_workload_image: required_journey(
                    "WAMN_RECEIVING_DEV_FLOW_HTTP_WORKLOAD_IMAGE",
                )?,
                component_artifact_base: required_journey("WAMN_ROUTE_COMPONENT_ARTIFACT_BASE")?,
                release_artifact_base: required_journey("WAMN_ROUTE_RELEASE_ARTIFACT_BASE")?,
                route_host: required_journey("WAMN_ROUTE_HOST")?,
                registry_auth_file: required_journey_path("WAMN_ROUTE_REGISTRY_AUTH_FILE")?,
                package_sources: vec![
                    package_root()
                        .canonicalize()
                        .context("resolve the base package root")?,
                ],
            },
        };
        anyhow::ensure!(
            inputs.wamn_binary.is_file(),
            "WAMN_RECEIVING_DEV_BIN does not name a built wamn binary"
        );
        anyhow::ensure!(
            inputs.environment.host_binary.is_file(),
            "WAMN_RECEIVING_DEV_HOST_BIN does not name a built wamn-host binary"
        );
        Ok(inputs)
    }
}

/// Fixed nameable port for `[WAMN-DEV-LIVE]`'s spawned Gate.
///
/// A spawned child cannot hand an ephemeral port back the way the in-process
/// launch did, and the configuration written from it outlives the process that
/// writes it, so the port is named here (wamn-10yt.10.32).
pub(super) const DEV_LIVE_GATE_BIND: &str = "127.0.0.1:18088";

pub(super) async fn run_dev_product_command(
    inputs: &DevJourneyInputs,
    config: &Path,
) -> anyhow::Result<std::process::Output> {
    let mut command = Command::new(&inputs.wamn_binary);
    command
        .current_dir(repository_root()?)
        .args(["dev", "--config"])
        .arg(config)
        .arg("--overlay-root")
        .arg(
            overlay_package_root()
                .canonicalize()
                .context("resolve the overlay package root")?,
        )
        .kill_on_drop(true);
    tokio::time::timeout(DEV_COMMAND_TIMEOUT, command.output())
        .await
        .context("wamn dev exceeded its twelve-minute product bound")?
        .context("run the literal wamn dev product command")
}

pub(super) fn verify_dev_command_output(output: &std::process::Output) -> anyhow::Result<()> {
    let stdout = String::from_utf8(output.stdout.clone()).context("wamn dev stdout is UTF-8")?;
    let stderr = String::from_utf8(output.stderr.clone()).context("wamn dev stderr is UTF-8")?;
    anyhow::ensure!(
        output.status.success(),
        "wamn dev failed with {}: stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    let stages = wamn_ctl::dev::DEV_STAGE_ORDER
        .iter()
        .map(|stage| stage.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let expected = format!("run completed: {stages}");
    let results = stdout
        .lines()
        .filter(|line| line.starts_with("run completed:"))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        results == [expected.as_str()],
        "wamn dev returned the wrong product receipt: {results:?}; stdout={stdout:?}"
    );
    // The endpoint the activated release served, read off the public dev seam.
    // Only the Activate stage can publish it, so dropping that publish leaves
    // this line absent — which is the gate wamn-10yt.10.28 asked for, and it
    // cannot be satisfied by anything the command knows before it activates.
    let served = stdout
        .lines()
        .filter(|line| line.starts_with("run served: "))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        served.len() == 1,
        "wamn dev reported no single served endpoint: {served:?}; stdout={stdout:?}"
    );
    let served = served[0].trim_start_matches("run served: ");
    let (base_url, route_host) = served
        .split_once(" host=")
        .with_context(|| format!("the served line names a host: {served:?}"))?;
    let (route_host, target_instance) = route_host
        .split_once(" target_instance=")
        .context("the served line names the exact target instance")?;
    anyhow::ensure!(
        !target_instance.is_empty(),
        "the served target instance is empty"
    );
    anyhow::ensure!(
        base_url.starts_with("http://127.0.0.1:")
            && base_url
                .trim_start_matches("http://127.0.0.1:")
                .parse::<u16>()
                .is_ok_and(|port| port != 0),
        "the served endpoint is not a loopback port the host actually bound: {base_url:?}"
    );
    anyhow::ensure!(
        route_host == required_journey("WAMN_ROUTE_HOST")?,
        "the served route host is not the deployment-owned one: {route_host:?}"
    );
    Ok(())
}

pub(super) fn declared_dev_data_access_grants() -> anyhow::Result<BTreeSet<(String, String, String, String)>> {
    let inputs = std::env::var_os(JOURNEY_DOCUMENT_ENV)
        .map(|_| JourneyDocument::required()).transpose()?;
    let mut expected = BTreeSet::new();
    for package in JOURNEY_PACKAGES {
        let path = journey_package_root(package, inputs.as_ref()).join("generated/platform-policy/data-access.json");
        let policy = read_json(&path)?;
        anyhow::ensure!(
            policy["role"] == "wamn_app",
            "{} does not declare the shared package application role",
            path.display()
        );
        let relations = policy["relations"]
            .as_array()
            .with_context(|| format!("{} carries a relations array", path.display()))?;
        for relation in relations {
            let schema = relation["schema"]
                .as_str()
                .context("data-access relation carries schema")?;
            let table = relation["table"]
                .as_str()
                .context("data-access relation carries table")?;
            for (field, privilege) in [
                ("select_fields", "SELECT"),
                ("insert_fields", "INSERT"),
                ("update_fields", "UPDATE"),
            ] {
                for column in relation[field]
                    .as_array()
                    .with_context(|| format!("{schema}.{table} carries {field}"))?
                {
                    expected.insert((
                        schema.to_owned(),
                        table.to_owned(),
                        column
                            .as_str()
                            .with_context(|| format!("{schema}.{table} {field} is a string"))?
                            .to_owned(),
                        privilege.to_owned(),
                    ));
                }
            }
            if relation["lock"].as_bool() == Some(true) {
                expected.insert((
                    schema.to_owned(),
                    table.to_owned(),
                    relation["lock_update_field"]
                        .as_str()
                        .with_context(|| format!("{schema}.{table} lock carries its carrier"))?
                        .to_owned(),
                    "UPDATE".to_owned(),
                ));
            }
        }
    }
    Ok(expected)
}

pub(super) async fn verify_dev_target_package_and_acl_state(project: &Client) -> anyhow::Result<()> {
    let packages = project
        .query(
            "SELECT package_id, package_version, manifest_sha256 \
             FROM catalog.packages WHERE tenant_id = $1 \
             ORDER BY package_id COLLATE \"C\", package_version COLLATE \"C\"",
            &[&TENANT],
        )
        .await
        .context("read product-command target package coordinates")?;
    let observed_packages = packages
        .iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
            )
        })
        .collect::<Vec<_>>();
    let expected_packages = [
        (OVERLAY_PACKAGE_ID, OVERLAY_PACKAGE_VERSION),
        (BASE_PACKAGE_ID, BASE_PACKAGE_VERSION),
    ];
    anyhow::ensure!(
        observed_packages.len() == expected_packages.len()
            && observed_packages.iter().zip(expected_packages).all(
                |((id, version, hash), expected)| {
                    (id.as_str(), version.as_str()) == expected
                        && hash.len() == 71
                        && hash.starts_with("sha256:")
                }
            ),
        "wamn dev installed the wrong exact package coordinates: {observed_packages:?}"
    );

    let migrations = project
        .query(
            "SELECT package_id, package_version, ordinal, relative_path, sha256 \
             FROM catalog.package_migrations WHERE tenant_id = $1 \
             ORDER BY package_id COLLATE \"C\", package_version COLLATE \"C\", ordinal",
            &[&TENANT],
        )
        .await
        .context("read product-command target migration ledgers")?;
    let observed_migrations = migrations
        .iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, i32>(2),
                row.get::<_, String>(3),
                row.get::<_, String>(4),
            )
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        observed_migrations.len() == DEV_EXPECTED_MIGRATIONS.len()
            && observed_migrations.iter().zip(DEV_EXPECTED_MIGRATIONS).all(
                |((id, version, ordinal, path, hash), expected)| {
                    (id.as_str(), version.as_str(), *ordinal, path.as_str()) == expected
                        && hash.len() == 71
                        && hash.starts_with("sha256:")
                }
            ),
        "wamn dev installed the wrong exact migration ledgers: {observed_migrations:?}"
    );

    let observed_permissions = project
        .query(
            "SELECT permission FROM app_system.permissions \
             WHERE tenant_id = $1 AND role_name = $2 \
             ORDER BY permission COLLATE \"C\"",
            &[&TENANT, &ROUTE_CALLER_ROLE],
        )
        .await
        .context("read product-command operation grants")?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    let expected_permissions = BASE_OPERATIONS
        .iter()
        .map(|(_, token)| (*token).to_owned())
        .chain(
            OVERLAY_OPERATIONS
                .iter()
                .map(|(_, token)| (*token).to_owned())
                .filter(|token| token != "client-acme-receiving:quality/create-inspection@3.0.0"),
        )
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        observed_permissions == expected_permissions,
        "wamn dev installed the wrong exact operation-grant union: {observed_permissions:?}"
    );

    let observed_column_grants = project
        .query(
            "SELECT table_schema::text, table_name::text, column_name::text, privilege_type::text \
             FROM information_schema.column_privileges \
             WHERE grantee = 'wamn_app' AND table_schema = 'receiving' \
             ORDER BY table_schema COLLATE \"C\", table_name COLLATE \"C\", \
                      column_name COLLATE \"C\", privilege_type COLLATE \"C\"",
            &[],
        )
        .await
        .context("read the installed package data-access union")?
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, String>(3),
            )
        })
        .collect::<BTreeSet<_>>();
    let expected_column_grants = declared_dev_data_access_grants()?;
    anyhow::ensure!(
        observed_column_grants == expected_column_grants,
        "wamn dev installed the wrong exact column-privilege union: {observed_column_grants:?}"
    );
    let table_grant_count: i64 = project
        .query_one(
            "SELECT count(*) FROM information_schema.table_privileges \
             WHERE grantee = 'wamn_app' AND table_schema = 'receiving'",
            &[],
        )
        .await
        .context("read package application table-level privilege residue")?
        .get(0);
    anyhow::ensure!(
        table_grant_count == 0,
        "wamn dev left {table_grant_count} table-level package privilege grants"
    );
    let schema_usage: bool = project
        .query_one(
            "SELECT pg_catalog.has_schema_privilege('wamn_app', 'receiving', 'USAGE')",
            &[],
        )
        .await
        .context("read package application schema usage")?
        .get(0);
    anyhow::ensure!(schema_usage, "wamn dev omitted receiving schema USAGE");
    Ok(())
}

pub(super) async fn verify_dev_release_state(
    control: &Client,
    inputs: &DevEnvironmentInputs,
    expected_source_commit: &str,
    expected_publisher_id: &str,
    expected_publisher_subject: &str,
) -> anyhow::Result<()> {
    let release = control
        .query_one(
            "SELECT environment, verified_publisher_principal \
             FROM catalog.effective_releases \
             WHERE tenant_id = $1 AND effective_release_id = $2",
            &[&TENANT, &(RELEASE_ID as i32)],
        )
        .await
        .context("read the product-command effective release")?;
    let environment: String = release.get(0);
    let publisher: Option<String> = release.get(1);
    anyhow::ensure!(
        environment == ENVIRONMENT && publisher.is_none(),
        "wamn dev projected more than the control-plane release identity: \
         environment={environment:?} publisher={publisher:?}"
    );

    let attestation = control
        .query_one(
            "SELECT deployed_manifest_hash, source_commit \
             FROM catalog.deployment_attestations \
             WHERE tenant_id = $1 AND effective_release_id = $2 \
               AND org_id = $3 AND project_id = $4 AND environment = $5",
            &[&TENANT, &(RELEASE_ID as i32), &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("read the product-command deployment attestation")?;
    let manifest_hash: String = attestation.get(0);
    let source_commit: String = attestation.get(1);
    anyhow::ensure!(
        manifest_hash.len() == 71 && manifest_hash.starts_with("sha256:"),
        "wamn dev recorded a malformed release attestation: {manifest_hash}"
    );
    anyhow::ensure!(
        source_commit == expected_source_commit,
        "wamn dev attested source commit {source_commit:?}, expected {expected_source_commit:?}"
    );

    let source = ReleaseManifestSource::new(
        &inputs.release_artifact_base,
        true,
        &inputs.registry_auth_file,
    )
    .context("configure the product-command release puller")?;
    let bytes = source
        .pull_verified(&manifest_hash)
        .await
        .context("pull the exact product-command release manifest")?;
    let origin = format!("{}@{manifest_hash}", inputs.release_artifact_base);
    let release = LoadedRelease::load_canonical_bytes(&bytes, &origin)
        .context("load the product-command release manifest")?;
    let expected_packages = JOURNEY_PACKAGES
        .iter()
        .map(|package| PackageCoordinate::new(package.id, package.version))
        .collect::<Result<BTreeSet<_>, _>>()?;
    anyhow::ensure!(
        release.manifest().release.tenant_id == TENANT
            && release.manifest().release.effective_release_id.get() == RELEASE_ID
            && release.manifest().release.environment == ENVIRONMENT
            && release.manifest().release.packages == expected_packages,
        "wamn dev published the wrong exact release closure: {:?}",
        release.manifest().release
    );

    let publish_audits = control
        .query(
            "SELECT principal_id, principal_subject, effective_role, provenance_commit, \
                    provenance_dirty \
             FROM catalog.authoring_command_audit \
             WHERE tenant_id = $1 AND command_kind = 'publish' \
               AND org = $2 AND project = $3 AND environment = $4 \
             ORDER BY command_id COLLATE \"C\"",
            &[&TENANT, &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("read the product-command Publish audit")?;
    let expected_publish_count = JOURNEY_PACKAGES
        .iter()
        .map(|package| package.operations.len())
        .sum::<usize>();
    anyhow::ensure!(
        publish_audits.len() == expected_publish_count,
        "wamn dev recorded {} Publish audits, expected {expected_publish_count}",
        publish_audits.len()
    );
    for audit in publish_audits {
        let principal_id: String = audit.get(0);
        let principal_subject: String = audit.get(1);
        let effective_role: String = audit.get(2);
        let provenance_commit: Option<String> = audit.get(3);
        let provenance_dirty: Option<bool> = audit.get(4);
        anyhow::ensure!(
            principal_id == expected_publisher_id
                && principal_subject == expected_publisher_subject
                && effective_role == "project-author"
                && provenance_commit.as_deref() == Some(expected_source_commit)
                && provenance_dirty == Some(false),
            "wamn dev Publish audit carried the wrong publisher or provenance: \
             principal_id={principal_id:?} principal_subject={principal_subject:?} \
             effective_role={effective_role:?} provenance_commit={provenance_commit:?} \
             provenance_dirty={provenance_dirty:?}"
        );
    }
    Ok(())
}

pub(super) async fn current_database_acl(client: &Client) -> anyhow::Result<(String, Option<String>)> {
    let row = client
        .query_one(
            "SELECT datname::text, datacl::text FROM pg_catalog.pg_database \
             WHERE datname = current_database()",
            &[],
        )
        .await
        .context("read the current database ACL sentinel")?;
    Ok((row.get(0), row.get(1)))
}

pub(super) async fn verify_dev_verification_database_absent(
    admin: &Client,
    database: &str,
) -> anyhow::Result<()> {
    let present: bool = admin
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_catalog.pg_database WHERE datname = $1)",
            &[&database],
        )
        .await
        .context("read disposable verification database cleanup")?
        .get(0);
    anyhow::ensure!(
        !present,
        "wamn dev left disposable verification database {database} behind"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable PG18, NATS, authenticated OCI, and built wamn/host/flow-http binaries"]
async fn product_dev_command_owns_the_clean_twelve_stage_output_and_cleanup() -> anyhow::Result<()>
{
    let system_url = required_journey(JOURNEY_URL_ENV)?;
    let inputs = DevJourneyInputs::required()?;
    let credentials = wamn_test_infrastructure::event_broker::Credentials {
        username: required_journey("WAMN_DEV_ENV_EVENT_PROVISIONING_USERNAME")?,
        password_file: required_journey_path("WAMN_DEV_ENV_EVENT_PROVISIONING_PASSWORD_FILE")?,
    };
    let provisioning = wamn_test_infrastructure::event_broker::connect(
        &credentials, &inputs.environment.event_nats_url,
    ).await?;
    assert_dev_command(&system_url, &inputs, &provisioning).await
}

pub(super) async fn assert_dev_command(
    system_url: &str,
    inputs: &DevJourneyInputs,
    event_provisioning: &async_nats::Client,
) -> anyhow::Result<()> {
    // Keep the expensive product gate to one clean run. Engine tests
    // `dirty_source_reaches_gate_then_refuses_before_publish` and
    // `dirty_watch_suffix_refuses_before_its_first_provenance_stage`, plus the
    // filesystem adapter's `filesystem_events_map_owned_inputs_and_ignore_generated_outputs`,
    // own dirty-stop and affected-suffix behavior deterministically.
    let repository = repository_root()?;
    let source = GitSource::discover(&repository)
        .await
        .context("discover the product-command source repository")?
        .snapshot()
        .await
        .context("read the product-command source identity")?;
    anyhow::ensure!(
        source.state() == DevSourceState::Clean,
        "the live product-command proof requires a clean worktree"
    );
    let source_commit = source.source_commit().to_owned();

    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    let (admin, admin_task) = connect(&system_url).await?;
    let gate_binary = journey_scenario_worker_binary()?;
    let environment =
        wamn_ctl::dev::environment::provision(&system_url, admin.as_ref(), root).await?;
    let publisher_subject = environment
        .route
        .management_principal_subject
        .as_deref()
        .context("project provisioning emitted no management-author principal")?;
    let publisher_id = resolve_subject(admin.as_ref(), PrincipalKind::Service, publisher_subject)
        .await
        .context("resolve the production management-author principal")?
        .context("the production management-author principal is absent")?
        .id()
        .to_string();
    let (project, project_task) = connect(&environment.route.database_url).await?;
    // The Gate the loop publishes through is the same real process an operator
    // starts with `wamn dev up`; nothing here links it in (wamn-10yt.10.32).
    let mut gate_server = spawn_journey_management_gate(
        &gate_binary,
        &environment.credentials,
        &environment.credentials.management_admitter,
        DEV_LIVE_GATE_BIND,
    )
    .await?;
    let system_acl_before = current_database_acl(admin.as_ref()).await?;
    let durable_acl_before = current_database_acl(project.as_ref()).await?;
    let event_scope = wamn_control_registry::Triple {
        org: environment.identity.org.clone(),
        project: environment.identity.project.clone(),
        env: wamn_control_registry::Env::new(environment.identity.environment.clone()),
    };
    wamn_ctl::event_streams::provision(
        &async_nats::jetstream::new(event_provisioning.clone()), &event_scope,
        inputs.environment.stream_replicas,
        Duration::from_secs(inputs.environment.dup_window_secs), &[],
    ).await?;
    let config = write_dev_config(
        root,
        &system_url,
        &environment.template,
        &environment.route,
        &environment.credentials,
        &environment.verification,
        gate_server.bind(),
        &inputs.environment,
        &environment.identity,
    )?;

    let command_result = async {
        let output = run_dev_product_command(&inputs, &config).await?;
        // The literal command emits this result only after native workload
        // stop and supervised host reaping have both succeeded.
        verify_dev_command_output(&output)?;
        let system_acl_after = current_database_acl(admin.as_ref()).await?;
        let durable_acl_after = current_database_acl(project.as_ref()).await?;
        anyhow::ensure!(
            system_acl_after == system_acl_before,
            "wamn dev changed the system database ACL: \
             before={system_acl_before:?} after={system_acl_after:?}"
        );
        anyhow::ensure!(
            durable_acl_after == durable_acl_before,
            "wamn dev changed the durable database ACL: \
             before={durable_acl_before:?} after={durable_acl_after:?}"
        );
        verify_dev_target_package_and_acl_state(project.as_ref()).await?;
        verify_dev_release_state(
            admin.as_ref(),
            &inputs.environment,
            &source_commit,
            &publisher_id,
            publisher_subject,
        )
        .await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;

    let gate_stop = gate_server.shutdown().await;
    let verification_cleanup =
        verify_dev_verification_database_absent(admin.as_ref(), &environment.verification.database)
            .await;
    // Exact fallback cleanup runs after the assertion, so it cannot make a
    // product cleanup failure look green when an earlier stage fails.
    let fixture_database_cleanup = admin
        .batch_execute(&provision_sql::drop_database_named_sql(
            &environment.verification.database,
        ))
        .await
        .context("remove the exact verification fixture after its cleanup assertion");
    let role_cleanup =
        clean_dev_verification_gate_roles(admin.as_ref(), &environment.verification).await;
    project_task.abort();
    admin_task.abort();

    command_result?;
    gate_stop?;
    verification_cleanup?;
    fixture_database_cleanup?;
    role_cleanup?;
    Ok(())
}

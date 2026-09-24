//! Receiving dev tests and helpers.

use super::*;

mod local_delivery;
mod password;

pub(super) const DEV_COMMAND_TIMEOUT: Duration = Duration::from_mins(12);
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
    /// Each test sets the local artifact directory under its own scratch root.
    fn required() -> anyhow::Result<Self> {
        let inputs = Self {
            wamn_binary: required_journey_path("WAMN_RECEIVING_DEV_BIN")?,
            environment: DevEnvironmentInputs {
                local_artifacts: wamn_control::dev::config::LocalArtifacts {
                    directory: PathBuf::new(),
                    flow_http_component: required_journey_path("WAMN_DEV_ENV_FLOW_HTTP_COMPONENT")?
                        .canonicalize()
                        .context("resolve the local flow-http component named by WAMN_DEV_ENV_FLOW_HTTP_COMPONENT")?,
                    bindings: None,
                },
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
                route_host: required_journey("WAMN_ROUTE_HOST")?,
                platform_domain: PLATFORM_DOMAIN.to_owned(),
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
    let stages = wamn_control::dev::DEV_STAGE_ORDER
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

pub(super) fn declared_dev_data_access_grants()
-> anyhow::Result<BTreeSet<(String, String, String, String)>> {
    let inputs = std::env::var_os(JOURNEY_DOCUMENT_ENV)
        .map(|_| JourneyDocument::required())
        .transpose()?;
    let mut expected = BTreeSet::new();
    for package in JOURNEY_PACKAGES {
        let path = journey_package_root(package, inputs.as_ref())
            .join("generated/platform-policy/data-access.json");
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

pub(super) async fn verify_dev_target_package_and_acl_state(
    project: &Client,
) -> anyhow::Result<()> {
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
        .context("read product-command target migration records")?;
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
        "wamn dev installed the wrong exact migration records: {observed_migrations:?}"
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

/// The entries are sorted, because a recreated database can list the same entries in another order.
pub(super) async fn current_database_acl(
    client: &Client,
) -> anyhow::Result<(String, Option<String>)> {
    let row = client
        .query_one(
            "SELECT datname::text, \
                    (SELECT array_agg(entry::text ORDER BY entry::text) FROM unnest(datacl) AS entry)::text \
             FROM pg_catalog.pg_database WHERE datname = current_database()",
            &[],
        )
        .await
        .context("read the current database ACL sentinel")?;
    Ok((row.get(0), row.get(1)))
}

#[tokio::test]
#[ignore = "requires: test-util identity binary, local invitation capture, RESEND_API_KEY, RESEND_FROM, WAMN_RECEIVING_DEV_BIN, WAMN_DEV_ENV_FLOW_HTTP_COMPONENT, WAMN_RECEIVING_DEV_HOST_BIN, WAMN_RECEIVING_DEV_NATS_URL, WAMN_EVT_NATS_URL, WAMN_EVT_NATS_USERNAME, WAMN_EVT_NATS_PASSWORD_FILE, WAMN_EVT_STREAM_REPLICAS, WAMN_EVT_DUP_WINDOW_SECS, WAMN_RECEIVING_DEV_TEMPO_QUERY_URL, WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT, WAMN_ROUTE_HOST, WAMN_DEV_ENV_EVENT_PROVISIONING_USERNAME, WAMN_DEV_ENV_EVENT_PROVISIONING_PASSWORD_FILE, cargo-sqlx, jq"]
async fn product_dev_command_owns_the_clean_ten_stage_output_and_cleanup() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "RESEND_API_KEY",
        "RESEND_FROM",
        "WAMN_TEST_INVITATION_FILE",
        "WAMN_TEST_RESEND_ENDPOINT",
        "WAMN_RECEIVING_DEV_BIN",
        "WAMN_DEV_ENV_FLOW_HTTP_COMPONENT",
        "WAMN_RECEIVING_DEV_HOST_BIN",
        "WAMN_RECEIVING_DEV_NATS_URL",
        "WAMN_EVT_NATS_URL",
        "WAMN_EVT_NATS_USERNAME",
        "WAMN_EVT_NATS_PASSWORD_FILE",
        "WAMN_EVT_STREAM_REPLICAS",
        "WAMN_EVT_DUP_WINDOW_SECS",
        "WAMN_RECEIVING_DEV_TEMPO_QUERY_URL",
        "WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT",
        "WAMN_ROUTE_HOST",
        "WAMN_DEV_ENV_EVENT_PROVISIONING_USERNAME",
        "WAMN_DEV_ENV_EVENT_PROVISIONING_PASSWORD_FILE",
        "cargo-sqlx",
        "jq",
    ]);
    // The environment resets the control store of the whole server, so the test starts its own.
    let mut server = wamn_test_infrastructure::postgres::start(&[])?;
    let system_url = server.create_database("wamn_system")?.url().to_owned();
    let mut inputs = DevJourneyInputs::required()?;
    let credentials = wamn_test_infrastructure::event_broker::Credentials {
        username: required_journey("WAMN_DEV_ENV_EVENT_PROVISIONING_USERNAME")?,
        password_file: required_journey_path("WAMN_DEV_ENV_EVENT_PROVISIONING_PASSWORD_FILE")?,
    };
    let provisioning = wamn_test_infrastructure::event_broker::connect(
        &credentials,
        &inputs.environment.event_nats_url,
    )
    .await?;
    assert_dev_command(&system_url, &mut inputs, &provisioning).await
}

pub(super) async fn assert_dev_command(
    system_url: &str,
    inputs: &mut DevJourneyInputs,
    event_provisioning: &async_nats::Client,
) -> anyhow::Result<()> {
    // Keep the expensive product gate to one run. The filesystem adapter's
    // `filesystem_events_map_owned_inputs_and_ignore_generated_outputs` owns
    // affected-suffix behavior deterministically.
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    inputs.environment.local_artifacts.directory = root.join("local-artifacts");
    let (admin, admin_task) = connect(system_url).await?;
    let environment = wamn_control::dev::environment::provision(
        system_url,
        admin.as_ref(),
        root,
        PLATFORM_DOMAIN,
        &inputs.environment.package_sources,
    )
    .await?;
    let (project, project_task) = connect(&environment.route.database_url).await?;
    let system_acl_before = current_database_acl(admin.as_ref()).await?;
    let durable_acl_before = current_database_acl(project.as_ref()).await?;
    // The first run recreates the target, which closes this connection.
    project_task.abort();
    let event_scope = wamn_control_registry::Triple {
        org: environment.identity.org.clone(),
        project: environment.identity.project.clone(),
        env: wamn_control_registry::Env::new(environment.identity.environment.clone()),
    };
    wamn_control::event_streams::provision(
        &async_nats::jetstream::new(event_provisioning.clone()),
        &event_scope,
        inputs.environment.stream_replicas,
        Duration::from_secs(inputs.environment.dup_window_secs),
        &[],
    )
    .await?;
    let config = write_dev_config(
        root,
        system_url,
        &environment.template,
        &environment.route,
        &environment.credentials,
        &inputs.environment,
        &environment.identity,
    )?;

    let pat_count: i64 = admin
        .query_one("SELECT count(*) FROM identity.pats", &[])
        .await?
        .get(0);
    let command_result = async {
        let output = run_dev_product_command(inputs, &config).await?;
        // The literal command emits this result only after native workload
        // stop and supervised host reaping have both succeeded.
        verify_dev_command_output(&output)?;
        let login = password::Login::start(&environment, system_url).await?;
        let mut watch =
            local_delivery::Watch::start(&inputs.wamn_binary, &repository_root()?, &config)?;
        let terminal = async {
            let served = watch.served().await?;
            login.terminal(&environment, &served, system_url).await
        }
        .await;
        let stopped = watch.stop().await;
        terminal?;
        stopped?;
        // Change only the input timestamp so Cargo recompiles the application
        // without changing authored source or its declared digest.
        std::fs::File::open(package_root().join("component/src/lib.rs"))?
            .set_modified(std::time::SystemTime::now())?;
        let rebuilt = run_dev_product_command(inputs, &config).await?;
        verify_dev_command_output(&rebuilt)?;
        login.available().await?;
        login.refuse_missing_membership(system_url).await?;
        let after: i64 = admin
            .query_one("SELECT count(*) FROM identity.pats", &[])
            .await?
            .get(0);
        anyhow::ensure!(after == pat_count, "password access created a PAT");
        let (project, project_task) = connect(&environment.route.database_url).await?;
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
        project_task.abort();
        Ok::<_, anyhow::Error>(login)
    }
    .await;

    let stopped = environment.issuer.stop().await;
    admin_task.abort();
    let login = command_result?;
    stopped?;
    login.stopped().await
}

//! Receiving dev tests and helpers.

use super::*;
use sqlx::Row as _;

mod local_delivery;
mod scheduler_outage;

pub(super) const DEV_COMMAND_TIMEOUT: Duration = Duration::from_mins(12);
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
                local_artifacts: wamn_dev_control::dev::config::LocalArtifacts {
                    directory: PathBuf::new(),
                    flow_http_component: required_journey_path("WAMN_DEV_ENV_FLOW_HTTP_COMPONENT")?
                        .canonicalize()
                        .context("resolve the local flow-http component named by WAMN_DEV_ENV_FLOW_HTTP_COMPONENT")?,
                },
                host_binary: required_journey_path("WAMN_RECEIVING_DEV_HOST_BIN")?,
                nats: wamn_broker::nats::PlatformNats {
                    nats_url: required_journey("WAMN_RECEIVING_DEV_NATS_URL")?,
                    nats_tls_ca: required_journey_path("WAMN_DEV_ENV_NATS_TLS_CA")?,
                    nats_tls_cert: required_journey_path("WAMN_DEV_ENV_NATS_TLS_CERT")?,
                    nats_tls_key: required_journey_path("WAMN_DEV_ENV_NATS_TLS_KEY")?,
                    nats_tls_server_name: None,
                },
                stream_replicas: required_journey("WAMN_EVT_STREAM_REPLICAS")?.parse()
                    .context("the dev test requires its declared NATS stream replica count")?,
                dup_window_secs: required_journey("WAMN_EVT_DUP_WINDOW_SECS")?.parse()
                    .context("the dev test requires its declared NATS duplicate window")?,
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
                cdc_reader: None,
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
    // The endpoint the activated release served, read off the public dev seam.
    // Only the Activate stage can publish it, so dropping that publish leaves
    // this line absent — which is the condition wamn-10yt.10.28 asked for, and it
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
    let mut expected = BTreeSet::new();
    for package in JOURNEY_PACKAGES {
        let path = wamn_compiler::output_root(&journey_package_root(package))
            .join("platform-policy/data-access.json");
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
    project: &mut Client,
) -> anyhow::Result<()> {
    let packages = sqlx::query(
        "SELECT package_id, package_version, artifact_digest \
             FROM catalog.packages WHERE project_env_key = $1 \
             ORDER BY package_id COLLATE \"C\", package_version COLLATE \"C\"",
    )
    .bind(&identity().project_env_key().as_str())
    .fetch_all(&mut *project)
    .await
    .context("read product-command target package coordinates")?;
    let observed_packages = packages
        .iter()
        .map(|row| {
            (
                row.get::<String, _>(0),
                row.get::<String, _>(1),
                row.get::<Option<String>, _>(2),
            )
        })
        .collect::<Vec<_>>();
    let expected_packages = [
        (OVERLAY_PACKAGE_ID, *OVERLAY_PACKAGE_VERSION),
        (BASE_PACKAGE_ID, *BASE_PACKAGE_VERSION),
    ];
    anyhow::ensure!(
        observed_packages.len() == expected_packages.len()
            && observed_packages.iter().zip(expected_packages).all(
                // wamn dev applies its working tree and records no artifact digest.
                |((id, version, digest), expected)| {
                    (id.as_str(), version.as_str()) == expected && digest.is_none()
                }
            ),
        "wamn dev installed the wrong exact package coordinates: {observed_packages:?}"
    );

    use sqlx::migrate::Migrate as _;
    for source in released_package_roots().values() {
        let directory = wamn_package_control::package_source::read_package_directory(source)?;
        let metadata = wamn_catalog::package_schema::package_metadata(&directory)?;
        let expected = wamn_compiler::migrations::migrator(
            &wamn_package_control::package_source::migration_root(source),
        )
        .await?;
        let table = wamn_control_provision::migration::history_table(
            &identity().project_env_key(),
            metadata.coordinate.package_id(),
        );
        let applied = project.list_applied_migrations(&table).await?;
        anyhow::ensure!(
            expected
                .iter()
                .filter(|file| file.migration_type.is_up_migration())
                .all(|file| applied
                    .iter()
                    .any(|row| row.version == file.version && row.checksum == file.checksum)),
            "dev SQLx history differs from its package sources"
        );
    }

    verify_journey_operation_grants(project).await?;

    let observed_column_grants = sqlx::query(
        "SELECT table_schema::text, table_name::text, column_name::text, privilege_type::text \
             FROM information_schema.column_privileges \
             WHERE grantee = 'wamn_app' AND table_schema = 'receiving' \
             ORDER BY table_schema COLLATE \"C\", table_name COLLATE \"C\", \
                      column_name COLLATE \"C\", privilege_type COLLATE \"C\"",
    )
    .fetch_all(&mut *project)
    .await
    .context("read the installed package data-access union")?
    .into_iter()
    .map(|row| {
        (
            row.get::<String, _>(0),
            row.get::<String, _>(1),
            row.get::<String, _>(2),
            row.get::<String, _>(3),
        )
    })
    .collect::<BTreeSet<_>>();
    let expected_column_grants = declared_dev_data_access_grants()?;
    anyhow::ensure!(
        observed_column_grants == expected_column_grants,
        "wamn dev installed the wrong exact column-privilege union: {observed_column_grants:?}"
    );
    let table_grant_count: i64 = sqlx::query(
        "SELECT count(*) FROM information_schema.table_privileges \
             WHERE grantee = 'wamn_app' AND table_schema = 'receiving'",
    )
    .fetch_one(&mut *project)
    .await
    .context("read package application table-level privilege residue")?
    .get(0);
    anyhow::ensure!(
        table_grant_count == 0,
        "wamn dev left {table_grant_count} table-level package privilege grants"
    );
    let schema_usage: bool =
        sqlx::query("SELECT pg_catalog.has_schema_privilege('wamn_app', 'receiving', 'USAGE')")
            .fetch_one(&mut *project)
            .await
            .context("read package application schema usage")?
            .get(0);
    anyhow::ensure!(schema_usage, "wamn dev omitted receiving schema USAGE");
    Ok(())
}

/// The entries are sorted, because a recreated database can list the same entries in another order.
pub(super) async fn current_database_acl(
    client: &mut Client,
) -> anyhow::Result<(String, Option<String>)> {
    let row = sqlx::query("SELECT datname::text, \
                    (SELECT array_agg(entry::text ORDER BY entry::text) FROM unnest(datacl) AS entry)::text \
             FROM pg_catalog.pg_database WHERE datname = current_database()").fetch_one(&mut *client)
        .await
        .context("read the current database ACL sentinel")?;
    Ok((row.get(0), row.get(1)))
}

#[tokio::test]
#[ignore = "requires: env:WAMN_IDENTITY_BINARY, env:RESEND_API_KEY, env:RESEND_FROM, env:WAMN_RECEIVING_DEV_BIN, env:WAMN_DEV_ENV_FLOW_HTTP_COMPONENT, env:WAMN_RECEIVING_DEV_HOST_BIN, env:WAMN_RECEIVING_DEV_NATS_URL, env:WAMN_DEV_ENV_NATS_TLS_CA, env:WAMN_DEV_ENV_NATS_TLS_CERT, env:WAMN_DEV_ENV_NATS_TLS_KEY, env:WAMN_EVT_STREAM_REPLICAS, env:WAMN_EVT_DUP_WINDOW_SECS, env:WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT, env:WAMN_ROUTE_HOST, bin:cargo-sqlx, bin:jq"]
async fn product_dev_command_applies_packages_and_cleans_up() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_IDENTITY_BINARY, env:RESEND_API_KEY, env:RESEND_FROM, env:WAMN_RECEIVING_DEV_BIN, env:WAMN_DEV_ENV_FLOW_HTTP_COMPONENT, env:WAMN_RECEIVING_DEV_HOST_BIN, env:WAMN_RECEIVING_DEV_NATS_URL, env:WAMN_DEV_ENV_NATS_TLS_CA, env:WAMN_DEV_ENV_NATS_TLS_CERT, env:WAMN_DEV_ENV_NATS_TLS_KEY, env:WAMN_EVT_STREAM_REPLICAS, env:WAMN_EVT_DUP_WINDOW_SECS, env:WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT, env:WAMN_ROUTE_HOST, bin:cargo-sqlx, bin:jq",
    );
    // The environment resets the control store of the whole server, so the test starts its own.
    let mut server = wamn_test_infrastructure::postgres::start(&[])?;
    let system_url = server.create_database("wamn_system")?.url().to_owned();
    let mut inputs = DevJourneyInputs::required()?;
    let provisioning = inputs.environment.nats.connect().await?;
    assert_dev_command(&system_url, &mut inputs, &provisioning).await
}

pub(super) async fn assert_dev_command(
    system_url: &str,
    inputs: &mut DevJourneyInputs,
    event_provisioning: &async_nats::Client,
) -> anyhow::Result<()> {
    // Keep the expensive product condition to one run. The filesystem adapter's
    // `filesystem_events_map_owned_inputs_and_ignore_generated_outputs` owns
    // affected-suffix behavior deterministically.
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    inputs.environment.local_artifacts.directory = root.join("local-artifacts");
    let mut admin = connect(system_url).await?;
    std::fs::write(
        root.join("environment.k"),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/integration/fixtures/local-environments/acme-receiving.k"
        )),
    )?;
    let environment = wamn_dev_control::dev::environment::provision(
        system_url,
        &mut admin,
        root,
        PLATFORM_DOMAIN,
        &wamn_environment::environment::document::EnvironmentDocument::compile(
            &root.join("environment.k"),
        )?,
    )
    .await?;
    let mut project = connect(&environment.route.database_url).await?;
    let system_acl_before = current_database_acl(&mut admin).await?;
    let durable_acl_before = current_database_acl(&mut project).await?;
    // The first run recreates the target, which closes this connection.

    let event_target = wamn_registry::Triple {
        org: environment.identity.org.clone(),
        project: environment.identity.project.clone(),
        env: wamn_registry::Env::new(environment.identity.environment.clone()),
    };
    wamn_broker::streams::provision(
        &async_nats::jetstream::new(event_provisioning.clone()),
        &event_target,
        inputs.environment.stream_replicas,
        Duration::from_secs(inputs.environment.dup_window_secs),
        &[],
    )
    .await?;

    let config = write_dev_config(
        root,
        system_url,
        &environment.route,
        &environment.credentials,
        &inputs.environment,
        &environment.identity,
    )?;

    let pat_count: i64 = sqlx::query("SELECT count(*) FROM identity.pats")
        .fetch_one(&mut admin)
        .await?
        .get(0);
    let command_result = async {
        let output = run_dev_product_command(inputs, &config).await?;
        // The literal command emits this result only after native workload
        // stop and supervised host reaping have both succeeded.
        verify_dev_command_output(&output)?;
        let mut watch =
            local_delivery::Watch::start(&inputs.wamn_binary, &repository_root()?, &config)?;
        let served = watch.served().await;
        let stopped = watch.stop().await;
        served?;
        stopped?;
        // Change only the input timestamp so Cargo recompiles the application
        // without changing authored source or its declared digest.
        std::fs::File::open(package_root().join("component/src/lib.rs"))?
            .set_modified(std::time::SystemTime::now())?;
        let rebuilt = run_dev_product_command(inputs, &config).await?;
        verify_dev_command_output(&rebuilt)?;
        let after: i64 = sqlx::query("SELECT count(*) FROM identity.pats")
            .fetch_one(&mut admin)
            .await?
            .get(0);
        anyhow::ensure!(
            after == pat_count,
            "the development lifecycle created a PAT"
        );
        let mut project = connect(&environment.route.database_url).await?;
        let system_acl_after = current_database_acl(&mut admin).await?;
        let durable_acl_after = current_database_acl(&mut project).await?;
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
        verify_dev_target_package_and_acl_state(&mut project).await?;

        Ok::<_, anyhow::Error>(())
    }
    .await;

    let stopped = environment.issuer.stop().await;

    command_result?;
    stopped?;
    Ok(())
}

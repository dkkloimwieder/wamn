//! Database install order on one fresh PostgreSQL 18 server, with each Job's roles.
//! The local-floor case uses an independent server because owner memberships are cluster-wide.
//! No cluster, package registry, or NATS server is required.
//! Job error output (wamn-gts3b) stays covered in platform/invite.rs by
//! a_passing_invite_logs_only_its_result and a_failed_invite_logs_the_verb_error.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::manifest::PackageManifest;
use wamn_control_provision::sql;
use wamn_environment_control::environment::{
    Platform, document::EnvironmentDocument, ensure::ensure_project_database,
};
use wamn_environment_control::provision_org::{OrgRecord, ProvisionOrgRequest, provision_org};
use wamn_environment_control::provision_project_env::{
    ProvisionProjectEnvRequest, provision_project_env,
};
use wamn_environment_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_environment_control::reconcile_run_plane;
use wamn_identity_client::PatIssuerConfig;
use wamn_package_control::apply_package::{ApplyPackageRequest, PackageRoot, apply_package};
use wamn_platform_identity::auth_changes::AuthChanges;
use wamn_test_infrastructure::{locked_database, scratch::ScratchRoot};

#[path = "../../environment/tests/support/install_invitation.rs"]
mod install_invitation;

const ORG: &str = "install-sequence";
const PROJECT: &str = "fixture";
const ENV: &str = "dev";
const OWNER: &str = "owner@example.test";
const DOMAIN: &str = "example.test";
const PACKAGE: &str = "wamn_inventory";
const SCHEMA: &str = "inventory";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        connection
            .await
            .expect("drive the install database connection");
    });
    Ok(client)
}

fn database_url(url: &str, database: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(url)?;
    url.set_path(database);
    Ok(url.into())
}

fn document() -> EnvironmentDocument {
    EnvironmentDocument::from_json(
        br#"{"org":"install-sequence","project":"fixture","env":"dev","target":"cloud","policy":"dev","integrations":{},"applications": {}}"#,
    ).unwrap()
}

fn platform(system_url: &str) -> Platform {
    Platform {
        system_database_url: system_url.into(),
        target: wamn_release_control::release_chart::Target {
            kubeconfig: PathBuf::new(),
            context: String::new(),
            namespace: "hosts".into(),
        },
        chart: PathBuf::new(),
        release_artifact_base: String::new(),
        registry_auth_file: PathBuf::new(),
        oci_ca_paths: Vec::new(),
        component_artifact_base: None,
        package_artifact_base: None,
        database_namespace: "platform".into(),
        database_host: None,
        database_port: 5432,
        host_group: None,
        pat_issuer: PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    }
}

fn org_request(system_url: &str) -> ProvisionOrgRequest {
    ProvisionOrgRequest {
        org: ORG.into(),
        template: wamn_control_registry::Template::trials(),
        pool: "wamn-pg".into(),
        cluster_namespace: "platform".into(),
        record: Some(OrgRecord {
            system_database_url: system_url.into(),
            admin: OWNER.into(),
        }),
    }
}

async fn bootstrap(admin: &Client, url: &str) -> anyhow::Result<String> {
    // CloudNativePG bootstraps the system owner and database before the Jobs.
    admin.batch_execute(
        "CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS",
    ).await?;
    admin
        .batch_execute(&sql::create_database_named_sql("wamn_system"))
        .await?;
    admin.batch_execute(
        "ALTER DATABASE wamn_system OWNER TO wamn_system; GRANT CONNECT ON DATABASE wamn_system TO wamn_system",
    ).await?;
    database_url(url, "wamn_system")
}

async fn step_4_7_provision(system_url: &str, work: &Path) -> anyhow::Result<String> {
    // The Job authenticates as postgres. provision-system installs as wamn_system.
    provision_system(&ProvisionSystemRequest {
        system_database_url: system_url.into(),
        platform_domain: DOMAIN.into(),
    })
    .await
    .context("4.7 provision-system as postgres, schema installation as wamn_system")?;
    provision_org(org_request(system_url))
        .await
        .context("4.7 provision-org --admin owner as wamn_system")?;
    let endpoint = url::Url::parse(system_url)?;
    wamn_environment_control::identity_issuer::provision_identity_issuer(
        wamn_environment_control::identity_issuer::IdentityIssuerRequest {
            issuer: "https://identity.example.test".into(),
            system_database_url: system_url.into(),
            prepare_generation: Some(wamn_control_provision::CredentialGeneration::A),
            retire_generation: None,
            abort_generation: None,
            emit_secret: Some(work.join("issuer.json")),
            db_host: Some(endpoint.host_str().unwrap().into()),
            db_port: endpoint.port().unwrap(),
            namespace: "identity".into(),
            secret_name: "identity-db".into(),
        },
    )
    .await
    .context("4.7 provision-identity-issuer as postgres")?;
    let path = work.join("apply.json");
    wamn_environment_control::provision_system::emit_apply_credential(
        system_url,
        endpoint.host_str().unwrap(),
        endpoint.port().unwrap(),
        &path,
    )
    .await?;
    wamn_environment_control::provision_project_env::secret_value(&path, "url")
}

async fn provision_project(system_url: &str, admin: &Client) -> anyhow::Result<String> {
    let outcome = provision_project_env(&ProvisionProjectEnvRequest {
        changes: AuthChanges::default(),
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENV.into(),
        policy: Some("dev".into()),
        target: "cloud".into(),
        disposable: None,
        system_database_url: Some(system_url.into()),
        cluster: Some("wamn-pg".into()),
        connection_limit: None,
        cluster_namespace: "platform".into(),
        namespace: "hosts".into(),
        secret_namespace: Some("hosts".into()),
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
        pat_issuer: PatIssuerConfig::default(),
        emit_management_author_pat_secret: None,
        emit_operator_pat_secret: None,
    })
    .await?;
    admin.batch_execute(&outcome.role_sql).await?;
    // The Database resource makes CNPG perform these two operations.
    admin
        .batch_execute(&sql::create_database_named_sql(&outcome.database))
        .await?;
    admin
        .batch_execute(&sql::set_database_owner_sql(&outcome.database))
        .await?;
    admin.batch_execute(&outcome.privilege_sql).await?;
    Ok(outcome.database)
}

async fn step_4_11_env_apply_reads_schema_baseline(
    platform: &Platform,
    admin: &Client,
    system_url: &str,
) -> anyhow::Result<()> {
    ensure_project_database(platform, &document(), &[], admin, system_url)
        .await
        .context(
            "4.11 env_apply_reads_schema_baseline as wamn_apply, then installs the project floor",
        )?;
    Ok(())
}

fn package_fixture(work: &Path) -> anyhow::Result<PackageRoot> {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../package/tests/fixtures/apply_package/base");
    let path = work.join("package");
    std::fs::create_dir_all(path.join("migrations"))?;
    for file in ["wamn.json", "migrations/0001_initial.sql"] {
        std::fs::copy(source.join(file), path.join(file))?;
    }
    wamn_test_infrastructure::fixture_digest::write_build_record(&path);
    let artifact_digest =
        Some(wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(&path)?);
    Ok(PackageRoot {
        path,
        artifact_digest,
    })
}

async fn step_4_13_package_job(project_url: &str, package: &PackageRoot) -> anyhow::Result<()> {
    apply_package(ApplyPackageRequest {
        package: package.clone(),
        database_url: project_url.into(),
        component_source: None,
        changes: AuthChanges::default(),
        org: ORG.into(),
        project: PROJECT.into(),
        environment: ENV.into(),
    })
    .await
    .context("4.13 package Job applies the package as postgres")?;
    let client = connect(project_url).await?;
    // Reuse the package fixture and the production builders used by provision/tests/cdc.rs.
    // Derive access evidence from the installed fixture, excluding the two control maps.
    let fields = client
        .query(
            "SELECT c.relname::text, array_agg(a.attname::text ORDER BY a.attnum) \
         FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
         JOIN pg_attribute a ON a.attrelid=c.oid \
         WHERE n.nspname=$1 AND c.relkind='r' AND a.attnum>0 AND NOT a.attisdropped \
         AND c.relname NOT IN ('wamn_entities','wamn_cdc_exclusions') GROUP BY c.relname",
            &[&SCHEMA],
        )
        .await?
        .into_iter()
        .map(|row| {
            wamn_schema_generator::DataAccessRelationFields::new(
                SCHEMA,
                row.get::<_, String>(0),
                row.get::<_, Vec<String>>(1),
            )
        })
        .collect::<Vec<_>>();
    let overlay = wamn_schema_generator::derive_data_access_overlay_from_relation_fields(
        &fields,
        &std::fs::read(package.path.join("wamn.json"))?,
    )?;
    let evidence = package
        .path
        .join(wamn_schema_generator::DATA_ACCESS_OVERLAY_PATH);
    std::fs::create_dir_all(evidence.parent().unwrap())?;
    std::fs::write(evidence, overlay.canonical_bytes())?;
    wamn_environment_control::reconcile_package_data_access::reconcile_package_data_access(
        wamn_environment_control::reconcile_package_data_access::ReconcilePackageDataAccessRequest {
            packages: vec![package.clone()],
            database_url: project_url.into(),
            org: ORG.into(),
            project: PROJECT.into(),
            environment: ENV.into(),
        },
    )
    .await
    .context("4.13 package Job reconciles data access as postgres")?;
    wamn_environment_control::reconcile_replica_identity::reconcile_package_replica_identity(
        wamn_environment_control::reconcile_replica_identity::ReconcileReplicaIdentityRequest {
            admin_database_url: project_url.into(),
            package: package.path.clone(),
            dry_run: false,
        },
    )
    .await
    .context("4.13 package Job reconciles replica identity as postgres")?;
    Ok(())
}

async fn step_4_17_env_apply_run_plane_and_package_job_maps(
    platform: &Platform,
    admin: &Client,
    system_url: &str,
    project_apply: &str,
    package: &PackageRoot,
    database: &str,
) -> anyhow::Result<()> {
    let manifest = PackageManifest::from_slice(&std::fs::read(package.path.join("wamn.json"))?)?;
    ensure_project_database(platform, &document(), &[manifest], admin, system_url)
        .await
        .context("4.17 env_apply_run_plane as the project database owner")?;
    let client = connect(project_apply).await?;
    client.batch_execute("SET ROLE wamn_db_owner").await?;
    let role = client.query_one(
        "SELECT session_user::text, current_user::text, rolsuper OR rolbypassrls FROM pg_roles WHERE rolname=current_user",
        &[],
    ).await?;
    ensure!(
        role.get::<_, String>(0) == "wamn_apply"
            && role.get::<_, String>(1) == "wamn_db_owner"
            && !role.get::<_, bool>(2),
        "4.17 expand must use the restricted login and owner role"
    );
    let owners = client
        .query(
            "SELECT nspname::text, pg_get_userbyid(nspowner)::text FROM pg_namespace \
             WHERE nspname IN ('wamn_authority', 'wamn_run') ORDER BY nspname",
            &[],
        )
        .await?;
    ensure!(
        owners.len() == 2
            && owners
                .iter()
                .all(|row| row.get::<_, String>(1) == "wamn_db_owner"),
        "4.17 env_apply_run_plane must keep wamn_authority and wamn_run under the database owner"
    );
    let cdc = "install_sequence_cdc";
    admin
        .batch_execute(&sql::ensure_replication_role_sql(cdc, "cdc-test-password"))
        .await?;
    // The existing CDC case covers both creation orders and repeated repairs.
    // Here the real package Job created both maps before this restricted expand.
    client
        .batch_execute(&sql::grant_replication_access_sql(database, cdc, SCHEMA))
        .await
        .context("4.17 env_apply_grants_maps_created_by_step_4_13_package_job")?;
    for table in ["wamn_entities", "wamn_cdc_exclusions"] {
        ensure!(
            client
                .query_one(
                    "SELECT has_table_privilege($1, $2, 'SELECT')",
                    &[&cdc, &format!("{SCHEMA}.{table}")]
                )
                .await?
                .get::<_, bool>(0),
            "4.17 CDC reader cannot read {table}"
        );
    }
    Ok(())
}

fn release(packages: BTreeSet<wamn_catalog::PackageCoordinate>) -> wamn_catalog::ServingManifest {
    wamn_catalog::ServingManifest {
        composed_entry: None,
        format_version: wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        release: wamn_catalog::ServingRelease { packages },
        components: BTreeSet::new(),
        routes: BTreeSet::new(),
        attachments: BTreeMap::new(),
        host_routes: BTreeSet::new(),
    }
}

async fn release_cache(
    project_url: &str,
    manifest: &wamn_catalog::ServingManifest,
) -> anyhow::Result<()> {
    let env_key = wamn_control_provision::project_env_key(ORG, PROJECT, ENV);
    let digest = manifest.digest();
    wamn_environment_control::environment::stage::write_release(
        project_url,
        &env_key,
        digest.as_str(),
        manifest,
    )
    .await?;
    let client = connect(project_url).await?;
    client.batch_execute("SET ROLE wamn_db_owner").await?;
    let bytes: Vec<u8> = client
        .query_one(
            "SELECT canonical_bytes FROM catalog.releases WHERE env_key=$1 AND manifest_digest=$2",
            &[&env_key, &digest.as_str()],
        )
        .await?
        .get(0);
    ensure!(
        bytes == manifest.canonical_bytes(),
        "release cache contains different bytes"
    );
    Ok(())
}

#[tokio::test]
async fn install_sequence_4_7_through_4_19_1_with_job_roles() -> anyhow::Result<()> {
    let database = locked_database::database(wamn_test_postgres::database);
    let admin = connect(&database).await?;
    let system_url = bootstrap(&admin, &database).await?;
    let work = ScratchRoot::create()?;
    let apply_url = step_4_7_provision(&system_url, work.path()).await?;
    let database_name = provision_project(&apply_url, &admin)
        .await
        .context("4.11 record and bootstrap the project database")?;
    let platform = platform(&apply_url);
    step_4_11_env_apply_reads_schema_baseline(&platform, &admin, &system_url).await?;
    let project_admin = database_url(&system_url, &database_name)?;
    let project_apply = database_url(&apply_url, &database_name)?;
    let package = package_fixture(work.path())?;
    step_4_13_package_job(&project_admin, &package).await?;
    step_4_17_env_apply_run_plane_and_package_job_maps(
        &platform,
        &admin,
        &system_url,
        &project_apply,
        &package,
        &database_name,
    )
    .await?;
    release_cache(
        &project_apply,
        &release(BTreeSet::from([wamn_catalog::PackageCoordinate::new(
            PACKAGE, "1.0.0",
        )?])),
    )
    .await
    .context("4.17 env_apply_release_cache_passes_owner_row_security")?;
    install_invitation::step_4_19_1_owner_invite(
        &system_url,
        org_request(&system_url),
        work.path(),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn local_floor_run_plane_and_release_cache_as_database_owner() -> anyhow::Result<()> {
    // The separate server keeps env apply's cluster-wide grant from hiding a local-floor defect.
    let mut server = wamn_test_postgres::start(&[])?;
    let database = server.create_database("local_floor")?;
    let client = connect(database.url()).await?;
    client
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await?;
    client
        .batch_execute(&sql::set_database_owner_sql("local_floor"))
        .await?;
    wamn_dev_control::dev::environment::install_journey_platform_floor(&client, DOMAIN)
        .await
        .context("local_floor installs the platform schema as the database owner")?;
    reconcile_run_plane::reconcile(
        &client,
        &wamn_schema_control::BareSchemaName::new("wamn_run")?,
        true,
    )
    .await
    .context(
        "4.17 local_floor_run_plane_as_database_owner without wamn_authority permission denial",
    )?;
    release_cache(database.url(), &release(BTreeSet::new()))
        .await
        .context("4.17 local_floor_release_cache_passes_owner_row_security")?;
    Ok(())
}

//! Production-shaped structural bootstrap for one disposable verification database.

use std::error::Error;
use std::fmt;

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::{DB_OWNER_ROLE, sql};
use wamn_pg_core::Identifier;
use wamn_schema_control::BareSchemaName;

use crate::reconcile_run_plane;

pub(crate) const RUN_SCHEMA: &str = "wamn_run";
const APP_SCHEMA_SQL: &str = include_str!("../../../../deploy/sql/app-schema.sql");

/// Exact database-local changes made by one verification-world bootstrap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerificationWorldBootstrapReceipt {
    package_owner_create_granted: bool,
    run_plane_actions: usize,
    application_authorization_installed: bool,
}

/// Contextual failure from the verification-world bootstrap boundary.
#[derive(Debug)]
pub struct VerificationWorldBootstrapError(anyhow::Error);

impl fmt::Display for VerificationWorldBootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Error for VerificationWorldBootstrapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.0.as_ref())
    }
}

impl VerificationWorldBootstrapReceipt {
    /// Whether every database-local structural surface was already present.
    pub const fn is_noop(self) -> bool {
        !self.package_owner_create_granted
            && self.run_plane_actions == 0
            && !self.application_authorization_installed
    }
}

/// Bootstrap the exact Migrate, Admit, and Gate surfaces in one verification database.
///
/// The URL is the only target. No durable-environment coordinate or policy fact
/// can enter this boundary. The order mirrors production prerequisites:
/// package-owner/effect-writer roles, package-owner database authority, the
/// converged catalog + run plane, the static application-authorization floor,
/// then the management-admitter surface that names those relations.
pub async fn bootstrap(
    verification_database_url: &str,
) -> Result<VerificationWorldBootstrapReceipt, VerificationWorldBootstrapError> {
    bootstrap_inner(verification_database_url)
        .await
        .map_err(VerificationWorldBootstrapError)
}

async fn bootstrap_inner(
    verification_database_url: &str,
) -> anyhow::Result<VerificationWorldBootstrapReceipt> {
    let (mut client, connection) = tokio_postgres::connect(verification_database_url, NoTls)
        .await
        .context("connect to the disposable verification database")?;
    let connection_task = tokio::spawn(connection);
    let result = bootstrap_with_client(&mut client).await;
    drop(client);
    if result.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join verification-world database connection")?
            .context("drive verification-world database connection")?;
    }
    result
}

async fn bootstrap_with_client(
    client: &mut Client,
) -> anyhow::Result<VerificationWorldBootstrapReceipt> {
    client
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .context("ensure the package-owner role")?;
    let package_owner_create_granted = ensure_package_owner_create(client).await?;

    // The run-plane record names this stable role in grants and policies, so
    // the identity must exist before the from-zero reconciler applies it.
    client
        .batch_execute(&sql::ensure_effect_writer_acl_role_sql())
        .await
        .context("ensure the effect-writer role before run-plane reconciliation")?;
    let run_schema = BareSchemaName::new(RUN_SCHEMA)
        .expect("the repository-owned run schema is a valid bare identifier");
    let run_plane = reconcile_run_plane::reconcile(client, &run_schema, true)
        .await
        .context("converge the verification catalog and run plane")?;

    let application_authorization_installed = ensure_application_authorization(client).await?;

    client
        .batch_execute(&sql::grant_management_admitter_surface_sql(RUN_SCHEMA))
        .await
        .context("converge the management-admitter verification surface")?;

    Ok(VerificationWorldBootstrapReceipt {
        package_owner_create_granted,
        run_plane_actions: run_plane.actions.len(),
        application_authorization_installed,
    })
}

async fn ensure_package_owner_create(client: &Client) -> anyhow::Result<bool> {
    let already_granted: bool = client
        .query_one(
            "SELECT pg_catalog.has_database_privilege($1, current_database(), 'CREATE')",
            &[&DB_OWNER_ROLE],
        )
        .await
        .context("read package-owner database authority")?
        .get(0);
    if already_granted {
        return Ok(false);
    }
    let database: String = client
        .query_one("SELECT current_database()", &[])
        .await
        .context("read verification database identity")?
        .get(0);
    let database = Identifier::new(database).context("validate verification database identity")?;
    client
        .batch_execute(&format!(
            "GRANT CREATE ON DATABASE {} TO \"{DB_OWNER_ROLE}\"",
            database.quoted()
        ))
        .await
        .context("grant the package-owner role database-local CREATE authority")?;
    Ok(true)
}

async fn ensure_application_authorization(client: &mut Client) -> anyhow::Result<bool> {
    let present: bool = client
        .query_one(
            "SELECT pg_catalog.to_regnamespace('app_system') IS NOT NULL",
            &[],
        )
        .await
        .context("read application-authorization schema presence")?
        .get(0);
    if !present {
        let transaction = client
            .transaction()
            .await
            .context("begin application-authorization installation")?;
        transaction
            .batch_execute(APP_SCHEMA_SQL)
            .await
            .context("install the production application-authorization schema")?;
        transaction
            .commit()
            .await
            .context("commit application-authorization installation")?;
    }
    client
        .batch_execute(&wamn_control_provision::operation_grants::operation_grant_floor_check_sql())
        .await
        .context("verify the application-authorization floor")?;
    Ok(!present)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use wit_component::{ComponentEncoder, StringEncoding, dummy_module, embed_component_metadata};
    use wit_parser::{ManglingAndAbi, Resolve};

    use super::*;
    use crate::apply_package::{self, ApplyPackageArgs};
    use crate::dev::config::{DevConfig, parse_config};
    use crate::dev::verification_database;
    use crate::push_component::{
        AdmitComponentArgs, admit_component, project_admitted_component_for_verification,
    };

    const LIVE_URL: &str = "WAMN_DEV_VERIFICATION_PG_URL";
    const TENANT: &str = "dev-verification-bootstrap";

    async fn connect(url: &str) -> Client {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .expect("connect to the fresh disposable verification database");
        tokio::spawn(async move {
            let _connection_result = connection.await;
        });
        client
    }

    fn database_url(base: &str, database: &str) -> String {
        let mut url = url::Url::parse(base).expect("parse live database URL");
        url.set_path(&format!("/{database}"));
        url.set_query(None);
        url.into()
    }

    fn live_config(verification_url: &str) -> DevConfig {
        let process = std::process::id();
        let document = json!({
            "verification_database_url": verification_url,
            "target_database_url": database_url(verification_url, &format!("wamn_dev_target_{process}")),
            "system_database_url": database_url(verification_url, &format!("wamn_dev_system_{process}")),
            "identity_database_url": database_url(verification_url, &format!("wamn_dev_identity_{process}")),
            "guest_database_url": database_url(verification_url, &format!("wamn_dev_guest_{process}")),
            "executor_platform_database_url": database_url(verification_url, &format!("wamn_dev_platform_{process}")),
            "http_admitter_database_url": database_url(verification_url, &format!("wamn_dev_admitter_{process}")),
            "event_materializer_database_url": database_url(verification_url, &format!("wamn_dev_materializer_{process}")),
            "scheduler_nats_url": "nats://127.0.0.1:4222",
            "event_nats_url": "nats://127.0.0.1:4223",
            "component_artifact_base": "127.0.0.1:5000/wamn/components",
            "release_artifact_base": "127.0.0.1:5001/wamn/releases",
            "registry_auth_file": "/run/secrets/registry.json",
            "insecure_registry": true,
            "gate_url": "http://127.0.0.1:8080/authoring",
            "gate_bearer_token": "live-test-token",
            "route_host": "receiving.localhost",
            "flow_http_workload_image": "127.0.0.1:5002/wamn/flow-http:dev",
            "package_sources": [],
            "effective_release_id": 1,
            "tenant": "00000000-0000-0000-0000-000000000001",
            "catalog": "default",
            "environment": "receiving-dev",
            "org": "acme",
            "project": "receiving",
            "schema": "receiving",
            "host_group": "wamn-dev-receiving",
            "host_name": "wamn-dev-receiving-1",
            "runner": "wamn-dev-receiving-1",
            "host_binary": "/opt/wamn/bin/wamn-host",
            "wasmtime_cache_dir": "/tmp/wamn-dev-cache",
        });
        parse_config(&serde_json::to_vec(&document).expect("serialize live config"))
            .expect("parse live config")
    }

    fn repository_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn receiving_admission_files(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("wall clock follows the Unix epoch")
            .as_nanos();
        let scratch = std::env::temp_dir().join(format!(
            "wamn-verification-world-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&scratch).expect("create unique admission scratch directory");

        let wit = root.join("components/data/receiving-data/wit");
        let mut resolve = Resolve::new();
        let (package, _) = resolve
            .push_dir(&wit)
            .expect("parse the real Receiving WIT");
        let world = resolve
            .select_world(&[package], Some("receiving"))
            .expect("select the real Receiving component world");
        let mut module = dummy_module(&resolve, world, ManglingAndAbi::Standard32);
        embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8)
            .expect("embed exact Receiving component metadata");
        let component = ComponentEncoder::default()
            .module(&module)
            .expect("admit the Receiving fixture core module")
            .validate(true)
            .encode()
            .expect("encode the structurally exact Receiving component");
        let component_path = scratch.join("receiving.wasm");
        std::fs::write(&component_path, component).expect("write Receiving component bytes");

        let declaration_source =
            root.join("packages/receiving/publication/components/receiving.json.in");
        let mut declaration: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&declaration_source).expect("read Receiving component declaration"),
        )
        .expect("parse Receiving component declaration");
        declaration["scope"]["tenant-id"] = serde_json::Value::String(TENANT.to_owned());
        let declaration_path = scratch.join("receiving.json");
        std::fs::write(
            &declaration_path,
            serde_json::to_vec(&declaration).expect("serialize Receiving component declaration"),
        )
        .expect("write Receiving component declaration");

        (scratch, component_path, declaration_path)
    }

    async fn runtime_fact_count(client: &Client) -> i64 {
        client
            .query_one(
                "SELECT (SELECT count(*) FROM wamn_run.environment_policies) \
                      + (SELECT count(*) FROM wamn_run.runs) \
                      + (SELECT count(*) FROM wamn_run.run_queue) \
                      + (SELECT count(*) FROM wamn_run.effect_attempts) \
                      + (SELECT count(*) FROM wamn_run.effect_attempt_dispatches) \
                      + (SELECT count(*) FROM wamn_run.effect_attempt_outcomes) \
                      + (SELECT count(*) FROM catalog.effective_releases) \
                      + (SELECT count(*) FROM catalog.effective_release_packages) \
                      + (SELECT count(*) FROM catalog.connection_bindings) \
                      + (SELECT count(*) FROM catalog.wirings) \
                      + (SELECT count(*) FROM catalog.wiring_activation) \
                      + (SELECT count(*) FROM catalog.release_components)",
                &[],
            )
            .await
            .expect("count policy, release, binding, wiring, and runtime facts")
            .get(0)
    }

    async fn prove_fresh_world(url: &str) {
        let client = connect(url).await;
        let major: i32 = client
            .query_one(
                "SELECT current_setting('server_version_num')::int / 10000",
                &[],
            )
            .await
            .expect("read PostgreSQL major version")
            .get(0);
        assert_eq!(major, 18, "the verification proof requires PostgreSQL 18");
        let public_connect: bool = client
            .query_one(
                "SELECT EXISTS (\
                   SELECT 1 \
                   FROM pg_catalog.pg_database AS database \
                   CROSS JOIN LATERAL pg_catalog.aclexplode(\
                     COALESCE(database.datacl, pg_catalog.acldefault('d', database.datdba))\
                   ) AS acl \
                   WHERE database.datname = pg_catalog.current_database() \
                     AND acl.grantee = 0 \
                     AND acl.privilege_type = 'CONNECT'\
                 )",
                &[],
            )
            .await
            .expect("inspect verification database CONNECT floor")
            .get(0);
        assert!(!public_connect, "PUBLIC CONNECT must be revoked");
        let preexisting: i64 = client
            .query_one(
                "SELECT count(*) FROM pg_catalog.pg_namespace \
                  WHERE nspname IN ('catalog', 'app_system', 'wamn_run', 'receiving')",
                &[],
            )
            .await
            .expect("prove the supplied database is fresh")
            .get(0);
        assert_eq!(preexisting, 0, "the verification database is not fresh");

        let first = bootstrap(url).await.expect("bootstrap the fresh world");
        assert!(!first.is_noop());
        assert_eq!(runtime_fact_count(&client).await, 0);
        let again = bootstrap(url).await.expect("replay verification bootstrap");
        assert!(again.is_noop(), "verification bootstrap did not converge");
        assert_eq!(runtime_fact_count(&client).await, 0);

        let root = repository_root();
        for package in [
            root.join("packages/receiving"),
            root.join("packages/client_acme_receiving"),
        ] {
            apply_package::run(ApplyPackageArgs {
                package,
                database_url: url.to_owned(),
                tenant: TENANT.to_owned(),
            })
            .await
            .expect("apply one canonical Receiving package");
        }
        assert_eq!(runtime_fact_count(&client).await, 0);
        let package_count: i64 = client
            .query_one("SELECT count(*) FROM catalog.packages", &[])
            .await
            .expect("count applied package coordinates")
            .get(0);
        assert_eq!(package_count, 2);

        let (scratch, component_bytes, declaration) = receiving_admission_files(&root);
        let admission = admit_component(AdmitComponentArgs {
            package: root.join("packages/receiving"),
            component_bytes,
            declaration,
            admitted_platform_packages: vec!["wamn:postgres".to_owned()],
        })
        .expect("admit the exact Receiving package/component contract");
        project_admitted_component_for_verification(&admission, &url)
            .await
            .expect("project the opaque admission receipt into the verification world");
        let component_count: i64 = client
            .query_one("SELECT count(*) FROM catalog.component_library", &[])
            .await
            .expect("count exact admitted component facts")
            .get(0);
        assert_eq!(component_count, 1);
        assert_eq!(runtime_fact_count(&client).await, 0);

        std::fs::remove_dir_all(scratch).expect("remove admission scratch directory");
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL 18 through WAMN_DEV_VERIFICATION_PG_URL"]
    async fn lifecycle_bootstraps_then_accepts_packages_and_one_exact_admission() {
        let url = std::env::var(LIVE_URL)
            .expect("WAMN_DEV_VERIFICATION_PG_URL must name a disposable PostgreSQL 18 database");
        let config = live_config(&url);
        verification_database::run(&config, |verification_url| async move {
            prove_fresh_world(&verification_url).await;
            Ok::<_, std::convert::Infallible>(())
        })
        .await
        .expect("create and clean up the disposable verification database")
        .expect("verification-world proof is infallible");
    }
}

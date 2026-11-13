//! The reset worker restores the pinned database inputs.

use sqlx::Connection as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// A loopback platform NATS with mutual TLS. Revocation deletes the PAT's key
/// from the auth bucket that hosts watch, so the verb needs this connection.
struct PlatformNatsServer {
    child: std::process::Child,
    url: String,
    client: wamn_test_infrastructure::nats_tls::ClientCertificate,
}

impl Drop for PlatformNatsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_platform_nats(directory: &Path) -> PlatformNatsServer {
    use wamn_test_infrastructure::nats_tls::{Authority, DEVELOPMENT_USER, full_permission_user};
    let binary = std::env::var_os("WAMN_NATIVE_C_NATS_BIN")
        .expect("set WAMN_NATIVE_C_NATS_BIN to a nats-server executable");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(directory)
        .expect("create the NATS directory");
    let authority = Authority::new(directory).expect("create the test NATS authority");
    let client = authority
        .client(DEVELOPMENT_USER)
        .expect("sign the platform client certificate");
    let configuration = directory.join("nats.conf");
    std::fs::write(
        &configuration,
        serde_json::json!({
            "tls": authority.server_tls(),
            "authorization": { "users": [full_permission_user(DEVELOPMENT_USER)] }
        })
        .to_string(),
    )
    .expect("write the NATS configuration");
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("reserve a NATS port");
    let log = std::fs::File::create(directory.join("nats.log")).expect("create the NATS log");
    let child = std::process::Command::new(binary)
        .args(["--jetstream", "--addr", "127.0.0.1", "--port"])
        .arg(address.port().to_string())
        .arg("--store_dir")
        .arg(directory.join("data"))
        .arg("--config")
        .arg(&configuration)
        .stdin(Stdio::null())
        .stdout(log.try_clone().expect("share the NATS log"))
        .stderr(log)
        .spawn()
        .expect("start nats-server");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::net::TcpStream::connect(address).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "nats-server did not listen"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    PlatformNatsServer {
        child,
        url: format!("nats://{address}"),
        client,
    }
}

#[tokio::test]
async fn reset_worker_restores_a_recreated_database() -> anyhow::Result<()> {
    use wamn_environment_control::environment::{
        Platform,
        document::EnvironmentDocument,
        ensure,
        sql::{self, ExpectedDatabase},
    };

    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let mut postgres = wamn_test_postgres::start(&[])?;
    let system = postgres.create_database("wamn_system")?;
    let mut admin = wamn_postgres::connect(system.url()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE wamn_system NOLOGIN NOINHERIT; ALTER DATABASE wamn_system OWNER TO wamn_system")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    wamn_environment_control::provision_system::provision_system(
        &wamn_environment_control::provision_system::ProvisionSystemRequest {
            system_database_url: system.url().into(),
            platform_domain: "example.invalid".into(),
        },
    )
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "BEGIN; SET LOCAL ROLE wamn_system; {} UPDATE registry.environments SET disposable=true; COMMIT;",
        wamn_control_provision::test_database::seed_org_project_env_sql("acme", "apps", "dev"),
    ))).execute(&mut admin).await.map(drop)?;
    let document = EnvironmentDocument::from_json(br#"{"org":"acme","project":"apps","env":"dev","target":"cloud","policy":"dev","applications":{}}"#)?;
    let instance = wamn_environment_control::provision_project_env::read_project_env_instance(
        system.url(),
        &document.triple(),
    )
    .await?;
    let name = wamn_control_provision::project_env_database_name("acme", "apps", "dev", &instance);
    let apply_input = wamn_test_infrastructure::credentials::apply_login(system.url()).await?;
    let platform = Platform {
        system_database_url: apply_input,
        target: wamn_release_control::release_chart::Target {
            kubeconfig: PathBuf::new(),
            context: String::new(),
            namespace: "hosts".into(),
        },
        chart: PathBuf::new(),
        release_artifact_base: "registry.invalid/releases".into(),
        registry_token_metadata: false,
        registry_auth_file: scratch.path().join("registry.json"),
        oci_ca_paths: Vec::new(),
        insecure_registry: false,
        package_artifact_base: None,
        component_artifact_base: None,
        database_namespace: "platform".into(),
        database_host: None,
        database_port: 5432,
        host_group: None,
        pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    };
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&name)
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    ensure::ensure_project_database(&platform, &document, &[], &mut admin, system.url()).await?;
    sql::reset::prepare_retained(&platform, &document, system.url()).await?;
    let expected_database = ExpectedDatabase::read(&platform, &document).await?;
    let file = scratch.path().join("environment.json");
    let expected = scratch.path().join("database.json");
    std::fs::write(&file, document.canonical_json()?)?;
    std::fs::write(&expected, serde_json::to_vec(&expected_database)?)?;
    // CNPG supplies an empty database; the SQL worker must restore its pinned inputs.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP DATABASE {} WITH (FORCE)",
            postgres_protocol::escape::escape_identifier(&name),
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&name)
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    let nats = start_platform_nats(&scratch.path().join("nats"));
    wamn_broker::kv_memory::create_buckets(&nats.client.connect(&nats.url).await?).await?;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_wamn-ctl"))
        .args(["env", "reset"])
        .arg(&file)
        .arg(&expected)
        .env("WAMN_APPLY_DATABASE_URL", &platform.system_database_url)
        .env("WAMN_SYSTEM_ADMIN_URL", system.url())
        .env(
            "WAMN_RELEASE_ARTIFACT_BASE",
            &platform.release_artifact_base,
        )
        .env("WAMN_REGISTRY_AUTH_FILE", &platform.registry_auth_file)
        .env("WAMN_NATS_URL", &nats.url)
        .env("WAMN_NATS_TLS_CA", &nats.client.ca)
        .env("WAMN_NATS_TLS_CERT", &nats.client.cert)
        .env("WAMN_NATS_TLS_KEY", &nats.client.key)
        .env_remove("WAMN_RELEASE_HOST_GROUP")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state = sql::schema::state(&platform, &document).await?;
    assert_eq!(
        state.initialization_identity.as_deref(),
        expected_database.pending.as_deref(),
        "the worker must restore the pending initialization before reconnect"
    );
    Ok(())
}

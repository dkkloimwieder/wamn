//! First-time provisioning sends PAT writes through the separate HTTPS authority.

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use wamn_control_provision::PlatformComponent;
use wamn_dev_control::dev::environment::{
    JourneyScope, connect, provision_journey_control, provision_route,
};
use wamn_platform_identity::{PrincipalType, authenticate_pat};

struct Files(PathBuf);

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

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_IDENTITY_BINARY, env:WAMN_NATIVE_C_NATS_BIN"]
async fn cli_bootstrap_mints_first_service_pats_over_https() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_IDENTITY_BINARY, env:WAMN_NATIVE_C_NATS_BIN",
    );
    wamn_dev_control::dev::pat_issuer::identity_binary().expect("find the wamn-identity binary");
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    database
        .execute(&[
            "DROP DATABASE IF EXISTS wamn_system WITH (FORCE)",
            "CREATE DATABASE wamn_system",
        ])
        .expect("create the system database on the test server");
    let mut url = url::Url::parse(database.url()).expect("test database URL");
    url.set_path("/wamn_system");
    let url = url.to_string();
    let (admin, driver) = connect(&url)
        .await
        .expect("connect the disposable database");
    let safe: bool = admin.query_one(
        "SELECT current_database()='wamn_system' AND current_setting('server_version_num')::int \
         BETWEEN 180000 AND 189999 AND rolsuper FROM pg_roles WHERE rolname=current_user", &[])
        .await.expect("read test preconditions").get(0);
    assert!(
        safe,
        "use only a disposable PostgreSQL 18 wamn_system administrator"
    );
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock")
        .as_nanos();
    let files = Files(
        std::env::temp_dir().join(format!("wamn-pat-bootstrap-{}-{nonce}", std::process::id())),
    );
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&files.0)
        .expect("create private test directory");
    provision_journey_control(&JourneyScope::receiving(), &url, admin.as_ref())
        .await
        .expect(
            "provision system authority; it requires a built wamn-identity binary beside wamn-ctl or named by WAMN_IDENTITY_BINARY",
        );
    // The server records the actual inserting login. A direct CLI INSERT must
    // fail this test even if it creates an otherwise valid PAT.
    admin
        .batch_execute(
            "CREATE SCHEMA pat_test; \
         CREATE TABLE pat_test.writers (writer name NOT NULL); \
         CREATE FUNCTION pat_test.record_writer() RETURNS trigger \
         LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog AS $$ \
         BEGIN INSERT INTO pat_test.writers VALUES (session_user); RETURN NEW; END $$; \
         REVOKE ALL ON FUNCTION pat_test.record_writer() FROM PUBLIC; \
         CREATE TRIGGER pat_test_writer AFTER INSERT ON identity.pats \
         FOR EACH ROW EXECUTE FUNCTION pat_test.record_writer();",
        )
        .await
        .expect("install test-only writer observation");
    let route = provision_route(
        &JourneyScope::receiving(),
        &url,
        admin.as_ref(),
        &files.0,
        Some(&files.0.join("management-author-pat.json")),
    )
    .await
    .expect("mint first PATs through the native identity service");
    let writers = admin
        .query(
            "SELECT writer::text FROM pat_test.writers ORDER BY writer",
            &[],
        )
        .await
        .expect("read actual PAT writer identities");
    assert_eq!(
        writers.len(),
        2,
        "both provisioning PATs reached the database"
    );
    for writer in writers {
        let writer: String = writer.get(0);
        assert!(
            writer.starts_with("wamn_identity_issuer_") && writer.ends_with('a'),
            "PAT minting bypassed the scoped identity process"
        );
    }
    for token in [
        &route.token,
        route.management_token.as_ref().expect("management token"),
    ] {
        let principal = authenticate_pat(admin.as_ref(), token)
            .await
            .ok()
            .flatten()
            .expect("full PAT authentication");
        assert_eq!(
            principal.principal().principal_type(),
            PrincipalType::Service
        );
        let plaintext: bool = admin
            .query_one(
                "SELECT EXISTS (SELECT FROM identity.pats WHERE token_hash=$1)",
                &[token],
            )
            .await
            .expect("read digest storage predicate")
            .get(0);
        assert!(!plaintext, "the database stored a raw PAT");
    }
    for name in ["operator-pat.json", "management-author-pat.json"] {
        assert_eq!(
            std::fs::metadata(files.0.join(name))
                .expect("Secret metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "secret output must be owner-only"
        );
    }
    assert!(
        std::fs::read_dir(&files.0)
            .expect("private output directory")
            .all(|entry| !entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".pat-issuer-")),
        "bootstrap certificate files remain"
    );
    let active: i64 = admin.query_one(
        "SELECT count(*) FROM pg_roles WHERE rolname LIKE 'wamn_identity_issuer_%' AND rolcanlogin", &[])
        .await.expect("read issuer login state").get(0);
    assert_eq!(active, 0, "bootstrap left active issuer authority");
    // Every provisioning write stamps wamn:provisioning: the service principals
    // and roles from wamn-ctl, and the tokens from wamn-identity.
    let provisioning = PlatformComponent::Provisioning.principal_id().to_string();
    let stamped: bool = admin
        .query_one(
            "SELECT (SELECT bool_and(created_by = $1::text::uuid AND updated_by = $1::text::uuid) \
                     FROM identity.principals) \
                AND (SELECT bool_and(created_by = $1::text::uuid AND updated_by = $1::text::uuid) \
                     FROM identity.project_roles) \
                AND (SELECT bool_and(created_by = $1::text::uuid AND updated_by = $1::text::uuid \
                                     AND created_at = updated_at) \
                     FROM identity.pats) \
                AND (SELECT count(*) = 2 FROM identity.pats)",
            &[&provisioning],
        )
        .await
        .expect("read provisioning stamps")
        .get(0);
    assert!(stamped, "provisioning writes must stamp wamn:provisioning");
    let issued_at: String = admin
        .query_one(
            "SELECT created_at::text FROM identity.pats WHERE token_prefix = $1",
            &[&route.token_prefix],
        )
        .await
        .expect("read the route PAT issuance time")
        .get(0);
    let nats = start_platform_nats(&files.0.join("nats"));
    wamn_broker::kv_memory::create_buckets(
        &nats
            .client
            .connect(&nats.url)
            .await
            .expect("connect the platform NATS"),
    )
    .await
    .expect("create the auth memory buckets");
    let revoked = std::process::Command::new(env!("CARGO_BIN_EXE_wamn-ctl"))
        .args([
            "provision-project-env",
            "--revoke-pat-prefix",
            &route.token_prefix,
        ])
        .env("WAMN_SYSTEM_ADMIN_URL", &url)
        .env("WAMN_NATS_URL", &nats.url)
        .env("WAMN_NATS_TLS_CA", &nats.client.ca)
        .env("WAMN_NATS_TLS_CERT", &nats.client.cert)
        .env("WAMN_NATS_TLS_KEY", &nats.client.key)
        .stdin(Stdio::null())
        .output()
        .expect("run the compiled PAT revocation verb");
    assert!(
        revoked.status.success(),
        "the PAT revocation verb failed: {}",
        String::from_utf8_lossy(&revoked.stderr)
    );
    let revocation: bool = admin
        .query_one(
            "SELECT revoked_at IS NOT NULL AND created_at::text = $2 \
                AND created_by = $1::text::uuid AND updated_by = $1::text::uuid \
                AND updated_at > created_at \
             FROM identity.pats WHERE token_prefix = $3",
            &[&provisioning, &issued_at, &route.token_prefix],
        )
        .await
        .expect("read the revocation stamps")
        .get(0);
    assert!(
        revocation,
        "the revocation keeps the created pair and stamps wamn:provisioning"
    );
    assert!(
        authenticate_pat(admin.as_ref(), &route.token)
            .await
            .expect("authenticate the revoked PAT")
            .is_none(),
        "the issued PAT ignored revocation"
    );
    println!(
        "PAT_BOOTSTRAP result=pass service_writers=2 plaintext_db=absent active_issuer_logins=0 private_files=removed"
    );
    drop(admin);
    driver.abort();
}

#[tokio::test]
#[ignore = "requires: env:RESEND_API_KEY, env:RESEND_FROM"]
#[expect(
    clippy::ok_expect,
    reason = "do not print credential-bearing upstream errors in fixture assertions"
)]
async fn development_identity_survives_target_recreation_and_owned_teardown() {
    const PASSWORD: &str = "owned-development-identity-fixture-password";
    use serde_json::{Value, json};
    use wamn_dev_control::dev::environment::{
        DEVELOPER_EMAIL, ENVIRONMENT, JourneyScope, ORG, PROJECT, provision,
        reconcile_journey_run_plane,
    };
    use wamn_platform_identity::{
        assign_project_role, create_or_reuse_user, grant_project_env_membership,
    };

    async fn assert_application_role(url: &str, user: &str) {
        let (project, task) = connect(url).await.ok().expect("application connection");
        let count: i64 = project
            .query_one(
                "SELECT count(*) FROM platform.user_role WHERE user_id=$1::text::uuid AND role_name='admin'",
                &[&user],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            count, 1,
            "reconciliation projects the developer's admin grant"
        );
        task.abort();
    }
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:RESEND_API_KEY, env:RESEND_FROM",
    );
    let mut postgres = wamn_test_postgres::start(&[]).expect("start owned PostgreSQL");
    let system = postgres
        .create_database("wamn_system")
        .expect("system database");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let files = Files(
        std::env::temp_dir().join(format!("wamn-dev-identity-{}-{nonce}", std::process::id())),
    );
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&files.0)
        .unwrap();
    let (admin, driver) = connect(system.url()).await.ok().expect("system connection");
    let document = wamn_environment_control::environment::document::EnvironmentDocument::compile(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/integration/fixtures/local-environments/acme-receiving.k"),
    )
    .unwrap();
    let environment = provision(system.url(), &admin, &files.0, "example.invalid", &document)
        .await
        .unwrap_or_else(|error| panic!("managed provisioning failed: {error}"));
    let endpoint = environment.issuer.args.endpoint.as_ref().unwrap().clone();
    let ca = std::fs::read(environment.issuer.args.server_ca.as_ref().unwrap()).unwrap();
    let http = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .tls_certs_only(reqwest::Certificate::from_pem_bundle(&ca).unwrap())
        .build()
        .unwrap();
    let target: Value =
        serde_json::from_slice(&std::fs::read(files.0.join("identity-target.json")).unwrap())
            .unwrap();
    let actor = PlatformComponent::Provisioning.principal_id().to_string();
    admin
        .execute("SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:seed-identity-fixture', false)", &[&actor])
        .await
        .unwrap();
    // provision-org created the developer as the journey org's first admin.
    let user = create_or_reuse_user(&*admin, DEVELOPER_EMAIL, "Developer")
        .await
        .ok()
        .expect("reuse the developer")
        .principal_id;
    grant_project_env_membership(&*admin, &user, ORG, PROJECT, ENVIRONMENT)
        .await
        .ok()
        .expect("grant environment membership");
    assign_project_role(
        &*admin,
        &user,
        ORG,
        PROJECT,
        "admin",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await
    .ok()
    .expect("assign the existing application role");
    reconcile_journey_run_plane(
        &JourneyScope::receiving(),
        system.url(),
        &environment.route.database_url,
    )
    .await
    .ok()
    .expect("project the user into the application");
    assert_application_role(&environment.route.database_url, user.as_str()).await;
    let (mut password_admin, connection) =
        tokio_postgres::connect(system.url(), tokio_postgres::NoTls)
            .await
            .ok()
            .expect("invitation fixture connection");
    let password_driver = tokio::spawn(connection);
    let invitation = wamn_platform_identity::password::issue_invitation(
        &mut password_admin,
        &actor.parse().unwrap(),
        &user,
    )
    .await
    .ok()
    .expect("issue invitation");
    let enrolled = http.post(format!("{endpoint}/password/enroll"))
        .json(&json!({"principal_id":user.as_str(), "invitation":invitation.secret(), "password":PASSWORD}))
        .send().await.ok().expect("HTTPS enrollment");
    assert_eq!(enrolled.status(), reqwest::StatusCode::NO_CONTENT);
    let login = || {
        http.post(format!("{endpoint}/password/session"))
            .json(&json!({"email":DEVELOPER_EMAIL, "password":PASSWORD, "aud":target["audience"]}))
    };
    let first = login()
        .send()
        .await
        .ok()
        .expect("password login before recreation");
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let keys: Value = http
        .get(format!("{endpoint}/.well-known/jwks.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pats_before: i64 = admin
        .query_one("SELECT count(*) FROM identity.pats", &[])
        .await
        .unwrap()
        .get(0);

    // Exercise the destructive database boundary used by structural rebuilds.
    // Identity remains outside that database and its role reader reconnects.
    let database =
        wamn_dev_control::dev::target_database::database_name(&environment.route.database_url)
            .unwrap();
    admin
        .batch_execute(&format!(
            "DROP DATABASE {} WITH (FORCE);",
            wamn_pg_core::Identifier::new(database.clone())
                .unwrap()
                .quoted()
        ))
        .await
        .unwrap();
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {} TEMPLATE {}",
            wamn_pg_core::Identifier::new(database).unwrap().quoted(),
            wamn_pg_core::Identifier::new(environment.template.clone())
                .unwrap()
                .quoted()
        ))
        .await
        .unwrap();
    admin
        .batch_execute(&std::fs::read_to_string(files.0.join("database-acl.sql")).unwrap())
        .await
        .unwrap();
    reconcile_journey_run_plane(
        &JourneyScope::receiving(),
        system.url(),
        &environment.route.database_url,
    )
    .await
    .ok()
    .expect("restore application membership after recreation");
    assert_application_role(&environment.route.database_url, user.as_str()).await;
    let second = login()
        .send()
        .await
        .ok()
        .expect("password login after recreation");
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    let keys_after: Value = http
        .get(format!("{endpoint}/.well-known/jwks.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        keys, keys_after,
        "application recreation changed signing keys"
    );
    let pats_after: i64 = admin
        .query_one("SELECT count(*) FROM identity.pats", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(pats_before, pats_after, "password login minted a PAT");
    environment.issuer.retain(&files.0).unwrap();
    assert_eq!(
        login()
            .send()
            .await
            .ok()
            .expect("login after ownership transfer")
            .status(),
        reqwest::StatusCode::OK
    );
    let record_path = files.0.join("identity-process.json");
    let record = std::fs::read(&record_path).unwrap();
    let mut foreign = tokio::process::Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut wrong_owner: Value = serde_json::from_slice(&record).unwrap();
    wrong_owner["pid"] = foreign.id().unwrap().into();
    wrong_owner["started"] = "not-the-owned-process".into();
    std::fs::write(&record_path, serde_json::to_vec(&wrong_owner).unwrap()).unwrap();
    assert!(
        wamn_dev_control::dev::pat_issuer::stop_environment(&files.0)
            .await
            .is_err()
    );
    assert!(
        foreign.try_wait().unwrap().is_none(),
        "teardown stopped an unowned process"
    );
    foreign.kill().await.unwrap();
    foreign.wait().await.unwrap();
    std::fs::write(record_path, record).unwrap();
    wamn_dev_control::dev::pat_issuer::stop_environment(&files.0)
        .await
        .unwrap_or_else(|error| panic!("owned teardown failed: {error}"));
    assert!(
        http.get(format!("{endpoint}/.well-known/jwks.json"))
            .send()
            .await
            .is_err()
    );
    assert!(!files.0.join("identity-process.json").exists());
    assert!(std::fs::read_dir(&files.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pat-issuer-")
    }));
    drop(password_admin);
    password_driver.abort();
    driver.abort();
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
    let admin = wamn_package_control::database::connect(system.url()).await?;
    admin
        .batch_execute(
            "CREATE ROLE wamn_system NOLOGIN; ALTER DATABASE wamn_system OWNER TO wamn_system",
        )
        .await?;
    wamn_environment_control::provision_system::provision_system(
        &wamn_environment_control::provision_system::ProvisionSystemRequest {
            system_database_url: system.url().into(),
            platform_domain: "example.invalid".into(),
        },
    )
    .await?;
    admin.batch_execute(&format!(
        "BEGIN; SET LOCAL ROLE wamn_system; {} UPDATE registry.project_envs SET disposable=true; COMMIT;",
        wamn_control_provision::test_database::seed_org_project_env_sql("acme", "apps", "dev"),
    )).await?;
    let document = EnvironmentDocument::from_json(br#"{"org":"acme","project":"apps","env":"dev","target":"cloud","policy":"dev","applications":{}}"#)?;
    let instance = wamn_environment_control::provision_project_env::read_project_env_instance(
        system.url(),
        &document.triple(),
    )
    .await?;
    let name = wamn_control_provision::project_env_database_name("acme", "apps", "dev", &instance);
    let admin_url = url::Url::parse(system.url())?;
    let credential = scratch.path().join("apply.json");
    wamn_environment_control::provision_system::emit_apply_credential(
        system.url(),
        admin_url.host_str().expect("test PostgreSQL host"),
        admin_url.port().expect("test PostgreSQL port"),
        &credential,
    )
    .await?;
    let credential: serde_json::Value = serde_json::from_slice(&std::fs::read(credential)?)?;
    let platform = Platform {
        system_database_url: credential["stringData"]["url"]
            .as_str()
            .expect("emitted apply URL")
            .into(),
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
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            wamn_pg_core::quote_ident(&name)
        ))
        .await?;
    ensure::ensure_project_database(&platform, &document, &[], &admin, system.url()).await?;
    sql::reset::prepare_retained(&platform, &document, system.url()).await?;
    let expected_database = ExpectedDatabase::read(&platform, &document).await?;
    let file = scratch.path().join("environment.json");
    let expected = scratch.path().join("database.json");
    std::fs::write(&file, document.canonical_json()?)?;
    std::fs::write(&expected, serde_json::to_vec(&expected_database)?)?;
    // CNPG supplies an empty database; the SQL worker must restore its pinned inputs.
    admin
        .batch_execute(&format!(
            "DROP DATABASE {} WITH (FORCE)",
            wamn_pg_core::quote_ident(&name),
        ))
        .await?;
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            wamn_pg_core::quote_ident(&name)
        ))
        .await?;
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

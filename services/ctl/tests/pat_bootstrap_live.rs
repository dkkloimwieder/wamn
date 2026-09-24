//! First-time provisioning sends PAT writes through the separate HTTPS authority.

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use wamn_control::dev::environment::{connect, provision_journey_control, provision_route};
use wamn_control_provision::PlatformComponent;
use wamn_platform_identity::{PrincipalKind, authenticate_pat};

struct Files(PathBuf);

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "requires:"]
async fn cli_bootstrap_mints_first_service_pats_over_https() {
    wamn_test_postgres::require_prerequisites(&[]);
    wamn_control::dev::pat_issuer::identity_binary().expect("find the wamn-identity binary");
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
    provision_journey_control(&url, admin.as_ref())
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
        assert_eq!(principal.principal().kind(), PrincipalKind::Service);
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
    for name in ["route-caller-pat.json", "management-author-pat.json"] {
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
    let revoked = std::process::Command::new(env!("CARGO_BIN_EXE_wamn-ctl"))
        .args([
            "provision-project-env",
            "--revoke-pat-prefix",
            &route.token_prefix,
        ])
        .env("WAMN_SYSTEM_ADMIN_URL", &url)
        .stdin(Stdio::null())
        .output()
        .expect("run the compiled PAT revocation verb");
    assert!(revoked.status.success(), "the PAT revocation verb failed");
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
#[ignore = "requires: RESEND_API_KEY, RESEND_FROM"]
#[expect(
    clippy::ok_expect,
    reason = "do not print credential-bearing upstream errors in fixture assertions"
)]
async fn development_identity_survives_target_recreation_and_owned_teardown() {
    const PASSWORD: &str = "owned-development-identity-fixture-password";
    use serde_json::{Value, json};
    use wamn_control::dev::environment::{
        ENVIRONMENT, ORG, PROJECT, provision, reconcile_journey_run_plane,
    };
    use wamn_platform_identity::{assign_project_role, create_human, grant_project_env_membership};
    async fn application_role(url: &str, person: &str, actor: &str) {
        let (project, task) = connect(url).await.ok().expect("application connection");
        project
            .execute("SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:seed-identity-fixture', false)", &[&actor])
            .await
            .unwrap();
        project.execute("INSERT INTO app_system.roles (tenant_id,name) VALUES ($1,'route-caller') ON CONFLICT DO NOTHING",
            &[&wamn_control::dev::environment::TENANT]).await.unwrap();
        project.execute("INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ($1,$2::text::uuid,'route-caller')",
            &[&wamn_control::dev::environment::TENANT, &person]).await.unwrap();
        task.abort();
    }
    wamn_test_postgres::require_prerequisites(&["RESEND_API_KEY", "RESEND_FROM"]);
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
    let receiving =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/wamn_receiving");
    let environment = provision(
        system.url(),
        &admin,
        &files.0,
        "example.invalid",
        &[receiving],
    )
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
    let human = create_human(
        &*admin,
        "development-person",
        "development@example.invalid",
        "Developer",
    )
    .await
    .ok()
    .expect("create invited person");
    grant_project_env_membership(&*admin, human.id(), ORG, PROJECT, ENVIRONMENT)
        .await
        .ok()
        .expect("grant environment membership");
    assign_project_role(&*admin, human.id(), ORG, PROJECT, "route-caller")
        .await
        .ok()
        .expect("assign the existing application role");
    reconcile_journey_run_plane(system.url(), &environment.route.database_url)
        .await
        .ok()
        .expect("project the person into the application");
    application_role(&environment.route.database_url, human.id().as_str(), &actor).await;
    let (mut password_admin, connection) =
        tokio_postgres::connect(system.url(), tokio_postgres::NoTls)
            .await
            .ok()
            .expect("invitation fixture connection");
    let password_driver = tokio::spawn(connection);
    let invitation = wamn_platform_identity::password::issue_invitation(
        &mut password_admin,
        &actor.parse().unwrap(),
        human.id(),
    )
    .await
    .ok()
    .expect("issue invitation");
    let enrolled = http.post(format!("{endpoint}/password/enroll"))
        .json(&json!({"principal_id":human.id().as_str(), "invitation":invitation.secret(), "password":PASSWORD}))
        .send().await.ok().expect("HTTPS enrollment");
    assert_eq!(enrolled.status(), reqwest::StatusCode::NO_CONTENT);
    let login = || {
        http.post(format!("{endpoint}/password/session"))
        .json(&json!({"email":"development@example.invalid", "password":PASSWORD, "aud":target["audience"]}))
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
        wamn_control::dev::target_database::database_name(&environment.route.database_url).unwrap();
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
    reconcile_journey_run_plane(system.url(), &environment.route.database_url)
        .await
        .ok()
        .expect("restore application membership after recreation");
    application_role(&environment.route.database_url, human.id().as_str(), &actor).await;
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
        wamn_control::dev::pat_issuer::stop_environment(&files.0)
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
    wamn_control::dev::pat_issuer::stop_environment(&files.0)
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

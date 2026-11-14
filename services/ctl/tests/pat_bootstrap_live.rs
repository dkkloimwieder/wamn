//! Development identity and PAT bootstrapping through the product.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;

use sqlx::Row as _;
use std::os::unix::fs::PermissionsExt as _;
use std::time::{SystemTime, UNIX_EPOCH};
use wamn_control_provision::PlatformComponent;
use wamn_platform_identity::{PrincipalType, authenticate_pat};
use wamn_postgres::connect;

struct Files(PathBuf);
impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn cli_bootstrap_mints_first_service_pats_over_https() {
    let url: String = panic!("wamn-eulfr.12.10: no product issuer-writer observation fixture");
    let mut admin = connect(&url)
        .await
        .expect("connect the disposable database");
    let safe: bool = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT current_database()='wamn_system' AND current_setting('server_version_num')::int \
         BETWEEN 180000 AND 189999 AND rolsuper FROM pg_roles WHERE rolname=current_user")
            .to_string(),
    ))
    .fetch_one(&mut admin)
    .await
    .expect("read test preconditions")
    .get(0);
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
    // The server records the actual inserting login. A direct CLI INSERT must
    // fail this test even if it creates an otherwise valid PAT.
    sqlx::raw_sql(sqlx::AssertSqlSafe(("CREATE SCHEMA pat_test; \
         CREATE TABLE pat_test.writers (writer name NOT NULL); \
         CREATE FUNCTION pat_test.record_writer() RETURNS trigger \
         LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog AS $$ \
         BEGIN IF NOT pg_has_role(session_user, 'wamn_identity_issuer', 'MEMBER') THEN RAISE EXCEPTION 'PAT writer lacks issuer authority'; END IF; INSERT INTO pat_test.writers VALUES (session_user); RETURN NEW; END $$; \
         REVOKE ALL ON FUNCTION pat_test.record_writer() FROM PUBLIC; \
         CREATE TRIGGER pat_test_writer AFTER INSERT ON identity.pats \
         FOR EACH ROW EXECUTE FUNCTION pat_test.record_writer();").to_string())).execute(&mut admin)
        .await
        .expect("install test-only writer observation");
    let (token, management_token, token_prefix): (String, String, String) =
        panic!("wamn-eulfr.12.10: no product first-PAT observation fixture");
    let writers = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT writer::text FROM pat_test.writers ORDER BY writer").to_string(),
    ))
    .fetch_all(&mut admin)
    .await
    .expect("read actual PAT writer identities");
    assert_eq!(
        writers.len(),
        2,
        "both provisioning PATs reached the database"
    );
    for token in [&token, &management_token] {
        let principal = authenticate_pat(&mut admin, token)
            .await
            .ok()
            .flatten()
            .expect("full PAT authentication");
        assert_eq!(
            principal.principal().principal_type(),
            PrincipalType::Service
        );
        let plaintext: bool = sqlx::query(sqlx::AssertSqlSafe(
            ("SELECT EXISTS (SELECT FROM identity.pats WHERE token_hash=$1)").to_string(),
        ))
        .bind(token)
        .fetch_one(&mut admin)
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
    let active: i64 = sqlx::query(sqlx::AssertSqlSafe(("SELECT count(*) FROM pg_roles WHERE rolname LIKE 'wamn_identity_issuer_%' AND rolcanlogin").to_string())).fetch_one(&mut admin)
        .await.expect("read issuer login state").get(0);
    assert_eq!(active, 0, "bootstrap left active issuer authority");
    // Every provisioning write stamps wamn:provisioning: the service principals
    // and roles from wamn-ctl, and the tokens from wamn-identity.
    let provisioning = PlatformComponent::Provisioning.principal_id().to_string();
    let stamped: bool = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT (SELECT bool_and(created_by = $1::text::uuid AND updated_by = $1::text::uuid) \
                     FROM identity.principals) \
                AND (SELECT bool_and(created_by = $1::text::uuid AND updated_by = $1::text::uuid) \
                     FROM identity.project_roles) \
                AND (SELECT bool_and(created_by = $1::text::uuid AND updated_by = $1::text::uuid \
                                     AND created_at = updated_at) \
                     FROM identity.pats) \
                AND (SELECT count(*) = 2 FROM identity.pats)")
            .to_string(),
    ))
    .bind(&provisioning)
    .fetch_one(&mut admin)
    .await
    .expect("read provisioning stamps")
    .get(0);
    assert!(stamped, "provisioning writes must stamp wamn:provisioning");
    let issued_at: String = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT created_at::text FROM identity.pats WHERE token_prefix = $1").to_string(),
    ))
    .bind(&token_prefix)
    .fetch_one(&mut admin)
    .await
    .expect("read the route PAT issuance time")
    .get(0);
    let changes: wamn_broker::Publisher =
        panic!("wamn-eulfr.12.10: no product revocation fault fixture");
    wamn_environment::provision_project_env::revoke_provisioning_pat(&url, &token_prefix, &changes)
        .await
        .expect("revoke the service PAT");
    let revocation: bool = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT revoked_at IS NOT NULL AND created_at::text = $2 \
                AND created_by = $1::text::uuid AND updated_by = $1::text::uuid \
                AND updated_at > created_at \
             FROM identity.pats WHERE token_prefix = $3")
            .to_string(),
    ))
    .bind(&provisioning)
    .bind(&issued_at)
    .bind(&token_prefix)
    .fetch_one(&mut admin)
    .await
    .expect("read the revocation stamps")
    .get(0);
    assert!(
        revocation,
        "the revocation keeps the created pair and stamps wamn:provisioning"
    );
    assert!(
        authenticate_pat(&mut admin, &token)
            .await
            .expect("authenticate the revoked PAT")
            .is_none(),
        "the issued PAT ignored revocation"
    );
    println!(
        "PAT_BOOTSTRAP result=pass service_writers=2 plaintext_db=absent active_issuer_logins=0 private_files=removed"
    );
    drop(admin);
}

#[tokio::test]
#[expect(
    clippy::ok_expect,
    reason = "do not print credential-bearing upstream errors in fixture assertions"
)]
async fn development_identity_survives_schema_reset() {
    const PASSWORD: &str = "owned-development-identity-fixture-password";
    use serde_json::{Value, json};
    const DEVELOPER_EMAIL: &str = "developer@example.invalid";
    const ORG: &str = "acme";
    const PROJECT: &str = "receiving";
    const ENVIRONMENT: &str = "dev";

    use wamn_platform_identity::{
        assign_project_role, create_or_reuse_user, grant_project_env_membership,
    };

    async fn assert_application_role(url: &str, user: &str) {
        let mut project = connect(url).await.ok().expect("application connection");
        let count: i64 = sqlx::query(sqlx::AssertSqlSafe(("SELECT count(*) FROM platform.user_role WHERE user_id=$1::text::uuid AND role_name='admin'").to_string())).bind(&user).fetch_one(&mut project)
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            count, 1,
            "reconciliation projects the developer's admin grant"
        );
    }
    let (system_url, application_url, endpoint, ca, target): (
        String,
        String,
        String,
        Vec<u8>,
        Value,
    ) = panic!("wamn-eulfr.12.8: native identity ownership was removed; use installed identity");
    let mut admin = connect(&system_url).await.ok().expect("system connection");
    let http = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .tls_certs_only(reqwest::Certificate::from_pem_bundle(&ca).unwrap())
        .build()
        .unwrap();
    let actor = PlatformComponent::Provisioning.principal_id().to_string();
    sqlx::query(sqlx::AssertSqlSafe(("SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:seed-identity-fixture', false)").to_string())).bind(&actor).execute(&mut admin)
        .await
        .unwrap();
    // provision-org created the developer as the journey org's first admin.
    let user = create_or_reuse_user(&mut admin, DEVELOPER_EMAIL, "Developer")
        .await
        .ok()
        .expect("reuse the developer")
        .principal_id;
    grant_project_env_membership(&mut admin, &user, ORG, PROJECT, ENVIRONMENT)
        .await
        .ok()
        .expect("grant environment membership");
    assign_project_role(
        &mut admin,
        &user,
        ORG,
        PROJECT,
        "admin",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await
    .ok()
    .expect("assign the existing application role");
    panic!("wamn-eulfr.12.9: no product identity projection fixture");
    assert_application_role(&application_url, user.to_string().as_str()).await;
    let mut password_admin = wamn_postgres::connect(&system_url)
        .await
        .ok()
        .expect("invitation fixture connection");
    let invitation = wamn_platform_identity::password::issue_invitation(
        &mut password_admin,
        &actor.parse().unwrap(),
        &user,
    )
    .await
    .ok()
    .expect("issue invitation");
    let enrolled = http.post(format!("{endpoint}/password/enroll"))
        .json(&json!({"principal_id":user.to_string().as_str(), "invitation":invitation.secret(), "password":PASSWORD}))
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
        .expect("password login before reset");
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let keys: Value = http
        .get(format!("{endpoint}/.well-known/jwks.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pats_before: i64 = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT count(*) FROM identity.pats").to_string(),
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap()
    .get(0);

    wamn_environment::environment::sql::reset::reset_database(
        &system_url,
        &application_url,
        &wamn_control_provision::test_database::package_migration_input(
            &application_url,
            "acme",
            "receiving",
            "dev",
        )
        .await
        .unwrap(),
        &wamn_registry::Triple::new("acme", "dev", "receiving"),
    )
    .await
    .unwrap();
    assert_application_role(&application_url, user.to_string().as_str()).await;
    let second = login()
        .send()
        .await
        .ok()
        .expect("password login after reset");
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    let keys_after: Value = http
        .get(format!("{endpoint}/.well-known/jwks.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(keys, keys_after, "application reset changed signing keys");
    let pats_after: i64 = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT count(*) FROM identity.pats").to_string(),
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap()
    .get(0);
    assert_eq!(pats_before, pats_after, "password login minted a PAT");
    drop(password_admin);
}

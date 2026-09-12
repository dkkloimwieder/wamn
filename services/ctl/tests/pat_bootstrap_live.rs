//! First-time provisioning sends PAT writes through the separate HTTPS authority.

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use wamn_ctl::dev::environment::{connect, provision_journey_control, provision_route};
use wamn_platform_identity::{PrincipalKind, authenticate_pat, revoke_pat};

struct Files(PathBuf);

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "requires armed disposable PostgreSQL 18 and a built wamn-identity binary"]
async fn cli_bootstrap_mints_first_service_pats_over_https() {
    assert_eq!(
        std::env::var("WAMN_PAT_BOOTSTRAP_ALLOW_SCHEMA_RESET").as_deref(),
        Ok("1"),
        "arm only a fresh disposable PostgreSQL server"
    );
    let url =
        std::env::var("WAMN_PAT_BOOTSTRAP_PG_URL").expect("provide the disposable wamn_system URL");
    let (admin, driver) = connect(&url)
        .await
        .ok()
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
        .ok()
        .expect("provision system authority");
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
    .ok()
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
    revoke_pat(admin.as_ref(), &route.token_prefix)
        .await
        .ok()
        .expect("revoke route PAT");
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

//! Run the compiled issuer-provisioning CLI on a disposable PostgreSQL 18 cluster.
//!
//! Set WAMN_IDENTITY_ISSUER_CLI_PG_URL to its wamn_system administrator URL and
//! WAMN_IDENTITY_ISSUER_CLI_ALLOW_SCHEMA_RESET=1. This ignored test resets the
//! system schemas and closes the cluster PUBLIC CONNECT floor.

mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use anyhow::Context as _;
use serde_json::Value;
use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_control_provision::identity_issuer::{
    IDENTITY_ISSUER_ROLE, identity_issuer_generation_role, parse_identity_issuer_url,
};
use wamn_control_provision::{CredentialGeneration, SYSTEM_SCHEMA_SQL, sql};

const ISSUER: &str = "https://identity-issuer-cli-proof.wamn-system.svc";

#[test]
fn compiled_identity_help_hides_administrator_environment_values() {
    const PASSWORD: &str = "identity-help-password-sentinel";
    const ADMIN_URL: &str =
        "postgres://identity-help-user:identity-help-password-sentinel@admin.invalid/wamn_system";
    for help in ["--help", "-h"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_wamn-ctl"))
            .args(["provision-identity-issuer", help])
            .env("WAMN_SYSTEM_ADMIN_URL", ADMIN_URL)
            .stdin(Stdio::null())
            .output()
            .expect("render compiled identity provisioning help");
        assert!(output.status.success(), "identity provisioning help failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("--system-database-url"));
        for secret in [ADMIN_URL, PASSWORD] {
            assert!(
                !stdout.contains(secret) && !stderr.contains(secret),
                "identity provisioning help exposed an environment credential"
            );
        }
    }
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .map_err(|_| anyhow::anyhow!("connect disposable identity database"))?;
    tokio::spawn(connection);
    Ok(client)
}

async fn cli(
    admin: &str,
    action: &str,
    generation: &str,
    path: Option<&Path>,
) -> anyhow::Result<Output> {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_wamn-ctl"));
    command
        .env("WAMN_SYSTEM_ADMIN_URL", admin)
        .args([
            "provision-identity-issuer",
            "--issuer",
            ISSUER,
            action,
            generation,
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(path) = path {
        command.arg("--emit-secret").arg(path);
    }
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .context("identity CLI timed out")?
        .context("run compiled identity CLI")?;
    let admin_url = Url::parse(admin).context("parse armed administrator URL")?;
    if let Some(password) = admin_url.password().filter(|password| !password.is_empty()) {
        anyhow::ensure!(
            !String::from_utf8_lossy(&output.stdout).contains(password)
                && !String::from_utf8_lossy(&output.stderr).contains(password),
            "identity CLI disclosed administrator credentials"
        );
    }
    Ok(output)
}

fn success(output: &Output) -> anyhow::Result<()> {
    anyhow::ensure!(
        output.status.success(),
        "identity CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn refusal(output: &Output, reason: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !output.status.success(),
        "identity CLI accepted a refused action"
    );
    anyhow::ensure!(
        String::from_utf8_lossy(&output.stderr).contains(reason),
        "identity CLI did not report the expected refusal: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn secret_url(path: &Path, generation: CredentialGeneration) -> anyhow::Result<String> {
    let metadata = fs::metadata(path).context("read emitted Secret metadata")?;
    anyhow::ensure!(
        metadata.permissions().mode() & 0o777 == 0o600,
        "Secret is not private mode 0600"
    );
    let document: Value =
        serde_json::from_slice(&fs::read(path)?).context("parse complete emitted Secret")?;
    anyhow::ensure!(
        document["kind"] == "Secret"
            && document["metadata"]["name"] == "wamn-identity-db"
            && document["metadata"]["namespace"] == "wamn-system",
        "emitted Secret has an unexpected identity"
    );
    let data = document["stringData"]
        .as_object()
        .context("Secret stringData")?;
    anyhow::ensure!(data.len() == 1, "Secret must carry only its database URL");
    let url = data["url"].as_str().context("Secret database URL")?;
    let connection = parse_identity_issuer_url(url, ISSUER)?;
    anyhow::ensure!(
        connection.generation() == generation,
        "Secret has the wrong generation"
    );
    Ok(url.to_owned())
}

async fn inactive(admin: &Client, generation: CredentialGeneration) -> anyhow::Result<()> {
    let role = identity_issuer_generation_role(ISSUER, generation)?;
    let row = admin
        .query_one(sql::workload_generation_state_sql(), &[&role])
        .await?;
    anyhow::ensure!(
        !row.get::<_, bool>("rolcanlogin")
            && !row.get::<_, bool>("password_set")
            && row.get::<_, Option<String>>("valid_until").as_deref()
                == Some("1970-01-01T00:00:00Z")
            && row.get::<_, Vec<String>>("memberships").is_empty()
            && row.get::<_, Vec<String>>("connect_databases").is_empty()
            && row.get::<_, i64>("sessions") == 0,
        "generation did not roll back to inactive"
    );
    Ok(())
}

async fn stable_acl(admin: &Client) -> anyhow::Result<Vec<String>> {
    Ok(admin
        .query(
            sql::role_database_acl_inventory_sql(),
            &[&IDENTITY_ISSUER_ROLE],
        )
        .await?
        .iter()
        .map(|row| {
            format!(
                "{}|{}|{}|{}|{}",
                row.get::<_, String>("object_kind"),
                row.get::<_, String>("schema_name"),
                row.get::<_, String>("object_name"),
                row.get::<_, String>("privilege_type"),
                row.get::<_, bool>("is_grantable")
            )
        })
        .collect())
}

/// Keep A connected while the real CLI upgrades the exact foundation ACL for B.
async fn upgrade_foundation_surface(
    admin: &Client,
    admin_url: &str,
    b_path: &Path,
) -> anyhow::Result<()> {
    let current = stable_acl(admin).await?;
    anyhow::ensure!(
        current.len() == 28,
        "current issuer must hold exactly the approved expanded ACL"
    );
    // Reproduce the installed foundation's actual privileges, not its SQL text.
    admin.batch_execute(
        "REVOKE SELECT (id,kind,subject,display_name,status) ON identity.principals FROM wamn_identity_issuer; \
         REVOKE SELECT (principal_id,token_prefix,token_hash,revoked_at,expires_at) ON identity.pats FROM wamn_identity_issuer; \
         REVOKE SELECT (principal_id,org,project,env) ON identity.project_env_memberships FROM wamn_identity_issuer; \
         REVOKE SELECT (org,project,env,instance_suffix) ON registry.project_envs FROM wamn_identity_issuer; \
         REVOKE USAGE ON SCHEMA registry FROM wamn_identity_issuer;"
    ).await?;
    let foundation = stable_acl(admin).await?;
    anyhow::ensure!(
        foundation
            == [
                "relation|identity|session_keys|DELETE|false",
                "relation|identity|session_keys|INSERT|false",
                "relation|identity|session_keys|SELECT|false",
                "relation|identity|session_keys|UPDATE|false",
                "relation|identity|session_signing_state|DELETE|false",
                "relation|identity|session_signing_state|INSERT|false",
                "relation|identity|session_signing_state|SELECT|false",
                "relation|identity|session_signing_state|UPDATE|false",
                "schema|identity|identity|USAGE|false",
            ],
        "foundation fixture must have exactly its nine original privilege rows"
    );

    // A recognized old surface is not permission to converge arbitrary drift.
    for (grant, revoke) in [
        (
            "GRANT SELECT ON identity.project_roles TO wamn_identity_issuer;",
            "REVOKE SELECT ON identity.project_roles FROM wamn_identity_issuer;",
        ),
        (
            "GRANT SELECT (id) ON identity.principals TO wamn_identity_issuer;",
            "REVOKE SELECT (id) ON identity.principals FROM wamn_identity_issuer;",
        ),
    ] {
        admin.batch_execute(grant).await?;
        let drifted = stable_acl(admin).await?;
        refusal(
            &cli(admin_url, "--prepare-generation", "b", Some(b_path)).await?,
            "identity role has unexpected direct privileges",
        )?;
        anyhow::ensure!(
            !b_path.exists(),
            "refused foundation upgrade published a Secret"
        );
        anyhow::ensure!(
            stable_acl(admin).await? == drifted,
            "refused foundation upgrade modified its ACL"
        );
        inactive(admin, CredentialGeneration::B).await?;
        admin.batch_execute(revoke).await?;
        anyhow::ensure!(
            stable_acl(admin).await? == foundation,
            "foundation control was not restored exactly"
        );
    }

    success(&cli(admin_url, "--prepare-generation", "b", Some(b_path)).await?)?;
    anyhow::ensure!(
        stable_acl(admin).await? == current,
        "foundation issuer upgrade must restore exactly the approved current ACL and no extras"
    );
    Ok(())
}

async fn reset(admin: &Client) -> anyhow::Result<()> {
    for generation in [CredentialGeneration::A, CredentialGeneration::B] {
        let role = identity_issuer_generation_role(ISSUER, generation)?;
        admin
            .batch_execute(&sql::terminate_workload_generation_sessions_sql(&role))
            .await?;
    }
    admin.batch_execute("DROP SCHEMA IF EXISTS identity CASCADE; DROP SCHEMA IF EXISTS registry CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE;").await?;
    for role in [
        identity_issuer_generation_role(ISSUER, CredentialGeneration::A)?,
        identity_issuer_generation_role(ISSUER, CredentialGeneration::B)?,
        IDENTITY_ISSUER_ROLE.to_owned(),
    ] {
        admin
            .batch_execute(&format!(
                "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
               DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\"; END IF; END $$;"
            ))
            .await?;
    }
    Ok(())
}

fn temporary_directory() -> anyhow::Result<PathBuf> {
    let output = std::process::Command::new("mktemp")
        .arg("-d")
        .arg(std::env::temp_dir().join("wamn-identity-issuer-cli.XXXXXXXX"))
        .output()
        .context("create unique identity proof directory")?;
    anyhow::ensure!(
        output.status.success(),
        "mktemp failed for the identity proof"
    );
    let path = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    anyhow::ensure!(
        path.parent() == Some(std::env::temp_dir().as_path())
            && path.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("wamn-identity-issuer-cli.")),
        "unexpected identity proof directory"
    );
    Ok(path)
}

async fn journey(admin: &Client, admin_url: &str, directory: &Path) -> anyhow::Result<()> {
    admin
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
           CREATE ROLE wamn_system NOLOGIN; END IF; END $$; \
         GRANT CREATE ON DATABASE wamn_system TO wamn_system;",
        )
        .await?;
    admin
        .batch_execute(&format!(
            "SET ROLE wamn_system; {SYSTEM_SCHEMA_SQL} RESET ROLE;"
        ))
        .await?;
    admin
        .batch_execute(sql::revoke_public_connect_floor_sql())
        .await?;
    admin
        .batch_execute("REVOKE TEMPORARY ON DATABASE wamn_system FROM PUBLIC;")
        .await?;
    let a_path = directory.join("a.json");
    let b_path = directory.join("b.json");
    let previous = b"{\"previous\":\"unchanged\"}\n";
    fs::write(&a_path, previous)?;
    fs::set_permissions(&a_path, fs::Permissions::from_mode(0o644))?;
    let previous_inode = fs::metadata(&a_path)?.ino();

    refusal(
        &cli(admin_url, "--prepare-generation", "b", Some(&b_path)).await?,
        "initial identity generation must be a",
    )?;
    anyhow::ensure!(!b_path.exists(), "refused initial B published a Secret");
    let output = cli(admin_url, "--prepare-generation", "a", Some(&a_path)).await?;
    success(&output)?;
    let a_url = secret_url(&a_path, CredentialGeneration::A)?;
    anyhow::ensure!(
        fs::metadata(&a_path)?.ino() != previous_inode,
        "Secret publication modified the existing file instead of replacing it"
    );
    let a_password = Url::parse(&a_url)?
        .password()
        .context("prepared password")?
        .to_owned();
    anyhow::ensure!(
        !String::from_utf8_lossy(&output.stdout).contains(&a_password)
            && !String::from_utf8_lossy(&output.stderr).contains(&a_password),
        "CLI disclosed its prepared credential"
    );
    let a = connect(&a_url).await?;
    let a_bytes = fs::read(&a_path)?;
    refusal(
        &cli(admin_url, "--prepare-generation", "a", Some(&a_path)).await?,
        "inactive identity credential slot",
    )?;
    anyhow::ensure!(
        fs::read(&a_path)? == a_bytes,
        "refused prepare changed the published Secret"
    );

    // The writer can create and sync its private temporary file, but cannot
    // replace this nonempty directory. The old path must survive unchanged.
    let broken_path = directory.join("blocked-output");
    fs::create_dir(&broken_path)?;
    let marker = broken_path.join("previous.json");
    fs::write(&marker, previous)?;
    let output = cli(admin_url, "--prepare-generation", "b", Some(&broken_path)).await?;
    refusal(&output, "install credential output")?;
    anyhow::ensure!(
        broken_path.is_dir() && fs::read(&marker)? == previous,
        "failed publication changed the existing output path"
    );
    inactive(admin, CredentialGeneration::B).await?;
    a.query_one("SELECT 1", &[])
        .await
        .context("A survived B publication rollback")?;
    fs::remove_file(&marker)?;
    fs::remove_dir(&broken_path)?;

    upgrade_foundation_surface(admin, admin_url, &b_path).await?;
    a.query("SELECT token_hash FROM identity.pats", &[])
        .await
        .context("existing A inherits the approved upgraded read surface")?;
    let b_url = secret_url(&b_path, CredentialGeneration::B)?;
    refusal(
        &cli(admin_url, "--retire-generation", "a", None).await?,
        "live session",
    )?;
    let b = connect(&b_url).await?;
    b.query(
        "SELECT id,kind,subject,display_name,status FROM identity.principals",
        &[],
    )
    .await
    .context("replacement B reads the approved principal columns")?;
    let denied = b
        .query("SELECT * FROM identity.project_roles", &[])
        .await
        .expect_err("replacement B must not gain unrelated project-role authority");
    anyhow::ensure!(
        denied.code() == Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE),
        "replacement B's unrelated read must be refused by the database ACL"
    );
    refusal(
        &cli(admin_url, "--abort-generation", "b", None).await?,
        "in-use identity credential",
    )?;
    success(&cli(admin_url, "--retire-generation", "a", None).await?)?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !a.is_closed() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("old generation session did not drain")?;
    anyhow::ensure!(
        a.query_one("SELECT 1", &[]).await.is_err(),
        "retired A session still works"
    );
    anyhow::ensure!(
        connect(&a_url).await.is_err(),
        "retired A credential still authenticates"
    );
    inactive(admin, CredentialGeneration::A).await?;
    b.query_one("SELECT 1", &[])
        .await
        .context("B survived A retirement")?;

    success(&cli(admin_url, "--prepare-generation", "a", Some(&a_path)).await?)?;
    let replacement = secret_url(&a_path, CredentialGeneration::A)?;
    anyhow::ensure!(
        replacement != a_url,
        "reused A slot retained its old password"
    );
    anyhow::ensure!(
        connect(&a_url).await.is_err(),
        "old A password works after slot reuse"
    );
    success(&cli(admin_url, "--abort-generation", "a", None).await?)?;
    inactive(admin, CredentialGeneration::A).await?;
    b.query_one("SELECT 1", &[])
        .await
        .context("B survived unused A abort")?;
    let empty: bool = admin.query_one(
        "SELECT NOT EXISTS (SELECT FROM identity.session_keys) AND NOT EXISTS (SELECT FROM identity.session_signing_state) \
           AND NOT EXISTS (SELECT FROM identity.pats) AND NOT EXISTS (SELECT FROM identity.principals)", &[]
    ).await?.get(0);
    anyhow::ensure!(
        empty,
        "credential provisioning minted identity or signing-key data"
    );
    let names = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        names.len() == 2
            && names
                .iter()
                .all(|name| name == "a.json" || name == "b.json"),
        "credential publication left unexpected temporary files"
    );
    drop(a);
    drop(b);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an explicitly armed disposable PostgreSQL 18 cluster"]
async fn compiled_cli_publishes_rolls_back_and_retires_identity_generations() {
    let _lock = support::lock();
    let url = std::env::var("WAMN_IDENTITY_ISSUER_CLI_PG_URL")
        .expect("set WAMN_IDENTITY_ISSUER_CLI_PG_URL");
    assert_eq!(
        std::env::var("WAMN_IDENTITY_ISSUER_CLI_ALLOW_SCHEMA_RESET").as_deref(),
        Ok("1"),
        "allow schema reset only on a disposable cluster"
    );
    assert_eq!(
        Url::parse(&url).expect("armed administrator URL").path(),
        "/wamn_system"
    );
    let admin = connect(&url)
        .await
        .expect("connect disposable administrator");
    let valid: bool = admin.query_one(
        "SELECT current_database() = 'wamn_system' AND current_setting('server_version_num')::int >= 180000 \
           AND current_setting('server_version_num')::int < 190000 AND rolsuper FROM pg_roles WHERE rolname = current_user", &[]
    ).await.expect("require PostgreSQL 18 administrator").get(0);
    assert!(
        valid,
        "proof requires PostgreSQL 18 and a system database administrator"
    );
    reset(&admin)
        .await
        .expect("reset disposable identity fixture");
    let directory = temporary_directory().expect("create proof directory");
    let result = journey(&admin, &url, &directory).await;
    let cleanup = reset(&admin).await;
    fs::remove_dir_all(&directory).expect("remove only the generated proof directory");
    cleanup.expect("clean disposable identity fixture");
    result.expect("compiled identity CLI lifecycle proof");
}

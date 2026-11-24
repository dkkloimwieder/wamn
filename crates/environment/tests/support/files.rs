//! Disposable file metadata and the production administration and guest logins.

use anyhow::Context as _;
use sqlx::PgConnection as Client;
use sqlx::Row as _;

use wamn_http_client::HttpClient;
use wamn_identity_protocol::workload_role::{
    WorkloadRoleFamily, WorkloadRoleIdentity, workload_login,
};

pub const CALLER: &str = "a0000000-0000-4000-8000-0000000000c4";
pub const CONNECTION: &str = "a0000000-0000-4000-8000-000000000041";
pub const FILE: &str = "a0000000-0000-4000-8000-000000000042";
pub const APPLICATION: &str = "files";
pub const PACKAGE: &str = include_str!("file_package/wamn.json");
pub const DOCUMENTS: &str = include_str!("file_package/migrations/0001_documents.sql");

pub struct Fixture {
    pub observer: Client,
    pub writer: Client,
    pub guest: Client,
    pub migrator: Client,
    database: url::Url,
    _postgres: wamn_test_infrastructure::containers::Postgres,
}

pub async fn connect(url: &str) -> anyhow::Result<Client> {
    let client = wamn_postgres::connect(url).await?;

    Ok(client)
}

fn login(url: &str, role: &wamn_role::LoginName) -> anyhow::Result<String> {
    let mut url = url::Url::parse(url)?;
    url.set_username(role.as_str())
        .map_err(|()| anyhow::anyhow!("fixture username"))?;
    url.set_password(Some("file-fixture-only"))
        .map_err(|()| anyhow::anyhow!("fixture password"))?;
    Ok(url.into())
}

/// Begin a fixture write under its principal and operation.
pub async fn transaction<'connection>(
    connection: &'connection mut Client,
    principal: &wamn_identity_protocol::PrincipalId,
    operation: &str,
) -> Result<sqlx::Transaction<'connection, sqlx::Postgres>, sqlx::Error> {
    let mut transaction = sqlx::Connection::begin(connection).await?;
    sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','fixture',$2)")
        .bind(uuid::Uuid::from(*principal)).bind(operation).execute(&mut *transaction).await?;
    Ok(transaction)
}

/// Install schema as bootstrap, then use targeted logins for every fixture write.
pub async fn fixture(name: &str) -> anyhow::Result<Fixture> {
    let postgres = wamn_test_infrastructure::containers::start_postgres().await?;
    let database = postgres.create_database(name).await?;
    let mut observer = connect(database.as_str()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "{} CREATE SCHEMA wamn_run",
        wamn_provision::sql::ensure_project_roles_sql()
    )))
    .execute(&mut observer)
    .await
    .map(drop)?;
    sqlx::raw_sql(wamn_provision::CATALOG_SCHEMA_SQL)
        .execute(&mut observer)
        .await
        .map(drop)?;
    sqlx::raw_sql(wamn_provision::APP_SCHEMA_SQL)
        .execute(&mut observer)
        .await
        .map(drop)?;
    let mut logins = Vec::new();
    for family in [WorkloadRoleFamily::Platform, WorkloadRoleFamily::App] {
        let target = WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &"files--dev--project".parse().unwrap(),
            database: name,
        };
        let role = workload_login(family, target)?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(crate::common::login_sql(
            family,
            name,
            &role,
            "file-fixture-only",
        )))
        .execute(&mut observer)
        .await
        .map(drop)?;
        logins.push(connect(&login(database.as_str(), &role)?).await?);
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE file_fixture_migrator LOGIN PASSWORD 'file-fixture-only' \
         NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
         GRANT CREATE ON DATABASE \"{name}\" TO file_fixture_migrator; \
         GRANT USAGE ON SCHEMA platform TO file_fixture_migrator; \
         GRANT REFERENCES (id) ON platform.file TO file_fixture_migrator;"
    )))
    .execute(&mut observer)
    .await
    .map(drop)?;
    let mut migrator = connect(&login(
        database.as_str(),
        &"file_fixture_migrator".parse().unwrap(),
    )?)
    .await?;
    sqlx::raw_sql(DOCUMENTS)
        .execute(&mut migrator)
        .await
        .map(drop)
        .context("apply the file fixture migration")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("REVOKE REFERENCES (id) ON platform.file FROM file_fixture_migrator; \
         REVOKE USAGE ON SCHEMA platform FROM file_fixture_migrator; \
         GRANT USAGE ON SCHEMA file_fixture TO wamn_app; \
         GRANT SELECT, INSERT, UPDATE, DELETE ON file_fixture.document TO wamn_app")
            .to_string(),
    ))
    .execute(&mut observer)
    .await
    .map(drop)?;
    let mut guest = logins.pop().expect("guest login");
    let mut writer = logins.pop().expect("administration login");
    for client in [&mut writer, &mut guest, &mut migrator] {
        let row =
            sqlx::query("SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname=current_user")
                .fetch_one(&mut *client)
                .await?;
        anyhow::ensure!(
            !row.get::<bool, _>(0) && !row.get::<bool, _>(1),
            "targeted fixture login"
        );
    }
    sqlx::raw_sql("BEGIN")
        .execute(&mut writer)
        .await
        .expect("begin fixture transaction");
    sqlx::query(
        "INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1::text::uuid,'','fixture','admin:file-fixture')",
    )
    .bind(CALLER)
    .execute(&mut writer)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query(
        "INSERT INTO platform.\"user\" (id,issuer,sub,type,email) \
         VALUES ($1::text::uuid,'https://fixture.example',$1,'user','file@example.test')",
    )
    .bind(CALLER)
    .execute(&mut writer)
    .await?;
    sqlx::query(
        "WITH connection AS (INSERT INTO platform.connection \
             (id,name,integration_type,auth_type,target_id,service_principal_id,active_generation) \
             VALUES ($1::text::uuid,'file-store','s3','custom',gen_random_uuid(),$2::text::uuid,1) \
             RETURNING id,active_generation) \
         INSERT INTO platform.connection_generation (connection_id,generation) \
         SELECT id,active_generation FROM connection",
    )
    .bind(CONNECTION)
    .bind(CALLER)
    .execute(&mut writer)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::raw_sql("COMMIT")
        .execute(&mut writer)
        .await
        .expect("commit fixture transaction");
    Ok(Fixture {
        observer,
        writer,
        guest,
        migrator,
        database,
        _postgres: postgres,
    })
}

/// Seed metadata for schema tests, without asserting that an object was uploaded.
pub async fn metadata(writer: &mut Client) -> anyhow::Result<()> {
    let mut transaction = transaction(
        writer,
        &CALLER
            .parse::<wamn_identity_protocol::PrincipalId>()
            .unwrap(),
        "admin:file-fixture",
    )
    .await?;
    sqlx::query(
        "INSERT INTO platform.file \
         (id,application,connection_id,name,media_type,size,sha256,blob_key) \
         VALUES ($1::text::uuid,$2,$3::text::uuid,'fixture.txt','text/plain',0,'sha256:' || repeat('0',64), \
                 'dev/_wamn_files/live/' || $1)",
    )
    .bind(FILE)
    .bind(APPLICATION)
    .bind(CONNECTION)
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())?;
    transaction.commit().await?;
    Ok(())
}

pub const STORE_NAME: &str = "file-store";
pub const STORE_PASSWORD: &str = "file-fixture-only";
pub const MINIO_IMAGE: &str =
    "chainguard/minio@sha256:aea6c163749e1caca37f907976dcf337177ddf5f558808f2aa591618fd3a6eef";
pub const MC_IMAGE: &str = "chainguard/minio-client@sha256:8dc1dceb7371c9091b17b450e4da67a492f6de3a22d716af14888a7d8597457e";

/// MinIO runs only when root executes a live test. Every test owns its container.
pub struct Minio {
    pub endpoint: String,
    pub cdn_origin: String,
    pub container: String,
}

pub async fn command(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "{program} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

impl Minio {
    pub async fn start() -> anyhow::Result<Self> {
        let container = format!("wamn-files-{}", uuid::Uuid::new_v4());
        command(
            "docker",
            &[
                "run",
                "--detach",
                "--name",
                &container,
                "--publish",
                "127.0.0.1::9000",
                "--env",
                "MINIO_ROOT_USER=file-fixture",
                "--env",
                "MINIO_ROOT_PASSWORD=file-fixture-only",
                "--env",
                "MINIO_DOMAIN=localhost",
                "--env",
                "MINIO_SCANNER_SPEED=fastest",
                MINIO_IMAGE,
                "server",
                "/tmp/data",
            ],
        )
        .await?;
        let mut fixture = Self {
            endpoint: String::new(),
            cdn_origin: String::new(),
            container,
        };
        let port = command("docker", &["port", &fixture.container, "9000/tcp"]).await?;
        let address: std::net::SocketAddr = port.parse()?;
        fixture.endpoint = format!("http://{address}");
        fixture.cdn_origin = format!("http://files-a.localhost:{}", address.port());
        let client = fixture.client()?;
        let mut ready = false;
        for _ in 0..100 {
            if client
                .get(format!("{}/minio/health/ready", fixture.endpoint))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        anyhow::ensure!(ready, "owned MinIO did not become ready");
        fixture
            .mc(&["mb", "file/files-a", "file/files-b"], b"")
            .await?;
        for (file_bucket, file_prefix) in
            [("files-a", "generation-one"), ("files-b", "generation-two")]
        {
            // ListBucket lets a missing public key return origin 404. GetObject
            // stays confined to public copies beneath the binding's prefix.
            // Keep the old prefix for the explicit legacy-key compatibility case.
            let policy = serde_json::json!({"Version":"2012-10-17","Statement":[
                {"Effect":"Allow","Principal":"*","Action":["s3:ListBucket","s3:GetBucketLocation"],
                    "Resource":[format!("arn:aws:s3:::{file_bucket}")]},
                {"Effect":"Allow","Principal":"*","Action":["s3:GetObject"],
                    "Resource":[format!("arn:aws:s3:::{file_bucket}/{file_prefix}/public/*"),
                        format!("arn:aws:s3:::{file_bucket}/{file_prefix}/dev/public/*")]}
            ]});
            fixture
                .mc(
                    &[
                        "anonymous",
                        "set-json",
                        "/input",
                        &format!("file/{file_bucket}"),
                    ],
                    policy.to_string().as_bytes(),
                )
                .await?;
        }
        Ok(fixture)
    }

    pub fn definition(&self) -> serde_json::Value {
        serde_json::json!({"provider":"s3","endpoint":self.endpoint,"container":"files-a",
            "prefix":"generation-one/","cdn_host":self.cdn_origin})
    }

    pub fn client(&self) -> anyhow::Result<HttpClient> {
        let address: std::net::SocketAddr = self.endpoint.trim_start_matches("http://").parse()?;
        HttpClient::builder()
            .resolve("files-a.localhost", address)
            .resolve("files-b.localhost", address)
            .timeout(std::time::Duration::from_secs(10))
            .build()
    }

    pub async fn mc(&self, args: &[&str], input: &[u8]) -> anyhow::Result<String> {
        use std::os::unix::fs::PermissionsExt as _;
        let work = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
        let path = work.path().join("input");
        std::fs::write(&path, input)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        let mount = format!("type=bind,source={},target=/input,readonly", path.display());
        let network = format!("container:{}", self.container);
        let mut command_args = vec![
            "run",
            "--rm",
            "--network",
            &network,
            "--mount",
            &mount,
            "--env",
            "MC_HOST_file=http://file-fixture:file-fixture-only@127.0.0.1:9000",
            MC_IMAGE,
            "--config-dir",
            "/tmp/mc",
        ];
        command_args.extend_from_slice(args);
        command("docker", &command_args).await
    }
}

impl Drop for Minio {
    fn drop(&mut self) {
        match std::process::Command::new("docker")
            .args(["rm", "--force", "--volumes", &self.container])
            .output()
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => eprintln!(
                "remove owned MinIO {}: {}",
                self.container,
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => eprintln!("remove owned MinIO {}: {error}", self.container),
        }
    }
}

/// Import an aged metadata fixture as a non-superuser table owner.
/// The stamp trigger is restored in the same transaction. All other tests keep
/// its normal production authority and exercise the unmodified trigger.
pub async fn age_metadata(fixture: &mut Fixture) -> anyhow::Result<()> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE file_aged_owner LOGIN PASSWORD 'file-fixture-only' NOSUPERUSER NOBYPASSRLS; \
         GRANT wamn_platform TO file_aged_owner; \
         ALTER TABLE platform.file OWNER TO file_aged_owner")
            .to_string(),
    ))
    .execute(&mut fixture.observer)
    .await
    .map(drop)?;
    let mut importer = connect(&login(
        fixture.database.as_str(),
        &"file_aged_owner".parse().unwrap(),
    )?)
    .await?;
    let identity =
        sqlx::query("SELECT rolsuper,rolbypassrls FROM pg_roles WHERE rolname=current_user")
            .fetch_one(&mut importer)
            .await?;
    anyhow::ensure!(
        !identity.get::<bool, _>(0) && !identity.get::<bool, _>(1),
        "fixture import uses no superuser"
    );

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("BEGIN; INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('a0000000-0000-4000-8000-0000000000c4','','fixture','admin:file-fixture'); ALTER TABLE platform.file DISABLE TRIGGER wamn_record_history_stamp; \
         UPDATE platform.file SET created_at=now()-interval '31 days'; \
         ALTER TABLE platform.file ENABLE TRIGGER wamn_record_history_stamp; COMMIT")
            .to_string(),
    ))
    .execute(&mut importer)
    .await
    .map(drop)?;
    let enabled: bool = sqlx::query(
        "SELECT tgenabled='O' FROM pg_trigger WHERE tgrelid='platform.file'::regclass \
         AND tgname='wamn_record_history_stamp'",
    )
    .fetch_one(&mut fixture.observer)
    .await?
    .get(0);
    anyhow::ensure!(enabled, "the aged fixture restores the stamp trigger");
    Ok(())
}

//! Disposable file metadata and the production administration and guest logins.

use anyhow::Context as _;
use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};

use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};

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
    database: wamn_test_postgres::OwnedDatabase,
    _postgres: wamn_test_postgres::OwnedPostgres,
}

pub async fn connect(url: &str) -> anyhow::Result<Client> {
    let client = wamn_postgres::connect(url).await?;

    Ok(client)
}

fn login(url: &str, role: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(url)?;
    url.set_username(role)
        .map_err(|()| anyhow::anyhow!("fixture username"))?;
    url.set_password(Some("file-fixture-only"))
        .map_err(|()| anyhow::anyhow!("fixture password"))?;
    Ok(url.into())
}

/// Install schema as bootstrap, then use targeted logins for every fixture write.
pub async fn fixture(name: &str) -> anyhow::Result<Fixture> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database(name).await?;
    let mut observer = connect(database.url()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE wamn_app NOLOGIN NOINHERIT NOSUPERUSER NOBYPASSRLS; \
         \
         CREATE SCHEMA wamn_run")
            .to_string(),
    ))
    .execute(&mut observer)
    .await
    .map(drop)?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut observer)
        .await
        .map(drop)?;
    sqlx::raw_sql(wamn_control_provision::APP_SCHEMA_SQL)
        .execute(&mut observer)
        .await
        .map(drop)?;
    let mut logins = Vec::new();
    for (family, target) in [
        (
            WorkloadRoleFamily::Administration,
            WorkloadRoleIdentity::ProjectEnvironment {
                org: "file-fixture",
                project: "files",
                environment: "dev",
                database: name,
            },
        ),
        (
            WorkloadRoleFamily::App,
            WorkloadRoleIdentity::ProjectEnvKey {
                project_env_key: "files",
                database: name,
            },
        ),
    ] {
        let role = workload_login(family, target)?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::login_sql(
                family,
                name,
                &role,
                "file-fixture-only",
            ))
                .to_string(),
        ))
        .execute(&mut observer)
        .await
        .map(drop)?;
        logins.push(connect(&login(database.url(), &role)?).await?);
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE ROLE file_fixture_migrator LOGIN PASSWORD 'file-fixture-only' \
         NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
         GRANT CREATE ON DATABASE \"{name}\" TO file_fixture_migrator; \
         GRANT USAGE ON SCHEMA platform TO file_fixture_migrator; \
         GRANT REFERENCES (id) ON platform.file TO file_fixture_migrator;"
        ))
            .to_string(),
    ))
    .execute(&mut observer)
    .await
    .map(drop)?;
    let mut migrator = connect(&login(database.url(), "file_fixture_migrator")?).await?;
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
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                set_config('app.operation', 'admin:file-fixture', false)",
    )
    .bind(&CALLER)
    .execute(&mut writer)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO platform.\"user\" (id,type,email) VALUES ($1::text::uuid,'user','file@example.test')").bind(&CALLER).execute(&mut writer).await.map(|result| result.rows_affected())?;
    sqlx::query(
        "WITH integration AS (INSERT INTO platform.integration (id,integration_type,name,status) \
             VALUES ($1::text::uuid,'s3','file-store','active') RETURNING id) \
             INSERT INTO platform.connection (id,integration_id,integration_type,status) \
             SELECT $1::text::uuid,id,'s3','active' FROM integration",
    )
    .bind(&CONNECTION)
    .execute(&mut writer)
    .await
    .map(|result| result.rows_affected())?;
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
    sqlx::query(
        "INSERT INTO platform.file \
         (id,application,connection_id,name,media_type,size,sha256,blob_key) \
         VALUES ($1::text::uuid,$2,$3::text::uuid,'fixture.txt','text/plain',0,repeat('0',64), \
                 'dev/_wamn_files/live/' || $1)",
    )
    .bind(&FILE)
    .bind(&APPLICATION)
    .bind(&CONNECTION)
    .execute(&mut *writer)
    .await
    .map(|result| result.rows_affected())?;
    Ok(())
}

pub const STORE_NAME: &str = "file-store";
pub const STORE_PASSWORD: &str = "file-fixture-only";
pub const MINIO_IMAGE: &str =
    "chainguard/minio@sha256:aea6c163749e1caca37f907976dcf337177ddf5f558808f2aa591618fd3a6eef";
pub const MC_IMAGE: &str = "chainguard/minio-client@sha256:8dc1dceb7371c9091b17b450e4da67a492f6de3a22d716af14888a7d8597457e";

/// A system database whose real reader login supplies file and secret authority.
pub struct Registry {
    pub owner: Client,
    pub control: Client,
    pub reader: sqlx::PgPool,
    pub unmapped: Client,
    pub unenabled: Client,
    pub org: String,
    project: String,
    pub credentials: std::sync::Arc<wamn_test_infrastructure::credentials::MemoryStore>,
    pub connection_id: String,
    pub generation: i64,
    pub definition: serde_json::Value,
    _database: wamn_test_postgres::OwnedDatabase,
    _postgres: wamn_test_postgres::OwnedPostgres,
}

impl Registry {
    pub async fn configure(
        &mut self,
        operation: &str,
        mut payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        payload["project"] = self.project.clone().into();
        payload["env"] = "dev".into();
        let mut tx = self.control.begin().await?;
        wamn_platform_identity::connections::administration::execute_with_store(
            &mut tx,
            &self.org,
            operation,
            &payload.to_string(),
            &mut wamn_broker::auth_changes::AuthDrops::default(),
            &mut wamn_broker::connection_changes::ConnectionDrops::default(),
            Some(self.credentials.as_ref()),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn define_store(
        &mut self,
        project: &str,
        name: &str,
        parameters: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let mut tx = self.control.begin().await?;
        wamn_test_infrastructure::credentials::define(&mut tx,&self.org,&serde_json::json!({"project":project,"env":"dev","name":name,"integration_type":"s3","auth_type":"custom","parameters":parameters,"credential":serde_json::json!({"ACCESS_KEY_ID":"file-fixture","ACCESS_SECRET_KEY":STORE_PASSWORD}).to_string()}),self.credentials.as_ref()).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn move_store(&mut self, definition: &serde_json::Value) -> anyhow::Result<()> {
        self.configure(
            "connection.write",
            serde_json::json!({"name":STORE_NAME,"parameters":definition}),
        )
        .await
    }
}

pub async fn active_file_connections<'e>(
    client: impl sqlx::Executor<'e, Database = sqlx::Postgres>,
    org: &str,
    project: &str,
    connection: Option<&str>,
) -> anyhow::Result<Vec<sqlx::postgres::PgRow>> {
    Ok(sqlx::query("SELECT connection_id::text,connection_name,generation,parameters FROM registry.read_file_connection($1,$2,'dev',$3::text::uuid)").bind(&org).bind(&project).bind(&connection).fetch_all(client).await?)
}

pub async fn registry(
    org: &str,
    project: &str,
    definition: serde_json::Value,
) -> anyhow::Result<Registry> {
    use std::sync::Arc;
    use wamn_control_provision::{SystemReader, system_reader_login};
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("file_registry").await?;
    let mut bootstrap = connect(database.url()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut bootstrap)
    .await
    .map(drop)?;

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE wamn_system LOGIN PASSWORD 'file-fixture-only' NOSUPERUSER NOBYPASSRLS; \
         ALTER DATABASE file_registry OWNER TO wamn_system")
            .to_string(),
    ))
    .execute(&mut bootstrap)
    .await
    .map(drop)?;
    let mut owner = connect(&login(database.url(), "wamn_system")?).await?;
    sqlx::raw_sql(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .execute(&mut owner)
        .await
        .map(drop)?;
    sqlx::raw_sql(wamn_control_provision::CONTROL_PORTABLE_STORE_SQL)
        .execute(&mut owner)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::grant_control_surface_sql()).to_string(),
    ))
    .execute(&mut bootstrap)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::grant_connection_reader_surface_sql()).to_string(),
    ))
    .execute(&mut bootstrap)
    .await
    .map(drop)?;
    let principal = wamn_control_provision::PlatformComponent::Provisioning;
    // These clients seed several autocommit transactions. Keep their actor
    // bound for the session, as the administration fixture does.
    let bind_actor = format!(
        "SELECT set_config('app.user_id', '{}', false), \
         set_config('app.operation', '{}', false)",
        principal.principal_id(),
        principal.principal_name(),
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe((&bind_actor).to_string()))
        .execute(&mut owner)
        .await
        .map(drop)?;
    sqlx::raw_sql("UPDATE registry.meta SET platform_domain='example.invalid'")
        .execute(&mut owner)
        .await
        .map(drop)?;
    for name in [project, "unenabled"] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::seed_org_project_env_sql(org, name, "dev"))
                .to_string(),
        ))
        .execute(&mut owner)
        .await
        .map(drop)?;
    }
    sqlx::raw_sql("UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud')").execute(&mut owner).await.map(drop)?;
    sqlx::raw_sql("CREATE ROLE file_fixture_control LOGIN PASSWORD 'file-fixture-only' NOSUPERUSER NOBYPASSRLS; \
         GRANT wamn_control TO file_fixture_control").execute(&mut bootstrap).await.map(drop)?;
    let mut control = connect(&login(database.url(), "file_fixture_control")?).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe((&bind_actor).to_string()))
        .execute(&mut control)
        .await
        .map(drop)?;
    let mut clients = Vec::new();
    for (target_project, mapped) in [(project, true), ("unmapped", false), ("unenabled", true)] {
        let role = system_reader_login(
            SystemReader::Connection,
            org,
            target_project,
            "dev",
            "file_registry",
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::login_sql(
                WorkloadRoleFamily::ConnectionReader,
                "file_registry",
                &role,
                STORE_PASSWORD,
            ))
                .to_string(),
        ))
        .execute(&mut bootstrap)
        .await
        .map(drop)?;
        if mapped {
            sqlx::query("INSERT INTO wamn_authority.author_login_projects \
                 (login_identity,project_env_key,org_id,project_id,environment) VALUES ($1,$2,$3,$4,'dev')").bind(&role).bind(&wamn_registry::project_env_key(org, target_project, "dev")).bind(&org).bind(&target_project).execute(&mut owner).await.map(|result| result.rows_affected())?;
        }
        let input = login(database.url(), &role)?;
        clients.push((connect(&input).await?, input));
    }
    let (mut unenabled, _) = clients.pop().unwrap();
    let (mut unmapped, _) = clients.pop().unwrap();
    let (mut reader, reader_input) = clients.pop().unwrap();
    for client in [
        &mut owner,
        &mut control,
        &mut reader,
        &mut unmapped,
        &mut unenabled,
    ] {
        let row =
            sqlx::query("SELECT rolsuper,rolbypassrls FROM pg_roles WHERE rolname=current_user")
                .fetch_one(&mut *client)
                .await?;
        anyhow::ensure!(
            !row.get::<bool, _>(0) && !row.get::<bool, _>(1),
            "file registry uses targeted logins"
        );
    }
    let reader = wamn_postgres::pool(&reader_input, 4).await?;
    let mut fixture = Registry {
        owner,
        control,
        reader,
        unmapped,
        unenabled,
        org: org.into(),
        project: project.into(),
        credentials: Arc::new(wamn_test_infrastructure::credentials::MemoryStore::default()),
        connection_id: String::new(),
        generation: 0,
        definition: definition.clone(),
        _database: database,
        _postgres: postgres,
    };
    for (name, cdn) in [(STORE_NAME, true), ("files-no-cdn", false)] {
        let mut parameters = definition.clone();
        if !cdn {
            parameters.as_object_mut().unwrap().remove("cdn_host");
        }
        let mut tx = fixture.control.begin().await?;
        wamn_test_infrastructure::credentials::define(&mut tx,org,&serde_json::json!({"project":project,"env":"dev","name":name,"integration_type":"s3","auth_type":"custom","parameters":parameters,"credential":serde_json::json!({"ACCESS_KEY_ID":"file-fixture","ACCESS_SECRET_KEY":STORE_PASSWORD}).to_string()}),fixture.credentials.as_ref()).await?;
        tx.commit().await?;
    }
    let row = sqlx::query("SELECT c.id::text,c.active_generation FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE c.org=$1 AND c.project=$3 AND i.name=$2").bind(&org).bind(&STORE_NAME).bind(&project).fetch_one(&mut fixture
        .owner)
        .await?;
    fixture.connection_id = row.get(0);
    fixture.generation = row.get(1);
    Ok(fixture)
}

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

    pub fn client(&self) -> anyhow::Result<reqwest::Client> {
        let address: std::net::SocketAddr = self.endpoint.trim_start_matches("http://").parse()?;
        Ok(reqwest::Client::builder()
            .no_proxy()
            .resolve("files-a.localhost", address)
            .resolve("files-b.localhost", address)
            .timeout(std::time::Duration::from_secs(10))
            .build()?)
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
         GRANT wamn_administration TO file_aged_owner; \
         ALTER TABLE platform.file OWNER TO file_aged_owner")
            .to_string(),
    ))
    .execute(&mut fixture.observer)
    .await
    .map(drop)?;
    let mut importer = connect(&login(fixture.database.url(), "file_aged_owner")?).await?;
    let identity =
        sqlx::query("SELECT rolsuper,rolbypassrls FROM pg_roles WHERE rolname=current_user")
            .fetch_one(&mut importer)
            .await?;
    anyhow::ensure!(
        !identity.get::<bool, _>(0) && !identity.get::<bool, _>(1),
        "fixture import uses no superuser"
    );
    sqlx::query("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:aged-file-fixture',false)").bind(&CALLER).execute(&mut importer).await.map(|result| result.rows_affected())?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("BEGIN; ALTER TABLE platform.file DISABLE TRIGGER wamn_record_history_stamp; \
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

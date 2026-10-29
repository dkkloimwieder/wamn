//! Disposable file metadata and the production administration and guest logins.

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
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
    database: wamn_test_postgres::OwnedDatabase,
    _postgres: wamn_test_postgres::OwnedPostgres,
}

pub async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, driver) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move { driver.await.expect("file fixture database connection") });
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

/// Install schema as bootstrap, then use scoped logins for every fixture write.
pub async fn fixture(name: &str) -> anyhow::Result<Fixture> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database(name)?;
    let observer = connect(database.url()).await?;
    observer
        .batch_execute(
            "CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOBYPASSRLS; \
         CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOBYPASSRLS; \
         CREATE SCHEMA wamn_run",
        )
        .await?;
    observer
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await?;
    observer
        .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
        .await?;
    let mut logins = Vec::new();
    for (family, scope) in [
        (
            WorkloadRoleFamily::Administration,
            WorkloadRoleScope::ProjectEnvironment {
                org: "file-fixture",
                project: "files",
                environment: "dev",
                database: name,
            },
        ),
        (
            WorkloadRoleFamily::App,
            WorkloadRoleScope::EnvKey {
                env_key: "files",
                database: name,
            },
        ),
    ] {
        let role = workload_generation_role(family, scope, CredentialGeneration::A)?;
        observer
            .batch_execute(&sql::prepare_workload_generation_sql(
                family,
                name,
                &role,
                "file-fixture-only",
                "2099-01-01T00:00:00Z",
            ))
            .await?;
        logins.push(connect(&login(database.url(), &role)?).await?);
    }
    observer
        .batch_execute(&format!(
            "CREATE ROLE file_fixture_migrator LOGIN PASSWORD 'file-fixture-only' \
         NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
         GRANT CREATE ON DATABASE \"{name}\" TO file_fixture_migrator; \
         GRANT USAGE ON SCHEMA platform TO file_fixture_migrator; \
         GRANT REFERENCES (id) ON platform.file TO file_fixture_migrator;"
        ))
        .await?;
    let migrator = connect(&login(database.url(), "file_fixture_migrator")?).await?;
    migrator
        .batch_execute(DOCUMENTS)
        .await
        .context("apply the file fixture migration")?;
    observer
        .batch_execute(
            "REVOKE REFERENCES (id) ON platform.file FROM file_fixture_migrator; \
         REVOKE USAGE ON SCHEMA platform FROM file_fixture_migrator; \
         GRANT USAGE ON SCHEMA file_fixture TO wamn_app; \
         GRANT SELECT, INSERT, UPDATE, DELETE ON file_fixture.document TO wamn_app",
        )
        .await?;
    let guest = logins.pop().expect("guest login");
    let writer = logins.pop().expect("administration login");
    for client in [&writer, &guest, &migrator] {
        let row = client
            .query_one(
                "SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname=current_user",
                &[],
            )
            .await?;
        anyhow::ensure!(
            !row.get::<_, bool>(0) && !row.get::<_, bool>(1),
            "scoped fixture login"
        );
    }
    writer
        .execute(
            "SELECT set_config('app.user_id', $1, false), \
                set_config('app.operation', 'admin:file-fixture', false)",
            &[&CALLER],
        )
        .await?;
    writer.execute(
        "INSERT INTO platform.\"user\" (id,type,email) VALUES ($1::text::uuid,'user','file@example.test')",
        &[&CALLER],
    ).await?;
    writer
        .execute(
            "INSERT INTO platform.connection (id,integration,name,transport,exchange,parameters) \
         VALUES ($1::text::uuid,'s3','file-store','s3','request', \
         '{\"container\":\"files\",\"prefix\":\"generation-one/\"}')",
            &[&CONNECTION],
        )
        .await?;
    Ok(Fixture {
        observer,
        writer,
        guest,
        migrator,
        database,
        _postgres: postgres,
    })
}

/// Seed metadata for schema tests, without claiming that an object was uploaded.
pub async fn metadata(writer: &Client) -> anyhow::Result<()> {
    writer
        .execute(
            "INSERT INTO platform.file \
         (id,application,connection_id,connection_generation,name,media_type,size,sha256,blob_key) \
         VALUES ($1::text::uuid,$2,$3::text::uuid,1,'fixture.txt','text/plain',0,repeat('0',64), \
                 'dev/_wamn_files/live/' || $1)",
            &[&FILE, &APPLICATION, &CONNECTION],
        )
        .await?;
    Ok(())
}

pub const STORE_NAME: &str = "file-store";
pub const STORE_PASSWORD: &str = "file-fixture-only";
pub const MINIO_IMAGE: &str =
    "minio/minio@sha256:14cea493d9a34af32f524e538b8346cf79f3321eff8e708c1e2960462bd8936e";
pub const MC_IMAGE: &str =
    "minio/mc@sha256:a7fe349ef4bd8521fb8497f55c6042871b2ae640607cf99d9bede5e9bdf11727";

/// A system database whose real reader login supplies file and secret authority.
pub struct Registry {
    pub owner: Client,
    pub control: Client,
    pub reader: std::sync::Arc<Client>,
    pub unmapped: Client,
    pub unenabled: Client,
    pub org: String,
    pub connection_id: String,
    pub generation: i64,
    pub definition: serde_json::Value,
    _database: wamn_test_postgres::OwnedDatabase,
    _postgres: wamn_test_postgres::OwnedPostgres,
}

impl Registry {
    pub async fn configure(
        &self,
        operation: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        let row: serde_json::Value = self
            .control
            .query_one(
                "SELECT registry.configure_connection($1,$2,$3)",
                &[&self.org, &operation, &payload],
            )
            .await?
            .get(0);
        anyhow::ensure!(
            row.get("error").is_none(),
            "connection fixture refused: {row}"
        );
        Ok(())
    }

    pub async fn move_store(&self, definition: &serde_json::Value) -> anyhow::Result<()> {
        self.configure(
            "connection.write",
            serde_json::json!({
                "name": STORE_NAME, "integration": "s3", "transport": "s3", "exchange": "request",
                "parameters": definition, "status": "active",
            }),
        )
        .await
    }
}

pub async fn lookup(
    client: &Client,
    org: &str,
    project: &str,
    application: &str,
    binding: Option<&str>,
    pin: Option<(&str, i64)>,
) -> anyhow::Result<Vec<tokio_postgres::Row>> {
    Ok(client
        .query(
            "SELECT connection_id::text,connection_name,generation,parameters,mode \
         FROM registry.lookup_file_connection($1,$2,'dev',$3,$4,$5,$6)",
            &[
                &org,
                &project,
                &application,
                &binding,
                &pin.map(|p| p.0),
                &pin.map(|p| p.1),
            ],
        )
        .await?)
}

pub async fn registry(
    org: &str,
    project: &str,
    definition: serde_json::Value,
) -> anyhow::Result<Registry> {
    use std::sync::Arc;
    use wamn_control_provision::{SystemReader, system_reader_generation_role};
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("file_registry")?;
    let bootstrap = connect(database.url()).await?;
    bootstrap
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await?;
    bootstrap
        .batch_execute(&sql::ensure_control_author_acl_role_sql())
        .await?;
    bootstrap
        .batch_execute(
            "CREATE ROLE wamn_system LOGIN PASSWORD 'file-fixture-only' NOSUPERUSER NOBYPASSRLS; \
         ALTER DATABASE file_registry OWNER TO wamn_system",
        )
        .await?;
    let owner = connect(&login(database.url(), "wamn_system")?).await?;
    owner
        .batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .await?;
    owner
        .batch_execute(wamn_control_provision::CONTROL_PORTABLE_STORE_SQL)
        .await?;
    bootstrap
        .batch_execute(&sql::grant_control_surface_sql())
        .await?;
    bootstrap
        .batch_execute(&sql::grant_connection_reader_surface_sql())
        .await?;
    let principal = wamn_control_provision::PlatformComponent::Provisioning;
    // These clients seed several autocommit transactions. Keep their actor
    // bound for the session, as the administration fixture does.
    let bind_actor = format!(
        "SELECT set_config('app.user_id', '{}', false), \
         set_config('app.operation', '{}', false)",
        principal.principal_id(),
        principal.principal_name(),
    );
    owner.batch_execute(&bind_actor).await?;
    for name in [project, "unenabled"] {
        owner
            .batch_execute(
                &wamn_control_provision::test_database::seed_org_project_env_sql(org, name, "dev"),
            )
            .await?;
    }
    bootstrap.batch_execute(
        "CREATE ROLE file_fixture_control LOGIN PASSWORD 'file-fixture-only' NOSUPERUSER NOBYPASSRLS; \
         GRANT wamn_control TO file_fixture_control",
    ).await?;
    let control = connect(&login(database.url(), "file_fixture_control")?).await?;
    control.batch_execute(&bind_actor).await?;
    let mut clients = Vec::new();
    for (scope_project, generation, mapped) in [
        (project, CredentialGeneration::A, true),
        (project, CredentialGeneration::B, false),
        ("unenabled", CredentialGeneration::A, true),
    ] {
        let role = system_reader_generation_role(
            SystemReader::Connection,
            org,
            scope_project,
            "dev",
            "file_registry",
            generation,
        );
        bootstrap
            .batch_execute(&sql::prepare_workload_generation_sql(
                WorkloadRoleFamily::ConnectionReader,
                "file_registry",
                &role,
                STORE_PASSWORD,
                "2099-01-01T00:00:00Z",
            ))
            .await?;
        if mapped {
            owner
                .execute(
                    "INSERT INTO wamn_authority.author_login_tenants \
                 (login_identity,env_key,org_id,project_id,environment) VALUES ($1,$2,$3,$4,'dev')",
                    &[
                        &role,
                        &format!("{org}--{scope_project}--dev"),
                        &org,
                        &scope_project,
                    ],
                )
                .await?;
        }
        clients.push(connect(&login(database.url(), &role)?).await?);
    }
    let unenabled = clients.pop().unwrap();
    let unmapped = clients.pop().unwrap();
    let reader = Arc::new(clients.pop().unwrap());
    for client in [&owner, &control, reader.as_ref(), &unmapped, &unenabled] {
        let row = client
            .query_one(
                "SELECT rolsuper,rolbypassrls FROM pg_roles WHERE rolname=current_user",
                &[],
            )
            .await?;
        anyhow::ensure!(
            !row.get::<_, bool>(0) && !row.get::<_, bool>(1),
            "file registry uses scoped logins"
        );
    }
    let mut fixture = Registry {
        owner,
        control,
        reader,
        unmapped,
        unenabled,
        org: org.into(),
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
        fixture
            .configure(
                "connection.write",
                serde_json::json!({"name":name,"integration":"s3",
            "transport":"s3","exchange":"request","parameters":parameters,"status":"active"}),
            )
            .await?;
        fixture
            .configure(
                "project_connection.enable",
                serde_json::json!({"project":project,"connection":name}),
            )
            .await?;
        let secret =
            serde_json::json!({"ACCESS_KEY_ID":"file-fixture", "ACCESS_SECRET_KEY":STORE_PASSWORD})
                .to_string();
        fixture
            .control
            .query_one(
                "SELECT registry.rotate_connection($1,$2,$3)",
                &[&org, &name, &secret],
            )
            .await?;
    }
    let row = fixture
        .owner
        .query_one(
            "SELECT id::text,current_generation FROM registry.connection WHERE org=$1 AND name=$2",
            &[&org, &STORE_NAME],
        )
        .await?;
    fixture.connection_id = row.get(0);
    fixture.generation = row.get(1);
    for (alias, name, mode) in [
        ("files", STORE_NAME, "write"),
        ("files_read", STORE_NAME, "read"),
        ("files_no_cdn", "files-no-cdn", "write"),
    ] {
        fixture.owner.execute(
            "INSERT INTO registry.environment_connection_binding (org,project,env,application,alias,connection_id,mode,target_id,generation,exclusive) \
             SELECT $1,$2,'dev','files',$3,id,$4,(SELECT id FROM registry.target WHERE name='cloud'),current_generation,false FROM registry.connection WHERE org=$1 AND name=$5",
            &[&org,&project,&alias,&mode,&name],
        ).await?;
    }
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
                "/data",
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
        fixture.mc("mc mb file/files-a file/files-b").await?;
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
            fixture.mc(&format!("printf '%s' '{policy}' > /tmp/file-public-policy.json; mc anonymous set-json /tmp/file-public-policy.json file/{file_bucket}")).await?;
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

    pub async fn mc(&self, script: &str) -> anyhow::Result<String> {
        let script = format!(
            "mc alias set file http://127.0.0.1:9000 file-fixture file-fixture-only >/dev/null; {script}"
        );
        command(
            "docker",
            &[
                "run",
                "--rm",
                "--network",
                &format!("container:{}", self.container),
                "--entrypoint",
                "/bin/sh",
                MC_IMAGE,
                "-ec",
                &script,
            ],
        )
        .await
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
pub async fn age_metadata(fixture: &Fixture) -> anyhow::Result<()> {
    fixture.observer.batch_execute(
        "CREATE ROLE file_aged_owner LOGIN PASSWORD 'file-fixture-only' NOSUPERUSER NOBYPASSRLS; \
         GRANT wamn_administration TO file_aged_owner; \
         ALTER TABLE platform.file OWNER TO file_aged_owner",
    ).await?;
    let importer = connect(&login(fixture.database.url(), "file_aged_owner")?).await?;
    let identity = importer
        .query_one(
            "SELECT rolsuper,rolbypassrls FROM pg_roles WHERE rolname=current_user",
            &[],
        )
        .await?;
    anyhow::ensure!(
        !identity.get::<_, bool>(0) && !identity.get::<_, bool>(1),
        "fixture import uses no superuser"
    );
    importer.execute("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:aged-file-fixture',false)",&[&CALLER]).await?;
    importer
        .batch_execute(
            "BEGIN; ALTER TABLE platform.file DISABLE TRIGGER wamn_record_history_stamp; \
         UPDATE platform.file SET created_at=now()-interval '31 days'; \
         ALTER TABLE platform.file ENABLE TRIGGER wamn_record_history_stamp; COMMIT",
        )
        .await?;
    let enabled: bool = fixture
        .observer
        .query_one(
            "SELECT tgenabled='O' FROM pg_trigger WHERE tgrelid='platform.file'::regclass \
         AND tgname='wamn_record_history_stamp'",
            &[],
        )
        .await?
        .get(0);
    anyhow::ensure!(enabled, "the aged fixture restores the stamp trigger");
    Ok(())
}

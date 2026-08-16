//! PostgreSQL 18 proof for the private control execution-bundle reader.
//!
//! Run only against a disposable cluster:
//! `WAMN_ARTIFACT_READER_POOL_PG18_URL=postgres://.../postgres cargo test \
//!   -p wamn-runtime --test control_artifact_reader_live -- --ignored --nocapture`

use std::num::NonZeroUsize;
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, ensure};
use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use tokio_postgres::{Client, Config, NoTls};
use url::Url;
use wamn_catalog::{
    ExecutionEffectPolicy, ExecutionNodeId, ExecutionPlanBody, ExecutionPlanNode, ExecutionPlanV2,
    ExecutionRuntimeRevision, ExecutionSourceMapEntry, HOST_EFFECT_CONTRACT_VERSION,
    RootTerminalBehavior, execution_bundle_hash,
};
use wamn_control_provision::{
    ARTIFACT_READER_APPLICATION_NAME, ArtifactReaderCredentialScope,
    ArtifactReaderCredentialValidity, CONTROL_BOOTSTRAP_SQL, CredentialGeneration,
    artifact_reader_credential, artifact_reader_endpoint, artifact_reader_generation_role,
    artifact_reader_generation_role_marker, artifact_reader_policy_name,
    artifact_reader_tenant_role, artifact_reader_tenant_role_marker, sql,
};
use wamn_pg_core::quote_ident;
use wamn_runtime::plugins::control_artifact_reader::{
    ControlArtifactReader, ControlArtifactReaderErrorKind,
};

const TENANT: &str = "tenant-a";
const FOREIGN_TENANT: &str = "tenant-b";
const ORG: &str = "pg18proof";
const PROJECT: &str = "artifact-reader";
const ENVIRONMENT: &str = "dev";

struct Fixture {
    admin_url: String,
    database: String,
    stable_role: String,
    generation_a: String,
    generation_b: String,
}

impl Fixture {
    fn new(admin_url: String) -> Self {
        let database = format!("wamn_ar_514_{}", std::process::id());
        Self {
            stable_role: artifact_reader_tenant_role(TENANT, &database),
            generation_a: artifact_reader_generation_role(
                TENANT,
                ORG,
                PROJECT,
                ENVIRONMENT,
                &database,
                CredentialGeneration::A,
            ),
            generation_b: artifact_reader_generation_role(
                TENANT,
                ORG,
                PROJECT,
                ENVIRONMENT,
                &database,
                CredentialGeneration::B,
            ),
            admin_url,
            database,
        }
    }

    fn database_url(&self) -> anyhow::Result<String> {
        database_url(&self.admin_url, &self.database)
    }

    async fn clean(&self) -> anyhow::Result<()> {
        let admin = connect(&self.admin_url).await?;
        for role in [&self.generation_a, &self.generation_b, &self.stable_role] {
            admin
                .query(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                     WHERE usename = $1 AND pid <> pg_backend_pid()",
                    &[role],
                )
                .await
                .with_context(|| format!("terminate exact role {role}"))?;
        }
        admin
            .batch_execute(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                quote_ident(&self.database)
            ))
            .await
            .context("drop exact artifact-reader database")?;
        for role in [&self.generation_a, &self.generation_b, &self.stable_role] {
            admin
                .batch_execute(&format!("DROP ROLE IF EXISTS {}", quote_ident(role)))
                .await
                .with_context(|| format!("drop exact role {role}"))?;
        }
        Ok(())
    }
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let config = Config::from_str(url).context("parse disposable PostgreSQL URL")?;
    let (client, connection) = config
        .connect(NoTls)
        .await
        .context("connect disposable PostgreSQL 18")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

fn database_url(admin_url: &str, database: &str) -> anyhow::Result<String> {
    let mut url = Url::parse(admin_url).context("parse disposable PostgreSQL admin URL")?;
    url.set_path(&format!("/{database}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

fn reader_url(
    admin_url: &str,
    database: &str,
    role: &str,
    password: &str,
) -> anyhow::Result<String> {
    let mut url = Url::parse(admin_url).context("parse artifact-reader authority URL")?;
    url.set_username(role)
        .map_err(|_| anyhow::anyhow!("set artifact-reader username"))?;
    url.set_password(Some(password))
        .map_err(|_| anyhow::anyhow!("set artifact-reader password"))?;
    url.set_path(&format!("/{database}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

fn scope(database: &str) -> ArtifactReaderCredentialScope {
    ArtifactReaderCredentialScope {
        tenant_id: TENANT.to_string(),
        org: ORG.to_string(),
        project: PROJECT.to_string(),
        environment: ENVIRONMENT.to_string(),
        database: database.to_string(),
    }
}

fn valid_plan_bytes() -> Vec<u8> {
    let entry = ExecutionNodeId::new("entry").expect("valid node id");
    let node = ExecutionPlanNode {
        local_node_id: entry.clone(),
        source_node_id: "entry".into(),
        node_type: "event".into(),
        config: serde_json::json!({}),
        effect_policy: ExecutionEffectPolicy::Pure,
        source_connection_requirement: None,
    };
    let plan = ExecutionPlanV2::new(
        ExecutionRuntimeRevision {
            flowrunner_component_digest: format!("sha256:{}", "2".repeat(64)),
            effect_provider_revision: format!("sha256:{}", "1".repeat(64)),
            host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.into(),
        },
        format!("sha256:{}", "3".repeat(64)),
        ExecutionPlanBody {
            entry_instruction: entry.clone(),
            nodes: vec![node],
            edges: Vec::new(),
            root_terminal_behavior: RootTerminalBehavior::FrontierExhaustion,
            entry_input_schema_guard: Value::Bool(true),
            callable_contract: None,
            source_map: vec![ExecutionSourceMapEntry {
                local_node_id: entry,
                source_node_id: "entry".into(),
            }],
        },
    )
    .expect("valid execution plan");
    serde_json::to_vec(&plan).expect("serialize execution plan")
}

async fn insert_bundle(client: &Client, tenant: &str, bytes: &[u8]) -> anyhow::Result<String> {
    let hash = execution_bundle_hash(bytes);
    client
        .execute(
            "INSERT INTO catalog.execution_bundles \
             (tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length) \
             VALUES ($1,$2,'0.1',$3,$4)",
            &[
                &tenant,
                &hash,
                &bytes,
                &i32::try_from(bytes.len()).context("bundle length fits i32")?,
            ],
        )
        .await
        .context("insert execution bundle")?;
    Ok(hash)
}

fn credential_document(
    fixture: &Fixture,
    generation: CredentialGeneration,
    password: &str,
    now: SystemTime,
) -> anyhow::Result<Vec<u8>> {
    let scope = scope(&fixture.database);
    let now = chrono::DateTime::<Utc>::from(now);
    let expires = now + chrono::Duration::hours(1);
    let role = match generation {
        CredentialGeneration::A => &fixture.generation_a,
        CredentialGeneration::B => &fixture.generation_b,
    };
    let credential = artifact_reader_credential(
        &scope,
        "0123456789abcdef0123456789abcdef",
        generation,
        &ArtifactReaderCredentialValidity {
            issued_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
            not_before: now.to_rfc3339_opts(SecondsFormat::Secs, true),
            expires_at: expires.to_rfc3339_opts(SecondsFormat::Secs, true),
            revoked_at: None,
        },
        &reader_url(&fixture.admin_url, &fixture.database, role, password)?,
    );
    serde_json::to_vec(&credential).context("serialize artifact-reader credential")
}

async fn setup(fixture: &Fixture) -> anyhow::Result<Client> {
    let admin = connect(&fixture.admin_url).await?;
    admin
        .batch_execute(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
                 CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await
        .context("prepare canonical control owner role")?;
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_system",
            quote_ident(&fixture.database)
        ))
        .await
        .context("create artifact-reader database")?;
    let database = connect(&fixture.database_url()?).await?;
    database
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("assume canonical control owner role")?;
    for bootstrap in CONTROL_BOOTSTRAP_SQL {
        database
            .batch_execute(bootstrap)
            .await
            .context("apply control portable-store bootstrap")?;
    }
    database
        .batch_execute(&format!(
            "RESET ROLE; REVOKE CONNECT, TEMPORARY ON DATABASE {} FROM PUBLIC",
            quote_ident(&fixture.database)
        ))
        .await
        .context("remove ambient database authority")?;

    let expected = scope(&fixture.database);
    let tenant_marker = artifact_reader_tenant_role_marker(&expected.tenant_scope());
    database
        .batch_execute(&sql::install_artifact_reader_tenant_role_sql(
            &fixture.stable_role,
            &artifact_reader_policy_name(TENANT, &fixture.database),
            TENANT,
            &tenant_marker,
        ))
        .await
        .context("install artifact-reader tenant role")?;
    for (generation, role, password) in [
        (
            CredentialGeneration::A,
            &fixture.generation_a,
            "a".repeat(64),
        ),
        (
            CredentialGeneration::B,
            &fixture.generation_b,
            "b".repeat(64),
        ),
    ] {
        database
            .batch_execute(&sql::prepare_artifact_reader_generation_sql(
                &fixture.database,
                &fixture.stable_role,
                role,
                TENANT,
                &password,
                "2099-01-01T00:00:00Z",
                &artifact_reader_generation_role_marker(&expected, generation),
            ))
            .await
            .context("prepare artifact-reader generation")?;
    }
    Ok(database)
}

async fn exercise(fixture: &Fixture) -> anyhow::Result<()> {
    let database = setup(fixture).await?;
    let now = SystemTime::now();
    let endpoint = artifact_reader_endpoint(&fixture.admin_url)
        .context("derive independently trusted control endpoint")?;
    let document = credential_document(fixture, CredentialGeneration::A, &"a".repeat(64), now)?;
    let reader = ControlArtifactReader::from_credential(
        &document,
        &scope(&fixture.database),
        &endpoint,
        now,
        NonZeroUsize::new(8).expect("nonzero cache"),
    )?;

    let own_one = valid_plan_bytes();
    let mut own_two = valid_plan_bytes();
    own_two.push(b'\n');
    let mut own_three = valid_plan_bytes();
    own_three.extend_from_slice(b"  ");
    let mut foreign = valid_plan_bytes();
    foreign.push(b'\t');
    let malformed = b"not-json".to_vec();
    let mut corrupted_original = valid_plan_bytes();
    corrupted_original.extend_from_slice(b"\r\n");
    let mut corrupted_replacement = valid_plan_bytes();
    corrupted_replacement.extend_from_slice(b" \n");

    let own_one_hash = insert_bundle(&database, TENANT, &own_one).await?;
    let own_two_hash = insert_bundle(&database, TENANT, &own_two).await?;
    let own_three_hash = insert_bundle(&database, TENANT, &own_three).await?;
    let foreign_hash = insert_bundle(&database, FOREIGN_TENANT, &foreign).await?;
    let malformed_hash = insert_bundle(&database, TENANT, &malformed).await?;
    let corrupted_hash = insert_bundle(&database, TENANT, &corrupted_original).await?;
    database
        .batch_execute(
            "ALTER TABLE catalog.execution_bundles \
             DROP CONSTRAINT execution_bundles_exact_hash; \
             ALTER TABLE catalog.execution_bundles \
             DISABLE TRIGGER execution_bundles_immutable",
        )
        .await
        .context("open exact corruption fixture")?;
    database
        .execute(
            "UPDATE catalog.execution_bundles SET exact_bytes=$3, byte_length=$4 \
             WHERE tenant_id=$1 AND execution_bundle_hash=$2",
            &[
                &TENANT,
                &corrupted_hash,
                &corrupted_replacement.as_slice(),
                &i32::try_from(corrupted_replacement.len())?,
            ],
        )
        .await
        .context("corrupt exact bundle bytes")?;
    database
        .batch_execute(
            "ALTER TABLE catalog.execution_bundles \
             ENABLE TRIGGER execution_bundles_immutable",
        )
        .await
        .context("restore immutable bundle trigger")?;

    let loaded = reader
        .read(TENANT, std::slice::from_ref(&own_one_hash))
        .await?;
    ensure!(loaded.len() == 1);
    ensure!(loaded[0].execution_bundle_hash() == own_one_hash);
    ensure!(loaded[0].exact_bytes() == own_one);
    let named_sessions: i64 = database
        .query_one(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE usename=$1 AND application_name=$2",
            &[&fixture.generation_a, &ARTIFACT_READER_APPLICATION_NAME],
        )
        .await
        .context("inspect artifact-reader application session")?
        .get(0);
    ensure!(
        named_sessions == 1,
        "artifact reader did not use its fixed application name"
    );

    for hash in [foreign_hash, format!("sha256:{}", "f".repeat(64))] {
        let error = reader
            .read(TENANT, &[hash])
            .await
            .expect_err("missing row must refuse");
        ensure!(error.kind() == ControlArtifactReaderErrorKind::NotFound);
    }
    ensure!(
        reader
            .read(TENANT, &[malformed_hash])
            .await
            .expect_err("malformed plan must refuse")
            .kind()
            == ControlArtifactReaderErrorKind::Malformed
    );
    ensure!(
        reader
            .read(TENANT, &[corrupted_hash])
            .await
            .expect_err("digest mismatch must refuse")
            .kind()
            == ControlArtifactReaderErrorKind::HashMismatch
    );

    database
        .batch_execute("BEGIN; LOCK TABLE catalog.execution_bundles IN ACCESS EXCLUSIVE MODE")
        .await
        .context("lock execution bundles for timeout proof")?;
    let started = Instant::now();
    let timeout_error = reader
        .read(TENANT, std::slice::from_ref(&own_two_hash))
        .await
        .expect_err("locked query must time out");
    ensure!(timeout_error.kind() == ControlArtifactReaderErrorKind::Timeout);
    ensure!(started.elapsed() < Duration::from_secs(8));
    database
        .batch_execute("ROLLBACK")
        .await
        .context("release execution-bundle lock")?;
    reader
        .read(TENANT, std::slice::from_ref(&own_two_hash))
        .await
        .context("reader recovers after bounded timeout")?;

    database
        .batch_execute(&format!(
            "REVOKE {} FROM {}",
            quote_ident(&fixture.stable_role),
            quote_ident(&fixture.generation_a)
        ))
        .await
        .context("revoke artifact-reader relation authority")?;
    reader
        .read(TENANT, std::slice::from_ref(&own_one_hash))
        .await
        .context("verified cache hit must not access the store")?;
    ensure!(
        reader
            .read(TENANT, &[own_three_hash])
            .await
            .expect_err("uncached read after revoke must fail")
            .kind()
            == ControlArtifactReaderErrorKind::Unavailable
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires disposable PostgreSQL 18 via WAMN_ARTIFACT_READER_POOL_PG18_URL"]
async fn control_artifact_reader_live() {
    let admin_url = std::env::var("WAMN_ARTIFACT_READER_POOL_PG18_URL")
        .expect("WAMN_ARTIFACT_READER_POOL_PG18_URL must name disposable PostgreSQL 18");
    let fixture = Fixture::new(admin_url);
    let preclean = fixture.clean().await;
    let result = match preclean {
        Ok(()) => exercise(&fixture).await,
        Err(error) => Err(error.context("preclean exact artifact-reader fixture")),
    };
    let cleanup = fixture.clean().await;
    if let Err(error) = result {
        panic!("artifact-reader live proof failed: {error:#}");
    }
    if let Err(error) = cleanup {
        panic!("artifact-reader live cleanup failed: {error:#}");
    }
}

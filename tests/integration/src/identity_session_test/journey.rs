//! Disposable identity HTTPS, rotation, and membership assertions.

use std::fs;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily};
use wamn_gate_harness::{case_release, kubernetes_gate, session_issuer};
use wamn_release_control::delivery::Candidate;
use wamn_test_infrastructure::cluster::{self, Cluster};
use wamn_test_infrastructure::scratch::ScratchRoot;

const NAMESPACE: &str = "wamn-identity-jwks";
const IDENTITY: &str = "host-session-identity";

#[tokio::test]
#[ignore = "requires: bin:docker, bin:kind, bin:kubectl, bin:helm, bin:openssl, bin:date, bin:git, env:WAMN_DELIVERY_CANDIDATE"]
async fn identity_jwks_rotation_tls_and_membership_revocation() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:docker, bin:kind, bin:kubectl, bin:helm, bin:openssl, bin:date, bin:git, env:WAMN_DELIVERY_CANDIDATE",
    );
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "the candidate amd64 images require an x86_64 machine"
    );
    let candidate = Candidate::from_env()?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let name = format!("wamn-identity-jwks-{}", uuid::Uuid::new_v4().simple());
    let work = std::env::temp_dir().join(&name);
    fs::DirBuilder::new().mode(0o700).create(&work)?;
    let work = ScratchRoot(work);
    let evidence = std::env::temp_dir().join(format!("{name}-results"));
    fs::DirBuilder::new().mode(0o700).create(&evidence)?;
    println!("Identity journey results: {}", evidence.display());
    let source = cluster::run(
        Command::new("git")
            .current_dir(&repository)
            .args(["rev-parse", "HEAD"]),
    )
    .await?;
    clean_source(&repository).await?;
    let mut fixture = Cluster::new(&name, work.path())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let run = tokio::select! {
      result = async {
        let images = [&candidate.qualification.identity[..], &candidate.qualification.gates[..]];
        let authority = candidate.images.host.split_once('/').context("candidate image registry")?.0;
        if let Some(endpoint) = case_release::registry_endpoint(authority).await? {
            case_release::registry_files(&endpoint, &candidate.images.host, work.path())?;
        } else {
            case_release::node_image_archives(&candidate, &images, work.path()).await?;
        }
        fixture.create(&repository).await?;
        fixture.import_images(&images).await?;
        // The database password stays in a protected file and Docker's env file.
        let password = hex::encode(ring::rand::generate::<[u8; 24]>(&ring::rand::SystemRandom::new()).map_err(|_| anyhow::anyhow!("fixture password generation failed"))?.expose());
        private(&work.path().join("postgres.env"), format!("POSTGRES_PASSWORD={password}\n").as_bytes())?;
        cluster::sidecar(&format!("{name}-postgres"), ["--env-file", &work.path().join("postgres.env").to_string_lossy(), "--publish", "127.0.0.1::5432", "postgres:18"]).await?;
        let network: Value = serde_json::from_slice(&cluster::run(&mut cluster::inspect(&format!("{name}-postgres"))).await?)?;
        let host = wamn_test_infrastructure::workload::kind_address(&network)?;
        let url = format!("postgresql://postgres:{password}@{host}:5432/postgres");
        let (admin, task) = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                if let Ok(pair) = wamn_dev_control::dev::environment::connect(&url).await { break pair; }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }).await.context("identity PostgreSQL TCP readiness")?;
        admin.batch_execute("CREATE ROLE wamn_system NOLOGIN; CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; CREATE ROLE wamn_db_owner NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; CREATE DATABASE wamn_system OWNER wamn_system").await?;
        drop(admin); task.abort();
        let system_url = format!("postgresql://postgres:{password}@{host}:5432/wamn_system");
        let (system, task) = wamn_dev_control::dev::environment::connect(&system_url).await?;
        let version: String = system.query_one("SELECT current_database() || '|' || current_setting('server_version_num')", &[]).await?.get(0);
        ensure!(version.starts_with("wamn_system|18") && version.len() == 18, "identity fixture requires PostgreSQL 18 and wamn_system");
        system.batch_execute("SET ROLE wamn_system").await?;
        system.batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL).await?;
        system.batch_execute("RESET ROLE").await?;
        system.batch_execute(wamn_control_provision::sql::revoke_public_connect_floor_sql()).await?;
        drop(system); task.abort();
        cluster::run(fixture.kubectl().args(["create", "namespace", NAMESPACE])).await?;
        let issuer_cluster = session_issuer::IssuerCluster {
            name: &name, namespace: NAMESPACE, work: work.path(), evidence: &evidence,
            repository: &repository, system_database_url: &system_url,
            identity_image: Some(&candidate.qualification.identity), org: "acme", project: "shared", environment: "dev",
        };
        let issuer = session_issuer::foundation(&issuer_cluster).await?;
        rotation(&fixture, &candidate, &issuer, &evidence).await?;
        sessions(&fixture, &candidate, &issuer_cluster, &issuer, &password, &host.to_string(), &evidence).await?;
        ensure!(cluster::run(Command::new("git").current_dir(&repository).args(["rev-parse", "HEAD"])).await? == source, "identity source changed during the journey");
        clean_source(&repository).await?;
        Ok::<(), anyhow::Error>(())
      } => result,
      _ = interrupt.recv() => Err(anyhow::anyhow!("identity journey received SIGINT")),
      _ = terminate.recv() => Err(anyhow::anyhow!("identity journey received SIGTERM")),
      _ = hangup.recv() => Err(anyhow::anyhow!("identity journey received SIGHUP")),
    };
    fixture.inspect(NAMESPACE, &evidence).await;
    let cleanup = fixture.cleanup().await;
    if !cleanup.leftovers.is_empty() {
        eprintln!("Identity leftover kind resources: {:?}", cleanup.leftovers);
    }
    let private_path = work.path().to_owned();
    drop(work);
    fs::write(
        evidence.join("journey-verdict.json"),
        serde_json::to_vec_pretty(&journey_verdict(
            &source,
            &name,
            run.is_ok(),
            i32::from(run.is_err()),
            &cleanup,
            !private_path.exists(),
        ))?,
    )?;
    run?;
    cleanup.result()?;
    ensure!(
        !private_path.exists(),
        "identity private files remain after cleanup"
    );
    Ok(())
}

fn private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

async fn key(fixture: &Cluster, action: &str, kid: Option<&str>) -> anyhow::Result<Value> {
    let mut command = fixture.kubectl();
    command.args([
        "-n",
        NAMESPACE,
        "exec",
        &format!("deployment/{IDENTITY}"),
        "-c",
        "identity",
        "--",
        "/usr/local/bin/wamn-identity",
        action,
    ]);
    if let Some(kid) = kid {
        command.args(["--kid", kid]);
    }
    let value: Value = serde_json::from_slice(&cluster::run(&mut command).await?)?;
    let observed = value["kid"]
        .as_str()
        .context("key command returns its kid")?;
    let parsed: uuid::Uuid = observed.parse().context("key command returns a UUID")?;
    ensure!(
        parsed.to_string() == observed,
        "key command returns a canonical lowercase UUID"
    );
    if let Some(kid) = kid {
        ensure!(
            observed == kid
                && value[if action == "activate" {
                    "activated"
                } else {
                    "removed"
                }] == true,
            "key command did not apply to its exact key"
        );
    }
    Ok(value)
}

async fn rotation(
    fixture: &Cluster,
    candidate: &Candidate,
    issuer: &str,
    evidence: &Path,
) -> anyhow::Result<()> {
    let a = key(fixture, "publish", None).await?["kid"]
        .as_str()
        .unwrap()
        .to_owned();
    keys_gate(fixture, candidate, issuer, evidence, "published-a", &[&a]).await?;
    key(fixture, "activate", Some(&a)).await?;
    let b = key(fixture, "publish", None).await?["kid"]
        .as_str()
        .unwrap()
        .to_owned();
    ensure!(a != b, "key publication reused an immutable kid");
    keys_gate(
        fixture,
        candidate,
        issuer,
        evidence,
        "published-b",
        &[&a, &b],
    )
    .await?;
    key(fixture, "activate", Some(&b)).await?;
    keys_gate(
        fixture,
        candidate,
        issuer,
        evidence,
        "activated-b",
        &[&a, &b],
    )
    .await?;
    key(fixture, "remove", Some(&a)).await?;
    keys_gate(fixture, candidate, issuer, evidence, "removed-a", &[&b]).await?;
    key(fixture, "remove", Some(&b)).await?;
    keys_gate(fixture, candidate, issuer, evidence, "removed-b", &[]).await
}

async fn keys_gate(
    fixture: &Cluster,
    candidate: &Candidate,
    issuer: &str,
    evidence: &Path,
    stage: &str,
    kids: &[&str],
) -> anyhow::Result<()> {
    let mut env = vec![
        json!({"name":"WAMN_IDENTITY_EXPECTED_KIDS","value":serde_json::to_string(kids)?}),
        json!({"name":"WAMN_IDENTITY_JWKS_URL","value":format!("{issuer}/.well-known/jwks.json")}),
        json!({"name":"WAMN_IDENTITY_WRONG_CA_FILE","value":"/etc/wamn-identity-ca/wrong-ca.crt"}),
    ];
    env.extend(public_env(issuer));
    let result = format!(
        "IDENTITY_JWKS result=pass expected_keys={} token_admission=not_tested expiry_outage=not_tested",
        kids.len()
    );
    let mut expected = vec!["configuration_https", "public_set", "issuer_binding"];
    if !kids.is_empty() {
        expected.push("two_cache_evidence");
    }
    expected.extend([
        "unknown_kid",
        "wrong_ca",
        "plaintext",
        "absent_authority_routes",
    ]);
    let mut lines = expected
        .iter()
        .map(|case| format!("IDENTITY_JWKS case={case} result=pass"))
        .collect::<Vec<_>>();
    lines.push(result.clone());
    gate(
        fixture,
        candidate,
        evidence,
        &format!("identity-keys-test-{stage}"),
        "identity-keys-test",
        env,
        None,
        &result,
        &lines,
    )
    .await
}

fn public_env(issuer: &str) -> Vec<Value> {
    vec![
        json!({"name":"WAMN_IDENTITY_ISSUER","value":issuer}),
        json!({"name":"WAMN_IDENTITY_CA_FILE","value":"/etc/wamn-identity-ca/ca.crt"}),
    ]
}

#[expect(
    clippy::too_many_arguments,
    reason = "the identity owner supplies each exact Job assertion"
)]
async fn gate(
    fixture: &Cluster,
    candidate: &Candidate,
    evidence: &Path,
    name: &str,
    command: &str,
    env: Vec<Value>,
    secret: Option<&str>,
    result: &str,
    lines: &[String],
) -> anyhow::Result<()> {
    let mut volumes =
        vec![json!({"name":"public-ca","configMap":{"name":"host-session-public-ca"}})];
    let mut mounts =
        vec![json!({"name":"public-ca","mountPath":"/etc/wamn-identity-ca","readOnly":true})];
    if let Some(secret) = secret {
        volumes.push(json!({"name":"cases","secret":{"secretName":secret,"defaultMode":256,"items":[{"key":"cases.json","path":"cases.json"}]}}));
        mounts.push(json!({"name":"cases","mountPath":"/etc/wamn-identity-cases","readOnly":true}));
    }
    let manifest = evidence.join(format!("{name}.json"));
    let record = evidence.join(format!("{name}-verdict.json"));
    fs::write(
        &manifest,
        serde_json::to_vec_pretty(
            &json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"name":name,"namespace":NAMESPACE},"spec":{"backoffLimit":0,"activeDeadlineSeconds":120,"template":{"spec":{"restartPolicy":"Never","automountServiceAccountToken":false,"containers":[{"name":command,"image":candidate.qualification.gates,"imagePullPolicy":"Never","args":[command],"env":env,"volumeMounts":mounts}],"volumes":volumes}}}}),
        )?,
    )?;
    let jobs = [kubernetes_gate::Job {
        name: name.into(),
        container: command.into(),
        expectation: "positive".into(),
        exit_code: 0,
        image: candidate.qualification.gates.clone(),
        log_contains: result.into(),
    }];
    let verdict = kubernetes_gate::run(
        &kubernetes_gate::Run {
            manifest: &manifest,
            record: &record,
            namespace: NAMESPACE,
            timeout: Duration::from_secs(180),
            jobs: &jobs,
            probe: None,
        },
        || fixture.kubectl(),
    )
    .await?;
    ensure!(verdict["verdict"] == "pass", "identity Job failed: {name}");
    let logs = cluster::run(fixture.kubectl().args([
        "-n",
        NAMESPACE,
        "logs",
        &format!("job/{name}"),
        "-c",
        command,
    ]))
    .await?;
    fs::write(evidence.join(format!("{name}.log")), &logs)?;
    let logs = std::str::from_utf8(&logs)?
        .lines()
        .filter(|line| {
            line.starts_with(if command == "identity-keys-test" {
                "IDENTITY_JWKS"
            } else {
                "IDENTITY_SESSION "
            })
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ensure!(logs == lines, "identity Job assertion lines differ: {name}");
    Ok(())
}

async fn sessions(
    fixture: &Cluster,
    candidate: &Candidate,
    issuer_cluster: &session_issuer::IssuerCluster<'_>,
    issuer: &str,
    password: &str,
    host: &str,
    evidence: &Path,
) -> anyhow::Result<()> {
    let (system, task) =
        wamn_dev_control::dev::environment::connect(issuer_cluster.system_database_url).await?;
    system.batch_execute("INSERT INTO registry.orgs (id,placement_type,pool_cluster) VALUES ('acme','pooled','fixture'),('other','pooled','fixture'); INSERT INTO registry.env_policies (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('acme','dev','\"own\"',0,1,'1Gi','1','1Gi','postgres:18'),('acme','prod','\"own\"',1,1,'1Gi','1','1Gi','postgres:18'),('other','dev','\"own\"',0,1,'1Gi','1','1Gi','postgres:18'); INSERT INTO registry.projects (org,id) VALUES ('acme','shared'),('other','shared'); INSERT INTO registry.project_envs (org,project,env,secret_name,instance_suffix,policy_name) VALUES ('acme','shared','dev','fixture-acme-dev','k3m9x2p7','dev'),('acme','shared','prod','fixture-acme-prod','p7x2m9k3','prod'),('other','shared','dev','fixture-other-dev','q2n5r8t4','dev');").await?;
    let private_fixture: Value =
        serde_json::from_slice(&super::create_fixture(issuer_cluster.system_database_url).await?)?;
    let user_id = private_fixture["user_id"]
        .as_str()
        .context("fixture user")?;
    let mut targets = Vec::new();
    let mut audiences = Vec::new();
    for (org, env, instance, role) in [
        ("acme", "dev", "k3m9x2p7", "member"),
        ("acme", "prod", "p7x2m9k3", "administrator"),
        ("other", "dev", "q2n5r8t4", "outsider"),
    ] {
        let database =
            wamn_control_provision::project_env_database_name(org, "shared", env, instance);
        system
            .batch_execute(&format!("CREATE DATABASE \"{database}\""))
            .await?;
        let url = format!("postgresql://postgres:{password}@{host}:5432/{database}");
        let (target, driver) = wamn_dev_control::dev::environment::connect(&url).await?;
        target
            .batch_execute(include_str!("../../../../deploy/sql/record-history.sql"))
            .await?;
        target
            .batch_execute(include_str!(
                "../../../../deploy/sql/record-history-app-grants.sql"
            ))
            .await?;
        target
            .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
            .await?;
        target
            .batch_execute(&format!(
                "BEGIN; {} COMMIT;",
                wamn_control_provision::platform_principals_sql("example.invalid")?
            ))
            .await?;
        target.batch_execute(&format!("SET app.user_id = '{user_id}'; SET app.operation = 'admin:seed-session-journey-fixture'; CREATE SCHEMA catalog; CREATE SCHEMA wamn_run; REVOKE CONNECT, TEMPORARY ON DATABASE \"{database}\" FROM PUBLIC; INSERT INTO platform.\"user\" (id,type,email) VALUES ('{user_id}','user','session@example.invalid'); INSERT INTO platform.role (name) VALUES ('{role}'); INSERT INTO platform.user_role (user_id,role_name) VALUES ('{user_id}','{role}');")).await?;
        drop(target);
        driver.abort();
        let secret = fixture
            .work
            .join(format!("session-{org}-{env}-target.json"));
        wamn_environment_control::provision_project_env::run_workload_action(
            &wamn_environment_control::provision_project_env::WorkloadActionRequest {
                org: org.into(),
                project: "shared".into(),
                env: env.into(),
                system_database_url: Some(issuer_cluster.system_database_url.into()),
                target_admin_database_url: Some(url),
                cluster: None,
                db_host: Some(host.into()),
                db_port: 5432,
                namespace: NAMESPACE.into(),
                action: wamn_environment_control::provision_project_env::WorkloadGenerationAction {
                    family: WorkloadRoleFamily::SessionRoleReader,
                    verb: wamn_environment_control::provision_project_env::WorkloadActionVerb::Prepare,
                    generation: CredentialGeneration::A,
                },
                secret: Some(secret.clone()),
                emit_role_sql: None,
                control_administration_patch: None,
            },
        )
        .await?;
        let document: Value = serde_json::from_slice(&fs::read(&secret)?)?;
        let name = wamn_control_provision::workload_secret_name(
            WorkloadRoleFamily::SessionRoleReader,
            org,
            "shared",
            env,
        );
        ensure!(
            document["kind"] == "Secret"
                && document["metadata"]["name"] == name
                && document["metadata"]["namespace"] == NAMESPACE
                && document["stringData"]
                    .as_object()
                    .is_some_and(|d| d.len() == 1 && d.contains_key("target.json")),
            "session target Secret has its exact name and single key"
        );
        let target: Value = serde_json::from_str(
            document["stringData"]["target.json"]
                .as_str()
                .context("target document")?,
        )?;
        audiences.push(
            target["audience"]
                .as_str()
                .context("target audience")?
                .to_owned(),
        );
        targets.push(name);
        cluster::run(fixture.kubectl().args(["apply", "-f"]).arg(&secret)).await?;
    }
    drop(system);
    task.abort();
    let membership = || super::membership::MembershipRequest {
        org: "acme".into(),
        project: "shared".into(),
        env: "dev".into(),
        principal_id: user_id.into(),
        system_database_url: issuer_cluster.system_database_url.into(),
    };
    super::membership::grant(membership()).await?;
    let kid = key(fixture, "publish", None).await?["kid"]
        .as_str()
        .unwrap()
        .to_owned();
    key(fixture, "activate", Some(&kid)).await?;
    session_issuer::install_mode(
        issuer_cluster,
        wamn_environment_control::platform::identity::Mode::Session {
            targets,
            nats: None,
        },
    )
    .await?;
    let cases = json!([
        {"name":"user-dev","pat":private_fixture["user_pat"],"audience":audiences[0],"expected_status":200,"subject":user_id,"org":"acme","roles":["member"]},
        {"name":"user-prod-no-membership","pat":private_fixture["user_pat"],"audience":audiences[1],"expected_status":401,"subject":"","org":"","roles":[]},
        {"name":"user-wrong-org","pat":private_fixture["user_pat"],"audience":audiences[2],"expected_status":401,"subject":"","org":"","roles":[]},
        {"name":"service-pat","pat":private_fixture["service_pat"],"audience":audiences[0],"expected_status":401,"subject":"","org":"","roles":[]}
    ]);
    session_gate(fixture, candidate, issuer, evidence, "initial", &cases).await?;
    super::membership::revoke(
        membership(),
        &wamn_platform_identity::auth_changes::AuthChanges::default(),
    )
    .await?;
    let revoked = json!([{"name":"membership-revoked","pat":private_fixture["user_pat"],"audience":audiences[0],"expected_status":401,"subject":"","org":"","roles":[]}]);
    session_gate(fixture, candidate, issuer, evidence, "revoked", &revoked).await?;
    public_results(
        evidence,
        &[
            password,
            private_fixture["user_pat"].as_str().context("user PAT")?,
            private_fixture["service_pat"]
                .as_str()
                .context("service PAT")?,
        ],
    )
}

async fn session_gate(
    fixture: &Cluster,
    candidate: &Candidate,
    issuer: &str,
    evidence: &Path,
    stage: &str,
    cases: &Value,
) -> anyhow::Result<()> {
    // Apply the private cases over stdin. Only the allowlisted public plan reaches results.
    let name = format!("identity-session-test-{stage}");
    let secret = format!("identity-session-cases-{stage}");
    let items = cases.as_array().context("session cases are an array")?;
    let public = items.iter().map(|case| json!({
        "name":case["name"], "audience":case["audience"], "expected_status":case["expected_status"],
        "subject":case["subject"], "org":case["org"], "roles":case["roles"],
    })).collect::<Vec<_>>();
    fs::write(
        evidence.join(format!("{name}-cases.json")),
        serde_json::to_vec_pretty(&public)?,
    )?;
    let resource = json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":secret,"namespace":NAMESPACE},"stringData":{"cases.json":cases.to_string()}});
    cluster::input(
        fixture.kubectl().args(["apply", "-f", "-"]),
        serde_json::to_vec(&resource)?,
    )
    .await?;
    let mut env = public_env(issuer);
    env.push(json!({"name":"WAMN_IDENTITY_SESSION_CASES_FILE","value":"/etc/wamn-identity-cases/cases.json"}));
    let result = format!(
        "IDENTITY_SESSION result=pass cases={} host_admission=not_tested",
        items.len()
    );
    let mut lines = items
        .iter()
        .map(|case| {
            format!(
                "IDENTITY_SESSION case={} result=pass",
                case["name"].as_str().unwrap()
            )
        })
        .collect::<Vec<_>>();
    lines.push(result.clone());
    gate(
        fixture,
        candidate,
        evidence,
        &name,
        "identity-session-test",
        env,
        Some(&secret),
        &result,
        &lines,
    )
    .await
}

fn public_results(evidence: &Path, secrets: &[&str]) -> anyhow::Result<()> {
    for entry in fs::read_dir(evidence)? {
        let path = entry?.path();
        if path.is_dir() {
            public_results(&path, secrets)?;
            continue;
        }
        let bytes = fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        ensure!(
            !text.contains("BEGIN PRIVATE KEY")
                && !text.contains("BEGIN RSA PRIVATE KEY")
                && secrets
                    .iter()
                    .all(|secret| secret.is_empty() || !text.contains(secret)),
            "identity public results contain private fixture bytes"
        );
    }
    Ok(())
}

#[test]
fn identity_public_results_refuse_credentials_and_private_keys() {
    let root = ScratchRoot::create().unwrap();
    fs::write(root.path().join("public.json"), "public case plan").unwrap();
    public_results(root.path(), &["raw-pat"]).unwrap();
    for secret in [
        "raw-pat",
        "-----BEGIN PRIVATE KEY-----",
        "-----BEGIN RSA PRIVATE KEY-----",
    ] {
        fs::write(root.path().join("leak"), secret).unwrap();
        assert!(public_results(root.path(), &["raw-pat"]).is_err());
    }
}

fn journey_verdict(
    source: &[u8],
    cluster: &str,
    completed: bool,
    case_status: i32,
    cleanup: &cluster::Teardown,
    private_removed: bool,
) -> Value {
    let cleanup_failed = cleanup.result().is_err() || !private_removed;
    let status = if case_status != 0 {
        case_status
    } else {
        i32::from(!completed || cleanup_failed)
    };
    json!({"gate":"IDENTITY-JWKS","source":String::from_utf8_lossy(source).trim(),"cluster":cluster,"completed":completed,"session_exchange_completed":completed,"cleanup_failed":cleanup_failed,"teardown_leftovers":cleanup.leftovers,"exit_code":status,"token_admission":"not_tested","host_admission":"not_tested","expiry_outage":"not_tested","private_files_removed":private_removed})
}

#[test]
fn identity_cleanup_retains_case_failure_and_reports_leftovers() {
    let cleanup = cluster::Teardown {
        leftovers: vec!["wamn-test".into(), "wamn-test-control-plane".into()],
        failures: Vec::new(),
    };
    for status in [0, 7] {
        let verdict = journey_verdict(b"source", "wamn-test", true, status, &cleanup, true);
        assert_eq!(verdict["exit_code"], status);
        assert_eq!(verdict["completed"], true);
        assert_eq!(verdict["cleanup_failed"], false);
        assert_eq!(
            verdict["teardown_leftovers"],
            json!(["wamn-test", "wamn-test-control-plane"])
        );
    }
    assert_eq!(
        journey_verdict(b"source", "wamn-test", true, 0, &cleanup, false)["exit_code"],
        1
    );
}

async fn clean_source(repository: &Path) -> anyhow::Result<()> {
    let status = cluster::run(Command::new("git").current_dir(repository).args([
        "status",
        "--porcelain",
        "--untracked-files=normal",
        "--",
        ".",
        ":(exclude).beads/issues.jsonl",
        ":(exclude).beads/interactions.jsonl",
    ]))
    .await?;
    ensure!(
        status.is_empty(),
        "commit the complete source before the identity journey"
    );
    Ok(())
}

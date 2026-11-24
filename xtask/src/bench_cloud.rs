//! Operator-selected benchmark runs on the existing cloud deployment.

use k8s_openapi::api::{
    batch::v1::Job,
    core::v1::{Pod, Secret},
};
use kube::{
    Api,
    api::{AttachParams, ListParams, Patch, PatchParams},
};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncSeekExt as _, AsyncWrite, AsyncWriteExt as _};
use wamn_kubernetes::{
    Target,
    client::{APPLY_MANAGER, delete_named, wait_named},
};

use crate::{bench_data as data, process};
use anyhow::{Context as _, ensure};
use clap::Args;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Args)]
pub struct TierArgs {}
#[derive(Debug, Args)]
pub struct PgArgs {}

pub async fn tier(root: &Path, _args: &TierArgs) -> anyhow::Result<()> {
    let version =
        wamn_kubernetes::release_chart::chart_version(&root.join("deploy/charts/application"))?;
    let work = tempfile::Builder::new()
        .prefix("wamn-cloud-bench-")
        .tempdir()?
        .keep();
    let setup = std::env::var("HOME")
        .map(PathBuf::from)?
        .join(".cache/wamn-install/platform.sh");
    // This operator-owned file supplies connection functions; repository tasks do not own credentials.
    let shell = format!(
        "source '{}'\ndb_url receiving",
        setup.display().to_string().replace('\'', "'\\''")
    );
    let database = process::output(root, "bash", &["-c", &shell]).await?;
    let database = database.trim();
    let name = database.rsplit('/').next().context("database name")?;
    let directory = std::env::var("G").context("set G to the operator credential directory")?;
    process::run(
        root,
        "gcloud",
        &[
            "compute",
            "scp",
            &format!("{directory}/operator-pat.json"),
            "wamn-bench-client:pat/operator-pat.json",
            "--project",
            "wamn-dev",
            "--zone",
            "us-central1-a",
            "--quiet",
        ],
    )
    .await?;
    let mut credential = tokio::process::Command::new("gcloud");
    credential
        .current_dir(root)
        .args([
            "compute",
            "ssh",
            "wamn-bench-client",
            "--project",
            "wamn-dev",
            "--zone",
            "us-central1-a",
            "--quiet",
            "--command",
            "umask 077; cat > ~/pat/db-url",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null());
    let mut child = credential.spawn()?;
    child
        .stdin
        .take()
        .context("credential stdin")?
        .write_all(database.as_bytes())
        .await?;
    ensure!(
        child.wait().await?.success(),
        "copy benchmark database credential"
    );
    let target = Target::developer("platform").await?;
    let pods = Api::<Pod>::namespaced(target.client.client(), &target.namespace);
    for call in ["get", "update"] {
        psql_in_pod(&pods, name,
            "SELECT pg_stat_statements_reset(0, (SELECT oid FROM pg_database WHERE datname = current_database()))",
        ).await?;
        let remote = format!(
            "cd wamn && cargo xtask bench --host receiving.wamn.dev --pat-file ~/pat/operator-pat.json --call {call} --concurrency 16 --duration-secs 60 --tier tier-2 --db-url \"$(cat ~/pat/db-url)\" --output /tmp/wamn-bench-{call}.json"
        );
        let text = process::output(
            root,
            "gcloud",
            &[
                "compute",
                "ssh",
                "wamn-bench-client",
                "--project",
                "wamn-dev",
                "--zone",
                "us-central1-a",
                "--quiet",
                "--",
                "-o",
                "ExitOnForwardFailure=yes",
                "-R",
                "15432:localhost:15432",
                &remote,
            ],
        )
        .await?;
        std::fs::write(work.join(format!("{call}.json")), text)?;
        let counts = psql_in_pod(&pods, name,
            "SELECT calls FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())",
        ).await?;
        std::fs::write(work.join(format!("{call}.pgss")), counts)?;
    }
    data::result(&work, &work.join(format!("platform-{version}.json")), 16)?;
    println!("Run directory: {}", work.display());
    Ok(())
}

pub async fn pg(root: &Path, _args: &PgArgs) -> anyhow::Result<()> {
    let concurrency = std::env::var("CONCURRENCY")
        .unwrap_or_else(|_| "16".into())
        .parse::<usize>()?;
    let duration = std::env::var("DURATION")
        .unwrap_or_else(|_| "60".into())
        .parse::<u64>()?;
    ensure!(
        concurrency > 0 && concurrency <= 1000 && duration > 0,
        "duration and concurrency must be positive; concurrency must fit the seed"
    );
    let calls = std::env::var("CALLS")
        .unwrap_or_else(|_| "select-only simple-update get update update-no-triggers".into());
    for call in calls.split_whitespace() {
        ensure!(
            [
                "select-only",
                "simple-update",
                "get",
                "update",
                "update-no-triggers"
            ]
            .contains(&call)
                || call
                    .strip_prefix("receipt-")
                    .is_some_and(|lines| lines.parse::<usize>().is_ok_and(|lines| lines > 0)),
            "unknown replay {call}"
        );
    }
    let work = tempfile::Builder::new()
        .prefix("wamn-cloud-pg-")
        .tempdir()?
        .keep();
    println!("Run directory: {}", work.display());
    let version =
        wamn_kubernetes::release_chart::chart_version(&root.join("deploy/charts/application"))?;
    let output = std::env::var_os("OUTPUT").map_or_else(
        || work.join(format!("gcp-pg-{version}.json")),
        PathBuf::from,
    );
    ensure!(!output.exists(), "result already exists");
    let target = Target::developer("platform").await?;
    let secrets = Api::<Secret>::namespaced(target.client.client(), &target.namespace);
    let (certificate, ca) = tokio::try_join!(
        secrets.get("wamn-pg-postgres-tls"),
        secrets.get("wamn-pg-ca")
    )?;
    let credentials = tempfile::tempdir().context("create a directory for benchmark TLS files")?;
    let mut database = url::Url::parse("postgresql://postgres@127.0.0.1:15432/wamn_pg_minimum")?;
    database
        .query_pairs_mut()
        .append_pair("sslmode", "verify-full");
    for (parameter, key, secret) in [
        ("sslcert", "tls.crt", &certificate),
        ("sslkey", "tls.key", &certificate),
        ("sslrootcert", "ca.crt", &ca),
    ] {
        let contents = secret
            .data
            .as_ref()
            .and_then(|data| data.get(key))
            .with_context(|| format!("benchmark TLS Secret has no {key}"))?;
        let path = credentials.path().join(key);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?
            .write_all(&contents.0)
            .with_context(|| format!("write benchmark TLS file {key}"))?;
        database.query_pairs_mut().append_pair(
            parameter,
            path.to_str().context("benchmark TLS path must be UTF-8")?,
        );
    }
    // libpq requires percent-encoded spaces in connection URIs.
    let query = database.query().map(|query| query.replace('+', "%20"));
    database.set_query(query.as_deref());
    let mut maintenance = database.clone();
    maintenance.set_path("postgres");
    let maintenance = maintenance.as_str();
    let database = database.as_str();
    let client_url = "postgresql://postgres@wamn-pg-rw.platform.svc:5432/wamn_pg_minimum";
    let sql = async |url: &str, sql: &str, csv: bool| -> anyhow::Result<String> {
        let mut command = tokio::process::Command::new("psql");
        command.current_dir(root).args([
            url,
            "-X",
            "-v",
            "ON_ERROR_STOP=1",
            if csv { "--csv" } else { "-At" },
            "-c",
            sql,
        ]);
        let result = command.output().await?;
        ensure!(
            result.status.success(),
            "PostgreSQL replay query failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(String::from_utf8(result.stdout)?)
    };
    // This disposable database is the only database whose instrumentation or triggers change.
    sql(
        maintenance,
        "DROP DATABASE IF EXISTS wamn_pg_minimum WITH (FORCE)",
        false,
    )
    .await?;
    sql(maintenance, "CREATE DATABASE wamn_pg_minimum", false).await?;
    for (name, value) in [
        ("pg_stat_statements.track", "'all'"),
        ("pg_stat_statements.track_planning", "on"),
        ("track_io_timing", "on"),
        ("track_wal_io_timing", "on"),
        ("log_transaction_sample_rate", "0.01"),
        ("log_min_duration_sample", "0"),
        ("log_statement_sample_rate", "0.01"),
        ("auto_explain.log_min_duration", "0"),
        ("auto_explain.log_analyze", "on"),
        ("auto_explain.log_buffers", "on"),
        ("auto_explain.log_wal", "on"),
        ("auto_explain.log_nested_statements", "on"),
        ("auto_explain.log_timing", "off"),
        ("auto_explain.sample_rate", "0.01"),
        ("auto_explain.log_format", "json"),
    ] {
        sql(
            maintenance,
            &format!("ALTER DATABASE wamn_pg_minimum SET {name} = {value}"),
            false,
        )
        .await?;
    }
    if let Ok(delay) = std::env::var("COMMIT_DELAY") {
        let delay = delay.parse::<u32>()?;
        sql(maintenance,&format!("ALTER DATABASE wamn_pg_minimum SET commit_delay = {delay}; ALTER DATABASE wamn_pg_minimum SET commit_siblings = 5"),false).await?;
    }
    let build_directory =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    process::execute(
        tokio::process::Command::new(build_directory.join("debug/examples/pg_minimum_database"))
            .current_dir(root)
            .arg(database)
            .arg(root.join("apps/wamn_receiving")),
    )
    .await?;
    process::execute(tokio::process::Command::new("psql").current_dir(root).args([database,"-X","-v","ON_ERROR_STOP=1","-v","scale=1000","-c","SET auto_explain.sample_rate=0; SET log_transaction_sample_rate=0; SET log_statement_sample_rate=0","-f"]).arg(root.join("tests/receiving-tests/fixtures/receiving-seed.sql"))).await?;
    sql(
        database,
        "CREATE EXTENSION IF NOT EXISTS pg_stat_statements",
        false,
    )
    .await?;
    let manifest = wamn_component_policy::PackageManifest::from_slice(&std::fs::read(
        wamn_codegen::package_manifest_path(&root.join("apps/wamn_receiving")),
    )?)?;
    let operation = manifest
        .compiled_operations
        .get("purchase_order.update")
        .context("compiled purchase_order.update")?
        .operation
        .operation
        .as_str();
    let receiving_operation = manifest
        .compiled_operations
        .get("receiving.record_receipt")
        .context("compiled receiving.record_receipt")?
        .operation
        .operation
        .as_str();
    let mut statement = std::fs::read_to_string(
        root.join("apps/wamn_receiving/command/receiving/record_receipt/record_receipt.sql"),
    )?;
    for (parameter, value) in [
        ("$1", ":purchase_order_id::uuid"),
        ("$2", ":line::jsonb"),
        ("$3", ":request_id::text"),
        ("$4", "'2026-10-17T00:00:00Z'::timestamptz"),
    ] {
        statement = statement.replace(parameter, value);
    }
    ensure!(
        !statement.contains('$'),
        "receipt statement has unbound parameters"
    );
    let template = std::fs::read_to_string(root.join("xtask/bench/pg-minimum/receipt.sql"))?;
    std::fs::write(
        work.join("receipt.sql"),
        template.replace("-- record_receipt.sql\n", &statement),
    )?;
    let manifest: Job = serde_yaml::from_slice(&std::fs::read(
        root.join("xtask/bench/pg-minimum/gcp-client.yaml"),
    )?)?;
    let job_name = manifest
        .metadata
        .name
        .as_deref()
        .context("benchmark Job name")?;
    let jobs = Api::<Job>::namespaced(target.client.client(), &target.namespace);
    jobs.patch(
        job_name,
        &PatchParams::apply(APPLY_MANAGER),
        &Patch::Apply(&manifest),
    )
    .await
    .context("apply benchmark client Job")?;
    let pods = Api::<Pod>::namespaced(target.client.client(), &target.namespace);
    let result = async {
        let client = benchmark_pod(&pods).await?;
        let params = AttachParams::default().container("pgbench");
        exec(&pods, &client, &params, ["mkdir", "-p", "/tmp/minimum"], None, &mut tokio::io::sink()).await?;

        let upload = tempfile::tempfile()?;
        let mut archive = tar::Builder::new(upload);
        for name in ["get.sql", "update.sql"] {
            archive.append_path_with_name(root.join("xtask/bench/pg-minimum").join(name), name)?;
        }
        archive.append_path_with_name(work.join("receipt.sql"), "receipt.sql")?;
        let mut upload = tokio::fs::File::from_std(archive.into_inner()?);
        upload.rewind().await?;
        exec(&pods, &client, &AttachParams::default().container("pgbench").stdin(true),
            ["tar", "-C", "/tmp/minimum", "-xf", "-"], Some(&mut upload), &mut tokio::io::sink()).await?;
        exec(&pods, &client, &params, ["pgbench", client_url, "-i", "-s", "10"], None, &mut tokio::io::sink()).await?;
        for call in calls.split_whitespace(){
            let run=SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();let receiving_lines=call.strip_prefix("receipt-");let operation=if receiving_lines.is_some(){receiving_operation}else{operation};
            if let Some(lines)=receiving_lines {process::execute(tokio::process::Command::new("psql").current_dir(root).args([database,"-XAt","-v","ON_ERROR_STOP=1","-v",&format!("clients={concurrency}"),"-v",&format!("lines={lines}"),"-f","xtask/bench/pg-minimum/receipt-slots.sql"])).await?;}
            if call=="update-no-triggers"{sql(database,"ALTER TABLE receiving.purchase_order DISABLE TRIGGER wamn_record_history_stamp, DISABLE TRIGGER wamn_record_history_log",false).await?;}
            sql(database,"SELECT pg_stat_statements_reset(); SELECT pg_stat_reset()",false).await?;std::fs::write(work.join(format!("{call}.database-before.json")),sql(database,include_str!("../bench/database-snapshot.sql"),false).await?)?;
            let builtin=matches!(call,"select-only"|"simple-update");let mut url=if builtin{client_url.to_owned()}else{format!("{client_url}?options=-csearch_path%3Dreceiving,public%20-cstatement_timeout%3D5000%20-crole%3Dwamn_app")};
            if receiving_lines.is_some() { url.push_str("%20-ctrack_functions%3Dpl"); }
            let clients = concurrency.to_string();
            let duration_secs = duration.to_string();
            let log = format!("--log-prefix=/tmp/minimum/{call}.latencies");
            let mut command = ["pgbench", &url, "-n", "-M", "prepared", "-c", &clients, "-j", "2",
                "-T", &duration_secs, "-r", "-l", &log].map(str::to_owned).to_vec();
            for (name, value) in [
                ("clients", clients),
                ("sequence", "0".into()),
                ("run", run.to_string()),
                ("operation", operation.to_owned()),
                ("claim_operation", operation.rsplit_once('@').map_or(operation, |(name, _)| name).to_owned()),
                ("statement_timeout", "5000".into()),
                ("schema", "receiving".into()),
                ("principal", "770df186-ac15-579e-b46b-c297cae2011b".into()),
            ] {
                command.extend(["-D".into(), format!("{name}={value}")]);
            }
            if builtin {
                command.extend(["-b".into(), call.to_owned()]);
            } else {
                let replay = if receiving_lines.is_some() { "receipt" } else { call.strip_suffix("-no-triggers").unwrap_or(call) };
                command.extend(["-f".into(), format!("/tmp/minimum/{replay}.sql")]);
            }
            let mut measured = Vec::new();
            let execution = exec(&pods, &client, &params, command, None, &mut measured).await;
            if call=="update-no-triggers"{sql(database,"ALTER TABLE receiving.purchase_order ENABLE TRIGGER wamn_record_history_stamp, ENABLE TRIGGER wamn_record_history_log",false).await?;}
            let diagnostics = execution?;
            measured.extend_from_slice(diagnostics.as_bytes());
            std::fs::write(work.join(format!("{call}.log")), measured)?;
            std::fs::write(work.join(format!("{call}.database-after.json")),sql(database,include_str!("../bench/database-snapshot.sql"),false).await?)?;
            std::fs::write(work.join(format!("{call}.statements.csv")),sql(database,"SELECT s.*, r.rolname FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) ORDER BY total_exec_time DESC",true).await?)?;data::database_delta(&work,call)?;
            if call.starts_with("update"){let base=operation.rsplit_once('@').map_or(operation,|(name,_)|name).replace('\'',"''");std::fs::write(work.join("update-counts"),sql(database,&format!("SELECT sum(row_version - 1), (SELECT count(*) FROM platform.write_log WHERE operation = '{base}' AND result IS NOT NULL) FROM receiving.purchase_order"),false).await?)?;}
            if receiving_lines.is_some(){tokio::time::sleep(Duration::from_secs(1)).await;std::fs::write(work.join(format!("{call}.functions.csv")),sql(database,"SELECT * FROM pg_stat_user_functions",true).await?)?;std::fs::write(work.join(format!("{call}.receipt-counts")),sql(database,&format!("SELECT count(DISTINCT r.id), count(l.id) FROM receiving.receipt AS r JOIN receiving.receipt_line AS l ON l.receipt_id = r.id WHERE r.receipt_reference LIKE 'bench-{run}-%'"),false).await?)?;}
        }
        let mut archive = tokio::fs::File::from_std(tempfile::tempfile()?);
        exec(&pods, &client, &params, ["tar", "-C", "/tmp/minimum", "-cf", "-", "."], None, &mut archive).await?;
        archive.rewind().await?;
        for file in tar::Archive::new(archive.into_std().await).entries()? {
            let mut file = file?;
            if file.header().entry_type().is_dir() {
                continue;
            }
            ensure!(file.header().entry_type().is_file() && file.unpack_in(&work)?,
                "benchmark archive contains an invalid file entry");
        }
        data::pg_result(&work, &output, concurrency)
    }.await;
    let cleanup = delete_named(&jobs, job_name, false).await;
    result?;
    cleanup.map(drop)
}

async fn psql_in_pod(pods: &Api<Pod>, database: &str, statement: &str) -> anyhow::Result<String> {
    let mut output = Vec::new();
    let params = AttachParams::default().container("postgres");
    exec(
        pods,
        "wamn-pg-1",
        &params,
        [
            "psql",
            "-U",
            "postgres",
            "-d",
            database,
            "-XAt",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            statement,
        ],
        None,
        &mut output,
    )
    .await?;
    String::from_utf8(output).context("benchmark psql output is not UTF-8")
}

async fn benchmark_pod(pods: &Api<Pod>) -> anyhow::Result<String> {
    let params = ListParams::default().labels("job-name=pg-minimum-client");
    let mut started = Vec::new();
    for _ in 0..30 {
        started = tokio::time::timeout(Duration::from_secs(5), pods.list(&params))
            .await??
            .items;
        if !started.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    ensure!(
        !started.is_empty(),
        "benchmark client Pod did not appear within 30 attempts"
    );
    tokio::time::timeout(Duration::from_secs(600), async {
        for pod in started {
            let name = pod.metadata.name.context("benchmark Pod name")?;
            wait_named(pods, &name, Duration::from_secs(600), |pod| {
                let ready = pod
                    .and_then(|pod| pod.status?.conditions)
                    .is_some_and(|conditions| {
                        conditions.iter().any(|condition| {
                            condition.type_ == "Ready" && condition.status == "True"
                        })
                    });
                Ok(ready.then_some(()))
            })
            .await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("wait for benchmark client Pods")??;

    pods.list(&params)
        .await?
        .items
        .into_iter()
        .next()
        .and_then(|pod| pod.metadata.name)
        .context("benchmark client Pod is absent")
}

async fn exec(
    pods: &Api<Pod>,
    pod: &str,
    params: &AttachParams,
    command: impl IntoIterator<Item = impl Into<String>> + std::fmt::Debug,
    input: Option<&mut (dyn AsyncRead + Unpin)>,
    output: &mut (dyn AsyncWrite + Unpin),
) -> anyhow::Result<String> {
    let mut process = pods
        .exec(pod, command, params)
        .await
        .with_context(|| format!("start command in benchmark Pod {pod}"))?;
    let mut stdout = process.stdout().context("benchmark command stdout")?;
    let mut stderr = process.stderr().context("benchmark command stderr")?;
    let status = process.take_status().context("benchmark command status")?;
    let mut diagnostics = Vec::new();
    let send = async {
        if let Some(input) = input {
            let mut stdin = process.stdin().context("benchmark command stdin")?;
            tokio::io::copy(input, &mut stdin).await?;
            stdin.shutdown().await?;
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::try_join!(
        send,
        async { Ok::<_, anyhow::Error>(tokio::io::copy(&mut stdout, output).await?) },
        async { Ok::<_, anyhow::Error>(stderr.read_to_end(&mut diagnostics).await?) },
    )?;
    process.join().await?;
    let status = status
        .await
        .context("benchmark command returned no exit status")?;
    let diagnostics = String::from_utf8_lossy(&diagnostics);
    ensure!(
        status.status.as_deref() == Some("Success"),
        "command in benchmark Pod {pod} failed: {}\n{diagnostics}",
        status.message.unwrap_or_default()
    );
    Ok(diagnostics.into_owned())
}

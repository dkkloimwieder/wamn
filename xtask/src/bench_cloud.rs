//! Operator-selected benchmark runs on the existing cloud deployment.

use base64::Engine as _;
use tokio::io::AsyncWriteExt as _;

use crate::{bench_data as data, process};
use anyhow::{Context as _, ensure};
use clap::Args;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Args)]
pub struct TierArgs {}
#[derive(Debug, Args)]
pub struct PgArgs {}

pub async fn tier(root: &Path, _args: &TierArgs) -> anyhow::Result<()> {
    let version =
        wamn_release_control::release_chart::chart_version(&root.join("deploy/platform/release"))?;
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
    for call in ["get", "update"] {
        process::run(root,"kubectl",&["-n","platform","exec","wamn-pg-1","-c","postgres","--","psql","-U","postgres","-d",name,"-XAt","-v","ON_ERROR_STOP=1","-c","SELECT pg_stat_statements_reset(0, (SELECT oid FROM pg_database WHERE datname = current_database()))"]).await?;
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
        let counts=process::output(root,"kubectl",&["-n","platform","exec","wamn-pg-1","-c","postgres","--","psql","-U","postgres","-d",name,"-XAt","-v","ON_ERROR_STOP=1","-c","SELECT calls FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())"]).await?;
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
        wamn_release_control::release_chart::chart_version(&root.join("deploy/platform/release"))?;
    let output = std::env::var_os("OUTPUT").map_or_else(
        || work.join(format!("gcp-pg-{version}.json")),
        PathBuf::from,
    );
    ensure!(!output.exists(), "result already exists");
    let encoded = process::output(
        root,
        "kubectl",
        &[
            "-n",
            "platform",
            "get",
            "secret",
            "wamn-pg-superuser",
            "-o",
            "jsonpath={.data.password}",
        ],
    )
    .await?;
    let password =
        String::from_utf8(base64::engine::general_purpose::STANDARD.decode(encoded.trim())?)?;
    let database = "postgresql://postgres@127.0.0.1:15432/wamn_pg_minimum?sslmode=disable";
    let client_url = "postgresql://postgres@wamn-pg-rw.platform.svc:5432/wamn_pg_minimum";
    let sql = async |url: &str, sql: &str, csv: bool| -> anyhow::Result<String> {
        let mut command = tokio::process::Command::new("psql");
        command
            .current_dir(root)
            .env("PGPASSWORD", &password)
            .args([
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
        "postgresql://postgres@127.0.0.1:15432/postgres?sslmode=disable",
        "DROP DATABASE IF EXISTS wamn_pg_minimum WITH (FORCE)",
        false,
    )
    .await?;
    sql(
        "postgresql://postgres@127.0.0.1:15432/postgres?sslmode=disable",
        "CREATE DATABASE wamn_pg_minimum",
        false,
    )
    .await?;
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
            "postgresql://postgres@127.0.0.1:15432/postgres?sslmode=disable",
            &format!("ALTER DATABASE wamn_pg_minimum SET {name} = {value}"),
            false,
        )
        .await?;
    }
    if let Ok(delay) = std::env::var("COMMIT_DELAY") {
        let delay = delay.parse::<u32>()?;
        sql("postgresql://postgres@127.0.0.1:15432/postgres?sslmode=disable",&format!("ALTER DATABASE wamn_pg_minimum SET commit_delay = {delay}; ALTER DATABASE wamn_pg_minimum SET commit_siblings = 5"),false).await?;
    }
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let mut url = url::Url::parse(database)?;
    url.set_password(Some(&password))
        .map_err(|()| anyhow::anyhow!("encode PostgreSQL password"))?;
    process::execute(
        tokio::process::Command::new(target.join("debug/examples/pg_minimum_database"))
            .current_dir(root)
            .arg(url.as_str())
            .arg(root.join("apps/wamn_receiving")),
    )
    .await?;
    process::execute(tokio::process::Command::new("psql").current_dir(root).env("PGPASSWORD",&password).args([database,"-X","-v","ON_ERROR_STOP=1","-v","scale=1000","-c","SET auto_explain.sample_rate=0; SET log_transaction_sample_rate=0; SET log_statement_sample_rate=0","-f"]).arg(root.join("apps/wamn_receiving/tests/fixtures/receiving-seed.sql"))).await?;
    sql(
        database,
        "CREATE EXTENSION IF NOT EXISTS pg_stat_statements",
        false,
    )
    .await?;
    let manifest = wamn_compiler::manifest::PackageManifest::from_slice(&std::fs::read(
        wamn_compiler::package_manifest_path(&root.join("apps/wamn_receiving")),
    )?)?;
    let operation = manifest
        .compiled_operations
        .get("purchase_order.update")
        .context("compiled purchase_order.update")?
        .operation
        .operation
        .as_str();
    let receipt = manifest
        .compiled_operations
        .get("receiving.record_receipt")
        .context("compiled receiving.record_receipt")?
        .operation
        .operation
        .as_str();
    let mut statement = std::fs::read_to_string(
        root.join("apps/wamn_receiving/command/record_receipt/record_receipt.sql"),
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
    process::run(
        root,
        "kubectl",
        &["apply", "-f", "xtask/bench/pg-minimum/gcp-client.yaml"],
    )
    .await?;
    let result=async {
        let mut pod_exists = false;
        for _ in 0..30 {
            let pods = process::output(root, "kubectl", &["-n", "platform", "get", "pod", "-l", "job-name=pg-minimum-client", "-o", "name", "--request-timeout=5s"]).await?;
            if !pods.trim().is_empty() { pod_exists = true; break; }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        ensure!(pod_exists, "benchmark client Pod did not appear within 30 attempts");
        process::run(root,"kubectl",&["-n","platform","wait","pod","-l","job-name=pg-minimum-client","--for=condition=Ready","--timeout=600s"]).await?;
        let client=process::output(root,"kubectl",&["-n","platform","get","pod","-l","job-name=pg-minimum-client","-o","jsonpath={.items[0].metadata.name}"]).await?;
        process::run(root,"kubectl",&["-n","platform","exec",&client,"--","mkdir","-p","/tmp/minimum"]).await?;
        for name in ["get","update","receipt"]{let path=if name=="receipt"{work.join("receipt.sql")}else{root.join(format!("xtask/bench/pg-minimum/{name}.sql"))};process::run(root,"kubectl",&["-n","platform","cp",path.to_str().context("replay path")?,&format!("{client}:/tmp/minimum/{name}.sql")]).await?;}
        process::run(root,"kubectl",&["-n","platform","exec",&client,"--","pgbench",client_url,"-i","-s","10"]).await?;
        for call in calls.split_whitespace(){
            let run=SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();let receipt_lines=call.strip_prefix("receipt-");let operation=if receipt_lines.is_some(){receipt}else{operation};
            if let Some(lines)=receipt_lines {process::execute(tokio::process::Command::new("psql").current_dir(root).env("PGPASSWORD",&password).args([database,"-XAt","-v","ON_ERROR_STOP=1","-v",&format!("clients={concurrency}"),"-v",&format!("lines={lines}"),"-f","xtask/bench/pg-minimum/receipt-slots.sql"])).await?;}
            if call=="update-no-triggers"{sql(database,"ALTER TABLE receiving.purchase_order DISABLE TRIGGER wamn_record_history_stamp, DISABLE TRIGGER wamn_record_history_log",false).await?;}
            sql(database,"SELECT pg_stat_statements_reset(); SELECT pg_stat_reset()",false).await?;std::fs::write(work.join(format!("{call}.database-before.json")),sql(database,include_str!("../bench/database-snapshot.sql"),false).await?)?;
            let builtin=matches!(call,"select-only"|"simple-update");let mut url=if builtin{client_url.to_owned()}else{format!("{client_url}?options=-csearch_path%3Dreceiving,public%20-cstatement_timeout%3D5000%20-crole%3Dwamn_app")};
            if receipt_lines.is_some() { url.push_str("%20-ctrack_functions%3Dpl"); }
            let mut command=tokio::process::Command::new("kubectl");command.current_dir(root).args(["-n","platform","exec",&client,"--","pgbench",&url,"-n","-M","prepared","-c",&concurrency.to_string(),"-j","2","-T",&duration.to_string(),"-r","-l",&format!("--log-prefix=/tmp/minimum/{call}.latencies")]);
            for (name,value) in [("clients",concurrency.to_string()),("sequence","0".into()),("run",run.to_string()),("operation",operation.to_owned()),("claim_operation",operation.rsplit_once('@').map_or(operation,|(name,_)|name).to_owned()),("statement_timeout","5000".into()),("schema","receiving".into()),("principal","770df186-ac15-579e-b46b-c297cae2011b".into())]{command.args(["-D",&format!("{name}={value}")]);}
            if builtin{command.args(["-b",call]);}else{command.args(["-f",&format!("/tmp/minimum/{}.sql",if receipt_lines.is_some(){"receipt"}else{call.strip_suffix("-no-triggers").unwrap_or(call)})]);}
            let measured=process::execute(&mut command).await;
            if call=="update-no-triggers"{sql(database,"ALTER TABLE receiving.purchase_order ENABLE TRIGGER wamn_record_history_stamp, ENABLE TRIGGER wamn_record_history_log",false).await?;}
            std::fs::write(work.join(format!("{call}.log")),measured?)?;
            std::fs::write(work.join(format!("{call}.database-after.json")),sql(database,include_str!("../bench/database-snapshot.sql"),false).await?)?;
            std::fs::write(work.join(format!("{call}.statements.csv")),sql(database,"SELECT s.*, r.rolname FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) ORDER BY total_exec_time DESC",true).await?)?;data::database_delta(&work,call)?;
            if call.starts_with("update"){let base=operation.rsplit_once('@').map_or(operation,|(name,_)|name).replace('\'',"''");std::fs::write(work.join("update-counts"),sql(database,&format!("SELECT sum(row_version - 1), (SELECT count(*) FROM platform.write_log WHERE operation = '{base}' AND result IS NOT NULL) FROM receiving.purchase_order"),false).await?)?;}
            if receipt_lines.is_some(){tokio::time::sleep(Duration::from_secs(1)).await;std::fs::write(work.join(format!("{call}.functions.csv")),sql(database,"SELECT * FROM pg_stat_user_functions",true).await?)?;std::fs::write(work.join(format!("{call}.receipt-counts")),sql(database,&format!("SELECT count(DISTINCT r.id), count(l.id) FROM receiving.receipt AS r JOIN receiving.receipt_line AS l ON l.receipt_id = r.id WHERE r.receipt_reference LIKE 'bench-{run}-%'"),false).await?)?;}
        }
        process::run(root,"kubectl",&["-n","platform","cp",&format!("{client}:/tmp/minimum/."),work.to_str().context("run directory")?]).await?;data::pg_result(&work,&output,concurrency)
    }.await;
    let cleanup = process::run(
        root,
        "kubectl",
        &[
            "-n",
            "platform",
            "delete",
            "job",
            "pg-minimum-client",
            "--ignore-not-found",
            "--wait=false",
        ],
    )
    .await;
    result?;
    cleanup
}

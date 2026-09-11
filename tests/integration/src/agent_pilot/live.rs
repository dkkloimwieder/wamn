//! The live grader uses the product binary recorded by its own run.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio_postgres::{NoTls, SimpleQueryMessage};

use super::{GradeContext, GradeFailure, contracts, failure, read_json, text, write, write_json};

#[derive(Debug)]
pub(super) struct HeldLoop {
    child: Child,
}

impl HeldLoop {
    pub async fn stop(&mut self) -> anyhow::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        if let Some(pid) = self.child.id() {
            // This is the exact child created below; no process search chooses a target.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGINT);
            }
        }
        match tokio::time::timeout(Duration::from_secs(30), self.child.wait()).await {
            Ok(result) => {
                result?;
            }
            Err(_) => {
                self.child.start_kill()?;
                self.child.wait().await?;
            }
        }
        Ok(())
    }
}

pub(super) async fn start(context: &GradeContext) -> Result<HeldLoop, GradeFailure> {
    let target = text(&context.recorded["build"]["target"]);
    if target.is_empty() {
        return Err(failure(
            10,
            "run.json names no build target; this run predates the fix and cannot be graded live",
        ));
    }
    let binary = Path::new(target).join("debug/wamn");
    if !binary.is_file() {
        return Err(failure(
            10,
            format!("the run's own wamn is missing at {}", binary.display()),
        ));
    }
    let output = context.grading.directory.join("grade/dev.out");
    let log = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&output)?;
    let child = Command::new(&binary)
        .current_dir(context.grading.directory.join("worktree"))
        .arg("dev")
        .arg("--config")
        .arg(context.grading.directory.join("env/dev.json"))
        .arg("--overlay-root")
        .arg(&context.grading.root)
        .arg("--hold")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()?;
    let mut held = HeldLoop { child };
    for _ in 0..900 {
        if fs::read_to_string(&output)
            .unwrap_or_default()
            .lines()
            .any(|line| line == "run holding")
        {
            break;
        }
        if held.child.try_wait()?.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(held)
}

fn append(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

async fn fire(
    http: &reqwest::Client,
    base: &str,
    path: &str,
    host: &str,
    token: &str,
    body: Value,
    error_path: &Path,
) -> (String, String) {
    let response = http
        .post(format!("{base}{path}"))
        .header("Host", host)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&json!([body])).expect("JSON values serialize"))
        .send()
        .await;
    match response {
        Ok(response) => {
            let status = response.status().as_u16().to_string();
            match response.text().await {
                Ok(payload) => (status, payload),
                Err(error) => {
                    let _ = append(error_path, format!("{error}\n").as_bytes());
                    ("000".to_owned(), String::new())
                }
            }
        }
        Err(error) => {
            let _ = append(error_path, format!("{error}\n").as_bytes());
            ("000".to_owned(), String::new())
        }
    }
}

pub(super) async fn steps(
    context: &mut GradeContext,
    loop_pass: bool,
    base: &str,
) -> anyhow::Result<Vec<Value>> {
    let log = context.grading.directory.join("grade/http.jsonl");
    let errors = context.grading.directory.join("grade/http.err");
    write(&log, b"")?;
    write(&errors, b"")?;
    if !loop_pass || base.is_empty() {
        return context.grading.replay(&[], false, base);
    }
    let token=read_json(&context.grading.directory.join("env/route-caller-pat.json"))?["stringData"]["token"]
        .as_str().context("the route-caller Secret has a token")?.to_owned();
    let host = text(&context.task["identity"]["route_host"]).to_owned();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()?;
    let mut output = Vec::new();
    for step in context.grading.steps.clone() {
        let id = text(&step["id"]);
        let mut payload = String::new();
        let (pass, evidence) = if let Some(sql) = step["sql"].as_str() {
            let environment = read_json(&context.grading.directory.join("env/dev.json"))?;
            let url = environment["target_database_url"]
                .as_str()
                .or(environment["project_database_url"].as_str())
                .unwrap_or_default();
            let rows = match sql_rows(url, sql).await {
                Ok(rows) => rows.to_string(),
                Err(error) => {
                    append(&errors, format!("{error}\n").as_bytes())?;
                    "0".to_owned()
                }
            };
            let expected = step["expect"]["rows"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| step["expect"]["rows"].to_string());
            let record = json!({"id":id,"sql":sql,"rows":rows});
            append(&log, format!("{record}\n").as_bytes())?;
            (
                rows == expected,
                format!("sql rows={rows} expected={expected}"),
            )
        } else if let Some(concurrent) = step["concurrent"].as_array() {
            let mut arms = Vec::new();
            for target in concurrent {
                let target = text(target);
                let arm = context
                    .grading
                    .steps
                    .iter()
                    .find(|arm| arm["id"] == target)
                    .context("a concurrent arm names an existing step")?;
                let operation = text(&arm["route"]["operation"]);
                let file = context
                    .grading
                    .directory
                    .join("grade")
                    .join(format!("{id}-{target}.out"));
                match context
                    .grading
                    .body(arm, operation, &format!("{id}-{target}"))
                {
                    Ok(body) => arms.push((
                        contracts::route(&context.grading.root, operation)?,
                        body,
                        file,
                    )),
                    Err(error) => write(&file, format!("{error}\n").as_bytes())?,
                }
            }
            let futures = arms.iter().map(|(path, body, file)| async {
                let (status, payload) =
                    fire(&http, base, path, &host, &token, body.clone(), &errors).await;
                write(file, format!("{payload}\n{status}\n").as_bytes())
            });
            for result in futures_util::future::join_all(futures).await {
                result?;
            }
            let code = text(&step["expect"]["exactly_one"]["error_code"]);
            let refusals = concurrent
                .iter()
                .filter(|target| {
                    fs::read_to_string(
                        context
                            .grading
                            .directory
                            .join("grade")
                            .join(format!("{id}-{}.out", text(target))),
                    )
                    .unwrap_or_default()
                    .contains(&format!("\"{code}\""))
                })
                .count();
            (
                refusals == 1,
                format!("concurrent refusals={refusals} expected=1 code={code}"),
            )
        } else {
            let operation = text(&step["route"]["operation"]);
            let path = contracts::route(&context.grading.root, operation)?;
            if path.is_empty() {
                (
                    false,
                    format!("no attachment publishes operation {operation}"),
                )
            } else {
                match context.grading.body(&step, operation, id) {
                    Err(error) => (false, error.to_string()),
                    Ok(body) => {
                        let (status, response) =
                            fire(&http, base, &path, &host, &token, body.clone(), &errors).await;
                        payload = response;
                        let record = json!({"id":id,"path":path,"request":body,"status":status,"response":payload});
                        append(&log, format!("{record}\n").as_bytes())?;
                        context
                            .grading
                            .verdict(&step, &status, &payload, operation)?
                    }
                }
            }
        };
        output.push(json!({"id":id,"must":step["must"].as_bool().unwrap_or(false),"invariant":text(&step["invariant"]),"proves":text(&step["proves"]),"pass":pass,"evidence":evidence}));
        context.grading.remember(id, &payload);
        write_json(&context.results, &context.grading.results)?;
    }
    Ok(output)
}

async fn sql_rows(url: &str, sql: &str) -> anyhow::Result<usize> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    let connection = tokio::spawn(connection);
    let result = client.simple_query(sql).await;
    drop(client);
    connection.await??;
    Ok(result?
        .iter()
        .filter_map(|message| match message {
            SimpleQueryMessage::Row(row) => Some(row),
            _ => None,
        })
        .flat_map(|row| {
            (0..row.len())
                .map(|index| row.get(index).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("|")
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|line| !line.is_empty())
        .count())
}

pub(super) async fn verification_removed(context: &GradeContext) -> bool {
    let result = async {
        let environment = read_json(&context.grading.directory.join("env/dev.json"))?;
        let admin = text(&environment["system_database_url"]);
        let configuration: tokio_postgres::Config =
            text(&environment["verification_database_url"]).parse()?;
        let database = configuration
            .get_dbname()
            .context("verification URL names its database")?;
        let (client, connection) = tokio_postgres::connect(admin, NoTls).await?;
        let connection = tokio::spawn(connection);
        let row = client
            .query_one(
                "select count(*) from pg_database where datname = $1",
                &[&database],
            )
            .await;
        drop(client);
        connection.await??;
        Ok::<_, anyhow::Error>(row?.get::<_, i64>(0) == 0)
    }
    .await;
    result.unwrap_or(false)
}

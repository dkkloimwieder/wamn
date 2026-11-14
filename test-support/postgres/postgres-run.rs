//! Run repository commands with private PostgreSQL 18 servers.

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use anyhow::{Context as _, ensure};
use rustix::process::{Pid, Signal, kill_process_group};
use wamn_test_postgres as postgres;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<std::process::ExitCode> {
    let mut args = std::env::args_os().skip(1);
    let mut database = None;
    let mut names = Vec::new();
    let mut schema = None;
    let mut migrations = Vec::new();
    let mut program: Option<OsString> = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--database") => {
                database = Some(
                    args.next()
                        .context("--database requires a name")?
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("the database name must be UTF-8"))?,
                );
            }
            Some("--url-env") => names.push(
                args.next()
                    .context("--url-env requires a variable name")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("the variable name must be UTF-8"))?,
            ),
            Some("--schema") => {
                schema = Some(
                    args.next()
                        .context("--schema requires a name")?
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("the schema must be UTF-8"))?,
                );
            }
            Some("--migration-dir") => migrations.push(PathBuf::from(
                args.next().context("--migration-dir requires a path")?,
            )),
            Some("--") => {
                program = args.next();
                break;
            }
            Some("--help") => {
                println!(
                    "usage: wamn-test-postgres --database NAME --url-env NAME [--url-env NAME...] [--schema NAME] [--migration-dir PATH...] -- COMMAND [ARG...]"
                );
                return Ok(std::process::ExitCode::SUCCESS);
            }
            _ => anyhow::bail!(
                "expected --database, --url-env, --schema, --migration-dir, --help, or -- before the test command"
            ),
        }
    }
    let database = database.context("--database is required")?;
    ensure!(!names.is_empty(), "at least one --url-env is required");
    ensure!(
        names.iter().all(|name| !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')),
        "URL variable names must be uppercase environment names"
    );
    let program = program.context("a test command after -- is required")?;
    let cargo = Path::new(&program).file_stem() == Some(std::ffi::OsStr::new("cargo"));
    let mut command = tokio::process::Command::new(program);
    if cargo {
        command.arg("--config").arg(cargo_runner(
            &database,
            &names,
            schema.as_deref(),
            &migrations,
        )?);
    }
    command.args(args);
    let result = if cargo {
        // Cargo starts this runner separately for each executable. Its binaries
        // therefore never share the server-wide roles of another binary.
        run(&mut command, None, &names).await
    } else {
        // Logical WAL lets a CDC reader stream from the server, as in the cluster.
        let mut server = postgres::start(&[("wal_level", "logical")])?;
        let owned = server.create_database(&database).await?;
        let setup = apply_migrations(&owned, schema.as_deref(), &migrations).await;
        let result = match setup {
            Ok(()) => run(&mut command, Some(&owned), &names).await,
            Err(error) => Err(error),
        };
        server.stop()?;
        result
    };
    let status = result?;
    Ok(std::process::ExitCode::from(
        u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
    ))
}

/// Prefix the native runner from the environment with this server owner.
fn cargo_runner(
    database: &str,
    names: &[String],
    schema: Option<&str>,
    migrations: &[PathBuf],
) -> anyhow::Result<String> {
    let executable = std::env::current_exe()?
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("the runner path must be UTF-8"))?;
    let mut arguments = vec![executable, "--database".to_owned(), database.to_owned()];
    for name in names {
        arguments.extend(["--url-env".to_owned(), name.to_owned()]);
    }
    if let Some(schema) = schema {
        arguments.extend(["--schema".to_owned(), schema.to_owned()]);
    }
    for directory in migrations {
        let directory = std::path::absolute(directory)?
            .into_os_string()
            .into_string()
            .map_err(|_| anyhow::anyhow!("the migration path must be UTF-8"))?;
        arguments.extend(["--migration-dir".to_owned(), directory]);
    }
    arguments.push("--".to_owned());
    if let Some(runner) = std::env::var_os("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER") {
        let runner = runner
            .into_string()
            .map_err(|_| anyhow::anyhow!("the Cargo runner must be UTF-8"))?;
        arguments.extend(runner.split_whitespace().map(str::to_owned));
    }
    Ok(format!(
        "target.x86_64-unknown-linux-gnu.runner={}",
        serde_json::to_string(&arguments)?
    ))
}

/// Run one child, optionally supplying a private database coordinate.
///
/// The child inherits no PostgreSQL variables of this process. A signal to
/// this process stops the child and its process group.
async fn run(
    command: &mut tokio::process::Command,
    database: Option<&postgres::OwnedDatabase>,
    url_env_names: &[String],
) -> anyhow::Result<ExitStatus> {
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            name.starts_with("PG") || name.ends_with("_PG_URL") || name.ends_with("DATABASE_URL")
        }) {
            command.env_remove(name);
        }
    }
    if let Some(database) = database {
        for name in url_env_names {
            command.env(name, database.url());
        }
    }
    command.env("RUST_TEST_THREADS", "1").kill_on_drop(true);
    command.as_std_mut().process_group(0);
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut child = command.spawn().context("start the owned database test")?;
    let group = ProcessGroup(
        Pid::from_raw(
            i32::try_from(child.id().context("the child has a process ID")?)
                .context("the child process ID fits a signed identifier")?,
        )
        .context("the child process ID is positive")?,
    );
    let status = tokio::select! {
        status = child.wait() => status.context("wait for the owned database test"),
        _ = interrupt.recv() => Err(anyhow::anyhow!("the owned database test was interrupted")),
        _ = terminate.recv() => Err(anyhow::anyhow!("the owned database test was terminated")),
        _ = hangup.recv() => Err(anyhow::anyhow!("the owned database test lost its session")),
    };
    drop(group);
    if status.is_err() {
        let _ = child.wait().await;
    }
    status
}

struct ProcessGroup(Pid);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = kill_process_group(self.0, Signal::KILL);
    }
}

/// Reconstruct only the database that this invocation owns.
async fn apply_migrations(
    database: &postgres::OwnedDatabase,
    schema: Option<&str>,
    directories: &[PathBuf],
) -> anyhow::Result<()> {
    let mut client = wamn_postgres::connect(database.url())
        .await
        .context("connect to the owned migration database")?;

    let result = async {
        if let Some(schema) = schema {
            ensure!(
                !schema.is_empty()
                    && schema
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                "the schema must be a lowercase SQL identifier"
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "CREATE SCHEMA {schema}; ALTER DATABASE {} SET search_path TO {schema}, public",
                url::Url::parse(database.url())?
                    .path()
                    .trim_start_matches('/')
            )))
            .execute(&mut client)
            .await
            .map(drop)?;
        }
        for directory in directories {
            let mut files = std::fs::read_dir(directory)
                .with_context(|| format!("read migrations {}", directory.display()))?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()?;
            files.retain(|path| path.extension().is_some_and(|extension| extension == "sql"));
            files.sort();
            ensure!(
                !files.is_empty(),
                "the migration directory contains no SQL files"
            );
            for file in files {
                sqlx::raw_sql(sqlx::AssertSqlSafe(
                    (&std::fs::read_to_string(&file)?).to_string(),
                ))
                .execute(&mut client)
                .await
                .map(drop)
                .with_context(|| format!("apply owned migration {}", file.display()))?;
            }
        }
        Ok(())
    }
    .await;
    drop(client);
    result
}

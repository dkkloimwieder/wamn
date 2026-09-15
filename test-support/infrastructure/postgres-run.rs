//! Run one repository test command against a fresh owned PostgreSQL 18 server.

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use anyhow::{Context as _, ensure};
use rustix::process::{Pid, Signal, kill_process_group};
use wamn_schema_generator::PackageManifest;
use wamn_test_infrastructure::postgres;

const RECORD_HISTORY_SQL: &str = include_str!("../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS_SQL: &str =
    include_str!("../../deploy/sql/record-history-app-grants.sql");

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<std::process::ExitCode> {
    let mut args = std::env::args_os().skip(1);
    let mut database = None;
    let mut names = Vec::new();
    let mut schema = None;
    let mut migrations = Vec::new();
    let mut history_manifest = None;
    let mut program: Option<OsString> = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--database") => {
                database = Some(
                    args.next()
                        .context("--database requires a name")?
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("the database name must be UTF-8"))?,
                )
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
                )
            }
            Some("--migration-dir") => migrations.push(PathBuf::from(
                args.next().context("--migration-dir requires a path")?,
            )),
            Some("--history-manifest") => {
                history_manifest = Some(PathBuf::from(
                    args.next().context("--history-manifest requires a path")?,
                ))
            }
            Some("--") => {
                program = args.next();
                break;
            }
            Some("--help") => {
                println!(
                    "usage: wamn-test-postgres --database NAME --url-env NAME [--url-env NAME...] [--schema NAME] [--migration-dir PATH...] [--history-manifest PATH] -- COMMAND [ARG...]"
                );
                return Ok(std::process::ExitCode::SUCCESS);
            }
            _ => anyhow::bail!("expected --database, --url-env, or -- before the test command"),
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
    let mut command =
        tokio::process::Command::new(program.context("a test command after -- is required")?);
    command.args(args);
    let mut server = postgres::start(&[])?;
    let owned = server.create_database(&database)?;
    let setup = apply_migrations(
        &owned,
        schema.as_deref(),
        &migrations,
        history_manifest.as_deref(),
    )
    .await;
    let result = match setup {
        Ok(()) => run(&mut command, &owned, &names).await,
        Err(error) => Err(error),
    };
    server.stop()?;
    let status = result?;
    Ok(std::process::ExitCode::from(
        u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
    ))
}

/// Run one child with the database coordinate in each named variable.
///
/// The child inherits no PostgreSQL variables of this process. A signal to
/// this process stops the child and its process group.
async fn run(
    command: &mut tokio::process::Command,
    database: &postgres::OwnedDatabase,
    url_env_names: &[String],
) -> anyhow::Result<ExitStatus> {
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            name.starts_with("PG") || name.ends_with("_PG_URL") || name.ends_with("DATABASE_URL")
        }) {
            command.env_remove(name);
        }
    }
    for name in url_env_names {
        command.env(name, database.url());
    }
    command.env("RUST_TEST_THREADS", "1").kill_on_drop(true);
    command.as_std_mut().process_group(0);
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut child = command.spawn().context("start the owned database test")?;
    let group = ProcessGroup(
        Pid::from_raw(child.id().context("the child has a process ID")? as i32)
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
    history_manifest: Option<&Path>,
) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .context("connect to the owned migration database")?;
    let task = tokio::spawn(connection);
    let result = async {
        if let Some(schema) = schema {
            ensure!(
                !schema.is_empty()
                    && schema
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                "the schema must be a lowercase SQL identifier"
            );
            client
                .batch_execute(&format!(
                    "CREATE SCHEMA {schema}; ALTER DATABASE {} SET search_path TO {schema}, public",
                    url::Url::parse(database.url())?
                        .path()
                        .trim_start_matches('/')
                ))
                .await?;
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
                client
                    .batch_execute(&std::fs::read_to_string(&file)?)
                    .await
                    .with_context(|| format!("apply owned migration {}", file.display()))?;
            }
        }
        if let Some(path) = history_manifest {
            create_history_tables(&client, path).await?;
        }
        Ok(())
    }
    .await;
    drop(client);
    let joined = task.await.context("join the owned migration connection")?;
    result?;
    joined.context("drive the owned migration connection")
}

/// Create the history table of each relation that the package declares a log for.
///
/// This follows the generation database step of `docs/operations/running-tests.md`.
/// `wamn_app` exists first, because `record-history-app-grants.sql` grants the history read functions to it.
async fn create_history_tables(client: &tokio_postgres::Client, path: &Path) -> anyhow::Result<()> {
    let manifest = PackageManifest::from_slice(
        &std::fs::read(path).with_context(|| format!("read manifest {}", path.display()))?,
    )
    .with_context(|| format!("parse manifest {}", path.display()))?;
    let logged: Vec<_> = manifest
        .models
        .values()
        .filter(|model| model.owner == manifest.package.id && model.log_retention().is_some())
        .collect();
    if logged.is_empty() {
        return Ok(());
    }
    client
        .batch_execute("CREATE ROLE wamn_app NOLOGIN")
        .await
        .context("create the application role")?;
    client
        .batch_execute(RECORD_HISTORY_SQL)
        .await
        .context("apply record-history.sql")?;
    client
        .batch_execute(RECORD_HISTORY_APP_GRANTS_SQL)
        .await
        .context("apply record-history-app-grants.sql")?;
    for model in logged {
        client
            .execute(
                "SELECT wamn_history.create_history_table($1, $2, false)",
                &[&model.schema, &model.table],
            )
            .await
            .with_context(|| format!("create history table of {}.{}", model.schema, model.table))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn owned_server_runs_children_with_role_coordinates_and_cleans_up() -> anyhow::Result<()>
    {
        let mut server = postgres::start(&[])?;
        let database = server.create_database("delivery_test")?;
        let mut setup = tokio::process::Command::new("sh");
        setup.args(["-c", "/usr/lib/postgresql/18/bin/psql \"$DATABASE_URL\" -XAtq -v ON_ERROR_STOP=1 -c \"CREATE ROLE fixture_reader LOGIN PASSWORD 'fixture-only'\" -c 'GRANT CONNECT ON DATABASE delivery_test TO fixture_reader'"]);
        assert!(
            run(&mut setup, &database, &["DATABASE_URL".to_owned()])
                .await?
                .success()
        );
        let role = database.with_credentials("fixture_reader", "fixture-only")?;
        let mut query = tokio::process::Command::new("sh");
        query.args(["-c", "test \"$(/usr/lib/postgresql/18/bin/psql \"$DATABASE_URL\" -XAt -v ON_ERROR_STOP=1 -c 'SELECT current_user')\" = fixture_reader"]);
        assert!(
            run(&mut query, &role, &["DATABASE_URL".to_owned()])
                .await?
                .success()
        );
        let mut failure = tokio::process::Command::new("sh");
        failure.args(["-c", "exit 17"]);
        assert_eq!(
            run(&mut failure, &database, &["DATABASE_URL".to_owned()])
                .await?
                .code(),
            Some(17)
        );
        let child_pid = std::env::temp_dir().join(format!(
            "wamn-test-postgres-interrupted-child-{}.pid",
            std::process::id()
        ));
        let mut interrupted = tokio::process::Command::new("sh");
        interrupted
            .args([
                "-c",
                "echo $$ > \"$1\"; kill -TERM \"$2\"; sleep 60",
                "fixture",
            ])
            .arg(&child_pid)
            .arg(std::process::id().to_string());
        assert!(
            run(&mut interrupted, &database, &["DATABASE_URL".to_owned()])
                .await
                .is_err()
        );
        let pid = std::fs::read_to_string(&child_pid)?;
        std::fs::remove_file(child_pid)?;
        assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists());
        server.stop()?;
        Ok(())
    }
}

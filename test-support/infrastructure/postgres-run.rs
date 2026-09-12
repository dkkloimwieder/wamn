//! Run one repository test command against a fresh owned PostgreSQL 18 server.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use wamn_test_infrastructure::postgres;

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
            _ => anyhow::bail!("expected --database, --url-env, or -- before the test command"),
        }
    }
    let database = database.context("--database is required")?;
    ensure!(!names.is_empty(), "at least one --url-env is required");
    ensure!(
        names.iter().all(|name| name != postgres::OWNERSHIP_ENV
            && !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')),
        "URL variable names must be uppercase environment names"
    );
    let mut command =
        tokio::process::Command::new(program.context("a test command after -- is required")?);
    command.args(args);
    let mut server = postgres::start()?;
    let owned = server.create_database(&database)?;
    let setup = apply_migrations(&server, &owned, schema.as_deref(), &migrations).await;
    let result = match setup {
        Ok(()) => server.run(&mut command, &owned, &names).await,
        Err(error) => Err(error),
    };
    server.stop()?;
    let status = result?;
    Ok(std::process::ExitCode::from(
        u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
    ))
}

/// Reconstruct only the database that this invocation owns.
async fn apply_migrations(
    server: &postgres::OwnedPostgres,
    database: &postgres::OwnedDatabase,
    schema: Option<&str>,
    directories: &[PathBuf],
) -> anyhow::Result<()> {
    server.require_url(database.url())?;
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
        Ok(())
    }
    .await;
    drop(client);
    let joined = task.await.context("join the owned migration connection")?;
    result?;
    joined.context("drive the owned migration connection")
}

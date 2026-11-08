use anyhow::{Context as _, ensure};
use tokio_postgres::{Client, Transaction};
use wamn_schema_control::RecordedMigration;

/// The developer's explicit decision about one uncertain file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationResolution {
    pub package: String,
    pub path: String,
    pub action: Resolution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Applied,
    Retry,
}

impl std::fmt::Display for MigrationResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}={}",
            self.package,
            self.path,
            match self.action {
                Resolution::Applied => "applied",
                Resolution::Retry => "retry",
            }
        )
    }
}

impl std::str::FromStr for MigrationResolution {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (path, action) = value
            .split_once('=')
            .context("resolution must be <package>:<relative_path>=applied|retry")?;
        let (package, path) = path
            .split_once(':')
            .context("resolution requires a package and migration path")?;
        ensure!(
            !package.is_empty() && !path.is_empty(),
            "resolution requires a package and migration path"
        );
        let action = match action {
            "applied" => Resolution::Applied,
            "retry" => Resolution::Retry,
            _ => anyhow::bail!("resolution must be <package>:<relative_path>=applied|retry"),
        };
        Ok(Self {
            package: package.into(),
            path: path.into(),
            action,
        })
    }
}

/// Route a path to exactly one selected package before applying any SQL.
/// A repeated resolution for an applied file is harmless after a lost result.
pub fn resolution_package<'a>(
    resolution: Option<&MigrationResolution>,
    packages: impl IntoIterator<Item = (&'a str, &'a [RecordedMigration])>,
) -> anyhow::Result<Option<&'a str>> {
    let mut matched = None;
    let mut recorded = false;
    for (id, files) in packages {
        for file in files {
            if file.outcome == wamn_schema_control::MigrationOutcome::Started
                && resolution.is_none()
            {
                anyhow::bail!(
                    "package {id}: migration {} has an uncertain outcome; inspect its effects and use --resolve {id}:{}=applied|retry",
                    file.relative_path,
                    file.relative_path
                );
            }
            if resolution
                .is_some_and(|choice| choice.package == id && choice.path == file.relative_path)
            {
                recorded = true;
                matched = Some(id);
            }
        }
    }
    if let Some(resolution) = resolution {
        ensure!(
            recorded,
            "resolution names no recorded migration: {}",
            resolution.path
        );
    }
    Ok(matched)
}

pub(super) async fn begin<'a>(
    client: &'a mut Client,
    env_key: &str,
) -> anyhow::Result<Transaction<'a>> {
    let tx = client.transaction().await.context("begin package SQL")?;
    tx.query_one(super::CLAIM_ENV_KEY_SQL, &[&env_key])
        .await
        .context("claim package tenant")?;
    super::bind_apply_package_principal(&tx).await?;
    Ok(tx)
}

pub(super) async fn complete(
    tx: &Transaction<'_>,
    env_key: &str,
    package: &str,
    path: &str,
) -> anyhow::Result<()> {
    let updated = tx
        .execute(
            "UPDATE catalog.package_migrations SET outcome = 'applied', applied_at = now() \
         WHERE env_key = $1 AND package_id = $2 AND relative_path = $3 AND outcome = 'started'",
            &[&env_key, &package, &path],
        )
        .await
        .context("record nontransactional completion")?;
    ensure!(
        updated == 1,
        "nontransactional start record disappeared: {path}"
    );
    Ok(())
}

/// Acquire the existing lock key without retaining a snapshot while waiting.
/// A blocking advisory-lock query can deadlock with CREATE INDEX CONCURRENTLY,
/// which waits for older snapshots. Each attempt finishes before the client sleeps.
pub(super) async fn lock_session(
    client: &Client,
    transaction_lock_sql: &str,
    parameters: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> anyhow::Result<()> {
    let key: i64 = client
        .query_one(
            &transaction_lock_sql.replace("pg_advisory_xact_lock", ""),
            parameters,
        )
        .await
        .context("read package schema lock key")?
        .get(0);
    let mut delay = std::time::Duration::from_millis(25);
    let mut reported_holders = None;
    loop {
        if client
            .query_one("SELECT pg_try_advisory_lock($1)", &[&key])
            .await
            .context("try package schema session lock")?
            .get::<_, bool>(0)
        {
            return Ok(());
        }
        let holders = client
            .query(
                "SELECT pid FROM pg_locks WHERE locktype='advisory' AND granted \
             AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) \
             AND classid::bigint=(($1::bigint >> 32) & 4294967295) \
             AND objid::bigint=($1::bigint & 4294967295) AND objsubid=1 ORDER BY pid",
                &[&key],
            )
            .await
            .context("read package schema lock holders")?
            .iter()
            .map(|row| row.get::<_, Option<i32>>(0))
            .collect::<Vec<_>>();
        if reported_holders.as_ref() != Some(&holders) {
            eprintln!("waiting for package schema lock {key}; PostgreSQL holders: {holders:?}");
            reported_holders = Some(holders);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(std::time::Duration::from_millis(250));
    }
}

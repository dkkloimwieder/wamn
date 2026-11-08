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

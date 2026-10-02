//! Own the snapshot dump, private PostgreSQL server, and ownership-preserving restore.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;
use std::str::FromStr as _;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use ring::rand::SecureRandom as _;
use tokio::process::Command;
use tokio_postgres::{Config, NoTls, config::Host};
use wamn_control_provision::sql;
use wamn_control_provision::workload_role::WorkloadRoleFamily;
use wamn_test_postgres::{OwnedDatabase, OwnedPostgres};

const POSTGRES_BIN: &str = "/usr/lib/postgresql/18/bin";
const DATABASE: &str = "wamn_upgrade";
// A copied installed database may be large; both external operations remain bounded.
const COPY_TIMEOUT: Duration = Duration::from_mins(30);
const TERMINATION_GRACE: Duration = Duration::from_secs(5);

/// A qualification-owned server whose destructor also cleans up an interrupted run.
#[derive(Debug)]
pub(crate) struct ScratchDatabase {
    server: OwnedPostgres,
    database: OwnedDatabase,
}

impl ScratchDatabase {
    pub(crate) fn url(&self) -> &str {
        self.database.url()
    }

    pub(crate) async fn finish(self) -> anyhow::Result<()> {
        tokio::task::spawn_blocking(move || {
            let mut server = self.server;
            server.stop()
        })
        .await
        .context("join upgrade scratch cleanup")?
        .context("clean up the upgrade scratch server")
    }
}

struct PrivateDump(PathBuf);

impl PrivateDump {
    fn create() -> anyhow::Result<Self> {
        let mut random = [0_u8; 16];
        ring::rand::SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| anyhow::anyhow!("generate a private upgrade dump name"))?;
        let directory =
            std::env::temp_dir().join(format!("wamn-upgrade-copy-{}", hex::encode(random)));
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .context("create the private upgrade dump directory")?;
        Ok(Self(directory))
    }
}

impl Drop for PrivateDump {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.0.display(), %error, "upgrade dump cleanup failed");
        }
    }
}

/// Copy the exact snapshot while its source transaction remains open in the caller.
pub(crate) async fn copy_database(
    source_url: &str,
    snapshot: &str,
) -> anyhow::Result<ScratchDatabase> {
    ensure!(
        !snapshot.is_empty(),
        "upgrade copy requires an exported snapshot"
    );
    let dump = PrivateDump::create()?;
    let archive = dump.0.join("project.dump");
    let scratch = tokio::task::spawn_blocking(|| {
        let mut server = wamn_test_postgres::start(&[])
            .context("start the private upgrade PostgreSQL server")?;
        let database = server
            .create_database(DATABASE)
            .context("create the upgrade scratch database")?;
        Ok::<_, anyhow::Error>(ScratchDatabase { server, database })
    })
    .await
    .context("join upgrade scratch creation")??;
    prepare_floor(scratch.url()).await?;

    let mut export = Command::new(format!("{POSTGRES_BIN}/pg_dump"));
    connection_environment(&mut export, source_url)?;
    export
        .args(["--format=custom", "--no-password", "--snapshot"])
        .arg(snapshot)
        .arg("--file")
        .arg(&archive);
    run_copy_command(&mut export, "dump the installed predecessor snapshot").await?;

    let mut restore = Command::new(format!("{POSTGRES_BIN}/pg_restore"));
    connection_environment(&mut restore, scratch.url())?;
    restore
        .args([
            "--dbname",
            DATABASE,
            "--no-password",
            "--no-acl",
            "--exit-on-error",
        ])
        .arg(&archive);
    // Deliberately retain owner commands: package DDL must still execute as
    // wamn_db_owner against objects that role actually owns.
    run_copy_command(&mut restore, "restore the owned predecessor copy").await?;
    restore_platform_grants(scratch.url()).await?;
    fs::remove_dir_all(&dump.0).context("remove the private predecessor dump")?;
    Ok(scratch)
}

/// Keep credentials out of process arguments and discard inherited libpq settings.
fn connection_environment(command: &mut Command, database_url: &str) -> anyhow::Result<()> {
    let config = Config::from_str(database_url).context("parse the upgrade database connection")?;
    ensure!(
        config.get_hosts().len() == 1 && config.get_ports().len() <= 1,
        "upgrade copy requires one explicit PostgreSQL host"
    );
    ensure!(
        config.get_hostaddrs().is_empty(),
        "upgrade copy does not accept a separate PostgreSQL hostaddr"
    );
    let hosts = config
        .get_hosts()
        .iter()
        .map(|host| match host {
            Host::Tcp(host) => Ok(host.clone()),
            Host::Unix(path) => path
                .to_str()
                .map(str::to_owned)
                .context("the PostgreSQL socket path must be UTF-8"),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let ports = config
        .get_ports()
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>();
    command.env_clear().env("LC_ALL", "C");
    command.env(
        "PGDATABASE",
        config.get_dbname().context("the database must be named")?,
    );
    if !hosts.is_empty() {
        command.env("PGHOST", hosts.join(","));
    }
    if !ports.is_empty() {
        command.env("PGPORT", ports.join(","));
    }
    if let Some(user) = config.get_user() {
        command.env("PGUSER", user);
    }
    if let Some(password) = config.get_password() {
        use std::os::unix::ffi::OsStrExt as _;
        command.env("PGPASSWORD", std::ffi::OsStr::from_bytes(password));
    }
    if let Some(options) = config.get_options() {
        command.env("PGOPTIONS", options);
    }
    if let Some(timeout) = config.get_connect_timeout() {
        command.env("PGCONNECT_TIMEOUT", timeout.as_secs().max(1).to_string());
    }
    command.env(
        "PGSSLMODE",
        match config.get_ssl_mode() {
            tokio_postgres::config::SslMode::Disable => "disable",
            tokio_postgres::config::SslMode::Prefer => "prefer",
            tokio_postgres::config::SslMode::Require => "require",
            _ => anyhow::bail!("unsupported upgrade database SSL mode"),
        },
    );
    Ok(())
}

async fn run_copy_command(command: &mut Command, action: &str) -> anyhow::Result<()> {
    let output = crate::owned_command::execute(command, COPY_TIMEOUT, TERMINATION_GRACE)
        .await
        .with_context(|| action.to_owned())?;
    ensure!(
        output.status.success(),
        "{action}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

async fn execute_sql(database_url: &str, statements: &str) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to upgrade scratch")?;
    let task = tokio::spawn(connection);
    let result = client.batch_execute(statements).await;
    drop(client);
    task.await
        .context("join upgrade scratch connection")?
        .context("drive upgrade scratch connection")?;
    result.context("apply the upgrade scratch platform floor")
}

async fn prepare_floor(database_url: &str) -> anyhow::Result<()> {
    let mut floor = sql::ensure_db_owner_role_sql().to_owned();
    floor.push_str(&sql::ensure_app_acl_role_sql());
    floor.push_str(&sql::ensure_platform_group_role_sql());
    floor.push_str(wamn_schema_control::ensure_scenario_author_role_sql());
    for family in WorkloadRoleFamily::ALL {
        floor.push_str(&sql::ensure_workload_acl_role_sql(family));
        floor.push_str(&sql::platform_group_membership_sql(family));
    }
    floor.push_str(&sql::install_platform_extensions_sql());
    floor.push_str("GRANT CREATE ON DATABASE wamn_upgrade TO wamn_db_owner;");
    execute_sql(database_url, &floor).await
}

async fn restore_platform_grants(database_url: &str) -> anyhow::Result<()> {
    // Only privileges are reapplied. Restored routines keep their exact source
    // definitions and owners, including the database-bound tenant-key function.
    let mut floor =
        include_str!("../../../../../deploy/sql/record-history-app-grants.sql").to_owned();
    floor.push_str(
        "GRANT USAGE ON SCHEMA wamn_history, wamn_cache TO wamn_db_owner; \
         GRANT EXECUTE ON FUNCTION \
           wamn_history.stamp_row(), \
           wamn_history.create_history_table(text, text, boolean), \
           wamn_history.row_image(record), \
           wamn_history.timestamptz_image(timestamptz), \
           wamn_history.log_row_change(), \
           wamn_cache.note_change(), \
           wamn_cache.bump_changed() TO wamn_db_owner; \
         GRANT USAGE ON SCHEMA wamn_authority TO wamn_app; \
         GRANT EXECUTE ON FUNCTION \
           wamn_authority.tenant_key(text), \
           wamn_authority.current_tenant_key() TO wamn_app;",
    );
    execute_sql(database_url, &floor).await
}

#[cfg(test)]
mod tests {
    use super::copy_database;
    use tokio_postgres::{Client, IsolationLevel, NoTls};

    async fn connect(url: &str) -> Client {
        let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap() });
        client
    }

    #[tokio::test]
    async fn copy_retains_exported_rows_and_owners_without_application_grants() {
        let mut server = wamn_test_postgres::start(&[]).unwrap();
        let database = server.create_database("upgrade_copy_source").unwrap();
        let mut source = connect(database.url()).await;
        source
            .batch_execute(
                "CREATE ROLE wamn_app NOLOGIN; \
                 CREATE ROLE wamn_scenario_author NOLOGIN; \
                 CREATE ROLE wamn_db_owner NOLOGIN;",
            )
            .await
            .unwrap();
        source
            .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
            .await
            .unwrap();
        source
            .batch_execute(
                "CREATE SCHEMA inventory AUTHORIZATION wamn_db_owner; \
                 CREATE TABLE inventory.retained (id int PRIMARY KEY, value text); \
                 ALTER TABLE inventory.retained OWNER TO wamn_db_owner; \
                 INSERT INTO inventory.retained VALUES (1, 'before'); \
                 GRANT USAGE ON SCHEMA inventory TO wamn_app; \
                 GRANT SELECT ON inventory.retained TO wamn_app;",
            )
            .await
            .unwrap();
        let snapshot = source
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await
            .unwrap();
        let exported: String = snapshot
            .query_one("SELECT pg_export_snapshot()", &[])
            .await
            .unwrap()
            .get(0);
        let writer = connect(database.url()).await;
        writer
            .batch_execute("INSERT INTO inventory.retained VALUES (2, 'after')")
            .await
            .unwrap();
        let scratch = copy_database(database.url(), &exported).await.unwrap();
        snapshot.commit().await.unwrap();
        let copy = connect(scratch.url()).await;
        let rows = copy
            .query("SELECT id, value FROM inventory.retained ORDER BY id", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, i32>(0), 1);
        assert_eq!(rows[0].get::<_, String>(1), "before");
        let facts = copy
            .query_one(
                "SELECT pg_get_userbyid(relowner), \
                 has_table_privilege('wamn_app', oid, 'SELECT') \
                 FROM pg_class WHERE oid = 'inventory.retained'::regclass",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(facts.get::<_, String>(0), "wamn_db_owner");
        assert!(!facts.get::<_, bool>(1));
        copy.batch_execute(
            "SET ROLE wamn_db_owner; ALTER TABLE inventory.retained ADD COLUMN note text; RESET ROLE;",
        )
        .await
        .unwrap();
        assert_eq!(
            writer
                .query_one("SELECT count(*) FROM inventory.retained", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            2
        );
        assert_eq!(
            writer.query_one("SELECT count(*) FROM pg_attribute WHERE attrelid = 'inventory.retained'::regclass AND attname = 'note'", &[]).await.unwrap().get::<_, i64>(0),
            0
        );
        drop(copy);
        scratch.finish().await.unwrap();
        drop(writer);
        drop(source);
        server.stop().unwrap();
    }
}

//! Private PostgreSQL 18 servers for tests.
//!
//! [`database`] gives the calling test a database of its own on the server of
//! its test process. [`start`] gives a test a separate server, for example one
//! with other server settings. The owned runner binary uses the same server to
//! pass database coordinates to one child command.

use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write as _;
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use ring::rand::{SecureRandom as _, SystemRandom};
use rustix::process::{Pid, Signal, kill_process_group};
use serde::{Deserialize, Serialize};
use url::Url;

/// The child runner's private record of its live PostgreSQL server and databases.
pub const OWNERSHIP_ENV: &str = "WAMN_TEST_POSTGRES_OWNERSHIP";
/// Presence makes selected test prerequisites mandatory instead of optional.
pub const REQUIRED_ENV: &str = "WAMN_TEST_REQUIRED";
const POSTGRES_BIN: &str = "/usr/lib/postgresql/18/bin";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    pid: u32,
    port: u16,
    databases: BTreeSet<String>,
}

/// Create a database that the calling test owns on the server of this test process.
///
/// The first call starts the server. The server stops, and its directory is
/// removed, when the test process exits, also after a panic. Dropping
/// the returned value drops the database.
///
/// Roles and server settings are shared by every database of the server. Tests
/// of one test process that change them hold [`lock`], or use [`start`].
///
/// # Panics
///
/// Panics when the PostgreSQL 18 binaries in `/usr/lib/postgresql/18/bin` cannot
/// start a server.
pub fn database() -> Database {
    static SERVER: Mutex<Option<OwnedPostgres>> = Mutex::new(None);
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut server = SERVER.lock().unwrap_or_else(PoisonError::into_inner);
    if server.is_none() {
        *server = Some(start(&[]).expect(
            "start the test PostgreSQL server; it requires the PostgreSQL 18 binaries in /usr/lib/postgresql/18/bin",
        ));
    }
    let server = server.as_mut().expect("the test PostgreSQL server started");
    let name = format!("test_{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let owned = server
        .create_database(&name)
        .expect("create the test database");
    Database {
        url: owned.url,
        name,
        port: server.ownership.port,
        password: server.password.clone(),
    }
}

/// Hold the lock of this test process for tests that change shared roles or settings.
///
/// Every test of the process that changes roles or server settings of the
/// [`database`] server, or depends on roles that such a test changes, holds the
/// returned value for its whole duration. A test that panicked releases it.
pub fn lock() -> ProcessLock {
    static LOCK: Mutex<()> = Mutex::new(());
    ProcessLock {
        _guard: LOCK.lock().unwrap_or_else(PoisonError::into_inner),
    }
}

/// The held lock of [`lock`]. Dropping it releases the lock.
#[derive(Debug)]
pub struct ProcessLock {
    _guard: MutexGuard<'static, ()>,
}

/// A database that one test owns on the server of its test process.
pub struct Database {
    url: String,
    name: String,
    port: u16,
    password: String,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Database")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Database {
    /// Return the superuser URL of this database.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Return the name of this database.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run each SQL batch in order, in one superuser session of this database.
    ///
    /// Each batch is one simple query, as `batch_execute` of a client sends it.
    /// The result is the unaligned output of the rows that the batches return.
    ///
    /// Calls in one test process run one at a time. Role bootstrap SQL takes an
    /// advisory lock, and an advisory lock does not reach other databases, so
    /// two setups in different databases of one server could otherwise create
    /// the same role at once.
    pub fn execute(&self, batches: &[&str]) -> anyhow::Result<String> {
        static SETUP: Mutex<()> = Mutex::new(());
        let _setup = SETUP.lock().unwrap_or_else(PoisonError::into_inner);
        psql(self.port, &self.password, &self.name, batches)
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = psql(
            self.port,
            &self.password,
            "postgres",
            &[&format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.name)],
        );
    }
}

/// A database created by its server owner, with credentials omitted from Debug.
pub struct OwnedDatabase {
    url: String,
}

impl std::fmt::Debug for OwnedDatabase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedDatabase")
            .finish_non_exhaustive()
    }
}

impl OwnedDatabase {
    /// Return the exact owned coordinate for the selected connection path.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Select the role under test without changing its owned database coordinate.
    pub fn with_credentials(&self, user: &str, password: &str) -> anyhow::Result<Self> {
        let mut url = Url::parse(&self.url)?;
        ensure!(!user.is_empty(), "the test database role must be named");
        url.set_username(user)
            .expect("PostgreSQL URLs accept a role");
        url.set_password(Some(password))
            .expect("PostgreSQL URLs accept credentials");
        Ok(Self { url: url.into() })
    }
}

/// One private PostgreSQL 18 process, including its server-wide roles and state.
pub struct OwnedPostgres {
    directory: PathBuf,
    watcher: Option<Child>,
    process: Option<Child>,
    ownership: Ownership,
    password: String,
}

impl std::fmt::Debug for OwnedPostgres {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedPostgres")
            .field("directory", &self.directory)
            .field("ownership", &self.ownership)
            .finish_non_exhaustive()
    }
}

/// Create a fresh server without reading any caller-supplied database URL.
///
/// Each setting is a server parameter name and value, such as
/// `("wal_level", "logical")`. Dropping the server stops it and removes its
/// directory. If the process exits first, the server stops and its directory is
/// removed after the exit.
pub fn start(settings: &[(&str, &str)]) -> anyhow::Result<OwnedPostgres> {
    let mut random = [0_u8; 48];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow::anyhow!("create test server identity"))?;
    let directory = std::env::temp_dir().join(format!(
        "wamn-test-postgres-{}-{}",
        std::process::id(),
        hex::encode(&random[..24])
    ));
    DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .context("reserve a private PostgreSQL test directory")?;
    let mut server = OwnedPostgres {
        directory,
        watcher: None,
        process: None,
        ownership: Ownership {
            pid: 0,
            port: 0,
            databases: BTreeSet::from(["postgres".to_owned()]),
        },
        password: hex::encode(&random[24..]),
    };
    server.initialize(settings)?;
    Ok(server)
}

impl OwnedPostgres {
    fn initialize(&mut self, settings: &[(&str, &str)]) -> anyhow::Result<()> {
        // The watcher holds the read end of a pipe whose only write end this
        // process holds. The kernel closes that end when this process exits in
        // any way, so the watcher then stops the server and removes the
        // directory. It runs in its own process group, so a terminal interrupt
        // of the test process does not reach it.
        self.watcher = Some(
            Command::new("/bin/sh")
                .env_clear()
                .env("LC_ALL", "C")
                .args([
                    "-c",
                    r#"read -r _; "$1/pg_ctl" -D "$2/data" -m immediate -w -t 10 stop; rm -rf "$2""#,
                    "wamn-test-postgres-watcher",
                ])
                .arg(POSTGRES_BIN)
                .arg(&self.directory)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .context("start the PostgreSQL test server watcher")?,
        );
        let password_file = self.directory.join("password");
        private_file(&password_file)?.write_all(self.password.as_bytes())?;
        let initialized = Command::new(Path::new(POSTGRES_BIN).join("initdb"))
            .env_clear()
            .env("LC_ALL", "C")
            .arg("-D")
            .arg(self.directory.join("data"))
            .args([
                "--username=postgres",
                "--auth-host=scram-sha-256",
                "--auth-local=reject",
                "--no-locale",
                "--encoding=UTF8",
            ])
            .arg("--pwfile")
            .arg(&password_file)
            .output()
            .context("initialize fresh PostgreSQL 18 state")?;
        ensure!(
            initialized.status.success(),
            "PostgreSQL 18 initialization failed: {}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        fs::remove_file(password_file)?;
        // PostgreSQL owns the port after this reservation closes. A collision
        // makes startup fail; the runner never adopts an existing listener.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        self.ownership.port = port;
        let log = private_file(&self.directory.join("server.log"))?;
        let mut command = Command::new(Path::new(POSTGRES_BIN).join("postgres"));
        command
            .env_clear()
            .env("LC_ALL", "C")
            .arg("-D")
            .arg(self.directory.join("data"))
            .arg("-k")
            .arg(&self.directory)
            .args(["-h", "127.0.0.1", "-p", &port.to_string()]);
        for (name, value) in settings {
            command.arg("-c").arg(format!("{name}={value}"));
        }
        let process = command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()
            .context("start the owned PostgreSQL process")?;
        self.ownership.pid = process.id();
        self.process = Some(process);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            ensure!(
                self.process
                    .as_mut()
                    .expect("the process was started")
                    .try_wait()?
                    .is_none(),
                "the owned PostgreSQL process exited before readiness"
            );
            if let Ok(pid) = fs::read_to_string(self.directory.join("data/postmaster.pid")) {
                if pid
                    .lines()
                    .nth(7)
                    .is_some_and(|state| state.trim() == "ready")
                {
                    break;
                }
            }
            ensure!(
                Instant::now() < deadline,
                "the owned PostgreSQL process did not become ready"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        self.write_ownership()?;
        let ready = self.sql(
            "postgres",
            "SELECT current_setting('server_version_num')::int / 10000",
        )?;
        ensure!(
            ready.trim() == "18",
            "the test server must run PostgreSQL 18"
        );
        Ok(())
    }

    /// Create and record a database before a test can receive its coordinate.
    pub fn create_database(&mut self, name: &str) -> anyhow::Result<OwnedDatabase> {
        ensure!(
            valid_database_name(name),
            "the test database name must be a lowercase SQL identifier"
        );
        ensure!(
            !self.ownership.databases.contains(name),
            "the test database already exists"
        );
        self.sql("postgres", &format!("CREATE DATABASE {name}"))?;
        self.ownership.databases.insert(name.to_owned());
        self.write_ownership()?;
        self.database(name)
    }

    /// Record a database that an existing setup owner created on this server.
    pub fn record_database(&mut self, name: &str) -> anyhow::Result<OwnedDatabase> {
        ensure!(
            valid_database_name(name) && !matches!(name, "template0" | "template1"),
            "the recorded test database must name a created lowercase SQL identifier"
        );
        // sql() checks the private live process record before connecting to
        // this server's recorded postgres database. No external URL is admitted.
        let existing = self.sql(
            "postgres",
            &format!("SELECT datname FROM pg_catalog.pg_database WHERE datname = '{name}'"),
        )?;
        ensure!(
            existing.trim() == name,
            "the setup owner did not create the named database"
        );
        self.ownership.databases.insert(name.to_owned());
        self.write_ownership()?;
        self.database(name)
    }

    /// Return only a database that this server created and recorded.
    pub fn database(&self, name: &str) -> anyhow::Result<OwnedDatabase> {
        ensure!(
            self.ownership.databases.contains(name),
            "the database is not owned by this test runner"
        );
        let mut url = Url::parse("postgresql://postgres@127.0.0.1/postgres")?;
        url.set_port(Some(self.ownership.port))
            .expect("PostgreSQL URLs accept a port");
        url.set_password(Some(&self.password))
            .expect("PostgreSQL URLs accept credentials");
        url.set_path(name);
        self.require_url(url.as_str())?;
        Ok(OwnedDatabase { url: url.into() })
    }

    /// Refuse unowned coordinates before opening a database connection.
    pub fn require_url(&self, url: &str) -> anyhow::Result<()> {
        require_recorded_url(&self.record_path(), url)
    }

    /// Run one child at a time, with only owned database inputs and signal cleanup.
    pub async fn run(
        &mut self,
        command: &mut tokio::process::Command,
        database: &OwnedDatabase,
        url_env_names: &[String],
    ) -> anyhow::Result<ExitStatus> {
        self.require_url(database.url())?;
        for (name, _) in std::env::vars_os() {
            if name.to_str().is_some_and(|name| {
                name.starts_with("PG")
                    || name.ends_with("_PG_URL")
                    || name.ends_with("DATABASE_URL")
            }) {
                command.env_remove(name);
            }
        }
        for name in url_env_names {
            command.env(name, database.url());
        }
        command
            .env(OWNERSHIP_ENV, self.record_path())
            .env(REQUIRED_ENV, "1")
            .env("RUST_TEST_THREADS", "1")
            .kill_on_drop(true);
        command.as_std_mut().process_group(0);
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
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

    /// Stop only this server and remove only its private directory.
    pub fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(mut watcher) = self.watcher.take() {
            drop(watcher.stdin.take());
            watcher
                .wait()
                .context("wait for the PostgreSQL test server watcher")?;
        }
        if let Some(mut process) = self.process.take() {
            if let Some(pid) = Pid::from_raw(process.id() as i32) {
                let _ = kill_process_group(pid, Signal::KILL);
            }
            process
                .wait()
                .context("reap the owned PostgreSQL process")?;
        }
        if self.directory.exists() {
            fs::remove_dir_all(&self.directory).context("remove the owned PostgreSQL directory")?;
        }
        Ok(())
    }

    fn record_path(&self) -> PathBuf {
        self.directory.join("ownership.json")
    }

    fn write_ownership(&self) -> anyhow::Result<()> {
        let path = self.record_path();
        let temporary = self.directory.join("ownership.next");
        serde_json::to_writer(private_file(&temporary)?, &self.ownership)?;
        fs::rename(temporary, path).context("record the owned PostgreSQL databases")
    }

    fn sql(&self, database: &str, sql: &str) -> anyhow::Result<String> {
        let coordinate = self.database(database)?;
        self.require_url(coordinate.url())?;
        psql(self.ownership.port, &self.password, database, &[sql])
    }
}

fn psql(port: u16, password: &str, database: &str, batches: &[&str]) -> anyhow::Result<String> {
    let mut command = Command::new(Path::new(POSTGRES_BIN).join("psql"));
    command
        .env_clear()
        .env("LC_ALL", "C")
        .env("PGHOST", "127.0.0.1")
        .env("PGPORT", port.to_string())
        .env("PGUSER", "postgres")
        .env("PGPASSWORD", password)
        .env("PGDATABASE", database)
        .args(["-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1"]);
    for batch in batches {
        command.arg("-c").arg(batch);
    }
    let output = command
        .output()
        .context("query the owned PostgreSQL connection")?;
    ensure!(
        output.status.success(),
        "the owned PostgreSQL query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("read the owned PostgreSQL result")
}

impl Drop for OwnedPostgres {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct ProcessGroup(Pid);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = kill_process_group(self.0, Signal::KILL);
    }
}

/// Require the current runner's ownership record before a child connects.
pub fn require_owned_url(url: &str) -> anyhow::Result<()> {
    let path = std::env::var_os(OWNERSHIP_ENV)
        .context("run this test through wamn-test-postgres; its ownership record is required")?;
    require_recorded_url(Path::new(&path), url)
}

/// Enforce ownership in delivery runs while retaining legacy manual test inputs.
pub fn require_owned_url_when_recorded(url: &str) -> anyhow::Result<()> {
    if std::env::var_os(OWNERSHIP_ENV).is_some() || std::env::var_os(REQUIRED_ENV).is_some() {
        require_owned_url(url)?;
    }
    Ok(())
}

fn require_recorded_url(path: &Path, input: &str) -> anyhow::Result<()> {
    let directory = path
        .parent()
        .context("the database ownership record has no directory")?;
    ensure!(
        directory.is_absolute()
            && directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("wamn-test-postgres-")),
        "the database ownership record is outside an owned fixture directory"
    );
    ensure!(
        fs::symlink_metadata(directory)?.file_type().is_dir()
            && fs::metadata(directory)?.permissions().mode() & 0o077 == 0,
        "the database fixture directory must be private"
    );
    let record_meta = fs::symlink_metadata(path)?;
    ensure!(
        record_meta.file_type().is_file() && record_meta.permissions().mode() & 0o077 == 0,
        "the database ownership record must be a private regular file"
    );
    let ownership: Ownership = serde_json::from_slice(&fs::read(path)?)?;
    let data = directory.join("data");
    let pid = fs::read_to_string(data.join("postmaster.pid"))
        .context("the owned PostgreSQL process is absent")?;
    let lines = pid.lines().collect::<Vec<_>>();
    ensure!(
        lines.first().and_then(|pid| pid.parse::<u32>().ok()) == Some(ownership.pid)
            && lines.get(1).is_some_and(|path| Path::new(path) == data)
            && lines.get(3).and_then(|port| port.parse::<u16>().ok()) == Some(ownership.port),
        "the PostgreSQL process no longer matches the ownership record"
    );
    let command = fs::read(format!("/proc/{}/cmdline", ownership.pid))
        .context("the owned PostgreSQL process is no longer running")?;
    let args = command.split(|byte| *byte == 0).collect::<Vec<_>>();
    ensure!(
        args.first()
            == Some(
                &Path::new(POSTGRES_BIN)
                    .join("postgres")
                    .as_os_str()
                    .as_encoded_bytes()
            )
            && args
                .windows(2)
                .any(|pair| pair[0] == b"-D" && pair[1] == data.as_os_str().as_encoded_bytes()),
        "the ownership record does not identify the test server process"
    );
    let url = Url::parse(input).context("the test database URL is invalid")?;
    ensure!(
        matches!(url.scheme(), "postgres" | "postgresql")
            && url.host_str() == Some("127.0.0.1")
            && url.port() == Some(ownership.port)
            && ownership
                .databases
                .iter()
                .any(|database| url.path() == format!("/{database}"))
            && url.query_pairs().all(|(key, _)| matches!(
                key.as_ref(),
                "sslmode" | "options" | "application_name" | "connect_timeout"
            )),
        "the database URL is not owned by this test runner"
    );
    Ok(())
}

fn private_file(path: &Path) -> anyhow::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context("create a private PostgreSQL fixture file")
}

fn valid_database_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_server_starts_on_first_use_and_gives_each_call_a_distinct_database() {
        let first = database();
        let second = database();
        assert_ne!(first.name(), second.name());
        assert_eq!(
            Url::parse(first.url()).unwrap().port(),
            Url::parse(second.url()).unwrap().port()
        );
        assert_eq!(
            first
                .execute(&[
                    "CREATE TABLE owned_by_first (id int)",
                    "SELECT current_database() || ' ' || current_setting('server_version_num')::int / 10000",
                ])
                .unwrap()
                .trim(),
            format!("{} 18", first.name())
        );
        assert_eq!(
            second
                .execute(&["SELECT to_regclass('owned_by_first') IS NULL"])
                .unwrap()
                .trim(),
            "t"
        );
        let name = first.name().to_owned();
        drop(first);
        assert_eq!(
            second
                .execute(&[&format!(
                    "SELECT count(*) FROM pg_catalog.pg_database WHERE datname = '{name}'"
                )])
                .unwrap()
                .trim(),
            "0"
        );
    }

    #[test]
    fn process_server_directory_is_removed_after_its_test_process_panics() {
        const CHILD: &str = "WAMN_TEST_POSTGRES_PANICKING_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let database = database();
            let data = database.execute(&["SHOW data_directory"]).unwrap();
            println!("data_directory={}", data.trim());
            panic!("the child test panics while the process server runs");
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::process_server_directory_is_removed_after_its_test_process_panics",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let data = stdout
            .lines()
            .find_map(|line| line.strip_prefix("data_directory="))
            .unwrap_or_else(|| panic!("the child reports its data directory: {stdout}"));
        let directory = Path::new(data).parent().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while directory.exists() {
            assert!(
                Instant::now() < deadline,
                "the process server directory remains after its test process exited"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn separate_server_applies_its_settings() -> anyhow::Result<()> {
        let mut server = start(&[("standard_conforming_strings", "off")])?;
        server.create_database("settings")?;
        assert_eq!(
            server
                .sql("settings", "SHOW standard_conforming_strings")?
                .trim(),
            "off"
        );
        let directory = server.directory.clone();
        drop(server);
        assert!(!directory.exists());
        Ok(())
    }

    #[test]
    fn setup_owner_databases_require_live_recorded_identity() -> anyhow::Result<()> {
        let mut server = start(&[])?;
        assert!(server.record_database("absent_database").is_err());
        assert!(server.record_database("template1").is_err());
        assert!(server.record_database("bad';SELECT 1").is_err());
        server.sql("postgres", "CREATE DATABASE setup_owned")?;
        assert!(server.database("setup_owned").is_err());
        let database = server.record_database("setup_owned")?;
        server.require_url(database.url())?;
        assert_eq!(
            server
                .sql("setup_owned", "SELECT current_database()")?
                .trim(),
            "setup_owned"
        );
        server.stop()?;
        assert!(server.record_database("setup_owned").is_err());
        Ok(())
    }

    #[test]
    fn unowned_input_refuses_before_contacting_its_listener() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "postgresql://postgres@127.0.0.1:{}/interactive",
            listener.local_addr().unwrap().port()
        );
        let error = require_recorded_url(
            Path::new("/does-not-own-a-test-server/ownership.json"),
            &url,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("outside an owned fixture directory")
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test]
    async fn owned_server_guards_coordinates_roles_child_execution_and_cleanup()
    -> anyhow::Result<()> {
        let mut server = start(&[])?;
        let directory = server.directory.clone();
        let record = server.record_path();
        let database = server.create_database("delivery_test")?;
        for input in [
            database.url().replace("/delivery_test", "/interactive"),
            database.url().replace("127.0.0.1", "localhost"),
            format!("{}?host=interactive.example", database.url()),
            format!("{}?hostaddr=127.0.0.1", database.url()),
        ] {
            assert!(server.require_url(&input).is_err());
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let mut foreign = Url::parse(database.url())?;
        foreign
            .set_port(Some(listener.local_addr()?.port()))
            .unwrap();
        assert!(server.require_url(foreign.as_str()).is_err());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );

        server.sql("delivery_test", "CREATE ROLE fixture_reader LOGIN PASSWORD 'fixture-only'; GRANT CONNECT ON DATABASE delivery_test TO fixture_reader")?;
        let role = database.with_credentials("fixture_reader", "fixture-only")?;
        let mut query = tokio::process::Command::new("sh");
        query.args(["-c", "test -f \"$WAMN_TEST_POSTGRES_OWNERSHIP\" && test \"$(/usr/lib/postgresql/18/bin/psql \"$DATABASE_URL\" -XAt -v ON_ERROR_STOP=1 -c 'SELECT current_user')\" = fixture_reader"]);
        assert!(
            server
                .run(&mut query, &role, &["DATABASE_URL".to_owned()])
                .await?
                .success()
        );
        let mut failure = tokio::process::Command::new("sh");
        failure.args(["-c", "exit 17"]);
        assert_eq!(
            server
                .run(&mut failure, &database, &["DATABASE_URL".to_owned()])
                .await?
                .code(),
            Some(17)
        );
        let child_pid = directory.join("interrupted-child.pid");
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
            server
                .run(&mut interrupted, &database, &["DATABASE_URL".to_owned()])
                .await
                .is_err()
        );
        let pid = fs::read_to_string(child_pid)?;
        assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists());
        server.stop()?;
        assert!(!directory.exists());
        assert!(require_recorded_url(&record, database.url()).is_err());
        Ok(())
    }
}

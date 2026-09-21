//! Private PostgreSQL 18 servers for tests.
//!
//! [`database`] gives the calling test a database of its own on the server of
//! its test process. [`start`] gives a test a separate server, for example one
//! with other server settings. The owned runner binary uses the same server to
//! pass database coordinates to one child command. [`require_prerequisites`]
//! fails an ignored test that is missing a declared prerequisite.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use url::Url;

const POSTGRES_BIN: &str = "/usr/lib/postgresql/18/bin";

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
        port: server.port,
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

/// Fail the calling test, naming every missing prerequisite, before it does any work.
///
/// An ignored test calls this on its first line with the names that its
/// `#[ignore = "requires: ..."]` reason lists. A name in upper case is an
/// environment variable that must be set and not empty. Any other name is a
/// program that must be an executable file on `PATH`.
///
/// # Panics
///
/// Panics with one message that names each missing prerequisite.
pub fn require_prerequisites(names: &[&str]) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let missing: Vec<String> = names
        .iter()
        .filter_map(|name| {
            if name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                std::env::var_os(name)
                    .is_none_or(|value| value.is_empty())
                    .then(|| format!("environment variable {name}"))
            } else {
                (!std::env::split_paths(&path).any(|directory| {
                    fs::metadata(directory.join(name))
                        .is_ok_and(|file| file.is_file() && file.permissions().mode() & 0o111 != 0)
                }))
                .then(|| format!("program {name} on PATH"))
            }
        })
        .collect();
    assert!(
        missing.is_empty(),
        "missing prerequisites: {}",
        missing.join(", ")
    );
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
        if let Err(error) = psql(
            self.port,
            &self.password,
            "postgres",
            &[&format!(
                "DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)",
                self.name
            )],
        ) {
            report_cleanup_failure("drop the owned test database", &error);
        }
    }
}

/// A database coordinate of an owned server, with credentials omitted from Debug.
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
    /// Return the superuser coordinate of this database.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Select the role under test without changing the database coordinate.
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
    port: u16,
    password: String,
}

impl std::fmt::Debug for OwnedPostgres {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedPostgres")
            .field("directory", &self.directory)
            .field("port", &self.port)
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
    File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut random))
        .context("create test server identity")?;
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
        port: 0,
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
        self.port = port;
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
            if let Ok(pid) = fs::read_to_string(self.directory.join("data/postmaster.pid"))
                && pid
                    .lines()
                    .nth(7)
                    .is_some_and(|state| state.trim() == "ready")
            {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "the owned PostgreSQL process did not become ready"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
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

    /// Create a database on this server and return its coordinate.
    pub fn create_database(&mut self, name: &str) -> anyhow::Result<OwnedDatabase> {
        ensure!(
            valid_database_name(name),
            "the test database name must be a lowercase SQL identifier"
        );
        self.sql("postgres", &format!("CREATE DATABASE {name}"))?;
        self.database(name)
    }

    /// Return the superuser coordinate of a database of this server.
    pub fn database(&self, name: &str) -> anyhow::Result<OwnedDatabase> {
        let mut url = Url::parse("postgresql://postgres@127.0.0.1/postgres")?;
        url.set_port(Some(self.port))
            .expect("PostgreSQL URLs accept a port");
        url.set_password(Some(&self.password))
            .expect("PostgreSQL URLs accept credentials");
        url.set_path(name);
        Ok(OwnedDatabase { url: url.into() })
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
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", process.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            process
                .wait()
                .context("reap the owned PostgreSQL process")?;
        }
        if self.directory.exists() {
            fs::remove_dir_all(&self.directory).context("remove the owned PostgreSQL directory")?;
        }
        Ok(())
    }

    fn sql(&self, database: &str, sql: &str) -> anyhow::Result<String> {
        psql(self.port, &self.password, database, &[sql])
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
        if let Err(error) = self.stop() {
            report_cleanup_failure("stop the owned PostgreSQL server", &error);
        }
    }
}

fn report_cleanup_failure(action: &str, error: &anyhow::Error) {
    if std::thread::panicking() {
        eprintln!("PostgreSQL test cleanup failed while unwinding: {action}: {error:#}");
    } else {
        panic!("PostgreSQL test cleanup failed: {action}: {error:#}");
    }
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
    fn prerequisites_fail_naming_every_missing_variable_and_program() {
        require_prerequisites(&["PATH", "sh"]);
        let panic = std::panic::catch_unwind(|| {
            require_prerequisites(&[
                "PATH",
                "WAMN_TEST_PREREQUISITE_NEVER_SET",
                "sh",
                "wamn-test-program-never-installed",
            ]);
        })
        .unwrap_err();
        assert_eq!(
            panic.downcast_ref::<String>().map(String::as_str),
            Some(
                "missing prerequisites: environment variable WAMN_TEST_PREREQUISITE_NEVER_SET, program wamn-test-program-never-installed on PATH"
            )
        );
    }

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
    fn database_drop_failure_fails_normal_test_execution() {
        let database = Database {
            url: "postgresql://unused.invalid/invalid".to_owned(),
            name: "invalid_cleanup_target".to_owned(),
            port: 0,
            password: "invalid".to_owned(),
        };
        let panic = std::panic::catch_unwind(|| drop(database)).unwrap_err();
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .expect("cleanup failure panic has a message");
        assert!(
            message.contains("PostgreSQL test cleanup failed: drop the owned test database"),
            "{message}"
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
}

//! Private PostgreSQL 18 servers for tests.
//!
//! SQLx owns per-test databases. [`start`] owns TLS servers and special server
//! settings. The runner passes a private server URL to one child command.
//! [`require_prerequisites`] fails a test missing a declared prerequisite.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use url::Url;

const POSTGRES_BIN: &str = "/usr/lib/postgresql/18/bin";

fn postgres_bin(override_directory: Option<std::ffi::OsString>) -> PathBuf {
    override_directory
        .filter(|directory| !directory.is_empty())
        .map_or_else(|| PathBuf::from(POSTGRES_BIN), PathBuf::from)
}

fn postgres_bin_dir() -> PathBuf {
    postgres_bin(std::env::var_os("WAMN_POSTGRES_BIN_DIR"))
}

/// Return a libpq-compatible test URL with usable TLS certificate paths.
///
/// SQLx 0.9 serializes certificate file paths with a `file: ` display prefix,
/// but its URL parser treats that prefix as part of the path.
pub fn url(pool: &sqlx::PgPool) -> Url {
    let mut url = sqlx::ConnectOptions::to_url_lossy(&*pool.connect_options());
    let parameters: Vec<_> = url
        .query_pairs()
        .filter(|(name, _)| {
            matches!(
                name.as_ref(),
                "sslmode" | "sslrootcert" | "sslcert" | "sslkey"
            )
        })
        .map(|(name, value)| {
            let value = value.into_owned();
            let value = if matches!(name.as_ref(), "sslrootcert" | "sslcert" | "sslkey")
                && let Some(path) = value.strip_prefix("file: ")
            {
                path.to_owned()
            } else {
                value
            };
            (name.into_owned(), value)
        })
        .collect();
    url.query_pairs_mut().clear().extend_pairs(parameters);
    url
}

/// Hold the lock of this test process for tests that change shared roles or settings.
///
/// Every test of the process that changes roles or server settings of the
/// SQLx server, or depends on roles that such a test changes, holds the
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
/// Callers pass untyped names. A name in upper case is an environment variable
/// that must be set and not empty. Any other name is an executable program on `PATH`.
/// Ignored tests use [`require_ignored_prerequisites`] with their full reason.
///
/// # Panics
///
/// Panics with one message that names each missing prerequisite.
pub fn require_prerequisites(names: &[&str]) {
    let names: Vec<String> = names
        .iter()
        .map(|name| {
            let type_ = if name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                "env"
            } else {
                "bin"
            };
            format!("{type_}:{name}")
        })
        .collect();
    require_ignored_prerequisites(&format!("requires: {}", names.join(", ")));
}

/// Parse the fixed prerequisite form used by ignored tests.
pub fn parse_prerequisites(reason: &str) -> Result<Vec<(&str, &str)>, String> {
    let names = reason
        .strip_prefix("requires: ")
        .ok_or_else(|| format!("invalid prerequisite reason: {reason:?}"))?;
    names
        .split(", ")
        .map(|item| {
            let (type_, name) = item
                .split_once(':')
                .ok_or_else(|| format!("invalid prerequisite: {item:?}"))?;
            let valid = match type_ {
                "env" => {
                    name.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
                        && name
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                }
                "bin" => {
                    !name.is_empty()
                        && name.chars().all(|c| {
                            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+')
                        })
                }
                _ => false,
            };
            if valid {
                Ok((type_, name))
            } else {
                Err(format!("invalid prerequisite: {item:?}"))
            }
        })
        .collect()
}

/// Fail before setup when an ignored test lacks a declared prerequisite.
pub fn require_ignored_prerequisites(reason: &str) {
    let names = parse_prerequisites(reason).unwrap_or_else(|error| panic!("{error}"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let missing: Vec<String> = names
        .into_iter()
        .filter_map(|(type_, name)| {
            if type_ == "env" {
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
                .arg(postgres_bin_dir())
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
        let initialized = Command::new(postgres_bin_dir().join("initdb"))
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
        let certificate =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
        private_file(&self.directory.join("server.key"))?
            .write_all(certificate.signing_key.serialize_pem().as_bytes())?;
        private_file(&self.directory.join("server.crt"))?
            .write_all(certificate.cert.pem().as_bytes())?;
        let mut config = fs::OpenOptions::new()
            .append(true)
            .open(self.directory.join("data/postgresql.conf"))?;
        writeln!(
            config,
            "ssl=on\nssl_cert_file='{}'\nssl_key_file='{}'",
            self.directory.join("server.crt").display(),
            self.directory.join("server.key").display()
        )?;
        // PostgreSQL owns the port after this reservation closes. A collision
        // makes startup fail; the runner never adopts an existing listener.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        self.port = port;
        let log = private_file(&self.directory.join("server.log"))?;
        let mut command = Command::new(postgres_bin_dir().join("postgres"));
        command
            .env_clear()
            .env("LC_ALL", "C")
            .arg("-D")
            .arg(self.directory.join("data"))
            // Test connections use authenticated loopback TCP. Do not create
            // unused Unix sockets whose path can exceed the platform limit.
            .arg("-k")
            .arg("")
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
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(status) = self
                .process
                .as_mut()
                .expect("the process was started")
                .try_wait()?
            {
                anyhow::bail!(
                    "the owned PostgreSQL process exited with {status} before readiness; last 20 lines of server.log:\n{}",
                    log_tail(&self.directory.join("server.log"))
                );
            }
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
                "the owned PostgreSQL process did not become ready within 120 s; last 20 lines of server.log:\n{}",
                log_tail(&self.directory.join("server.log"))
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        ensure!(
            fs::read_to_string(self.directory.join("data/PG_VERSION"))?.trim() == "18",
            "the test server must run PostgreSQL 18"
        );
        Ok(())
    }

    /// Create a database on this server and return its coordinate.
    pub async fn create_database(&mut self, name: &str) -> anyhow::Result<OwnedDatabase> {
        ensure!(
            valid_database_name(name),
            "the test database name must be a lowercase SQL identifier"
        );
        let mut connection = wamn_postgres::connect(self.database("postgres")?.url()).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&mut connection)
            .await?;
        self.database(name)
    }

    /// Return the superuser coordinate of a database of this server.
    pub fn database(&self, name: &str) -> anyhow::Result<OwnedDatabase> {
        let mut url = Url::parse("postgresql://postgres@127.0.0.1/postgres")?;
        url.query_pairs_mut()
            .append_pair("sslmode", "verify-full")
            .append_pair(
                "sslrootcert",
                self.directory
                    .join("server.crt")
                    .to_str()
                    .context("test certificate path is not UTF-8")?,
            );
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

fn log_tail(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(log) => {
            let mut lines: Vec<_> = log.lines().rev().take(20).collect();
            lines.reverse();
            lines.join("\n")
        }
        Err(error) => format!("cannot read server.log: {error}"),
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
    fn ignored_prerequisites_use_only_explicit_programs_and_variables() {
        assert_eq!(
            parse_prerequisites("requires: bin:sh, env:PATH").unwrap(),
            [("bin", "sh"), ("env", "PATH")]
        );
        for reason in [
            "",
            "requires:",
            "requires: ",
            "requires: sh",
            "requires: env:path",
            "requires: bin:sh,env:PATH",
            "requires: bin:sh; live only",
            "requires: bin:sh, ",
        ] {
            assert!(parse_prerequisites(reason).is_err(), "accepted {reason:?}");
        }
        require_ignored_prerequisites("requires: bin:/bin/sh, env:PATH");
        assert!(std::panic::catch_unwind(|| {
            require_ignored_prerequisites(
                "requires: env:WAMN_TEST_PREREQUISITE_NEVER_SET, bin:wamn-test-program-never-installed",
            );
        }).is_err());
    }

    #[test]
    fn missing_prerequisites_refuse() {
        require_prerequisites(&["PATH", "sh"]);
        for names in [
            &["PATH", "WAMN_TEST_PREREQUISITE_NEVER_SET"][..],
            &["sh", "wamn-test-program-never-installed"][..],
        ] {
            assert!(std::panic::catch_unwind(|| require_prerequisites(names)).is_err());
        }
    }

    #[tokio::test]
    async fn separate_server_applies_its_settings() -> anyhow::Result<()> {
        let mut server = start(&[("standard_conforming_strings", "off")])?;
        let database = server.create_database("settings").await?;
        let mut connection = wamn_postgres::connect(database.url()).await?;
        let enabled: bool =
            sqlx::query_scalar("SELECT current_setting('standard_conforming_strings')::boolean")
                .fetch_one(&mut connection)
                .await?;
        assert!(!enabled);
        Ok(())
    }
}

pub mod run_result;

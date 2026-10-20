//! One exclusive install lock, released when its guard is dropped.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use chrono::{SecondsFormat, Utc};

/// The open file keeps the install lock held until this guard is dropped.
#[derive(Debug)]
#[must_use = "keep the guard alive for the entire install"]
pub struct InstallLock {
    _file: File,
}

/// Return `$HOME/.cache/wamn-install` without creating it.
///
/// # Errors
///
/// Returns an error if HOME is unset.
pub fn work_directory() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is unset")?;
    Ok(PathBuf::from(home).join(".cache/wamn-install"))
}

/// Take the install lock immediately, or refuse with the current holder.
///
/// # Errors
///
/// Returns an error if the lock is held or a filesystem operation fails.
pub fn take(directory: &Path) -> anyhow::Result<InstallLock> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("create the install directory {}", directory.display()))?;
    let path = directory.join("install.lock");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("open the install lock {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let mut holder = String::new();
            file.read_to_string(&mut holder)
                .with_context(|| format!("read the install lock holder {}", path.display()))?;
            bail!(
                "install lock {} is held by {:?}",
                path.display(),
                holder.trim_end()
            );
        }
        Err(TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("lock {}", path.display()));
        }
    }
    let holder = format!(
        "pid={} started={}\n",
        std::process::id(),
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
    );
    file.set_len(0)
        .with_context(|| format!("clear the install lock holder {}", path.display()))?;
    file.write_all(holder.as_bytes())
        .with_context(|| format!("write the install lock holder {}", path.display()))?;
    Ok(InstallLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::take;

    #[test]
    fn a_second_take_names_the_holder_and_drop_releases_the_lock() {
        let directory = std::env::temp_dir().join(format!(
            "wamn-install-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = take(&directory).unwrap();
        let path = directory.join("install.lock");
        let holder = std::fs::read_to_string(&path).unwrap();
        let (pid, started) = holder.trim_end().split_once(" started=").unwrap();
        assert_eq!(pid, format!("pid={}", std::process::id()));
        chrono::DateTime::parse_from_rfc3339(started).unwrap();

        let error = take(&directory).unwrap_err().to_string();
        assert!(error.contains(pid), "{error}");
        assert!(
            error.contains(&format!("{:?}", holder.trim_end())),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), holder);

        drop(first);
        let second = take(&directory).expect("the dropped guard releases its lock");
        drop(second);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}

use std::fs::{File, OpenOptions};
use std::ops::Deref;

const CTL_DATABASE_URL_ENV: &str = concat!("WAMN_CTL_", "PG_URL");
pub(crate) const LOCK_FILE_NAME: &str = "wamn-ctl-live-database.lock";

/// A control-plane live-test URL held together with its cross-process lock.
#[derive(Debug)]
pub(crate) struct LockedUrl {
    url: String,
    _lock: File,
}

impl LockedUrl {
    /// Read the optional live-test URL and serialize its caller when present.
    pub(crate) fn optional() -> Option<Self> {
        std::env::var(CTL_DATABASE_URL_ENV).ok().map(Self::acquire)
    }

    /// Require the live-test URL and serialize its caller.
    pub(crate) fn required(expectation: &str) -> Self {
        let url = std::env::var(CTL_DATABASE_URL_ENV).expect(expectation);
        Self::acquire(url)
    }

    fn acquire(url: String) -> Self {
        Self { url, _lock: lock() }
    }
}

// Cargo compiles this module once per integration-test binary, whose URL contract
// may use only one constructor. Keep both constructors in each binary's item graph.
type OptionalConstructor = fn() -> Option<LockedUrl>;
type RequiredConstructor = fn(&str) -> LockedUrl;
const _: (OptionalConstructor, RequiredConstructor) = (LockedUrl::optional, LockedUrl::required);

impl Deref for LockedUrl {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.url
    }
}

fn open_lock_file() -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(std::env::temp_dir().join(LOCK_FILE_NAME))
        .expect("open the wamn-ctl live-database lock file")
}

/// Acquire the one host-wide lock shared by every `WAMN_CTL_PG_URL` test.
pub(crate) fn lock() -> File {
    let file = open_lock_file();
    file.lock()
        .expect("lock the wamn-ctl live database across test processes");
    file
}

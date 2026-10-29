//! Store credential values outside the database.

pub mod file;
pub mod kubernetes;
pub mod kubernetes_api;

use std::error::Error as StdError;
use std::fmt;

/// The immutable database reference returned by a write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreVersion {
    pub store: &'static str,
    pub reference: String,
    pub version: String,
}

/// A store failure with operation context and its source.
#[derive(Debug)]
pub struct Error {
    pub operation: &'static str,
    pub reference: String,
    source: Box<dyn StdError + Send + Sync>,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} credential {} failed", self.operation, self.reference)
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(crate) fn failure(
    operation: &'static str,
    reference: &str,
    source: impl Into<Box<dyn StdError + Send + Sync>>,
) -> Error {
    Error {
        operation,
        reference: reference.to_owned(),
        source: source.into(),
    }
}

/// A store scoped to one environment. Values never enter database columns.
#[async_trait::async_trait]
pub trait Store: Send + Sync + fmt::Debug {
    async fn put(&self, credential_id: uuid::Uuid, value: &[u8]) -> Result<StoreVersion, Error>;
    async fn get(&self, reference: &str, version: &str) -> Result<Vec<u8>, Error>;
    async fn destroy(&self, reference: &str, version: &str) -> Result<(), Error>;
}

/// Select the backend named by a persisted credential reference.
pub fn open(store: &str) -> Result<Box<dyn Store>, Error> {
    match store {
        "file" => {
            let directory = std::env::var_os("WAMN_CREDENTIAL_DIR")
                .ok_or_else(|| failure("open", "file", "WAMN_CREDENTIAL_DIR is absent"))?;
            Ok(Box::new(file::FileStore::new(directory.into())))
        }
        "k8s" => Ok(Box::new(kubernetes::KubernetesStore::in_cluster(
            "", "", "",
        )?)),
        _ => Err(failure("open", store, "unknown credential store")),
    }
}

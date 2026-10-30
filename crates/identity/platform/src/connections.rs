//! Project integration administration and environment connection authority.

pub mod administration;
pub mod references;

use std::fmt;

/// A refused configuration write, or its database failure.
#[derive(Debug)]
pub struct ConnectionError {
    pub code: &'static str,
    pub field: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.field)
    }
}
impl std::error::Error for ConnectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}
impl From<tokio_postgres::Error> for ConnectionError {
    fn from(source: tokio_postgres::Error) -> Self {
        Self {
            code: "database_error",
            field: "connection",
            source: Some(Box::new(source)),
        }
    }
}
fn refuse(code: &'static str, field: &'static str) -> ConnectionError {
    ConnectionError {
        code,
        field,
        source: None,
    }
}

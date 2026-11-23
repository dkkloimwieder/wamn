//! PostgreSQL storage for registry records and lifecycle ownership.

mod configuration;
mod connections;
mod deployment;
mod lifecycle_lock;
mod migrations;

pub use lifecycle_lock::{ENVIRONMENT_LOCK_SQL, ENVIRONMENT_TRANSACTION_LOCK_SQL, LifecycleLock};
pub use migrations::REGISTRY_HISTORY_TABLE;

/// Registry operations on the caller's database session.
#[derive(Debug)]
pub struct Registry<'a> {
    connection: &'a mut sqlx::PgConnection,
}

impl<'a> Registry<'a> {
    pub fn new(connection: &'a mut sqlx::PgConnection) -> Self {
        Self { connection }
    }
}

/// Registry operations whose lock must live inside an open transaction.
#[derive(Debug)]
pub struct RegistryTransaction<'a, 'connection> {
    transaction: &'a mut sqlx::Transaction<'connection, sqlx::Postgres>,
}

impl<'a, 'connection> RegistryTransaction<'a, 'connection> {
    pub fn new(transaction: &'a mut sqlx::Transaction<'connection, sqlx::Postgres>) -> Self {
        Self { transaction }
    }
}

/// Convert plain PostgreSQL text to a domain value, preserving the decode error.
///
/// # Errors
/// Returns a column decode error when the domain constructor refuses the stored value.
pub fn decode_text<T, E>(value: String, index: impl std::fmt::Debug) -> Result<T, sqlx::Error>
where
    T: TryFrom<String, Error = E>,
    E: std::error::Error + Send + Sync + 'static,
{
    value
        .try_into()
        .map_err(|source| sqlx::Error::ColumnDecode {
            index: format!("{index:?}"),
            source: Box::new(source),
        })
}

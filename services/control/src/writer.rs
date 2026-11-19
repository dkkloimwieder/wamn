//! The dedicated, reconnecting control database writer.

use std::{fmt, time::Duration};

use sqlx::{Connection as _, PgConnection};
use tokio::{
    sync::{MappedMutexGuard, Mutex, MutexGuard},
    time::timeout,
};

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// The control session that serializes writes and replaces a failed connection.
pub struct Writer {
    database: String,
    connection: Mutex<Option<PgConnection>>,
}

impl fmt::Debug for Writer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Writer").finish_non_exhaustive()
    }
}

impl Writer {
    /// Keep the startup connection and the configuration used to open it.
    pub fn new(database: String, connection: PgConnection) -> Self {
        Self {
            database,
            connection: Mutex::new(Some(connection)),
        }
    }

    pub(crate) async fn lock(&self) -> anyhow::Result<MappedMutexGuard<'_, PgConnection>> {
        let mut slot = self.connection.lock().await;
        if let Some(mut connection) = slot.take()
            && matches!(
                timeout(CONNECTION_TIMEOUT, connection.ping()).await,
                Ok(Ok(()))
            )
        {
            return Ok(MutexGuard::map(slot, |slot| slot.insert(connection)));
        }

        // The old session has dropped. Keep the slot locked for one bounded
        // attempt so another caller cannot open a second writer.
        let connection = timeout(CONNECTION_TIMEOUT, wamn_postgres::connect(&self.database))
            .await
            .map_err(|source| ReconnectError {
                source: source.into(),
            })?
            .map_err(|source| ReconnectError { source })?;
        Ok(MutexGuard::map(slot, |slot| slot.insert(connection)))
    }
}

#[derive(Debug)]
struct ReconnectError {
    source: anyhow::Error,
}

impl fmt::Display for ReconnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("reconnect control writer")
    }
}

impl std::error::Error for ReconnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

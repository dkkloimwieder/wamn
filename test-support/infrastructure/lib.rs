//! Repository-only temporary services and test adapters.
//!
//! MVP outcome: test coverage.

pub mod collector;
pub mod ctl_process;
pub mod event_broker;
pub mod fixture_digest;
pub mod locked_database;
pub mod nats_tls;
pub mod operations;
pub use wamn_test_postgres as postgres;

pub mod record_history;
pub mod scratch;

pub mod credentials;

/// Report whether any contextual source in an error is a refused connection.
#[must_use]
pub fn connection_refused(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
    })
}

#[cfg(test)]
mod tests {
    use super::connection_refused;

    #[test]
    fn only_refused_connections_are_retryable() {
        let refused =
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
                .context("connect to the allocated NodePort");
        assert!(connection_refused(&refused));
        let denied = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(!connection_refused(&denied));
    }
}

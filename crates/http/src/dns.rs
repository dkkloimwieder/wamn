//! DNS resolution through the host's Tokio resolver.

use std::net::SocketAddr;

use wamn_authority::connection_authority::{AuthorityError, DnsResolver};

pub(crate) struct TokioDnsResolver;

impl DnsResolver for TokioDnsResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, AuthorityError> {
        tokio::net::lookup_host((host, port))
            .await
            .map(Iterator::collect)
            .map_err(|error| {
                AuthorityError::dns_resolution_failed(format!(
                    "DNS resolution failed for {host}:{port}: {error}"
                ))
            })
    }
}

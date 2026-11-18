//! Identity service addresses.

/// The platform broker Service used by in-cluster clients.
pub const NATS_URL: &str = "nats://wamn-nats.platform.svc.cluster.local:4222";

pub(crate) fn issuer(namespace: &str) -> String {
    format!("https://identity.{namespace}.svc.cluster.local")
}

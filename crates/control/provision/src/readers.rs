//! The distinct control database reader roles.
use crate::workload_role::{
    WorkloadRoleFamily, WorkloadRoleIdentity, workload_login, workload_role_identity_hash,
};
use std::fmt;

/// The closed set of control-database read consumers.
///
/// Not `WorkloadRoleFamily`: only these three families have a read surface on the
/// control database, and taking the wider enum here would let a caller derive a
/// plausible-looking system-reader login for a family that has no grant set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemReader {
    /// `services/cdc-reader` — `registry.event_readers`, one `SELECT`.
    Registry,
    /// the identity service — `identity.*`, two
    /// `SELECT`s.
    Identity,
    /// Host connection configuration and credentials.
    Connection,
}

impl SystemReader {
    /// All readers, in family declaration order.
    pub const ALL: [Self; 3] = [Self::Registry, Self::Identity, Self::Connection];

    /// The provisioning family this reader's credential belongs to.
    pub const fn family(self) -> WorkloadRoleFamily {
        match self {
            Self::Registry => WorkloadRoleFamily::RegistryReader,
            Self::Identity => WorkloadRoleFamily::IdentityReader,
            Self::Connection => WorkloadRoleFamily::ConnectionReader,
        }
    }
}

impl fmt::Display for SystemReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.family().label())
    }
}

/// Deterministic 160-bit suffix for one system-reader target.
pub fn system_reader_identity_hash(
    reader: SystemReader,
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
) -> String {
    workload_role_identity_hash(
        reader.family(),
        WorkloadRoleIdentity::Control {
            org,
            project,
            environment,
            database,
        },
    )
    .expect("a system reader always uses control target")
}

pub fn system_reader_login(
    reader: SystemReader,
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
) -> String {
    workload_login(
        reader.family(),
        WorkloadRoleIdentity::Control {
            org,
            project,
            environment,
            database,
        },
    )
    .expect("a system reader always uses control target")
}

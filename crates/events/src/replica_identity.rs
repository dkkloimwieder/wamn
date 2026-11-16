//! Reconcile captured models to PostgreSQL REPLICA IDENTITY FULL.

use wamn_catalog::canonical_name::CanonicalName;

/// A table's modeled REPLICA IDENTITY target.
///
/// Only `FULL` versus not-`FULL` is load-bearing. PostgreSQL's index and nothing
/// modes are treated as `Default` so they remain untouched unless a live `FULL`
/// requirement requires a flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaIdentity {
    Full,
    Default,
}

impl ReplicaIdentity {
    /// Fold PostgreSQL's `relreplident` character into the controlled target.
    pub const fn from_relreplident(value: char) -> Self {
        if value == 'f' {
            Self::Full
        } else {
            Self::Default
        }
    }

    const fn keyword(self) -> &'static str {
        match self {
            Self::Full => "FULL",
            Self::Default => "DEFAULT",
        }
    }
}

pub fn alter_replica_identity_sql(
    schema: &CanonicalName,
    table: &CanonicalName,
    to: ReplicaIdentity,
) -> String {
    format!(
        "ALTER TABLE {}.{} REPLICA IDENTITY {}",
        postgres_protocol::escape::escape_identifier(schema.as_str()),
        postgres_protocol::escape::escape_identifier(table.as_str()),
        to.keyword(),
    )
}

/// Read ordinary tables from the exact set of schemas named by the manifest.
pub fn select_replica_identity_sql() -> &'static str {
    "SELECT n.nspname, c.relname, c.relreplident::text FROM pg_class c \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = ANY($1::text[]) AND c.relkind = 'r' \
     ORDER BY n.nspname, c.relname"
}

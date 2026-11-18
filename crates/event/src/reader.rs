//! Configuration supplied by the CDC lifecycle owner.
use std::path::PathBuf;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderConfig {
    /// Organization id.
    pub org: wamn_registry::OrgId,

    /// Project id.
    pub project: wamn_registry::ProjectId,

    /// Environment slug.
    pub env: wamn_registry::EnvName,

    /// Provisioned publication name.
    pub publication: String,
    /// Provisioned replication slot name.
    pub slot: String,
    /// Provisioned source stream.
    pub stream: String,
    /// Path to the replication connection configuration, or its JSON value.
    pub cdc_url: String,

    /// The NATS (JetStream) the events publish to (docs/plan/alignment.md R8).
    pub nats_url: String,

    /// The CA of the NATS server.
    pub nats_tls_ca: Option<PathBuf>,

    /// The reader's client certificate, which the server maps to its user
    /// (docs/plan/alignment.md R4).
    pub nats_tls_cert: Option<PathBuf>,

    /// The key of the client certificate.
    pub nats_tls_key: Option<PathBuf>,

    /// Standby-status feedback interval, seconds (how often the confirmed
    /// LSN reaches the server).
    pub feedback_secs: u64,

    /// How long a single JetStream publish may retry unacked before the reader
    /// emits the distinct `CDC_PUBLISH_STALLED` alert event (E2). Below this the
    /// retries are ordinary warns; at/past it every retry is an error alerts can
    /// bind to. The LSN is held throughout — a stall silently freezes WAL
    /// retention on the source DB, so this is a SAFETY INTERLOCK, not a metric.
    pub stall_threshold_secs: u64,

    /// How often the slot-headroom monitor polls `pg_replication_slots` over a
    /// SEPARATE plain connection (E2 backstop). Zero disables the monitor.
    pub slot_poll_secs: u64,

    /// Warn while the slot is still `reserved` but `safe_wal_size` has fallen
    /// below this many bytes — the early alert that fires BEFORE `wal_status`
    /// leaves `reserved` (E2). Default 256 MiB (≈16 WAL segments of headroom).
    pub slot_safe_wal_warn_bytes: i64,
}

impl ReaderConfig {
    /// The restricted certificate identity used for publishing and replies.
    pub fn nats_user(&self) -> String {
        wamn_registry::name::cdc_nats_user(&self.org, &self.project, &self.env)
    }
}

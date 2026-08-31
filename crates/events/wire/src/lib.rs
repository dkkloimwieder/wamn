//! The D19 v3 event-plane **wire contract** (docs/archive/events/event-plane-jetstream.md §4):
//! the envelope a CDC reader publishes per row event, the subject it lands on,
//! and the `Nats-Msg-Id` the whole plane keys dedupe on.
//!
//! MVP outcome: event spine (causation depth = loop guard).
//!
//! **STATUS: FROZEN 0.1.0** (2026-07-19, wamn-l5i9.30). These shapes are the
//! Phase-2 cutover contract: the reader service (wamn-l5i9.10) publishes them,
//! the materializer (wamn-l5i9.17) consumes them, and `readerbench` /
//! `streambench` (wamn-gates) bind this crate directly — no stand-in copy.
//! Compatibility rule (the WIT-freeze discipline): 0.1.x admits only additive
//! or clarifying changes; any breaking change waits for 0.2. Field removal or
//! rename must break a named golden test below.
//!
//! Pure — no IO, no clock; every string this crate emits is pinned by a test.

use serde::ser::SerializeStruct as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

/// The only derived-event record format admitted by this release line.
pub const DERIVED_EVENT_FORMAT_VERSION: &str = "0.1";

/// Row operation — the `<op>` subject segment. v3 publishes exactly these
/// three; TRUNCATE is not part of the event plane (a reader logs and skips it).
///
/// STATUS: FROZEN 0.1.0 (wamn-l5i9.30) — additive/clarifying only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Insert,
    Update,
    Delete,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Insert => "insert",
            Op::Update => "update",
            Op::Delete => "delete",
        }
    }
}

/// The v3 §4 causation stamp `{run, root, depth}` — stitched onto a
/// transaction's envelopes by the reader when the `wamn:postgres` plugin
/// emitted one (wamn-l5i9.12). Depth is bounded by the materializer (max 16).
///
/// STATUS: FROZEN 0.1.0 (wamn-l5i9.30) — additive/clarifying only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Causation {
    pub run: String,
    pub root: String,
    pub depth: u32,
}

/// One host-published event emitted by a wiring terminal.
///
/// This is deliberately not [`Envelope`]. A derived event has no WAL
/// provenance to report: its identity is the admitted terminal selector, the
/// author's logical deduplication operand, and the causation the host derives
/// from the delivery it is completing. Tenant, project, and environment are
/// copied from bound host claims so no guest or wiring field can redirect the
/// event; package identity comes from the welded release/run/wiring source the
/// native host emitter resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DerivedEvent {
    pub format_version: String,
    pub tenant: String,
    pub project: String,
    pub environment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_id: Option<String>,
    pub entity: String,
    pub op: Op,
    pub payload: serde_json::Value,
    pub dedup_id: String,
    pub causation: Causation,
}

impl DerivedEvent {
    /// Construct the record from host-owned scope and selector facts.
    #[expect(
        clippy::too_many_arguments,
        reason = "the constructor makes every frozen wire field explicit"
    )]
    pub fn new(
        tenant: impl Into<String>,
        project: impl Into<String>,
        environment: impl Into<String>,
        package_id: impl Into<String>,
        entity: impl Into<String>,
        op: Op,
        payload: serde_json::Value,
        dedup_id: impl Into<String>,
        causation: Causation,
    ) -> Self {
        Self {
            format_version: DERIVED_EVENT_FORMAT_VERSION.to_owned(),
            tenant: tenant.into(),
            project: project.into(),
            environment: environment.into(),
            package_id: Some(package_id.into()),
            entity: entity.into(),
            op,
            payload,
            dedup_id: dedup_id.into(),
            causation,
        }
    }

    /// Parse a stored record and fail closed on a foreign version.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let event: Self = serde_json::from_slice(bytes)?;
        if event.format_version != DERIVED_EVENT_FORMAT_VERSION {
            return Err(<serde_json::Error as serde::de::Error>::custom(format!(
                "unsupported derived-event format-version {}",
                event.format_version
            )));
        }
        for (field, value) in [
            ("tenant", event.tenant.as_str()),
            ("project", event.project.as_str()),
            ("environment", event.environment.as_str()),
            ("entity", event.entity.as_str()),
            ("causation.run", event.causation.run.as_str()),
            ("causation.root", event.causation.root.as_str()),
        ] {
            if value.is_empty() || value.trim() != value || value.as_bytes().contains(&0) {
                return Err(<serde_json::Error as serde::de::Error>::custom(format!(
                    "derived-event {field} is empty or noncanonical"
                )));
            }
        }
        if event.package_id.as_deref().is_some_and(|package_id| {
            package_id.is_empty()
                || package_id.trim() != package_id
                || package_id.as_bytes().contains(&0)
        }) {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "derived-event package-id is empty or noncanonical",
            ));
        }
        if event.dedup_id.is_empty() {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "derived-event dedup-id is empty",
            ));
        }
        Ok(event)
    }
}

/// One row event on the wire: `{op, old, new, package_id?, entity?, table, lsn,
/// txid, commit_ts, causation?}` (v3 §4). `package_id` and `entity` are the
/// package owner and package-local model key from `wamn.json`; both are ABSENT
/// when the table is not package-mapped (hand-created, or a platform
/// table the schema-scoped publication auto-includes) — absence IS the
/// unmapped marker, unambiguous even when an entity id equals a table name.
/// `table` always carries the physical table name at decode time.
///
/// `old`/`new` are column→value maps in pgoutput **text** representation
/// (values are JSON strings or `null`). An **unchanged TOAST column is ABSENT
/// from the map** — distinguishable from a real NULL, which is present as
/// `null` (the S-CDC-1 finding). `old` is present only when the source
/// provided an old image (REPLICA IDENTITY, or the key columns of a delete).
///
/// STATUS: FROZEN 0.1.0 (wamn-l5i9.30) — additive/clarifying only. The field
/// set, spellings, and serde omission rules are pinned by the golden tests.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub op: Op,
    pub old: Option<serde_json::Map<String, serde_json::Value>>,
    pub new: Option<serde_json::Map<String, serde_json::Value>>,
    pub package_id: Option<String>,
    pub entity: Option<String>,
    pub table: String,
    pub lsn: u64,
    pub txid: u32,
    pub commit_ts: chrono::DateTime<chrono::Utc>,
    pub causation: Option<Causation>,
}

const ENVELOPE_IDENTITY_CLOSURE: &str =
    "event envelope package_id and entity must be both present or both absent";

impl Serialize for Envelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.package_id.is_some() != self.entity.is_some() {
            return Err(serde::ser::Error::custom(ENVELOPE_IDENTITY_CLOSURE));
        }
        let field_count = 5
            + self.old.is_some() as usize
            + self.new.is_some() as usize
            + self.package_id.is_some() as usize
            + self.entity.is_some() as usize
            + self.causation.is_some() as usize;
        let mut state = serializer.serialize_struct("Envelope", field_count)?;
        state.serialize_field("op", &self.op)?;
        if let Some(old) = &self.old {
            state.serialize_field("old", old)?;
        }
        if let Some(new) = &self.new {
            state.serialize_field("new", new)?;
        }
        if let Some(package_id) = &self.package_id {
            state.serialize_field("package_id", package_id)?;
        }
        if let Some(entity) = &self.entity {
            state.serialize_field("entity", entity)?;
        }
        state.serialize_field("table", &self.table)?;
        state.serialize_field("lsn", &self.lsn)?;
        state.serialize_field("txid", &self.txid)?;
        state.serialize_field("commit_ts", &self.commit_ts)?;
        if let Some(causation) = &self.causation {
            state.serialize_field("causation", causation)?;
        }
        state.end()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeWire {
    op: Op,
    #[serde(default)]
    old: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    new: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    package_id: Option<String>,
    #[serde(default)]
    entity: Option<String>,
    table: String,
    lsn: u64,
    txid: u32,
    commit_ts: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    causation: Option<Causation>,
}

impl<'de> Deserialize<'de> for Envelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = EnvelopeWire::deserialize(deserializer)?;
        if wire.package_id.is_some() != wire.entity.is_some() {
            return Err(serde::de::Error::custom(ENVELOPE_IDENTITY_CLOSURE));
        }
        Ok(Self {
            op: wire.op,
            old: wire.old,
            new: wire.new,
            package_id: wire.package_id,
            entity: wire.entity,
            table: wire.table,
            lsn: wire.lsn,
            txid: wire.txid,
            commit_ts: wire.commit_ts,
            causation: wire.causation,
        })
    }
}

/// The single bounded stream that stores operator-visible registration poison.
pub const DEAD_LETTER_STREAM: &str = "WAMN_DLQ";

/// Subject filter owned by [`DEAD_LETTER_STREAM`].
pub const DEAD_LETTER_STREAM_SUBJECTS: &str = "dlq.>";

/// Maximum retained poison messages for one exact registration subject.
pub const DEAD_LETTER_MAX_MESSAGES_PER_REGISTRATION: i64 = 1_000;

/// Maximum age of one dead-letter message: seven days.
pub const DEAD_LETTER_MAX_AGE_SECONDS: u64 = 7 * 24 * 60 * 60;

/// The original header preserved inside a dead-letter record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DeadLetterHeader {
    pub name: String,
    pub value: String,
}

/// One server-acknowledged registration refusal retained for operator action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DeadLetter {
    pub format_version: u32,
    pub reason: String,
    pub source_stream: String,
    pub source_stream_sequence: u64,
    pub delivered: u64,
    pub original_subject: String,
    pub headers: Vec<DeadLetterHeader>,
    pub body: Vec<u8>,
}

impl DeadLetter {
    /// Parse and validate one retained dead-letter record.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let dead_letter: Self = serde_json::from_slice(bytes)?;
        if dead_letter.format_version != 1 {
            return Err(<serde_json::Error as serde::de::Error>::custom(format!(
                "unsupported dead-letter format-version {}",
                dead_letter.format_version
            )));
        }
        Ok(dead_letter)
    }
}

impl Envelope {
    /// The subject's `<entity>` segment: the stable entity id when mapped, the
    /// physical table name otherwise (the FD fallback — delayed, never lost).
    pub fn entity_segment(&self) -> &str {
        self.entity.as_deref().unwrap_or(&self.table)
    }
}

/// `<project>_<env>` — the `Nats-Msg-Id` prefix a project-env's events dedupe
/// under (subject segments already isolate per-project, the id prefix keeps
/// dedupe ids from colliding across projects inside one org+env stream).
pub fn project_env(project: &str, env: &str) -> String {
    format!("{project}_{env}")
}

/// `Nats-Msg-Id = <project_env>:<lsn>` — the at-least-once dedupe key. The LSN
/// is the row event's WAL position (decimal), unique per event.
pub fn msg_id(project: &str, env: &str, lsn: u64) -> String {
    format!("{}:{lsn}", project_env(project, env))
}

/// Host-owned, header-safe `Nats-Msg-Id` for a derived event.
///
/// The type discriminator keeps author ids that happen to be decimal apart
/// from CDC LSN identities. The trusted scope prevents the same author id in a
/// different tenant/project/environment/package and admitted terminal selector
/// from colliding. Only a bounded digest reaches the NATS header; the logical
/// operand itself remains byte-for-byte author supplied in
/// [`DerivedEvent::dedup_id`]. Length framing prevents concatenation ambiguity
/// between scope fields.
pub fn derived_msg_id(
    tenant: &str,
    project: &str,
    env: &str,
    package_id: &str,
    entity: &str,
    op: Op,
    dedup_id: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"wamn.event.derived-msg-id.v0.1\0");
    for field in [
        tenant,
        project,
        env,
        package_id,
        entity,
        op.as_str(),
        dedup_id,
    ] {
        digest.update(u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(field.as_bytes());
    }
    let digest = digest.finalize();
    let mut message_id = String::with_capacity("derived:".len() + digest.len() * 2);
    message_id.push_str("derived:");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        message_id.push(char::from(HEX[usize::from(byte >> 4)]));
        message_id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    message_id
}

/// `evt.<org>.<project>.<env>.<entity>.<op>` — the subject one event lands on.
/// The entity segment is sanitized ([`subject_token`]); the envelope keeps the
/// true name.
pub fn subject(org: &str, project: &str, env: &str, entity: &str, op: Op) -> String {
    format!(
        "evt.{org}.{project}.{env}.{}.{}",
        subject_token(entity),
        op.as_str()
    )
}

/// The subject filter an org+env `EVT_` stream binds — every project's events
/// for that org+env (`evt.<org>.*.<env>.>`).
pub fn stream_subjects(org: &str, env: &str) -> String {
    format!("evt.{org}.*.{env}.>")
}

/// `EVT_<org>_<env>` — the JetStream stream name a project-env's events land in
/// (the registration default; one stream per org+env, D19 v3 §5).
pub fn stream_name(org: &str, env: &str) -> String {
    format!("EVT_{org}_{env}")
}

/// Host-derived per-registration dead-letter subject.
pub fn dead_letter_subject(
    tenant: &str,
    environment: &str,
    package_id: &str,
    registration_id: &str,
) -> String {
    format!(
        "dlq.{}.{}.{}.{}",
        subject_token(tenant),
        subject_token(environment),
        subject_token(package_id),
        subject_token(registration_id)
    )
}

/// Stable JetStream dedup identity for one source message's DLQ publication.
pub fn dead_letter_message_id(
    subject: &str,
    source_stream: &str,
    source_stream_sequence: u64,
) -> String {
    format!(
        "{subject}:{}:{source_stream_sequence}",
        subject_token(source_stream)
    )
}

/// Make a raw name safe as ONE subject token: NATS reserves `.` (separator),
/// `*`/`>` (wildcards), and whitespace/control break parsing — each becomes
/// `_`. Catalog-managed tables are already clean idents; this is the backstop
/// for hand-created tables the schema-scoped publication auto-includes.
///
/// Bare sanitization collides: `a.b` and `a_b` both become `a_b`. So when
/// sanitization CHANGED the string, a short stable hash of the RAW name is
/// appended (`a_b_108bf50c`), keeping distinct raws distinct (R22). An
/// already-clean input is returned byte-identical — the frozen-token guarantee
/// (l5i9.30): only dirty names (which never reach here from the catalog path)
/// change shape.
pub fn subject_token(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| match c {
            '.' | '*' | '>' => '_',
            c if c.is_whitespace() || c.is_control() => '_',
            c => c,
        })
        .collect();
    if sanitized == raw {
        sanitized
    } else {
        format!("{sanitized}_{}", raw_hash(raw))
    }
}

/// A short, build-stable hash of `raw` (FNV-1a, 32-bit, hex) used to
/// disambiguate sanitized tokens. Deterministic across Rust versions — unlike
/// `DefaultHasher` — because the token is a wire subject the materializer and
/// `streambench` pin; it must not shift under us. Its alphabet ([0-9a-f]) is
/// itself a safe token.
fn raw_hash(raw: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5; // FNV-1a 32-bit offset basis
    for b in raw.as_bytes() {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193); // FNV-1a 32-bit prime
    }
    format!("{hash:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_is_the_v3_grammar() {
        // The exact string streambench pins on the stream side.
        assert_eq!(
            subject("acme", "app", "dev", "receipts", Op::Insert),
            "evt.acme.app.dev.receipts.insert"
        );
        assert_eq!(
            subject("acme", "app", "prod", "quality_holds", Op::Delete),
            "evt.acme.app.prod.quality_holds.delete"
        );
    }

    #[test]
    fn msg_id_is_project_env_colon_decimal_lsn() {
        assert_eq!(msg_id("app", "dev", 0x0100_0000), "app_dev:16777216");
        assert_eq!(project_env("app", "dev"), "app_dev");
    }

    #[test]
    fn derived_msg_id_is_host_scoped_and_keeps_the_author_operand() {
        let id = derived_msg_id(
            "acme",
            "app",
            "dev",
            "orders",
            "orders",
            Op::Insert,
            "orders:7:created",
        );
        assert!(id.starts_with("derived:"));
        assert_eq!(id.len(), "derived:".len() + 64);
        assert_eq!(
            id, "derived:3357ce56ef66f155bc2bb6a09d30fefe01c7a9c2a9d854839d82fc9200b127a3",
            "the domain, field order, selector scope, and length framing are wire identity"
        );
        assert!(!id.contains("orders:7:created"));
        assert!(
            id.chars()
                .all(|c| c == ':' || c.is_ascii_hexdigit() || c.is_ascii_lowercase())
        );
        assert_ne!(
            derived_msg_id("acme", "app", "dev", "orders", "orders", Op::Insert, "42"),
            msg_id("app", "dev", 42),
            "a numeric author id cannot collide with a CDC LSN"
        );
        assert_ne!(
            derived_msg_id("acme", "app", "dev", "orders", "orders", Op::Insert, "same"),
            derived_msg_id(
                "other",
                "app",
                "dev",
                "orders",
                "orders",
                Op::Insert,
                "same"
            )
        );
        assert_ne!(
            derived_msg_id("a", "bc", "dev", "orders", "orders", Op::Insert, "same"),
            derived_msg_id("ab", "c", "dev", "orders", "orders", Op::Insert, "same"),
            "length framing keeps adjacent fields unambiguous"
        );
        assert_ne!(
            derived_msg_id("acme", "app", "dev", "orders", "orders", Op::Insert, "same"),
            derived_msg_id(
                "acme",
                "app",
                "dev",
                "orders",
                "receipts",
                Op::Insert,
                "same"
            ),
            "the admitted entity is part of stream-wide dedup scope"
        );
        assert_ne!(
            derived_msg_id("acme", "app", "dev", "orders", "orders", Op::Insert, "same"),
            derived_msg_id("acme", "app", "dev", "orders", "orders", Op::Update, "same"),
            "the admitted operation is part of stream-wide dedup scope"
        );
        assert_ne!(
            derived_msg_id(
                "acme",
                "app",
                "dev",
                "orders",
                "receipt",
                Op::Insert,
                "same"
            ),
            derived_msg_id(
                "acme",
                "app",
                "dev",
                "overlay",
                "receipt",
                Op::Insert,
                "same"
            ),
            "the package-local entity is disambiguated by its owning package"
        );
        assert_eq!(
            derived_msg_id("acme", "app", "dev", "orders", "orders", Op::Insert, "same"),
            derived_msg_id("acme", "app", "dev", "orders", "orders", Op::Insert, "same"),
            "an exact replay retains one stream-wide dedup identity"
        );
    }

    #[test]
    fn stream_binds_every_project_of_the_org_env() {
        assert_eq!(stream_subjects("acme", "dev"), "evt.acme.*.dev.>");
    }

    #[test]
    fn stream_name_is_evt_org_env() {
        assert_eq!(stream_name("acme", "dev"), "EVT_acme_dev");
        assert_eq!(stream_name("acme", "prod"), "EVT_acme_prod");
    }

    #[test]
    fn dead_letter_identity_is_host_derivable_and_environment_exact() {
        let subject = dead_letter_subject("tenant-a", "prod", "orders", "on.shipped");
        assert_eq!(subject, "dlq.tenant-a.prod.orders.on_shipped_7567c5f1");
        assert_eq!(
            dead_letter_message_id(&subject, "EVT_acme_prod", 42),
            "dlq.tenant-a.prod.orders.on_shipped_7567c5f1:EVT_acme_prod:42"
        );
        assert_eq!(DEAD_LETTER_STREAM, "WAMN_DLQ");
        assert_eq!(DEAD_LETTER_STREAM_SUBJECTS, "dlq.>");
        assert!(DEAD_LETTER_MAX_MESSAGES_PER_REGISTRATION > 0);
        assert!(DEAD_LETTER_MAX_AGE_SECONDS > 0);
    }

    #[test]
    fn dead_letter_format_refuses_unknown_versions() {
        let record = DeadLetter {
            format_version: 1,
            reason: "poison-invalid-envelope".into(),
            source_stream: "EVT_acme_prod".into(),
            source_stream_sequence: 42,
            delivered: 1,
            original_subject: "evt.acme.app.prod.orders.insert".into(),
            headers: vec![DeadLetterHeader {
                name: "Nats-Msg-Id".into(),
                value: "app_prod:42".into(),
            }],
            body: br#"{\"bad\":true}"#.to_vec(),
        };
        let canonical = serde_json::to_vec(&record).unwrap();
        assert_eq!(DeadLetter::from_slice(&canonical).unwrap(), record);

        let mut future = serde_json::to_value(&record).unwrap();
        future["format-version"] = 2.into();
        assert!(DeadLetter::from_slice(&serde_json::to_vec(&future).unwrap()).is_err());
    }

    #[test]
    fn subject_token_neutralizes_nats_specials() {
        // A DIRTY name is sanitized AND suffixed with a stable hash of the raw
        // (R22) — the specials are gone and the token is one safe segment.
        assert_eq!(subject_token("weird.name*x"), "weird_name_x_f5638491");
        assert_eq!(subject_token("a>b c\td"), "a_b_c_d_495ab2f0");
        // An already-clean name is byte-identical — the frozen-token guarantee.
        assert_eq!(subject_token("receipt_lines"), "receipt_lines");
    }

    #[test]
    fn subject_token_distinguishes_dot_from_underscore() {
        // R22: `a.b` and `a_b` used to collide on `a_b`. Now `a_b` is clean
        // (byte-identical) and `a.b` sanitizes-then-hashes, so they diverge.
        assert_eq!(subject_token("a_b"), "a_b");
        assert_eq!(subject_token("a.b"), "a_b_108bf50c");
        assert_ne!(subject_token("a.b"), subject_token("a_b"));
    }

    #[test]
    fn envelope_wire_shape_is_the_v3_draft() {
        // Freeze the DRAFT field set + spellings: this literal is the wire.
        // A MAPPED event carries the package-local entity id and its owning
        // package; `table` remains the physical name at decode time.
        let mut new = serde_json::Map::new();
        new.insert("id".into(), serde_json::Value::String("7".into()));
        new.insert("note".into(), serde_json::Value::Null);
        let env = Envelope {
            op: Op::Update,
            old: None,
            new: Some(new),
            package_id: Some("orders".into()),
            entity: Some("sales_orders".into()),
            table: "orders2".into(),
            lsn: 42,
            txid: 731,
            commit_ts: chrono::DateTime::parse_from_rfc3339("2026-07-18T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            causation: None,
        };
        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            r#"{"op":"update","new":{"id":"7","note":null},"package_id":"orders","entity":"sales_orders","table":"orders2","lsn":42,"txid":731,"commit_ts":"2026-07-18T12:00:00Z"}"#
        );
        assert_eq!(env.entity_segment(), "sales_orders");
        // Round-trip; an unchanged-TOAST column stays ABSENT (not null).
        let back: Envelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(back, env);
        assert!(back.new.as_ref().unwrap().get("big").is_none());
        assert!(back.new.as_ref().unwrap().get("note").unwrap().is_null());
    }

    #[test]
    fn unmapped_envelope_omits_entity_and_falls_back_to_the_table() {
        // The FD marker: an unmapped table publishes WITHOUT `entity` —
        // absence is the marker (unambiguous even when an entity id equals a
        // table name); the subject segment falls back to the table name.
        let env = Envelope {
            op: Op::Insert,
            old: None,
            new: Some(serde_json::Map::new()),
            package_id: None,
            entity: None,
            table: "receipts".into(),
            lsn: 7,
            txid: 3,
            commit_ts: chrono::DateTime::parse_from_rfc3339("2026-07-18T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            causation: None,
        };
        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            r#"{"op":"insert","new":{},"table":"receipts","lsn":7,"txid":3,"commit_ts":"2026-07-18T12:00:00Z"}"#
        );
        assert_eq!(env.entity_segment(), "receipts");
        let back: Envelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert!(back.package_id.is_none());
        assert!(back.entity.is_none());
    }

    #[test]
    fn envelope_decode_refuses_half_a_package_entity_identity() {
        let mapped = serde_json::json!({
            "op": "insert",
            "new": {"id": "7"},
            "package_id": "orders",
            "entity": "sales_orders",
            "table": "orders",
            "lsn": 7,
            "txid": 3,
            "commit_ts": "2026-07-18T12:00:00Z"
        });
        for missing in ["package_id", "entity"] {
            let mut half = mapped.clone();
            half.as_object_mut().unwrap().remove(missing);
            let error = serde_json::from_value::<Envelope>(half)
                .expect_err("a half package/entity identity must be refused");
            assert!(
                error.to_string().contains(ENVELOPE_IDENTITY_CLOSURE),
                "the refusal must name the closed identity pair: {error}"
            );
        }
    }

    #[test]
    fn envelope_encode_refuses_half_a_package_entity_identity() {
        let complete = Envelope {
            op: Op::Insert,
            old: None,
            new: Some(serde_json::Map::new()),
            package_id: Some("orders".into()),
            entity: Some("sales_orders".into()),
            table: "orders".into(),
            lsn: 7,
            txid: 3,
            commit_ts: chrono::DateTime::parse_from_rfc3339("2026-07-18T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            causation: None,
        };
        for missing in ["package_id", "entity"] {
            let mut half = complete.clone();
            if missing == "package_id" {
                half.package_id = None;
            } else {
                half.entity = None;
            }
            let error = serde_json::to_string(&half)
                .expect_err("a half package/entity identity must not reach the wire");
            assert!(
                error.to_string().contains(ENVELOPE_IDENTITY_CLOSURE),
                "the refusal must name the closed identity pair: {error}"
            );
        }
    }

    #[test]
    fn causation_carries_run_root_depth() {
        let c: Causation =
            serde_json::from_str(r#"{"run":"f1:evt:9","root":"f1:evt:1","depth":3}"#).unwrap();
        assert_eq!(c.depth, 3);
        // Freeze the serialized field order + spellings (run, root, depth).
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"run":"f1:evt:9","root":"f1:evt:1","depth":3}"#
        );
        // The frozen shape rejects smuggled fields.
        let smuggled = r#"{"run":"a","root":"b","depth":1,"x":2}"#;
        assert!(serde_json::from_str::<Causation>(smuggled).is_err());
    }

    #[test]
    fn derived_event_is_a_distinct_versioned_wire_without_wal_provenance() {
        let event = DerivedEvent::new(
            "acme",
            "app",
            "dev",
            "orders",
            "orders",
            Op::Update,
            serde_json::json!(["arbitrary", {"nested": true}]),
            "orders:7:updated",
            Causation {
                run: "registration:delivery:9".into(),
                root: "root:1".into(),
                depth: 3,
            },
        );
        let bytes = serde_json::to_vec(&event).unwrap();
        assert_eq!(
            String::from_utf8(bytes.clone()).unwrap(),
            r#"{"format-version":"0.1","tenant":"acme","project":"app","environment":"dev","package-id":"orders","entity":"orders","op":"update","payload":["arbitrary",{"nested":true}],"dedup-id":"orders:7:updated","causation":{"run":"registration:delivery:9","root":"root:1","depth":3}}"#
        );
        assert_eq!(DerivedEvent::from_slice(&bytes).unwrap(), event);
        assert!(
            serde_json::from_slice::<Envelope>(&bytes).is_err(),
            "derived bytes must never decode as the frozen CDC envelope"
        );
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        for absent in ["lsn", "txid", "commit-ts", "commit_ts", "table"] {
            assert!(
                value.get(absent).is_none(),
                "fabricated {absent} reached the wire"
            );
        }

        let future = serde_json::to_vec(&serde_json::json!({
            "format-version": "0.2",
            "tenant": "acme",
            "project": "app",
            "environment": "dev",
            "entity": "orders",
            "op": "insert",
            "payload": null,
            "dedup-id": "d1",
            "causation": {"run": "r", "root": "r", "depth": 0}
        }))
        .unwrap();
        assert!(DerivedEvent::from_slice(&future).is_err());
    }

    #[test]
    fn fully_populated_envelope_freezes_every_field() {
        // The freeze golden: every field present — old, new, entity, AND
        // causation. Pins the full field ORDER, spellings, and nesting; a
        // rename/removal of any wire field breaks THIS string.
        let mut old = serde_json::Map::new();
        old.insert("status".into(), serde_json::Value::String("draft".into()));
        let mut new = serde_json::Map::new();
        new.insert("status".into(), serde_json::Value::String("shipped".into()));
        let env = Envelope {
            op: Op::Update,
            old: Some(old),
            new: Some(new),
            package_id: Some("orders".into()),
            entity: Some("sales_orders".into()),
            table: "orders".into(),
            lsn: 42,
            txid: 731,
            commit_ts: chrono::DateTime::parse_from_rfc3339("2026-07-18T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            causation: Some(Causation {
                run: "f1:evt:00000000000000000009".into(),
                root: "f1:evt:00000000000000000001".into(),
                depth: 3,
            }),
        };
        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            r#"{"op":"update","old":{"status":"draft"},"new":{"status":"shipped"},"package_id":"orders","entity":"sales_orders","table":"orders","lsn":42,"txid":731,"commit_ts":"2026-07-18T12:00:00Z","causation":{"run":"f1:evt:00000000000000000009","root":"f1:evt:00000000000000000001","depth":3}}"#
        );
        let back: Envelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(back, env);
    }
}

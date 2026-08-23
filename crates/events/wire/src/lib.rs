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

use serde::{Deserialize, Serialize};

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

/// One row event on the wire: `{op, old, new, entity?, table, lsn, txid,
/// commit_ts, causation?}` (v3 §4). `entity` is the **stable catalog entity
/// id** (wamn-l5i9.11) — the rename-proof key registrations bind to; it is
/// ABSENT when the table is not catalog-mapped (hand-created, or a platform
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub op: Op,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    pub table: String,
    pub lsn: u64,
    pub txid: u32,
    pub commit_ts: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation: Option<Causation>,
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
    catalog_id: &str,
    registration_id: &str,
) -> String {
    format!(
        "dlq.{}.{}.{}.{}",
        subject_token(tenant),
        subject_token(environment),
        subject_token(catalog_id),
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
        // A MAPPED event: `entity` = the stable catalog entity id, `table` =
        // the physical name at decode time (they differ after a rename).
        let mut new = serde_json::Map::new();
        new.insert("id".into(), serde_json::Value::String("7".into()));
        new.insert("note".into(), serde_json::Value::Null);
        let env = Envelope {
            op: Op::Update,
            old: None,
            new: Some(new),
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
            r#"{"op":"update","new":{"id":"7","note":null},"entity":"sales_orders","table":"orders2","lsn":42,"txid":731,"commit_ts":"2026-07-18T12:00:00Z"}"#
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
        assert!(back.entity.is_none());
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
            r#"{"op":"update","old":{"status":"draft"},"new":{"status":"shipped"},"entity":"sales_orders","table":"orders","lsn":42,"txid":731,"commit_ts":"2026-07-18T12:00:00Z","causation":{"run":"f1:evt:00000000000000000009","root":"f1:evt:00000000000000000001","depth":3}}"#
        );
        let back: Envelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(back, env);
    }
}

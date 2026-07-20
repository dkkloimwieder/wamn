//! The D19 v3 event-plane **wire contract** (docs/event-plane-jetstream.md §4):
//! the envelope a CDC reader publishes per row event, the subject it lands on,
//! and the `Nats-Msg-Id` the whole plane keys dedupe on.
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

/// Make a raw name safe as ONE subject token: NATS reserves `.` (separator),
/// `*`/`>` (wildcards), and whitespace/control break parsing — each becomes
/// `_`. Catalog-managed tables are already clean idents; this is the backstop
/// for hand-created tables the schema-scoped publication auto-includes.
pub fn subject_token(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            '.' | '*' | '>' => '_',
            c if c.is_whitespace() || c.is_control() => '_',
            c => c,
        })
        .collect()
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
    fn subject_token_neutralizes_nats_specials() {
        assert_eq!(subject_token("weird.name*x"), "weird_name_x");
        assert_eq!(subject_token("a>b c\td"), "a_b_c_d");
        assert_eq!(subject_token("receipt_lines"), "receipt_lines");
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

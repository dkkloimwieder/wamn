//! The D19 v3 event-plane **wire contract** (docs/event-plane-jetstream.md §4):
//! the envelope a CDC reader publishes per row event, the subject it lands on,
//! and the `Nats-Msg-Id` the whole plane keys dedupe on.
//!
//! **WORKING DRAFT** (wamn-l5i9.1 decision c): these shapes are frozen only at
//! the Phase-2 cutover (wamn-l5i9.30). Consumers today: the reader service
//! (wamn-l5i9.10); next: the materializer (wamn-l5i9.17). `streambench`
//! (wamn-gates) carries an inline stand-in of the same contract and migrates
//! here at freeze.
//!
//! Pure — no IO, no clock; every string this crate emits is pinned by a test.

use serde::{Deserialize, Serialize};

/// Row operation — the `<op>` subject segment. v3 publishes exactly these
/// three; TRUNCATE is not part of the event plane (a reader logs and skips it).
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Causation {
    pub run: String,
    pub root: String,
    pub depth: u32,
}

/// One row event on the wire: `{op, old, new, entity, lsn, txid, commit_ts,
/// causation?}` (v3 §4). `entity` is the MVP naming — the pgoutput relation's
/// table name (the catalog-entity keying replaces it in wamn-l5i9.11).
///
/// `old`/`new` are column→value maps in pgoutput **text** representation
/// (values are JSON strings or `null`). An **unchanged TOAST column is ABSENT
/// from the map** — distinguishable from a real NULL, which is present as
/// `null` (the S-CDC-1 finding). `old` is present only when the source
/// provided an old image (REPLICA IDENTITY, or the key columns of a delete).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub op: Op,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<serde_json::Map<String, serde_json::Value>>,
    pub entity: String,
    pub lsn: u64,
    pub txid: u32,
    pub commit_ts: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation: Option<Causation>,
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
    fn subject_token_neutralizes_nats_specials() {
        assert_eq!(subject_token("weird.name*x"), "weird_name_x");
        assert_eq!(subject_token("a>b c\td"), "a_b_c_d");
        assert_eq!(subject_token("receipt_lines"), "receipt_lines");
    }

    #[test]
    fn envelope_wire_shape_is_the_v3_draft() {
        // Freeze the DRAFT field set + spellings: this literal is the wire.
        let mut new = serde_json::Map::new();
        new.insert("id".into(), serde_json::Value::String("7".into()));
        new.insert("note".into(), serde_json::Value::Null);
        let env = Envelope {
            op: Op::Update,
            old: None,
            new: Some(new),
            entity: "receipts".into(),
            lsn: 42,
            txid: 731,
            commit_ts: chrono::DateTime::parse_from_rfc3339("2026-07-18T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            causation: None,
        };
        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            r#"{"op":"update","new":{"id":"7","note":null},"entity":"receipts","lsn":42,"txid":731,"commit_ts":"2026-07-18T12:00:00Z"}"#
        );
        // Round-trip; an unchanged-TOAST column stays ABSENT (not null).
        let back: Envelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(back, env);
        assert!(back.new.as_ref().unwrap().get("big").is_none());
        assert!(back.new.as_ref().unwrap().get("note").unwrap().is_null());
    }

    #[test]
    fn causation_carries_run_root_depth() {
        let c: Causation =
            serde_json::from_str(r#"{"run":"f1:evt:9","root":"f1:evt:1","depth":3}"#).unwrap();
        assert_eq!(c.depth, 3);
        // The draft rejects smuggled fields.
        let smuggled = r#"{"run":"a","root":"b","depth":1,"x":2}"#;
        assert!(serde_json::from_str::<Causation>(smuggled).is_err());
    }
}

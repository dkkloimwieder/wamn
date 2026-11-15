//! Event records and source identity.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Row operation — the `<op>` subject segment. v3 publishes exactly these
/// three; TRUNCATE is not part of the event plane (a reader logs and skips it).
///
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
/// emitted one (wamn-l5i9.12). Workflow bounds depth at 16.
///
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
/// from the delivery it is completing. The environment key, project, and
/// environment are copied from bound host identity so no guest or wiring field can redirect the
/// event; package identity comes from the bound release/run/wiring source the
/// native host emitter resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedEvent {
    pub project_env_key: String,
    pub project: String,
    pub environment: String,
    #[serde(deserialize_with = "source_name")]
    pub package_id: String,
    #[serde(deserialize_with = "source_name")]
    pub entity: String,
    pub op: Op,
    pub payload: serde_json::Value,
    pub dedup_id: String,
    pub causation: Causation,
}

impl DerivedEvent {
    /// Construct the record from host-owned target and selector facts.
    #[expect(
        clippy::too_many_arguments,
        reason = "the constructor names the event identity and content"
    )]
    pub fn new(
        project_env_key: impl Into<String>,
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
            project_env_key: project_env_key.into(),
            project: project.into(),
            environment: environment.into(),
            package_id: package_id.into(),
            entity: entity.into(),
            op,
            payload,
            dedup_id: dedup_id.into(),
            causation,
        }
    }

    /// Read one tagged derived event.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let SourceEvent::Derived(event) = SourceEvent::decode(bytes)? else {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "expected derived event",
            ));
        };
        Ok(event)
    }
}

/// The stable coordinate of a row within a committed transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CdcIdentity {
    pub system_id: u64,
    pub timeline: u32,
    pub commit_lsn: u64,
    /// Zero-based position among all row changes, before relation filtering.
    pub row_ordinal: u64,
}

/// One captured row. Unchanged TOAST values are absent; SQL NULL stays null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub op: Op,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(deserialize_with = "source_name")]
    pub package_id: String,
    #[serde(deserialize_with = "source_name")]
    pub entity: String,
    pub table: String,
    pub identity: CdcIdentity,
    pub txid: u32,
    pub commit_ts: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation: Option<Causation>,
}

impl Envelope {
    /// The subject's `<entity>` segment: the required stable package-local id.
    pub fn entity_segment(&self) -> &str {
        &self.entity
    }
}

/// The broker identity of a captured row in one project environment.
pub fn msg_id(project: &str, env: &str, identity: &CdcIdentity) -> String {
    format!(
        "cdc:{project}:{env}:{}:{}:{}:{}",
        identity.system_id, identity.timeline, identity.commit_lsn, identity.row_ordinal
    )
}

/// Host-owned, header-safe `Nats-Msg-Id` for a derived event.
///
/// The type discriminator keeps author ids that happen to be decimal apart
/// from CDC LSN identities. The trusted target prevents the same author id in a
/// different project_env_key/project/environment/package and admitted terminal selector
/// from colliding. Only a bounded digest reaches the NATS header; the logical
/// operand itself remains byte-for-byte author supplied in
/// [`DerivedEvent::dedup_id`]. Length framing prevents concatenation ambiguity
/// between target fields.
pub fn derived_msg_id(
    project_env_key: &str,
    project: &str,
    env: &str,
    package_id: &str,
    entity: &str,
    op: Op,
    dedup_id: &str,
) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut digest = Sha256::new();
    digest.update(b"wamn.event.derived-msg-id.v0.1\0");
    for field in [
        project_env_key,
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
    for byte in digest {
        message_id.push(char::from(HEX[usize::from(byte >> 4)]));
        message_id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    message_id
}

/// The subject of one event in its project deployment.
pub fn subject(org: &str, project: &str, env: &str, entity: &str, op: Op) -> String {
    wamn_registry::name::event_subject(
        org,
        project,
        env,
        &format!("{}.{}", subject_token(entity), op.as_str()),
    )
}

/// Whether a name is one exact broker token.
pub fn valid_subject_token(raw: &str) -> bool {
    !raw.is_empty()
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// Return a validated subject token. Callers validate external names at admission.
pub fn subject_token(raw: &str) -> &str {
    assert!(valid_subject_token(raw), "invalid event subject token");
    raw
}

/// The filter for one captured entity.
pub fn entity_filter(org: &str, project: &str, environment: &str, entity: &str) -> String {
    wamn_registry::name::event_subject(
        org,
        project,
        environment,
        &format!("{}.>", subject_token(entity)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdc_identity_distinguishes_every_coordinate_and_refuses_old_rows() {
        let identity = CdcIdentity {
            system_id: 1,
            timeline: 2,
            commit_lsn: 42,
            row_ordinal: 0,
        };
        let id = msg_id("app", "dev", &identity);
        for other in [
            CdcIdentity {
                system_id: 2,
                ..identity
            },
            CdcIdentity {
                timeline: 3,
                ..identity
            },
            CdcIdentity {
                commit_lsn: 43,
                ..identity
            },
            CdcIdentity {
                row_ordinal: 1,
                ..identity
            },
        ] {
            assert_ne!(id, msg_id("app", "dev", &other));
        }
        assert_ne!(id, msg_id("app", "prod", &identity));
        assert_ne!(id, msg_id("other", "dev", &identity));
        let mut body = serde_json::json!({
            "type":"cdc", "op":"insert", "new":{"id":"7"},
            "package_id":"orders", "entity":"order", "table":"orders",
            "identity":identity, "txid":7, "commit_ts":"2026-07-18T12:00:00Z"
        });
        let source = SourceEvent::decode(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert!(source.identity("project", "app", "dev", &[&id]).is_some());
        assert!(
            source
                .identity("project", "app", "dev", &[&id, &id])
                .is_none()
        );
        body.as_object_mut().unwrap().remove("identity");
        body["lsn"] = 42.into();
        assert!(SourceEvent::decode(&serde_json::to_vec(&body).unwrap()).is_err());
    }

    #[test]
    fn derived_msg_id_is_host_targeted_and_keeps_the_author_operand() {
        let id = derived_msg_id(
            "acme",
            "app",
            "dev",
            "orders",
            "orders",
            Op::Insert,
            "orders:7:created",
        );
        assert!(!id.is_empty());
        assert_ne!(
            derived_msg_id("acme", "app", "dev", "orders", "orders", Op::Insert, "42"),
            msg_id(
                "app",
                "dev",
                &CdcIdentity {
                    system_id: 1,
                    timeline: 1,
                    commit_lsn: 42,
                    row_ordinal: 0
                }
            ),
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
            "the admitted entity is part of stream-wide dedup target"
        );
        assert_ne!(
            derived_msg_id("acme", "app", "dev", "orders", "orders", Op::Insert, "same"),
            derived_msg_id("acme", "app", "dev", "orders", "orders", Op::Update, "same"),
            "the admitted operation is part of stream-wide dedup target"
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
    fn stream_names_keep_all_declared_coordinates_distinct() {
        assert_ne!(
            stream_name("acme", "receiving", "dev"),
            stream_name("acme", "wms", "dev")
        );
        assert_ne!(
            stream_name("acme", "receiving", "dev"),
            stream_name("acme", "receiving", "prod")
        );
        assert_ne!(
            stream_name("a_b", "c", "dev"),
            stream_name("a", "b_c", "dev")
        );
        assert_ne!(
            stream_name("a", "b_c", "dev"),
            stream_name("a", "b", "c_dev")
        );
    }

    #[test]
    fn envelope_round_trip_preserves_optional_fields() {
        // A MAPPED event carries the package-local entity id and its owning
        // package; `table` remains the physical name at decode time.
        let mut new = serde_json::Map::new();
        new.insert("id".into(), serde_json::Value::String("7".into()));
        new.insert("note".into(), serde_json::Value::Null);
        let env = Envelope {
            op: Op::Update,
            old: None,
            new: Some(new),
            package_id: "orders".into(),
            entity: "sales_orders".into(),
            table: "orders2".into(),
            identity: CdcIdentity {
                system_id: 1,
                timeline: 1,
                commit_lsn: 42,
                row_ordinal: 0,
            },
            txid: 731,
            commit_ts: chrono::DateTime::parse_from_rfc3339("2026-07-18T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            causation: None,
        };
        // Round-trip; an unchanged-TOAST column stays ABSENT (not null).
        let back: Envelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(back, env);
        assert!(back.new.as_ref().unwrap().get("big").is_none());
        assert!(back.new.as_ref().unwrap().get("note").unwrap().is_null());
    }

    #[test]
    fn envelope_decode_requires_both_package_and_entity_identity() {
        let mapped = serde_json::json!({
            "op": "insert",
            "new": {"id": "7"},
            "package_id": "orders",
            "entity": "sales_orders",
            "table": "orders",
            "identity": {"system_id": 1, "timeline": 1, "commit_lsn": 7, "row_ordinal": 0},
            "txid": 3,
            "commit_ts": "2026-07-18T12:00:00Z"
        });
        for missing in ["package_id", "entity"] {
            let mut half = mapped.clone();
            half.as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<Envelope>(half).is_err(),
                "missing {missing} must refuse at decode"
            );
        }
    }

    #[test]
    fn source_decoding_requires_an_event_type_and_safe_source_names() {
        let event = DerivedEvent::new(
            "acme--dev--app",
            "app",
            "dev",
            "orders",
            "order",
            Op::Insert,
            serde_json::json!({"id": "7"}),
            "row:7",
            Causation {
                run: "run".into(),
                root: "run".into(),
                depth: 0,
            },
        );
        let body = serde_json::to_vec(&SourceEvent::Derived(event.clone())).unwrap();
        assert!(
            matches!(SourceEvent::decode(&body).unwrap(), SourceEvent::Derived(decoded) if decoded == event)
        );
        assert!(SourceEvent::decode(&serde_json::to_vec(&event).unwrap()).is_err());
        let mut missing: serde_json::Value = serde_json::from_slice(&body).unwrap();
        missing.as_object_mut().unwrap().remove("project_env_key");
        assert!(SourceEvent::decode(&serde_json::to_vec(&missing).unwrap()).is_err());
        let duplicated = String::from_utf8(body).unwrap().replacen(
            "\"project_env_key\":",
            "\"project_env_key\":\"other\",\"project_env_key\":",
            1,
        );
        assert!(SourceEvent::decode(duplicated.as_bytes()).is_err());

        for name in ["", "a.b", "a*", "a b", "a>\n"] {
            assert!(!valid_subject_token(name));
        }
    }

    #[test]
    fn causation_carries_run_root_depth() {
        let c: Causation =
            serde_json::from_str(r#"{"run":"f1:evt:9","root":"f1:evt:1","depth":3}"#).unwrap();
        assert_eq!(c.depth, 3);
        // The causation record rejects smuggled fields.
        let smuggled = r#"{"run":"a","root":"b","depth":1,"x":2}"#;
        assert!(serde_json::from_str::<Causation>(smuggled).is_err());
    }
}

pub use wamn_registry::name::{stream_name, stream_subjects};

pub use wamn_registry::name::{delivery_advisory_stream, delivery_advisory_subjects};

/// A source-event coordinate that matches the delivered NATS identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSourceEventId(String);

impl VerifiedSourceEventId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Accept exactly one `Nats-Msg-Id` matching the envelope's source coordinate.
pub fn verified_source_event_id(
    project: &str,
    environment: &str,
    envelope: &Envelope,
    message_ids: &[&str],
) -> Option<VerifiedSourceEventId> {
    let expected = msg_id(project, environment, &envelope.identity);
    match message_ids {
        [actual] if *actual == expected => Some(VerifiedSourceEventId(expected)),
        _ => None,
    }
}

/// Accept exactly one host-targeted `Nats-Msg-Id` matching a derived record.
///
/// Target equality is validated before identity derivation, so bytes naming a
/// foreign project/project/environment cannot borrow the local header identity.
pub fn verified_derived_source_event_id(
    project_env_key: &str,
    project: &str,
    environment: &str,
    event: &DerivedEvent,
    message_ids: &[&str],
) -> Option<VerifiedSourceEventId> {
    if event.project_env_key != project_env_key
        || event.project != project
        || event.environment != environment
    {
        return None;
    }
    let expected = derived_msg_id(
        project_env_key,
        project,
        environment,
        &event.package_id,
        &event.entity,
        event.op,
        &event.dedup_id,
    );
    match message_ids {
        [actual] if *actual == expected => Some(VerifiedSourceEventId(expected)),
        _ => None,
    }
}

/// A decoded event with its source identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceEvent {
    Cdc(Envelope),
    Derived(DerivedEvent),
}

impl SourceEvent {
    pub fn decode(body: &[u8]) -> Result<Self, serde_json::Error> {
        let source: Self = serde_json::from_slice(body)?;
        if let Self::Derived(event) = &source {
            for (field, value) in [
                ("project_env_key", event.project_env_key.as_str()),
                ("project", event.project.as_str()),
                ("environment", event.environment.as_str()),
                ("package-id", event.package_id.as_str()),
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
            if event.dedup_id.is_empty() {
                return Err(<serde_json::Error as serde::de::Error>::custom(
                    "derived-event dedup-id is empty",
                ));
            }
        }
        Ok(source)
    }

    pub fn identity(
        &self,
        project_env_key: &str,
        project: &str,
        environment: &str,
        headers: &[&str],
    ) -> Option<VerifiedSourceEventId> {
        match self {
            Self::Cdc(envelope) => {
                verified_source_event_id(project, environment, envelope, headers)
            }
            Self::Derived(event) => verified_derived_source_event_id(
                project_env_key,
                project,
                environment,
                event,
                headers,
            ),
        }
    }

    pub fn source(&self) -> (&str, &str) {
        match self {
            Self::Cdc(envelope) => (&envelope.package_id, &envelope.entity),
            Self::Derived(event) => (&event.package_id, &event.entity),
        }
    }
}

fn source_name<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let name = String::deserialize(deserializer)?;
    if valid_subject_token(&name) {
        Ok(name)
    } else {
        Err(serde::de::Error::custom("invalid event source name"))
    }
}

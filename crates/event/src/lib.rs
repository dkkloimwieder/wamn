//! Event records and source identity.

pub mod deploy_broker;
pub mod matching;
mod oldref;
pub mod reader;

use serde::{Deserialize, Serialize};

pub use wamn_package::RowOperation as Op;

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

/// The row image that contains a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Image {
    Old,
    New,
}

/// A column whose value PostgreSQL or the reader omitted from an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnavailableColumn {
    pub image: Image,
    pub column: String,
}

/// One captured row. SQL NULL stays null in an image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub op: Op,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<serde_json::Map<String, serde_json::Value>>,
    /// Unchanged TOAST columns and columns omitted to fit the broker limit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<UnavailableColumn>,
    pub package_id: wamn_package::PackageName,
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
pub fn msg_id(
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    identity: &CdcIdentity,
) -> String {
    format!(
        "cdc:{project}:{env}:{}:{}:{}:{}",
        identity.system_id, identity.timeline, identity.commit_lsn, identity.row_ordinal
    )
}

/// The subject of one event in its project deployment.
pub fn subject(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    entity: &str,
    op: Op,
) -> String {
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
pub fn entity_filter(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    entity: &str,
) -> String {
    wamn_registry::name::event_subject(org, project, env, &format!("{}.>", subject_token(entity)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cdc_identity_distinguishes_every_coordinate_and_refuses_old_rows() {
        let identity = CdcIdentity {
            system_id: 1,
            timeline: 2,
            commit_lsn: 42,
            row_ordinal: 0,
        };
        let id = msg_id(&"app".parse().unwrap(), &"dev".parse().unwrap(), &identity);
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
            assert_ne!(
                id,
                msg_id(&"app".parse().unwrap(), &"dev".parse().unwrap(), &other)
            );
        }
        assert_ne!(
            id,
            msg_id(&"app".parse().unwrap(), &"prod".parse().unwrap(), &identity)
        );
        assert_ne!(
            id,
            msg_id(
                &"other".parse().unwrap(),
                &"dev".parse().unwrap(),
                &identity
            )
        );
        let mut body = serde_json::json!({
            "type":"cdc", "op":"insert", "new":{"id":"7"},
            "package_id":"orders", "entity":"order", "table":"orders",
            "identity":identity, "txid":7, "commit_ts":"2026-07-18T12:00:00Z"
        });
        let SourceEvent::Cdc(envelope) =
            SourceEvent::decode(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert!(
            verified_source_event_id(
                &"app".parse().unwrap(),
                &"dev".parse().unwrap(),
                &envelope,
                &[&id]
            )
            .is_some()
        );
        assert!(
            verified_source_event_id(
                &"app".parse().unwrap(),
                &"dev".parse().unwrap(),
                &envelope,
                &[&id, &id]
            )
            .is_none()
        );
        body.as_object_mut().unwrap().remove("identity");
        body["lsn"] = 42.into();
        assert!(SourceEvent::decode(&serde_json::to_vec(&body).unwrap()).is_err());
    }

    #[test]
    fn stream_names_keep_all_declared_coordinates_distinct() {
        assert_ne!(
            stream_name(
                &"acme".parse().unwrap(),
                &"receiving".parse().unwrap(),
                &"dev".parse().unwrap()
            ),
            stream_name(
                &"acme".parse().unwrap(),
                &"wms".parse().unwrap(),
                &"dev".parse().unwrap()
            )
        );
        assert_ne!(
            stream_name(
                &"acme".parse().unwrap(),
                &"receiving".parse().unwrap(),
                &"dev".parse().unwrap()
            ),
            stream_name(
                &"acme".parse().unwrap(),
                &"receiving".parse().unwrap(),
                &"prod".parse().unwrap()
            )
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
            unavailable: Vec::new(),
            package_id: "orders".parse().unwrap(),
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
        let back: Envelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(back, env);
        assert!(back.old.is_none());
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
        assert!(SourceEvent::decode(&serde_json::to_vec(&admission_envelope()).unwrap()).is_err());

        for name in ["", "a.b", "a*", "a b", "a>\n"] {
            assert!(!valid_subject_token(name));
        }
    }

    fn admission_envelope() -> Envelope {
        serde_json::from_value(json!({
            "op": "insert",
            "new": {"project_id": "t1--dev--project", "status": "ready"},
            "package_id": "cat",
            "entity": "receipts",
            "table": "receipts",
            "identity": {"system_id": 1, "timeline": 1, "commit_lsn": 42, "row_ordinal": 0},
            "txid": 7,
            "commit_ts": "2026-08-15T12:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn source_identity_requires_one_exact_header() {
        let envelope = admission_envelope();
        let expected = msg_id(
            &"app".parse().unwrap(),
            &"dev".parse().unwrap(),
            &envelope.identity,
        );
        assert_eq!(
            verified_source_event_id(
                &"app".parse().unwrap(),
                &"dev".parse().unwrap(),
                &envelope,
                &[&expected]
            )
            .unwrap()
            .as_str(),
            expected
        );
        assert!(
            verified_source_event_id(
                &"app".parse().unwrap(),
                &"dev".parse().unwrap(),
                &envelope,
                &[]
            )
            .is_none()
        );
    }
}

pub use wamn_registry::name::{stream_name, stream_subjects};

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
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    envelope: &Envelope,
    message_ids: &[&str],
) -> Option<VerifiedSourceEventId> {
    let expected = msg_id(project, env, &envelope.identity);
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
}

impl SourceEvent {
    pub fn decode(body: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(body)
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

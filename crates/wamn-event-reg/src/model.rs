//! The event-registration model (EVT-REG, D19 v3 §5).
//!
//! An [`EventRegistration`] is a **subscribing flow's declaration** — the "a
//! registration, not code" of §5: WHICH entity's row events it wants
//! ([`entity`](EventRegistration::entity)), WHICH ops
//! ([`ops`](EventRegistration::ops)), an optional
//! [`condition`](EventRegistration::condition) filter, and an optional
//! [`partition_key`](EventRegistration::partition_key) extractor. The
//! **materializer (l5i9.17) is the consumer**: it opens a durable consumer per
//! registration, evaluates the condition (hot-editable — filtered-out events
//! stay on the stream, so a condition edit is replayable), and partitions the
//! enqueue by the key. This crate is ONLY the declaration surface; it does not
//! decode, materialize, or enqueue.
//!
//! **Rename-proof by entity-id keying (EVT-OIDMAP, wamn-l5i9.11):**
//! [`entity`](EventRegistration::entity) is the stable catalog **entity id**, not
//! a table name — the same id the CDC envelope carries in its `entity` segment
//! ([`wamn_event_wire::Envelope`]). A table rename never orphans a registration.
//!
//! **Impact analysis (11.8, wamn-wvb) covers registrations:** because the entity
//! is referenced by id, "what breaks if I drop/rename entity X" enumerates the
//! registrations that reference it (the `entity_id` storage column in
//! `catalog.event_registrations` is the query handle).
//!
//! **Data, not code:** stored as jsonb in `catalog.event_registrations` (this
//! crate is the source of truth for the semantics; the storage schema denormalizes
//! `flow_id`/`entity_id` as columns for lookup).
//!
//! **WORKING DRAFT** (wamn-l5i9.1 decision c): the event-plane wire/declaration
//! shapes freeze at the Phase-2 cutover (wamn-l5i9.30) — `0.1.x` is additive only.

use serde::{Deserialize, Serialize};

use wamn_catalog::EntityId;
use wamn_event_wire::Op;

/// The registration-model **format** version. Compatibility rule mirrors the
/// catalog / flow / RLS / WIT freezes: `0.1.x` is additive/clarifying only; a
/// breaking change waits for `0.2`.
pub const SCHEMA_VERSION: &str = "0.1";

/// One event registration — a subscribing flow's declaration of the row events
/// it wants and how they are filtered and partitioned.
///
/// The `(catalog_id, registration_id)` pair is the identity (unique within a
/// tenant); `flow_id` + `entity` are denormalized into storage columns for the
/// materializer's per-entity sweep and impact analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct EventRegistration {
    /// The registration-model format version (e.g. `"0.1"`). See [`SCHEMA_VERSION`].
    pub schema_version: String,
    /// Stable id, unique within its catalog (the storage row key).
    pub registration_id: String,
    /// The catalog this registration belongs to (`Catalog::catalog_id`).
    pub catalog_id: String,
    /// The subscribing flow (`Flow::id`) — the durable consumer the materializer
    /// opens, and the `run_id = <flow>:evt:<seq>` prefix (§5).
    pub flow_id: String,
    /// The entity whose row events fire this registration — the stable catalog
    /// **entity id** (rename-proof, EVT-OIDMAP), matching the CDC envelope's
    /// `entity` segment.
    pub entity: EntityId,
    /// The row ops that fire it. **Non-empty** — a registration matching no op is
    /// inert (rejected by [`validate`](crate::validate)).
    pub ops: Vec<Op>,
    /// Optional filter: a JMESPath **predicate** over the event context
    /// `{"op", "old", "new"}` (the envelope's op + column images), evaluated at
    /// the materializer. Absent = every op-matching event fires. A predicate
    /// referencing `old` is a "changed-to" condition and needs the entity's
    /// **REPLICA IDENTITY FULL** (the per-entity DDL knob l5i9.31) for the old
    /// image to be present — this surface can EXPRESS such a condition but never
    /// flips replica identity (l5i9.1 decision d).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Optional partition key: a JMESPath **expression** extracting the
    /// `partitioned(key)` value from the event context (§5, R6 ordering). Absent
    /// = the flow's runs carry no partition key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_key: Option<String>,
}

impl EventRegistration {
    /// Parse a registration from canonical JSON (import; also the jsonb stored
    /// per row in `catalog.event_registrations`).
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Serialize to canonical pretty JSON (export / the stored document).
    /// Default-valued fields are omitted, so a round-trip re-imports identically.
    pub fn to_json(&self) -> String {
        // Infallible for this type; a plain data struct never fails to encode.
        serde_json::to_string_pretty(self).expect("EventRegistration serializes")
    }
}

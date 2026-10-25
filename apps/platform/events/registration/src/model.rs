//! Predicate input used to evaluate one environment event trigger.
//!
//! The materializer constructs this value from a workflow row. Conditions are
//! JMESPath predicates over the event context `{"op", "old", "new"}`.

use serde::{Deserialize, Serialize};

use wamn_event_wire::Op;

/// The registration-model **format** version.
pub const SCHEMA_VERSION: &str = "0.1";

/// Whether one pull fetch is delivered to the wiring event-by-event or as one batch.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegistrationInput {
    /// Invoke the registration wiring once for each admitted event.
    #[default]
    Event,
    /// Invoke the registration wiring once with the fetch's ordered events.
    Batch,
}

/// One event registration — a subscribing operation's declaration of the row events
/// it wants and how they are filtered.
///
/// The `(package_id, registration_id)` pair is the identity (unique within a
/// tenant); the owning workflow row supplies this identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct EventRegistration {
    /// The registration-model format version (e.g. `"0.1"`). See [`SCHEMA_VERSION`].
    pub schema_version: String,
    /// Stable id, unique within its package (the storage row key).
    pub registration_id: String,
    /// Package-local ownership and delivery target. Package version is absent
    /// because the effective release selects the exact owner version.
    pub package_id: String,
    /// Package whose emitted event identity this registration matches. It may
    /// differ from the owner for a base-to-overlay post-commit invocation.
    pub source_package_id: String,
    /// Stable package-local model key, matching the CDC envelope's `entity`
    /// segment. It is resolved through the strict package manifest.
    pub entity: String,
    /// The row ops that fire it. **Non-empty** — a registration matching no op is
    /// inert (rejected by [`validate`](crate::validate)).
    pub ops: Vec<Op>,
    /// Delivery grain for one ordered pull fetch.
    #[serde(default)]
    pub input: RegistrationInput,
    /// Optional filter: a JMESPath **predicate** over the event context
    /// `{"op", "old", "new"}` (the envelope's op + column images), evaluated at
    /// the materializer. Absent = every op-matching event fires. A predicate
    /// referencing `old` is a "changed-to" condition and needs the entity's
    /// **REPLICA IDENTITY FULL** (the per-entity DDL knob l5i9.31) for the old
    /// image to be present — this surface can EXPRESS such a condition but never
    /// flips replica identity (l5i9.1 decision d).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

impl EventRegistration {
    /// Owner-qualified identity carried through the existing router bridge string.
    pub fn qualified_id(&self) -> String {
        format!("{}::{}", self.package_id, self.registration_id)
    }

    /// Parse a predicate input from JSON.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Serialize to canonical pretty JSON (export / the stored document).
    /// Default-valued fields are omitted, so a round-trip re-imports identically.
    pub fn to_json(&self) -> String {
        // Infallible for this type; a plain data struct never fails to encode.
        serde_json::to_string_pretty(self).expect("EventRegistration serializes")
    }

    /// Whether this registration's condition reads the ROOT `old` image — the
    /// "changed-to" shape that needs the entity's REPLICA IDENTITY FULL
    /// (l5i9.31). Delegates to the single detector ([`crate::condition_references_old`]).
    pub fn condition_references_old_image(&self) -> bool {
        self.condition
            .as_deref()
            .is_some_and(crate::condition_references_old)
    }

    /// Whether this registration subscribes to `delete` events. A delete's old
    /// image carries the tenant-scoping (and any delete-payload condition), so a
    /// delete subscription needs REPLICA IDENTITY FULL for the event to be
    /// scopable at all — a DELETE under DEFAULT is an alertable unscopable refusal.
    pub fn subscribes_to_delete(&self) -> bool {
        self.ops.contains(&wamn_event_wire::Op::Delete)
    }

    /// Whether serving this registration requires the entity to run REPLICA
    /// IDENTITY FULL — the union rule the reconciler folds across every
    /// registration on an entity (l5i9.31): an old-image condition OR a delete
    /// subscription.
    pub fn requires_replica_identity_full(&self) -> bool {
        self.condition_references_old_image() || self.subscribes_to_delete()
    }
}

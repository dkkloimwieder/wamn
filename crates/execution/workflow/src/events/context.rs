//! Build event condition inputs and read the row project.

use serde_json::{Map, Value, json};
use wamn_event_wire::{DerivedEvent, Envelope, Op};

/// Row-level tenancy carried by one CDC image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowProject<'a> {
    /// The image is present and has no `project_id`; database residency governs.
    Absent,
    /// The image carries a string project identity.
    Project(&'a str),
    /// The image is absent or carries a non-string project identity.
    Unscopable,
}

/// Build the condition context from one envelope:
/// `{"op": "<insert|update|delete>", "old": {…}|null, "new": {…}|null}`.
///
/// The column maps pass through VERBATIM (pgoutput **text** representation —
/// values are JSON strings or `null`), so exact-decimal / >2^53 numbers arrive
/// as strings and the platform's no-float rule holds trivially. An unchanged
/// out-of-line TOAST column is ABSENT from the map (distinguishable from a
/// real NULL, which is present as `null`) — a condition over such a column
/// sees `null` either way in v1; the distinction becomes load-bearing only
/// with old-image conditions (l5i9.31).
pub fn event_context(envelope: &Envelope) -> Value {
    // The members stand in sorted order, so the context serializes the same
    // with or without serde_json/preserve_order (wamn-g1jd).
    json!({
        "new": envelope.new.clone().map_or(Value::Null, Value::Object),
        "old": envelope.old.clone().map_or(Value::Null, Value::Object),
        "op": envelope.op.as_str(),
    })
}

/// Build the existing registration-condition context for a derived event.
///
/// Derived events deliberately have no CDC old/new images. Their arbitrary
/// payload occupies `new` while `old` remains null, preserving the declared
/// `{op, old, new}` condition language without pretending the payload came
/// from WAL.
pub fn derived_event_context(event: &DerivedEvent) -> Value {
    json!({
        "new": event.payload,
        "old": Value::Null,
        "op": event.op.as_str(),
    })
}

/// The row-level project carried by an event image. [`RowProject::Absent`] means
/// the package row relies on its separately verified database-residency target:
///
/// - a DELETE under REPLICA IDENTITY DEFAULT — the old image carries the key
///   column (`id`) ONLY, not `project_id` (the .17 design contract); or
/// - a package application table with no `project_id` column.
///
/// The caller can accept `Absent` only after the catalog credential confirms
/// that the package resides in the project database. A present string
/// remains an additional equality guard; any other carrier is unscopable.
pub fn row_project(envelope: &Envelope) -> RowProject<'_> {
    let image: &Map<String, Value> = match envelope.op {
        Op::Insert | Op::Update => match envelope.new.as_ref() {
            Some(image) => image,
            None => return RowProject::Unscopable,
        },
        // A DELETE's only image is the old key columns; under DEFAULT that is
        // never project-bearing, but read it anyway — if the entity later runs
        // REPLICA IDENTITY FULL (l5i9.31) the old image carries project_id and
        // deletes become scopable with zero change here.
        Op::Delete => match envelope.old.as_ref() {
            Some(image) => image,
            None => return RowProject::Unscopable,
        },
    };
    match image.get("project_id") {
        None => RowProject::Absent,
        Some(Value::String(project)) => RowProject::Project(project),
        Some(_) => RowProject::Unscopable,
    }
}

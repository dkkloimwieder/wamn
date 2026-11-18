//! Build event condition inputs and read the row project.

use serde::Serialize;
use serde_json::{Map, Value, json};
use wamn_catalog::WiringEventOperation as Op;
use wamn_event::Envelope;

/// Project deployment identity carried by one CDC image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowProject {
    /// The image is present and has no `project_id`; database residency governs.
    Absent,
    /// The image carries a string project identity.
    Project(wamn_registry::ProjectEnvKey),
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
        "new": envelope.new,
        "old": envelope.old,
        "op": envelope.op.as_str(),
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
pub fn row_project(envelope: &Envelope) -> RowProject {
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
        Some(Value::String(project)) => project
            .parse()
            .map_or(RowProject::Unscopable, RowProject::Project),
        Some(_) => RowProject::Unscopable,
    }
}

#[derive(Serialize)]
struct EventInput<'a> {
    event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    new: Option<&'a Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    old: Option<&'a Map<String, Value>>,
}

/// Project one wire envelope into the component-facing event document.
pub fn event_input(envelope: &Envelope) -> Value {
    serde_json::to_value(EventInput {
        event: envelope.op.as_str(),
        new: envelope.new.as_ref(),
        old: envelope.old.as_ref(),
    })
    .expect("event input serializes")
}

/// Check the CDC project, lineage depth and optional JMESPath condition.
pub(super) fn matches(
    envelope: &Envelope,
    condition: Option<&str>,
    project: &wamn_registry::ProjectEnvKey,
) -> Result<bool, wamn_run_state::run_store::ProductionLeaseError> {
    use wamn_run_state::run_store::ProductionLeaseError;
    if envelope
        .causation
        .as_ref()
        .is_some_and(|parent| parent.depth >= 16)
    {
        return Err(ProductionLeaseError::contract(
            "match data-change workflow",
            "event lineage is too deep",
        ));
    }
    match row_project(envelope) {
        RowProject::Project(other) if other != *project => return Ok(false),
        RowProject::Unscopable => {
            return Err(ProductionLeaseError::contract(
                "match data-change workflow",
                "invalid event project",
            ));
        }
        _ => {}
    }
    let Some(condition) = condition else {
        return Ok(true);
    };
    let condition = jmespath::compile(condition)
        .map_err(|error| ProductionLeaseError::invalid("compile data-change condition", error))?;
    if envelope.old.is_none() && super::oldref::references_old(condition.as_ast()) {
        return Err(ProductionLeaseError::contract(
            "match data-change workflow",
            "old image is missing",
        ));
    }
    condition
        .search(event_context(envelope))
        .map(|value| value.is_truthy())
        .map_err(|error| ProductionLeaseError::invalid("evaluate data-change condition", error))
}

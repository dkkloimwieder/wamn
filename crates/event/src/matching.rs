//! Build event condition inputs and read the row project.

use crate::Envelope;
use serde::Serialize;
use serde_json::{Map, Value, json};
use wamn_package::RowOperation as Op;

/// Project deployment identity carried by one CDC image.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowProject {
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
fn event_context(envelope: &Envelope) -> Value {
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
fn row_project(envelope: &Envelope) -> RowProject {
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

/// Refusal while matching a data-change workflow.
#[derive(Debug)]
pub struct MatchError {
    operation: &'static str,
    failure: MatchFailure,
}

#[derive(Debug)]
enum MatchFailure {
    Contract(&'static str),
    Condition(Box<jmespath::JmespathError>),
}

impl MatchError {
    /// The operation that refused the event.
    pub fn operation(&self) -> &'static str {
        self.operation
    }

    fn contract(detail: &'static str) -> Self {
        Self {
            operation: "match data-change workflow",
            failure: MatchFailure::Contract(detail),
        }
    }

    fn condition(operation: &'static str, source: jmespath::JmespathError) -> Self {
        Self {
            operation,
            failure: MatchFailure::Condition(Box::new(source)),
        }
    }
}

impl std::fmt::Display for MatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.failure {
            MatchFailure::Condition(source) => source.fmt(formatter),
            MatchFailure::Contract(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for MatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.failure {
            MatchFailure::Condition(source) => Some(source.as_ref()),
            MatchFailure::Contract(_) => None,
        }
    }
}

/// Validate the syntax accepted by data-change matching.
///
/// # Errors
/// Returns the condition parser's error when the expression is invalid.
pub fn validate_condition(condition: &str) -> Result<(), MatchError> {
    compile(condition).map(drop)
}

fn compile(condition: &str) -> Result<jmespath::Expression<'_>, MatchError> {
    jmespath::compile(condition)
        .map_err(|error| MatchError::condition("compile data-change condition", error))
}

/// Check the CDC project, lineage depth and optional JMESPath condition.
pub fn matches(
    envelope: &Envelope,
    condition: Option<&str>,
    project: &wamn_registry::ProjectEnvKey,
) -> Result<bool, MatchError> {
    if envelope
        .causation
        .as_ref()
        .is_some_and(|parent| parent.depth >= 16)
    {
        return Err(MatchError::contract("event lineage is too deep"));
    }
    match row_project(envelope) {
        RowProject::Project(other) if other != *project => return Ok(false),
        RowProject::Unscopable => {
            return Err(MatchError::contract("invalid event project"));
        }
        _ => {}
    }
    let Some(condition) = condition else {
        return Ok(true);
    };
    let condition = compile(condition)?;
    if envelope.old.is_none() && super::oldref::references_old(condition.as_ast()) {
        return Err(MatchError::contract("old image is missing"));
    }
    condition
        .search(event_context(envelope))
        .map(|value| value.is_truthy())
        .map_err(|error| MatchError::condition("evaluate data-change condition", error))
}

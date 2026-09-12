//! Shared submission evidence and lifecycle for generated and composed screens.

use std::fmt::Write as _;

use serde_json::{Value, json};
use wamn_client::request::{BuiltRequest, validate_result, validate_schema};
use wamn_client::{ClientError, HttpResponse};

/// The route's declared guarantee for replaying one captured command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replay {
    Claim,
    State,
    Unknown,
}

/// Operator action offered after an uncertain submission.
#[must_use]
pub const fn recovery_message(contract: &ResponseContract) -> &'static str {
    match contract.replay {
        Replay::Claim => "Retry the captured command with the same key and body.",
        Replay::State => {
            "Refresh the record before a new command. The original command can still complete."
        }
        Replay::Unknown if !contract.direct => {
            "Refresh the result. Repeating this route can repeat downstream effects."
        }
        Replay::Unknown => {
            "Refresh the result. The contract does not guarantee safe captured retry."
        }
    }
}

/// One declared error case. Its origins determine whether it shows refusal.
#[derive(Debug, Clone, Copy)]
pub struct ErrorCase {
    pub literal: &'static str,
    pub required: &'static [&'static str],
    pub sources: &'static [&'static str],
}

/// The response facts emitted from the served route.
#[derive(Debug, Clone, Copy)]
pub struct ResponseContract {
    pub schema: Option<&'static str>,
    pub partial_schema: Option<&'static str>,
    pub fields: &'static [wamn_client::descriptor::FieldSchema],
    pub result_class: Option<&'static str>,
    pub errors: &'static [ErrorCase],
    pub kind: &'static str,
    pub transaction: Option<&'static str>,
    pub direct: bool,
    pub replay: Replay,
}

/// Evidence about the whole submitted intent.
#[derive(Debug, Clone, PartialEq)]
pub enum Evidence {
    Succeeded {
        value: Value,
        opaque: bool,
    },
    Refused(Value),
    /// Only an interpreter of a declared completion contract supplies this.
    /// An error next to a value, or a trace outcome, does not establish it.
    PartiallyCompleted {
        committed_result: Value,
        failed_outcome: Value,
    },
    Uncertain(String),
}

/// The visible lifecycle. A refusal retains the editable draft in its screen.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Editable,
    Pending,
    Succeeded {
        value: Value,
        opaque: bool,
    },
    Refused(Value),
    PartiallyCompleted {
        committed_result: Value,
        failed_outcome: Value,
    },
    Uncertain {
        reason: String,
        retry_refusal: Option<Value>,
    },
}

/// One activation identity, supplied at launch rather than emitted into code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBinding {
    pub url: String,
    pub host: Option<String>,
    pub target_instance: String,
}

/// Identifies an attempt within a session. Old responses cannot resolve a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    generation: u64,
    sequence: u64,
}

/// Captures a command once and retains its uncertainty across retries.
#[derive(Debug)]
pub struct Submission {
    binding: SessionBinding,
    available: bool,
    generation: u64,
    sequence: u64,
    state: State,
    captured: Option<BuiltRequest>,
    unresolved: Option<String>,
    replay: Replay,
}

impl Submission {
    #[must_use]
    pub fn new(binding: SessionBinding) -> Self {
        Self {
            binding,
            available: true,
            generation: 0,
            sequence: 0,
            state: State::Editable,
            captured: None,
            unresolved: None,
            replay: Replay::Unknown,
        }
    }

    #[must_use]
    pub fn state(&self) -> &State {
        &self.state
    }

    #[must_use]
    pub fn captured(&self) -> Option<&BuiltRequest> {
        self.captured.as_ref()
    }

    #[must_use]
    pub fn binding(&self) -> &SessionBinding {
        &self.binding
    }

    #[must_use]
    pub const fn available(&self) -> bool {
        self.available
    }

    /// Start a new intent only from an editable or refused draft.
    pub fn begin(
        &mut self,
        request: BuiltRequest,
        contract: &ResponseContract,
    ) -> Result<Attempt, SubmissionError> {
        if !self.available || !matches!(self.state, State::Editable | State::Refused(_)) {
            return Err(SubmissionError::new(
                "the draft cannot submit in its current state",
            ));
        }
        self.captured = Some(request);
        self.replay = contract.replay;
        self.unresolved = None;
        Ok(self.pending())
    }

    /// Replay the immutable body only when the served route guarantees the same result.
    pub fn retry(&mut self) -> Result<Attempt, SubmissionError> {
        if !self.available
            || self.replay != Replay::Claim
            || !matches!(self.state, State::Uncertain { .. })
        {
            return Err(SubmissionError::new(
                "this submission does not permit captured retry",
            ));
        }
        Ok(self.pending())
    }

    fn pending(&mut self) -> Attempt {
        self.sequence += 1;
        self.state = State::Pending;
        Attempt {
            generation: self.generation,
            sequence: self.sequence,
        }
    }

    /// Consume one response while preserving an earlier attempt's unresolved intent.
    pub fn resolve(
        &mut self,
        attempt: Attempt,
        contract: &ResponseContract,
        response: Result<HttpResponse, ClientError>,
    ) -> bool {
        let Some(request) = self.captured.as_ref() else {
            return false;
        };
        let request_id = request
            .item()
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let evidence = classify(contract, request_id, response);
        self.resolve_evidence(attempt, evidence)
    }

    /// Apply completion evidence from the served contract's interpreter.
    ///
    /// Compositions use this same reducer after their declared response interpreter
    /// establishes completion. It does not derive partial completion from error names.
    pub fn resolve_evidence(&mut self, attempt: Attempt, evidence: Evidence) -> bool {
        if !self.available
            || self.state != State::Pending
            || attempt.generation != self.generation
            || attempt.sequence != self.sequence
        {
            return false;
        }
        self.state = match evidence {
            Evidence::Succeeded { value, opaque } => {
                self.unresolved = None;
                State::Succeeded { value, opaque }
            }
            Evidence::PartiallyCompleted {
                committed_result,
                failed_outcome,
            } => {
                self.unresolved = None;
                State::PartiallyCompleted {
                    committed_result,
                    failed_outcome,
                }
            }
            Evidence::Refused(refusal) => match &self.unresolved {
                Some(reason) => State::Uncertain {
                    reason: reason.clone(),
                    retry_refusal: Some(refusal),
                },
                None => State::Refused(refusal),
            },
            Evidence::Uncertain(reason) => {
                self.unresolved.get_or_insert_with(|| reason.clone());
                State::Uncertain {
                    reason,
                    retry_refusal: None,
                }
            }
        };
        true
    }

    /// Discard local intent explicitly. This does not cancel work on the server.
    pub fn new_command(&mut self) -> Result<(), SubmissionError> {
        if !self.available || self.state == State::Pending {
            return Err(SubmissionError::new(
                "a pending or unavailable session cannot start a new command",
            ));
        }
        self.state = State::Editable;
        self.captured = None;
        self.unresolved = None;
        Ok(())
    }

    /// Call only when the old activation becomes invalid, not when a rebuild starts.
    /// Returns whether an old command can still complete on the server.
    pub fn invalidate(&mut self) -> bool {
        let unresolved = matches!(self.state, State::Pending | State::Uncertain { .. });
        self.available = false;
        self.generation += 1;
        self.captured = None;
        self.unresolved = None;
        self.state = State::Editable;
        unresolved
    }

    /// Bind a successful replacement. The screen resets drafts, rows and cursors
    /// whenever this returns true, even when its generated contract is unchanged.
    pub fn activate(&mut self, binding: SessionBinding) -> bool {
        let reset = !self.available || binding != self.binding;
        if reset {
            self.invalidate();
            self.binding = binding;
        }
        self.available = true;
        reset
    }
}

/// Why a local lifecycle transition is unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionError {
    detail: &'static str,
}

impl SubmissionError {
    const fn new(detail: &'static str) -> Self {
        Self { detail }
    }
}
impl std::fmt::Display for SubmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.detail)
    }
}
impl std::error::Error for SubmissionError {}

/// Classify the original bytes before any legacy error decoder discards evidence.
#[must_use]
pub fn classify(
    contract: &ResponseContract,
    request_id: &str,
    response: Result<HttpResponse, ClientError>,
) -> Evidence {
    let response = match response {
        Ok(response) => response,
        Err(error) => return Evidence::Uncertain(error.to_string()),
    };
    if response.status == 401 && contract.direct {
        return Evidence::Refused(json!({"code":"unauthenticated"}));
    }
    if response.status == 413
        && let Some(limit) = response
            .body
            .strip_prefix("request body exceeds ")
            .and_then(|body| body.strip_suffix("-byte limit\n"))
        && limit
            .parse::<usize>()
            .is_ok_and(|value| value.to_string() == limit)
    {
        return Evidence::Refused(Value::String(response.body));
    }
    let document: Value = match serde_json::from_str(&response.body) {
        Ok(value) => value,
        Err(_) => return unknown("the response is not valid JSON"),
    };
    let uncertain = |reason| unknown_with_literal(reason, &document);
    if document.get("committed_result").is_some() || document.get("failed_outcome").is_some() {
        if !(400..600).contains(&response.status) {
            return unknown("partial completion requires an HTTP error response");
        }
        return classify_partial(contract, request_id, &document);
    }
    // These complete envelopes originate before ingress calls deliver. A
    // downstream failure carrying the same code includes error.message.
    for (status, code) in [
        (400, "schema-invalid"),
        (400, "malformed-json"),
        (413, "mapped-payload-too-large"),
        (429, "route-capacity-exhausted"),
    ] {
        if response.status == status && document == json!({"error":{"code":code}}) {
            return Evidence::Refused(document["error"].clone());
        }
    }
    if response.status == 403 && contract.direct {
        if let Some(error) = document.get("error")
            && error.get("code").and_then(Value::as_str) == Some("permission-denied")
            && error
                .get("operation")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        {
            return Evidence::Refused(error.clone());
        }
        return uncertain("the authorization response is malformed");
    }
    if response.status != 200 {
        return uncertain("the response does not establish completion");
    }
    if let Some(schema) = contract.schema {
        let Ok(schema) = serde_json::from_str(schema) else {
            return uncertain("the served response contract is invalid");
        };
        if validate_schema(&schema, &document).is_err() {
            return uncertain("the response violates the served contract");
        }
    }
    let Some(items) = document.as_array().filter(|items| items.len() == 1) else {
        return uncertain("the response must contain exactly one outcome");
    };
    let item = &items[0];
    if request_id.is_empty() || item.get("request_id").and_then(Value::as_str) != Some(request_id) {
        return uncertain("the response does not match the submitted request");
    }
    match (item.get("value"), item.get("error")) {
        (Some(value), None) => match validate_value(contract, value) {
            Ok(opaque) => Evidence::Succeeded {
                value: value.clone(),
                opaque,
            },
            Err(reason) => uncertain(reason),
        },
        (None, Some(error)) if refusal_is_confirmed(contract, error) => {
            Evidence::Refused(error.clone())
        }
        // Neither member, both members, and unknown error cases establish no
        // completion fact. A value beside an error is not a partial contract.
        _ => uncertain("the response does not establish the submission outcome"),
    }
}

fn classify_partial(contract: &ResponseContract, request_id: &str, document: &Value) -> Evidence {
    let Some(schema) = contract.partial_schema else {
        return unknown("the route declares no partial completion contract");
    };
    let Ok(schema) = serde_json::from_str(schema) else {
        return unknown("the partial completion contract is invalid");
    };
    if validate_schema(&schema, document).is_err() {
        return unknown("the response violates the partial completion contract");
    }
    let Some(items) = document
        .get("committed_result")
        .and_then(Value::as_array)
        .filter(|items| items.len() == 1)
    else {
        return unknown("the committed result must contain exactly one outcome");
    };
    let item = &items[0];
    if request_id.is_empty() || item.get("request_id").and_then(Value::as_str) != Some(request_id) {
        return unknown("the committed result does not match the submitted request");
    }
    let (Some(value), None, Some(failed_outcome)) = (
        item.get("value"),
        item.get("error"),
        document.get("failed_outcome"),
    ) else {
        return unknown("the response does not establish partial completion");
    };
    Evidence::PartiallyCompleted {
        committed_result: value.clone(),
        failed_outcome: failed_outcome.clone(),
    }
}

fn unknown(reason: &str) -> Evidence {
    Evidence::Uncertain(reason.to_owned())
}

// A reported literal is diagnostic even when its envelope shows no outcome.
fn unknown_with_literal(reason: &str, document: &Value) -> Evidence {
    let outcome = document
        .as_array()
        .filter(|items| items.len() == 1)
        .map_or(document, |items| &items[0]);
    match outcome
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
    {
        Some(literal) => {
            let mut diagnostic = format!("{reason}; the server reported {literal}");
            if literal == "concurrency_conflict" {
                let revision = |name: &str| {
                    let value = outcome.get("error")?.get("detail")?.get(name)?;
                    value
                        .as_i64()
                        .or_else(|| value.as_str()?.parse::<i64>().ok())
                };
                if let (Some(expected), Some(observed)) = (
                    revision("expected_row_version"),
                    revision("observed_row_version"),
                ) {
                    write!(
                        diagnostic,
                        " (expected_row_version={expected}, observed_row_version={observed})"
                    )
                    .expect("write to String");
                }
            }
            Evidence::Uncertain(diagnostic)
        }
        None => unknown(reason),
    }
}

fn validate_value(contract: &ResponseContract, value: &Value) -> Result<bool, &'static str> {
    match contract.result_class {
        Some("one") => validate_result(contract.fields, value)
            .map(|opaque| opaque || contract.fields.is_empty())
            .map_err(|_| "the result violates its field contract"),
        Some("bounded_list" | "page") => {
            let key = if contract.result_class == Some("page") {
                "item"
            } else {
                "rows"
            };
            let rows = value
                .get(key)
                .and_then(Value::as_array)
                .ok_or("the result has no declared row collection")?;
            if key == "item"
                && !value
                    .get("next_cursor")
                    .is_some_and(|cursor| cursor.is_null() || cursor.is_string())
            {
                return Err("the page has no valid next_cursor");
            }
            rows.iter()
                .try_fold(contract.fields.is_empty(), |opaque, row| {
                    validate_result(contract.fields, row)
                        .map(|next| opaque || next)
                        .map_err(|_| "a result row violates its field contract")
                })
        }
        _ => Ok(true),
    }
}

fn refusal_is_confirmed(contract: &ResponseContract, error: &Value) -> bool {
    if !contract.direct {
        return false;
    }
    let Some(code) = error.get("code").and_then(Value::as_str) else {
        return false;
    };
    let Some(case) = contract.errors.iter().find(|case| case.literal == code) else {
        return false;
    };
    let detail = error.get("detail");
    if !case.required.iter().all(|key| {
        detail
            .and_then(|detail| detail.get(key))
            .is_some_and(|value| match *key {
                "expected_row_version" | "observed_row_version" => {
                    value.as_i64().is_some()
                        || value
                            .as_str()
                            .is_some_and(|text| text.parse::<i64>().is_ok())
                }
                _ => value.as_str().is_some_and(|text| !text.is_empty()),
            })
    }) {
        return false;
    }
    let transactional = matches!(
        contract.transaction,
        Some("implicit" | "explicit_per_input")
    );
    if !transactional {
        return false;
    }
    if case.sources.is_empty() {
        return matches!(
            contract.kind,
            "get" | "query" | "create" | "update" | "delete"
        ) && matches!(
            code,
            "invalid_input" | "not_found" | "concurrency_conflict" | "idempotency_conflict"
        );
    }
    case.sources.iter().all(|source| {
        matches!(
            *source,
            "malformed_input"
                | "envelope_count"
                | "line_count"
                | "duplicate_line"
                | "nonpositive_quantity"
                | "transaction_invariant"
                | "same_key_different_canonical_command"
                | "unique_violation"
                | "foreign_key_violation"
                | "check_violation"
                | "exclusion_violation"
                | "not_null_violation"
                | "permission_denied"
        )
    })
}

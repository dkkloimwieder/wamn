//! Generated screen definitions and data-dependent state.

use std::fmt;

use serde_json::Value;
use wamn_client::descriptor::FieldSchema;
use wamn_client::request::{RequestError, validate_result};
use wamn_client::{ClientError, HttpResponse, RouteMetadata};

use crate::draft::{Draft, DraftError, FieldState, InputKind, input_kind};
use crate::submission::{
    Attempt, Evidence, ResponseContract, SessionBinding, State, Submission, SubmissionError,
};

/// A declared relationship between a result row and its model record.
#[derive(Debug, Clone, Copy)]
pub struct RecordLink {
    pub relation: &'static str,
    pub key_field: &'static str,
    pub key_input: Option<&'static str>,
}

/// The exact declared read that supplies a command's key and revision.
#[derive(Debug, Clone, Copy)]
pub struct RevisionBinding {
    pub read_operation: &'static str,
    pub read_key_input: &'static str,
    pub key_field: &'static str,
    pub revision_field: &'static str,
    pub command_key_input: &'static str,
    pub command_revision_input: &'static str,
}

/// Platform values supplied once when a submission intent starts.
#[derive(Debug, Clone, Copy)]
pub enum SuppliedKind {
    RequestId,
    IdempotencyKey,
    OccurredAt,
}

/// A declared input path reserved for a platform value.
#[derive(Debug, Clone, Copy)]
pub struct SuppliedField {
    pub path: &'static str,
    pub kind: SuppliedKind,
}

/// All facts that a generated or scaffolded screen consumes.
#[derive(Debug, Clone, Copy)]
pub struct ScreenSpec {
    pub model: &'static str,
    pub name: &'static str,
    pub operation: &'static str,
    pub kind: &'static str,
    pub input: &'static [FieldSchema],
    pub input_schema: Option<&'static str>,
    pub response: ResponseContract,
    pub route: Option<fn() -> RouteMetadata>,
    pub record: Option<RecordLink>,
    pub revision: Option<RevisionBinding>,
    pub revision_inputs: &'static [&'static str],
    pub requires_composition: bool,
    pub supplied: &'static [SuppliedField],
}

/// Values minted by the session driver for one new submission intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentValues {
    pub request_id: String,
    pub idempotency_key: String,
    pub occurred_at: String,
}

/// Why a screen can or cannot start a new intent, before input validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Ready,
    Unavailable,
    NotExposed,
    Unsupported,
    RequiresComposition,
    NeedsRecord,
    Pending,
    Spent,
    Uncertain,
}

impl fmt::Display for Availability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ready => "ready for input validation",
            Self::Unavailable => "the session is unavailable",
            Self::NotExposed => "not exposed over HTTP",
            Self::Unsupported => "unsupported input or invalid screen definition",
            Self::RequiresComposition => "requires composition",
            Self::NeedsRecord => "read the declared record before submitting",
            Self::Pending => "a submission is pending",
            Self::Spent => "start a new command explicitly",
            Self::Uncertain => "the submission outcome is uncertain",
        })
    }
}

/// Whether leaving this screen requires an operator decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitState {
    Ready,
    ConfirmDiscard,
    Pending,
}

/// The local boundary that refused a screen action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenErrorKind {
    Availability,
    ConfirmationRequired,
    Binding,
    Draft,
    Request,
    Submission,
}

/// A screen refusal with its original validation or lifecycle cause.
#[derive(Debug)]
pub struct ScreenError {
    kind: ScreenErrorKind,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ScreenError {
    #[must_use]
    pub const fn kind(&self) -> ScreenErrorKind {
        self.kind
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    fn new(kind: ScreenErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: None,
        }
    }
}

impl fmt::Display for ScreenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for ScreenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|source| source.as_ref() as _)
    }
}

impl From<DraftError> for ScreenError {
    fn from(source: DraftError) -> Self {
        Self {
            kind: ScreenErrorKind::Draft,
            detail: source.to_string(),
            source: Some(Box::new(source)),
        }
    }
}

impl From<RequestError> for ScreenError {
    fn from(source: RequestError) -> Self {
        Self {
            kind: ScreenErrorKind::Request,
            detail: source.to_string(),
            source: Some(Box::new(source)),
        }
    }
}

impl From<SubmissionError> for ScreenError {
    fn from(source: SubmissionError) -> Self {
        Self {
            kind: ScreenErrorKind::Submission,
            detail: source.to_string(),
            source: Some(Box::new(source)),
        }
    }
}

/// Data-dependent state shared by generated screens and ordinary Rust composition.
#[derive(Debug)]
pub struct Screen {
    spec: &'static ScreenSpec,
    draft: Draft,
    submission: Submission,
    input_schema: Option<Value>,
    definition_error: Option<String>,
    rows: Vec<Value>,
    cursor: Option<String>,
    dirty: bool,
    composed: bool,
    record_bound: bool,
}

impl Screen {
    #[must_use]
    pub fn new(spec: &'static ScreenSpec, binding: SessionBinding) -> Self {
        let mut screen = Self {
            spec,
            draft: Draft::new(spec.input),
            submission: Submission::new(binding),
            input_schema: None,
            definition_error: None,
            rows: Vec::new(),
            cursor: None,
            dirty: false,
            composed: false,
            record_bound: false,
        };
        screen.reset_data();
        screen
    }

    #[must_use]
    pub const fn spec(&self) -> &'static ScreenSpec {
        self.spec
    }
    #[must_use]
    pub const fn draft(&self) -> &Draft {
        &self.draft
    }
    #[must_use]
    pub const fn submission(&self) -> &Submission {
        &self.submission
    }
    #[must_use]
    pub fn rows(&self) -> &[Value] {
        &self.rows
    }
    #[must_use]
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
    #[must_use]
    pub const fn dirty(&self) -> bool {
        self.dirty
    }

    /// Structural availability; the request builder checks all required inputs at begin.
    #[must_use]
    pub fn availability(&self) -> Availability {
        if !self.submission.available() {
            return Availability::Unavailable;
        }
        if self.spec.route.is_none() {
            return Availability::NotExposed;
        }
        if self.definition_error.is_some() || unsupported(self.spec.input) {
            return Availability::Unsupported;
        }
        match self.submission.state() {
            State::Pending => return Availability::Pending,
            State::Succeeded { .. } | State::PartiallyCompleted { .. } => {
                return Availability::Spent;
            }
            State::Uncertain { .. } => return Availability::Uncertain,
            State::Editable | State::Refused(_) => {}
        }
        if !self.composed {
            return Availability::RequiresComposition;
        }
        if !self.record_bound {
            return Availability::NeedsRecord;
        }
        Availability::Ready
    }

    #[must_use]
    pub fn exit_state(&self) -> ExitState {
        if self.submission.state() == &State::Pending {
            ExitState::Pending
        } else if self.dirty {
            ExitState::ConfirmDiscard
        } else {
            ExitState::Ready
        }
    }

    /// Edit a typed input. Changing read inputs clears old rows and paging.
    pub fn edit(&mut self, pointer: &str, state: FieldState) -> Result<(), ScreenError> {
        self.ensure_editable()?;
        let mut draft = self.draft.clone();
        draft.edit(pointer, state)?;
        self.apply_edit(draft)
    }

    pub fn insert_row(
        &mut self,
        pointer: &str,
        index: usize,
        value: Value,
    ) -> Result<(), ScreenError> {
        self.ensure_editable()?;
        let mut draft = self.draft.clone();
        draft.insert_row(pointer, index, value)?;
        self.apply_edit(draft)
    }

    pub fn remove_row(&mut self, pointer: &str, index: usize) -> Result<(), ScreenError> {
        self.ensure_editable()?;
        let mut draft = self.draft.clone();
        draft.remove_row(pointer, index)?;
        self.apply_edit(draft)
    }

    /// Supply an exact JSON Pointer from trusted Rust composition.
    pub fn bind(&mut self, pointer: &str, value: Value) -> Result<(), ScreenError> {
        self.ensure_editable()?;
        let mut draft = self.draft.clone();
        draft.bind(pointer, value)?;
        self.apply_edit(draft)
    }

    /// Finish explicit composition after all reserved record/revision paths are bound.
    pub fn mark_composed(&mut self) -> Result<(), ScreenError> {
        self.ensure_editable()?;
        if !self.binding_paths().iter().all(|path| {
            self.draft
                .item()
                .pointer(&pointer(path))
                .is_some_and(|value| !value.is_null())
        }) {
            return Err(binding_error(
                "composition must supply every declared record and revision path",
            ));
        }
        self.composed = true;
        self.record_bound = true;
        Ok(())
    }

    /// Capture a valid first attempt, supplying envelope values once for this intent.
    pub fn begin(
        &mut self,
        values: &IntentValues,
        delete_confirmed: bool,
    ) -> Result<Attempt, ScreenError> {
        let availability = self.availability();
        if availability != Availability::Ready {
            return Err(ScreenError::new(
                ScreenErrorKind::Availability,
                self.definition_error
                    .clone()
                    .unwrap_or_else(|| availability.to_string()),
            ));
        }
        if self.spec.kind == "delete" && !delete_confirmed {
            return Err(ScreenError::new(
                ScreenErrorKind::ConfirmationRequired,
                "confirm deletion before submitting",
            ));
        }
        let mut draft = self.draft.clone();
        for supplied in self.spec.supplied {
            let value = match supplied.kind {
                SuppliedKind::RequestId => &values.request_id,
                SuppliedKind::IdempotencyKey => &values.idempotency_key,
                SuppliedKind::OccurredAt => &values.occurred_at,
            };
            draft.bind(&pointer(supplied.path), Value::String(value.clone()))?;
        }
        let request = draft.build(self.input_schema.as_ref())?;
        let attempt = self.submission.begin(request, &self.spec.response)?;
        self.draft = draft;
        self.rows.clear();
        self.cursor = None;
        Ok(attempt)
    }

    /// Retry only the submission layer's immutable capture, without minting any values.
    pub fn retry(&mut self) -> Result<Attempt, ScreenError> {
        Ok(self.submission.retry()?)
    }

    pub fn resolve(
        &mut self,
        attempt: Attempt,
        response: Result<HttpResponse, ClientError>,
    ) -> bool {
        let resolved = self
            .submission
            .resolve(attempt, &self.spec.response, response);
        if resolved {
            self.show_result();
        }
        resolved
    }

    /// Apply evidence supplied by a declared response interpreter in a composition.
    pub fn resolve_evidence(&mut self, attempt: Attempt, evidence: Evidence) -> bool {
        let resolved = self.submission.resolve_evidence(attempt, evidence);
        if resolved {
            self.show_result();
        }
        resolved
    }

    /// Copy key and revision only from the exact declared successful record read.
    pub fn bind_from_read(&mut self, read: &Screen) -> Result<(), ScreenError> {
        self.ensure_editable()?;
        let mapping = self
            .spec
            .revision
            .ok_or_else(|| binding_error("no record read is declared for this command"))?;
        if read.spec.operation != mapping.read_operation
            || read.spec.kind != "get"
            || read.spec.response.result_class != Some("one")
        {
            return Err(binding_error("the source is not the declared record read"));
        }
        let row = self.validated_row(read, 0)?;
        let key = declared_value(read, row, mapping.key_field)?;
        let revision = declared_value(read, row, mapping.revision_field)?;
        if read
            .submission
            .captured()
            .and_then(|request| request.item().pointer(&pointer(mapping.read_key_input)))
            != Some(key)
        {
            return Err(binding_error(
                "the returned record key does not match the submitted read",
            ));
        }
        let mut draft = self.draft.clone();
        draft.bind(&pointer(mapping.command_key_input), key.clone())?;
        draft.bind(&pointer(mapping.command_revision_input), revision.clone())?;
        self.draft = draft;
        self.record_bound = true;
        Ok(())
    }

    /// Populate only a declared compatible record key, leaving other read inputs alone.
    pub fn populate_read_from_record(
        &mut self,
        source: &Screen,
        row_index: usize,
    ) -> Result<(), ScreenError> {
        self.ensure_editable()?;
        if !matches!(self.spec.kind, "get" | "projection") {
            return Err(binding_error("record selection can populate only a read"));
        }
        let target = self
            .spec
            .record
            .ok_or_else(|| binding_error("the read has no declared record link"))?;
        let origin = source
            .spec
            .record
            .ok_or_else(|| binding_error("the source has no declared record link"))?;
        if target.relation != origin.relation || target.key_field != origin.key_field {
            return Err(binding_error(
                "the selected row belongs to a different declared relation or key",
            ));
        }
        let path = target
            .key_input
            .ok_or_else(|| binding_error("the read has no declared key input"))?;
        let row = self.validated_row(source, row_index)?;
        let key = declared_value(source, row, origin.key_field)?.clone();
        let mut draft = self.draft.clone();
        draft.bind(&pointer(path), key)?;
        self.apply_edit(draft)
    }

    /// Prepare the declared read of this command's bound record for refresh.
    ///
    /// Other read inputs remain unchanged and must still pass input validation.
    ///
    /// # Errors
    /// Refuses missing bindings, a different read or target, and a pending read.
    pub fn prepare_record_refresh(&self, read: &mut Screen) -> Result<(), ScreenError> {
        let mapping = self
            .spec
            .revision
            .ok_or_else(|| binding_error("no record read is declared for this command"))?;
        if !self.submission.available()
            || !read.submission.available()
            || self.submission.binding() != read.submission.binding()
            || read.spec.operation != mapping.read_operation
            || read.spec.kind != "get"
            || read.spec.route.is_none()
        {
            return Err(binding_error(
                "refresh requires the declared read on the same active target",
            ));
        }
        if !self.record_bound {
            return Err(binding_error("the command has no bound record to refresh"));
        }
        let key = self
            .draft
            .item()
            .pointer(&pointer(mapping.command_key_input))
            .filter(|key| !key.is_null())
            .ok_or_else(|| binding_error("the command's bound record key is absent"))?
            .clone();
        read.bind(&pointer(mapping.read_key_input), key)
    }

    /// Preserve the opaque server cursor exactly and ready the next read.
    pub fn next_page(&mut self) -> Result<(), ScreenError> {
        if !self.is_read() || !self.has_cursor() {
            return Err(binding_error("this screen has no declared cursor input"));
        }
        let cursor = self
            .cursor
            .clone()
            .ok_or_else(|| binding_error("the result has no next page"))?;
        let mut draft = self.draft.clone();
        draft.bind("/cursor", Value::String(cursor))?;
        self.submission.new_command()?;
        self.draft = draft;
        self.rows.clear();
        self.cursor = None;
        Ok(())
    }

    /// Ready a fresh read of the current inputs, discarding its old rows and cursor.
    pub fn refresh(&mut self) -> Result<(), ScreenError> {
        if !self.is_read() {
            return Err(binding_error(
                "refresh the declared read before starting a new command",
            ));
        }
        self.submission.new_command()?;
        self.clear_page()?;
        Ok(())
    }

    /// Explicitly discard a local intent and all data-dependent input bindings.
    pub fn new_command(&mut self) -> Result<(), ScreenError> {
        self.submission.new_command()?;
        self.reset_data();
        Ok(())
    }

    /// Invalidate the old target; true means an old submission can still complete.
    pub fn invalidate(&mut self) -> bool {
        let unresolved = self.submission.invalidate();
        self.reset_data();
        unresolved
    }

    /// Replace all target-dependent state only when the activation actually changes.
    pub fn activate(&mut self, binding: SessionBinding) -> bool {
        let changed = self.submission.activate(binding);
        if changed {
            self.reset_data();
        }
        changed
    }

    fn ensure_editable(&self) -> Result<(), ScreenError> {
        if !self.submission.available() {
            return Err(ScreenError::new(
                ScreenErrorKind::Availability,
                Availability::Unavailable.to_string(),
            ));
        }
        match self.submission.state() {
            State::Pending => Err(ScreenError::new(
                ScreenErrorKind::Availability,
                Availability::Pending.to_string(),
            )),
            State::Editable | State::Refused(_) => Ok(()),
            _ if self.is_read() => Ok(()),
            _ => Err(ScreenError::new(
                ScreenErrorKind::Availability,
                self.availability().to_string(),
            )),
        }
    }

    fn apply_edit(&mut self, draft: Draft) -> Result<(), ScreenError> {
        let changed = self.draft.item() != draft.item();
        if changed && self.is_read() {
            self.submission.new_command()?;
        }
        self.draft = draft;
        if changed {
            if self.is_read() {
                self.clear_page()?;
            }
            self.dirty = true;
        }
        Ok(())
    }

    fn is_read(&self) -> bool {
        matches!(self.spec.kind, "get" | "query" | "projection")
    }

    fn has_cursor(&self) -> bool {
        self.spec.response.result_class == Some("page")
            && self
                .spec
                .input
                .iter()
                .any(|field| field.field.path == "cursor")
    }

    fn clear_page(&mut self) -> Result<(), ScreenError> {
        self.rows.clear();
        self.cursor = None;
        if self.has_cursor() {
            self.draft.clear_binding("/cursor")?;
        }
        Ok(())
    }

    fn binding_paths(&self) -> Vec<&'static str> {
        let mut paths = self.spec.revision_inputs.to_vec();
        if let Some(binding) = self.spec.revision {
            paths.extend([binding.command_key_input, binding.command_revision_input]);
        } else if !paths.is_empty()
            && let Some(path) = self.spec.record.and_then(|record| record.key_input)
        {
            paths.push(path);
        }
        paths
    }

    fn reset_data(&mut self) {
        self.draft = Draft::new(self.spec.input);
        self.definition_error = None;
        self.input_schema = None;
        if let Some(schema) = self.spec.input_schema {
            match serde_json::from_str(schema) {
                Ok(schema) => self.input_schema = Some(schema),
                Err(error) => {
                    self.definition_error = Some(format!("invalid input schema: {error}"));
                }
            }
        }
        let mut paths = self.binding_paths();
        paths.extend(self.spec.supplied.iter().map(|field| field.path));
        if self.has_cursor() {
            paths.push("cursor");
        }
        for path in paths {
            if let Err(error) = self.draft.protect(&pointer(path)) {
                self.definition_error = Some(error.to_string());
            }
        }
        self.rows.clear();
        self.cursor = None;
        self.dirty = false;
        self.composed = !self.spec.requires_composition;
        self.record_bound = self.binding_paths().is_empty();
    }

    fn show_result(&mut self) {
        self.rows.clear();
        self.cursor = None;
        if let State::Succeeded { value, .. } = self.submission.state() {
            match self.spec.response.result_class {
                Some("bounded_list" | "page") => {
                    let key = if self.spec.response.result_class == Some("page") {
                        "item"
                    } else {
                        "rows"
                    };
                    if let Some(rows) = value.get(key).and_then(Value::as_array) {
                        self.rows.clone_from(rows);
                    }
                    if self.has_cursor() {
                        self.cursor = value
                            .get("next_cursor")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                }
                _ => self.rows.push(value.clone()),
            }
            self.dirty = false;
        } else if matches!(self.submission.state(), State::PartiallyCompleted { .. }) {
            self.dirty = false;
        }
    }

    fn validated_row<'a>(
        &self,
        source: &'a Screen,
        index: usize,
    ) -> Result<&'a Value, ScreenError> {
        if !source.submission.available()
            || source.submission.binding() != self.submission.binding()
            || !matches!(source.submission.state(), State::Succeeded { .. })
        {
            return Err(binding_error(
                "the source must be a successful read from the same active target",
            ));
        }
        if source.spec.response.fields.is_empty() {
            return Err(binding_error(
                "opaque results cannot supply record bindings",
            ));
        }
        let row = source
            .rows
            .get(index)
            .ok_or_else(|| binding_error("the selected result row is absent"))?;
        validate_result(source.spec.response.fields, row)?;
        Ok(row)
    }
}

fn binding_error(detail: &str) -> ScreenError {
    ScreenError::new(ScreenErrorKind::Binding, detail)
}

fn pointer(path: &str) -> String {
    let mut pointer = String::new();
    for part in path.split('.') {
        pointer.push('/');
        pointer.push_str(&part.replace('~', "~0").replace('/', "~1"));
    }
    pointer
}

fn unsupported(fields: &[FieldSchema]) -> bool {
    fields
        .iter()
        .any(|field| input_kind(field) == InputKind::Unsupported || unsupported(field.children))
}

fn declared_value<'a>(
    screen: &Screen,
    row: &'a Value,
    path: &str,
) -> Result<&'a Value, ScreenError> {
    fn declared(fields: &[FieldSchema], path: &str) -> bool {
        fields.iter().any(|field| {
            (field.field.path == path
                && field.required
                && !field.field.nullable
                && input_kind(field) != InputKind::Unsupported)
                || declared(field.children, path)
        })
    }
    if !declared(screen.spec.response.fields, path) {
        return Err(binding_error(
            "the result does not declare the required record binding field",
        ));
    }
    row.pointer(&pointer(path))
        .filter(|value| !value.is_null())
        .ok_or_else(|| binding_error("the result has no declared record binding value"))
}

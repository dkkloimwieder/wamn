//! The document-number renderer of `wamn:postgres`
//! (`docs/plan/platform-config.md`, "Numbering").
//!
//! `next-number` and `peek-number` render the pattern of one sequence of the
//! composing application, never an argument and never a claim. The plugin
//! reads the application from host state: the operation transaction holds
//! the application it was begun for. The plugin resolves (application,
//! sequence name) to the sequence id, so a sequence of another application is
//! not found. The `app.application` claim stays set for logging only; guest
//! SQL can rewrite it, so it decides nothing here. Both calls run only in an
//! open operation transaction, on its connection and under its claims, so a
//! counter that `next-number` advances is consumed on commit and given back
//! on rollback.
//!
//! Counters advance only through the two platform functions, which take the
//! sequence id that the plugin resolved. Statement admission refuses guest SQL
//! that names them or writes a `platform.*` relation, so only the plugin
//! reaches them:
//! `platform.bump_counter` for a counter with no key or a `reset=` key, and
//! `platform.assign_counter` for a `per=` key that has no value yet. A `per=`
//! key that has one is read with no lock. The counters of one pattern are
//! taken in name order, except that a counter keyed by another counter comes
//! after it, so concurrent callers of one sequence take its locks in one
//! order. `peek-number` reads the current values with no lock and writes
//! nothing.

use std::collections::BTreeMap;

use deadpool_postgres::Object;
use tracing::Instrument as _;
use wamn_number_pattern::{Counter, DateFormat, Key, Part, Pattern, Scope};
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::wasmtime::component::Accessor;

use super::resources::{db_span_for_project, plugin_of};
use super::{PgError, StatementError, WamnPostgres};

/// What one call does with the counters of the pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NumberCall {
    /// `next-number`: advance each counter and render the new values.
    Next,
    /// `peek-number`: render the last values issued, with no lock.
    Peek,
}

impl NumberCall {
    const fn operation(self) -> &'static str {
        match self {
            Self::Next => "statement.next_number",
            Self::Peek => "statement.peek_number",
        }
    }
}

/// The SQLSTATE of a `peek-number` for a counter that has issued no number
/// yet: `no_data_found`.
pub(super) const NO_NUMBER_ISSUED: &str = "P0002";
/// A sequence that the composing application does not own:
/// `undefined_object`.
const UNKNOWN_SEQUENCE: &str = "42704";
/// A record or a stored pattern that the renderer cannot use:
/// `invalid_parameter_value`.
const INVALID_INPUT: &str = "22023";
/// A pattern field that the record carries as null:
/// `null_value_not_allowed`.
const NULL_FIELD: &str = "22004";

/// The sequence id and pattern of sequence `$2` of application `$1`, and the
/// `timezone` setting at the scope of `$1`. The host binds `$1` from host
/// state. A sequence that `$1` does not own reads as null.
const SEQUENCE_SQL: &str = "SELECT s.id::text, s.pattern, \
    (SELECT t.value #>> '{}' FROM platform.setting AS t \
      WHERE t.scope_type = 'application' AND t.scope_id = c.application \
        AND t.key = 'timezone') \
    FROM (SELECT $1::text AS application) AS c \
    LEFT JOIN platform.number_sequence AS s \
      ON s.application = c.application AND s.name = $2";
/// The transaction time in one time zone, in every format of the renderer,
/// in the order of [`DateFormat::ALL`].
const DATE_SQL: &str =
    "SELECT to_char(transaction_timestamp() AT TIME ZONE $1, 'YYYY|YY|MM|DD|IW|Q')";
const BUMP_SQL: &str = "SELECT platform.bump_counter($1::text::uuid, $2, $3)";
const ASSIGN_SQL: &str = "SELECT platform.assign_counter($1::text::uuid, $2, $3)";
const COUNTER_SQL: &str = "SELECT value FROM platform.number_counter \
    WHERE sequence_id = $1::text::uuid AND counter = $2 AND key_value = $3";

/// Render one number of the composing application in the operation
/// transaction of the invocation. A member's named import renders in the
/// same transaction, for the same application.
pub(super) async fn statement_number<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    name: String,
    record: String,
    call: NumberCall,
) -> wash_runtime::wasmtime::Result<Result<String, StatementError>> {
    let (plugin, component_id, trace) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            trace,
        ))
    })?;
    trace
        .run(async move { Ok(render_number(&plugin, &component_id, name, record, call).await) })
        .await
}

/// Render the next or the current number of sequence `name` for `record`,
/// a JSON object of strings and nulls, in the operation transaction of
/// `component_id`.
///
/// # Errors
///
/// `postgres(permission-denied)` outside an open operation transaction or
/// when host state holds no application for it; `query-error` with SQLSTATE `42704` for a
/// sequence the application does not own, `22023` for a record or a stored
/// pattern the renderer cannot use, `22004` for a null pattern field, and
/// [`NO_NUMBER_ISSUED`] for a `peek-number` before any number; and the error
/// of any statement.
async fn render_number(
    plugin: &WamnPostgres,
    component_id: &str,
    name: String,
    record: String,
    call: NumberCall,
) -> Result<String, StatementError> {
    let Some(transaction) = plugin.operation_transaction_for(component_id) else {
        return Err(StatementError::Postgres(PgError::PermissionDenied));
    };
    let Some(application) = transaction.application.clone() else {
        return Err(StatementError::Postgres(PgError::PermissionDenied));
    };
    let record =
        serde_json::from_str::<BTreeMap<String, Option<String>>>(&record).map_err(|error| {
            refusal(
                INVALID_INPUT,
                format!("the record is not one JSON object of strings and nulls: {error}"),
            )
        })?;
    let project = plugin.project_for(component_id);
    let span = db_span_for_project(plugin, component_id, &project, call.operation());
    let rendered = transaction
        .with_host_connection(|connection| async move {
            let result =
                match render(connection.connection(), &application, &name, &record, call).await {
                    Ok(number) => Ok(Ok(number)),
                    Err(Failure::Refused(refused)) => Ok(Err(refused)),
                    Err(Failure::Postgres(error)) => Err(error),
                };
            (connection, result)
        })
        .instrument(span)
        .await;
    match rendered {
        Ok(result) => result,
        Err(error) => Err(StatementError::Postgres(error)),
    }
}

/// Why one render did not produce a number.
enum Failure {
    /// The renderer refused, with the statement error the guest sees.
    Refused(StatementError),
    /// A statement failed.
    Postgres(tokio_postgres::Error),
}

impl From<tokio_postgres::Error> for Failure {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Postgres(error)
    }
}

impl From<StatementError> for Failure {
    fn from(refused: StatementError) -> Self {
        Self::Refused(refused)
    }
}

fn refusal(code: &str, message: String) -> StatementError {
    StatementError::Postgres(PgError::QueryError((code.to_owned(), message)))
}

async fn render(
    connection: &Object,
    application: &str,
    name: &str,
    record: &BTreeMap<String, Option<String>>,
    call: NumberCall,
) -> Result<String, Failure> {
    let statement = connection.prepare_cached(SEQUENCE_SQL).await?;
    let row = connection
        .query_one(&statement, &[&application, &name])
        .await?;
    let (Some(sequence), Some(pattern)) = (
        row.get::<_, Option<String>>(0),
        row.get::<_, Option<String>>(1),
    ) else {
        return Err(refusal(
            UNKNOWN_SEQUENCE,
            format!("{name} is not a number sequence of application {application}"),
        )
        .into());
    };
    let timezone = row
        .get::<_, Option<String>>(2)
        .unwrap_or_else(|| "UTC".to_owned());
    let pattern = wamn_number_pattern::parse(&pattern).map_err(|error| {
        refusal(
            INVALID_INPUT,
            format!("the pattern of number sequence {name} does not parse: {error}"),
        )
    })?;
    let fields = record_fields(&pattern, record)?;
    let dates = if uses_date(&pattern) {
        let statement = connection.prepare_cached(DATE_SQL).await?;
        let text: String = connection.query_one(&statement, &[&timezone]).await?.get(0);
        Some(text.split('|').map(str::to_owned).collect::<Vec<_>>())
    } else {
        None
    };
    let date = |format: DateFormat| {
        let index = DateFormat::ALL
            .iter()
            .position(|candidate| *candidate == format)
            .expect("every format is in ALL");
        dates.as_ref().expect("a date part reads the dates")[index].clone()
    };

    let mut counters = BTreeMap::<&str, String>::new();
    for counter in counter_order(&pattern) {
        let key = match counter.scope().key() {
            None => String::new(),
            Some(Key::Field(field)) => fields[&**field].to_owned(),
            Some(Key::Counter(other)) => counters[&**other].clone(),
            Some(Key::Date(format)) => date(*format),
        };
        let per = matches!(counter.scope(), Scope::Per(_));
        if per && key.is_empty() {
            return Err(refusal(
                INVALID_INPUT,
                format!(
                    "counter {} of number sequence {name} has an empty per= key",
                    counter.name()
                ),
            )
            .into());
        }
        let value = counter_value(connection, &sequence, counter, &key, per, call).await?;
        let Some(value) = value else {
            return Err(refusal(
                NO_NUMBER_ISSUED,
                format!(
                    "counter {} of number sequence {name} has issued no number for key {key:?}",
                    counter.name()
                ),
            )
            .into());
        };
        counters.insert(counter.name(), render_counter(counter, value));
    }

    Ok(pattern
        .parts()
        .iter()
        .map(|part| match part {
            Part::Literal(text) => text.to_string(),
            Part::Counter(counter) => counters[counter.name()].clone(),
            Part::Field(field) => fields[&**field].to_owned(),
            Part::Date(format) => date(*format),
        })
        .collect())
}

/// The value of one counter for one key: advanced for [`NumberCall::Next`],
/// and the last issued for [`NumberCall::Peek`], or `None` when none was.
async fn counter_value(
    connection: &Object,
    sequence: &str,
    counter: &Counter,
    key: &str,
    per: bool,
    call: NumberCall,
) -> Result<Option<i64>, tokio_postgres::Error> {
    let name = counter.name();
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] = [&sequence, &name, &key];
    if call == NumberCall::Next && !per {
        let bump = connection.prepare_cached(BUMP_SQL).await?;
        return Ok(Some(connection.query_one(&bump, &params).await?.get(0)));
    }
    // A per= value never changes once assigned, so a hit needs no lock.
    let read = connection.prepare_cached(COUNTER_SQL).await?;
    let stored = connection
        .query_opt(&read, &params)
        .await?
        .map(|row| row.get::<_, i64>(0));
    Ok(match (call, stored) {
        (NumberCall::Next, Some(value)) => Some(value),
        (NumberCall::Next, None) => {
            let assign = connection.prepare_cached(ASSIGN_SQL).await?;
            Some(connection.query_one(&assign, &params).await?.get(0))
        }
        (NumberCall::Peek, stored) if per => stored,
        // The row holds the next value to render, so the last issued is one
        // less; a row at 1 has issued none.
        (NumberCall::Peek, stored) => stored.filter(|next| *next > 1).map(|next| next - 1),
    })
}

/// The text of each record field the pattern names.
fn record_fields<'a>(
    pattern: &Pattern,
    record: &'a BTreeMap<String, Option<String>>,
) -> Result<BTreeMap<&'a str, &'a str>, StatementError> {
    pattern
        .fields()
        .into_iter()
        .map(|field| match record.get_key_value(field) {
            Some((name, Some(value))) => Ok((name.as_str(), value.as_str())),
            Some((_, None)) => Err(refusal(NULL_FIELD, format!("record field {field} is null"))),
            None => Err(refusal(
                INVALID_INPUT,
                format!("the record has no field {field}"),
            )),
        })
        .collect()
}

fn uses_date(pattern: &Pattern) -> bool {
    pattern.parts().iter().any(|part| match part {
        Part::Date(_) => true,
        Part::Counter(counter) => matches!(counter.scope().key(), Some(Key::Date(_))),
        Part::Literal(_) | Part::Field(_) => false,
    })
}

/// The counters of `pattern` in name order, each after the counter its key
/// names. The parser refuses a cycle, so the order is total.
fn counter_order(pattern: &Pattern) -> Vec<&Counter> {
    let mut pending = pattern.counters().collect::<Vec<_>>();
    pending.sort_by(|left, right| left.name().cmp(right.name()));
    let mut ordered = Vec::<&Counter>::with_capacity(pending.len());
    while !pending.is_empty() {
        let ready = pending
            .iter()
            .position(|counter| match counter.scope().key() {
                Some(Key::Counter(other)) => ordered.iter().any(|done| done.name() == &**other),
                _ => true,
            })
            .expect("the parser refuses a counter cycle");
        ordered.push(pending.remove(ready));
    }
    ordered
}

/// One counter value as its token states: decimal or upper-case hex, padded
/// to the width, never truncated.
fn render_counter(counter: &Counter, value: i64) -> String {
    let digits = if counter.hex() {
        format!("{value:X}")
    } else {
        value.to_string()
    };
    let width = usize::from(counter.width().unwrap_or(0));
    let mut text = String::with_capacity(width.max(digits.len()));
    text.extend(std::iter::repeat_n(
        counter.pad(),
        width.saturating_sub(digits.len()),
    ));
    text.push_str(&digits);
    text
}

mod set;
pub use set::{CounterSet, CounterSetError, CounterSetRefusal};

#[cfg(test)]
mod tests;

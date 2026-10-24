//! The write of the `number_counter.set` host route
//! (`docs/plan/platform-config.md`, "Numbering", "Administration").
//!
//! A set runs in an operation transaction of the guest authority, under the
//! caller, the route's operation and the application as claims, so the
//! engine write log records it by its request id and the counter row's
//! `updated_by` names the caller. `platform.set_counter` writes the value
//! under the counter row's lock, so a set waits for a concurrent next number
//! and a next number waits for the set.
//!
//! Before the commit, the set renders the number that the next call would
//! give, with the one renderer of this plugin, and refuses when a record of
//! the application already carries it. The application's records are the
//! single-column unique text columns of its schemas: the uniqueness of a
//! number is the model's `UQ` on it. The set refuses a `per=` counter, whose
//! values are mappings that never change.

use std::collections::BTreeMap;

use deadpool_postgres::Object;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use wamn_number_pattern::{Counter, DateFormat, Key, Part, Pattern, Scope};

use super::super::write_log::OpaqueBegun;
use super::super::{SessionClaims, WamnPostgres};
use super::{COUNTER_SQL, DATE_SQL, SEQUENCE_SQL, render_counter};

/// One set of one counter of a number sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CounterSet {
    /// The composing application that owns the sequence.
    pub application: String,
    /// The sequence name.
    pub sequence: String,
    /// The counter name in the sequence's pattern.
    pub counter: String,
    /// The key value of the counter: `''` for a counter with no key, the
    /// field value for `reset=<field>`, the rendered value of the other
    /// counter for `reset=<counter>`, and the date text for `reset=date:FMT`.
    pub key: String,
    /// The next value to render, 1 or more.
    pub value: i64,
}

/// Why a set was refused. The route answers each with its declared literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CounterSetRefusal {
    /// A request member the set cannot take, by its name.
    InvalidInput { field: &'static str },
    /// The application has no sequence of this name.
    SequenceNotFound,
    /// The sequence's pattern has no counter of this name.
    CounterNotFound,
    /// The counter is a `per=` counter.
    AssignedPerKey,
    /// A record already carries the number the set would give next.
    NumberLive { number: String },
    /// The request id was used for a different set.
    IdempotencyConflict,
}

/// Why a set did not commit.
#[derive(Debug)]
pub enum CounterSetError {
    /// The set was refused and changed nothing.
    Refused(CounterSetRefusal),
    /// The set failed, and changed nothing.
    Failed(anyhow::Error),
}

impl From<CounterSetRefusal> for CounterSetError {
    fn from(refusal: CounterSetRefusal) -> Self {
        Self::Refused(refusal)
    }
}

/// The write of the route.
const SET_SQL: &str = "SELECT platform.set_counter($1::text::uuid, $2, $3, $4)";

/// The single-column unique text columns of the schemas in `$1` whose
/// values the caller can read: the columns that can hold a number.
const NUMBER_COLUMNS_SQL: &str = "SELECT DISTINCT n.nspname::text, c.relname::text, a.attname::text \
    FROM pg_catalog.pg_index AS i \
    JOIN pg_catalog.pg_class AS c ON c.oid = i.indrelid \
    JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
    JOIN pg_catalog.pg_attribute AS a ON a.attrelid = c.oid AND a.attnum = i.indkey[0] \
    WHERE i.indisunique AND i.indnatts = 1 AND i.indpred IS NULL AND i.indkey[0] > 0 \
      AND c.relkind IN ('r', 'p') AND n.nspname = ANY($1::text[]) \
      AND a.atttypid IN ('text'::regtype, 'varchar'::regtype) \
      AND has_column_privilege(c.oid, a.attnum, 'SELECT') \
    ORDER BY 1, 2, 3";

impl WamnPostgres {
    /// Set one counter of a sequence of `set.application` for the caller
    /// `user_id`, in one operation transaction of the guest authority of
    /// `tenant` in `project`, logged in the write log under `operation` and
    /// `request_id`. `schemas` are the schemas of the application's records.
    /// Returns the number the next call renders. A retry of a committed set
    /// answers its stored number.
    ///
    /// # Errors
    ///
    /// [`CounterSetError::Refused`] for a refused set, and
    /// [`CounterSetError::Failed`] for a failed statement or transaction.
    /// Neither commits anything.
    #[expect(
        clippy::too_many_arguments,
        reason = "each claim of the transaction is an independent trusted input"
    )]
    pub async fn set_number_counter(
        &self,
        project: &str,
        tenant: &str,
        user_id: &str,
        operation: &str,
        request_id: &str,
        set: &CounterSet,
        schemas: &[String],
    ) -> Result<String, CounterSetError> {
        if set.value < 1 {
            return Err(CounterSetRefusal::InvalidInput { field: "value" }.into());
        }
        if request_id.is_empty() {
            return Err(CounterSetRefusal::InvalidInput {
                field: "request_id",
            }
            .into());
        }
        let scope = format!("wamn:number-counter-set:{}", uuid::Uuid::new_v4());
        let claims = SessionClaims {
            tenant: tenant.to_owned(),
            project: Some(project.to_owned()),
            user_id: Some(user_id.to_owned()),
            operation: Some(operation.to_owned()),
            application: Some(set.application.clone()),
            ..SessionClaims::default()
        };
        let result = async {
            self.bind_session_claims(&scope, &claims)
                .await
                .map_err(CounterSetError::Failed)?;
            self.set_in_transaction(&scope, operation, request_id, set, schemas)
                .await
        }
        .await;
        self.revoke_session_claims(&scope);
        result
    }

    async fn set_in_transaction(
        &self,
        scope: &str,
        operation: &str,
        request_id: &str,
        set: &CounterSet,
        schemas: &[String],
    ) -> Result<String, CounterSetError> {
        let failed = |context: &str, error: &dyn std::fmt::Debug| {
            CounterSetError::Failed(anyhow::anyhow!("{context}: {error:?}"))
        };
        let transaction = self
            .begin_operation_transaction(scope)
            .await
            .map_err(|error| failed("begin the set's transaction", &error))?;
        let request =
            Sha256::digest(serde_json::to_vec(set).expect("a counter set serializes to JSON"))
                .to_vec();
        let begun = match transaction
            .begin_opaque(operation, request_id, request)
            .await
        {
            Ok(begun) => begun,
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(failed("claim the request id in the write log", &error));
            }
        };
        let stored = match begun {
            OpaqueBegun::New => None,
            OpaqueBegun::Finished(result) => Some(result),
            OpaqueBegun::Conflict => {
                let _ = transaction.rollback().await;
                return Err(CounterSetRefusal::IdempotencyConflict.into());
            }
            OpaqueBegun::Uncertain => {
                let _ = transaction.rollback().await;
                return Err(CounterSetError::Failed(anyhow::anyhow!(
                    "the write log holds an unfinished claim of request {request_id}"
                )));
            }
        };
        if let Some(result) = stored {
            let _ = transaction.rollback().await;
            return serde_json::from_str::<serde_json::Value>(&result)
                .ok()
                .and_then(|value| value["next_number"].as_str().map(str::to_owned))
                .ok_or_else(|| {
                    CounterSetError::Failed(anyhow::anyhow!(
                        "the stored result of request {request_id} names no next number"
                    ))
                });
        }
        let outcome = transaction
            .with_host_connection(|connection| async move {
                let result = set_and_render(connection.connection(), set, schemas).await;
                (connection, result)
            })
            .await;
        let number = match outcome {
            Ok(Ok(number)) => number,
            Ok(Err(refusal)) => {
                let _ = transaction.rollback().await;
                return Err(refusal.into());
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(failed("set the counter", &error));
            }
        };
        transaction.finish_opaque(
            operation,
            request_id,
            serde_json::json!({ "next_number": number }).to_string(),
        );
        transaction
            .commit()
            .await
            .map_err(|error| failed("commit the set", &error))?;
        Ok(number)
    }
}

/// Set the counter under its row lock, then render the number the next call
/// gives and refuse when a record carries it. The caller rolls back a
/// refusal, which undoes the set.
async fn set_and_render(
    connection: &Object,
    set: &CounterSet,
    schemas: &[String],
) -> Result<Result<String, CounterSetRefusal>, tokio_postgres::Error> {
    let statement = connection.prepare_cached(SEQUENCE_SQL).await?;
    let row = connection.query_one(&statement, &[&set.sequence]).await?;
    let (Some(sequence), Some(pattern)) = (
        row.get::<_, Option<String>>(1),
        row.get::<_, Option<String>>(2),
    ) else {
        return Ok(Err(CounterSetRefusal::SequenceNotFound));
    };
    let timezone = row
        .get::<_, Option<String>>(3)
        .unwrap_or_else(|| "UTC".to_owned());
    // A stored pattern that does not parse has no counter to set.
    let Ok(pattern) = wamn_number_pattern::parse(&pattern) else {
        return Ok(Err(CounterSetRefusal::CounterNotFound));
    };
    let Some(target) = pattern
        .counters()
        .find(|counter| counter.name() == set.counter)
    else {
        return Ok(Err(CounterSetRefusal::CounterNotFound));
    };
    match target.scope() {
        Scope::Per(_) => return Ok(Err(CounterSetRefusal::AssignedPerKey)),
        Scope::Always if !set.key.is_empty() => {
            return Ok(Err(CounterSetRefusal::InvalidInput { field: "key" }));
        }
        Scope::Always | Scope::Reset(_) => {}
    }

    let write = connection.prepare_cached(SET_SQL).await?;
    connection
        .execute(&write, &[&sequence, &set.counter, &set.key, &set.value])
        .await?;

    let dates = if uses_date(&pattern) {
        let statement = connection.prepare_cached(DATE_SQL).await?;
        let text: String = connection.query_one(&statement, &[&timezone]).await?.get(0);
        text.split('|').map(str::to_owned).collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let render = DryRender {
        connection,
        sequence: &sequence,
        target,
        set,
        dates: &dates,
    };
    let Some(number) = render.number(&pattern).await? else {
        return Ok(Err(CounterSetRefusal::InvalidInput { field: "key" }));
    };
    if number_is_live(connection, schemas, &number).await? {
        return Ok(Err(CounterSetRefusal::NumberLive { number }));
    }
    Ok(Ok(number))
}

/// The render of the number that the next call gives after the set: the
/// target counter at the set value, and every other part as the next call
/// renders it. A part that needs a record field other than the key is
/// unknown, so the number is `None`.
struct DryRender<'a> {
    connection: &'a Object,
    sequence: &'a str,
    target: &'a Counter,
    set: &'a CounterSet,
    dates: &'a [String],
}

impl DryRender<'_> {
    async fn number(&self, pattern: &Pattern) -> Result<Option<String>, tokio_postgres::Error> {
        let mut text = String::new();
        for part in pattern.parts() {
            let rendered = match part {
                Part::Literal(literal) => Some(literal.to_string()),
                Part::Field(field) => self.field(field),
                Part::Date(format) => Some(self.date(*format)),
                Part::Counter(counter) => self.counter(counter).await?,
            };
            let Some(rendered) = rendered else {
                return Ok(None);
            };
            text.push_str(&rendered);
        }
        Ok(Some(text))
    }

    /// A record field: known only as the target's key.
    fn field(&self, field: &str) -> Option<String> {
        matches!(self.target.scope().key(), Some(Key::Field(key)) if &**key == field)
            .then(|| self.set.key.clone())
    }

    /// A date part: the key for the target's date key, else the transaction
    /// date.
    fn date(&self, format: DateFormat) -> String {
        if matches!(self.target.scope().key(), Some(Key::Date(key)) if *key == format) {
            return self.set.key.clone();
        }
        let index = DateFormat::ALL
            .iter()
            .position(|candidate| *candidate == format)
            .expect("every format is in ALL");
        self.dates[index].clone()
    }

    /// The text of one key, when it is known without a record.
    fn key(&self, key: &Key) -> Option<String> {
        match key {
            Key::Field(field) => self.field(field),
            Key::Date(format) => Some(self.date(*format)),
            Key::Counter(other) if other.as_ref() == self.target.name() => {
                Some(render_counter(self.target, self.set.value))
            }
            Key::Counter(other) => {
                matches!(self.target.scope().key(), Some(Key::Counter(key)) if key == other)
                    .then(|| self.set.key.clone())
            }
        }
    }

    async fn counter(&self, counter: &Counter) -> Result<Option<String>, tokio_postgres::Error> {
        if counter.name() == self.target.name() {
            return Ok(Some(render_counter(counter, self.set.value)));
        }
        // The counter that keys the target renders as the key.
        if matches!(self.target.scope().key(), Some(Key::Counter(key)) if key.as_ref() == counter.name())
        {
            return Ok(Some(self.set.key.clone()));
        }
        let key = match counter.scope().key() {
            None => String::new(),
            Some(key) => match self.key(key) {
                Some(key) => key,
                None => return Ok(None),
            },
        };
        let read = self.connection.prepare_cached(COUNTER_SQL).await?;
        let stored = self
            .connection
            .query_opt(&read, &[&self.sequence, &counter.name(), &key])
            .await?
            .map(|row| row.get::<_, i64>(0));
        let value = match (counter.scope(), stored) {
            (_, Some(value)) => value,
            // A per= key with no value yet takes the allocator's next value.
            (Scope::Per(_), None) => self
                .connection
                .query_opt(&read, &[&self.sequence, &counter.name(), &""])
                .await?
                .map_or(1, |row| row.get::<_, i64>(0)),
            (Scope::Always | Scope::Reset(_), None) => 1,
        };
        Ok(Some(render_counter(counter, value)))
    }
}

fn uses_date(pattern: &Pattern) -> bool {
    pattern.parts().iter().any(|part| match part {
        Part::Date(_) => true,
        Part::Counter(counter) => matches!(counter.scope().key(), Some(Key::Date(_))),
        Part::Literal(_) | Part::Field(_) => false,
    })
}

/// Whether a record in `schemas` carries `number` in a column that can hold
/// a number.
async fn number_is_live(
    connection: &Object,
    schemas: &[String],
    number: &str,
) -> Result<bool, tokio_postgres::Error> {
    if schemas.is_empty() {
        return Ok(false);
    }
    let statement = connection.prepare_cached(NUMBER_COLUMNS_SQL).await?;
    let columns = connection.query(&statement, &[&schemas]).await?;
    let mut by_table = BTreeMap::<(String, String), Vec<String>>::new();
    for row in &columns {
        by_table
            .entry((row.get(0), row.get(1)))
            .or_default()
            .push(row.get(2));
    }
    for ((schema, table), columns) in by_table {
        let condition = columns
            .iter()
            .map(|column| format!("{} = $1", wamn_pg_core::quote_ident(column)))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            "SELECT EXISTS (SELECT 1 FROM {}.{} WHERE {condition})",
            wamn_pg_core::quote_ident(&schema),
            wamn_pg_core::quote_ident(&table)
        );
        if connection
            .query_one(&sql, &[&number])
            .await?
            .get::<_, bool>(0)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

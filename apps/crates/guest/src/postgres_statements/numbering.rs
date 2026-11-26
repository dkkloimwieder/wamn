//! Render document numbers with SQL in the operation's existing transaction.

use super::{FromSqlValue, SqlValue, StatementError, Transaction, wire};
use std::collections::BTreeMap;
use wamn_number_pattern::{
    Counter, DateFormat, Key, Part, Policy, counter_order, date_formats, render_counter,
};

const SEQUENCE_SQL: &str = "SELECT s.id::text, s.pattern, t.value #>> '{}', jsonb_typeof(t.value), \
    platform.valid_timezone(COALESCE(t.value #>> '{}', 'UTC')) \
    FROM platform.number_sequence AS s LEFT JOIN platform.setting AS t \
    ON t.target_type = 'app' AND t.target_id = s.app AND t.key = 'timezone' \
    WHERE s.app = (SELECT wamn_authority.current_app()) AND s.name = $1";
const COUNTER_SQL: &str = "SELECT value FROM platform.number_counter \
    WHERE sequence_id = $1::text::uuid AND counter = $2 AND key_value = $3";

fn refusal(code: &str, context: impl Into<String>) -> StatementError {
    StatementError::from_postgres(wire::PgError::QueryError((code.to_owned(), context.into())))
}

fn column<T: FromSqlValue>(
    values: &mut impl Iterator<Item = SqlValue>,
) -> Result<T, StatementError> {
    values
        .next()
        .and_then(T::from_sql_value)
        .ok_or_else(|| StatementError::invalid_result("number sequence", "invalid SQL result"))
}

/// Take a number in this transaction. Rollback returns its counters.
pub async fn next_number(
    transaction: &mut Transaction,
    name: &str,
    keys: &[(&str, Option<String>)],
) -> Result<String, StatementError> {
    render(transaction, name, keys, true).await
}

/// Read the last issued number without advancing or locking its counters.
pub async fn peek_number(
    transaction: &mut Transaction,
    name: &str,
    keys: &[(&str, Option<String>)],
) -> Result<String, StatementError> {
    render(transaction, name, keys, false).await
}

async fn render(
    transaction: &mut Transaction,
    name: &str,
    keys: &[(&str, Option<String>)],
    advance: bool,
) -> Result<String, StatementError> {
    let mut fields = BTreeMap::new();
    for (field, value) in keys {
        if fields.insert(*field, value.as_deref()).is_some() {
            return Err(refusal("22023", format!("key {field} is given twice")));
        }
    }
    let rows = transaction
        .query(SEQUENCE_SQL, vec![SqlValue::Text(name.to_owned())])
        .await?;
    let mut row = rows
        .rows
        .into_iter()
        .next()
        .ok_or_else(|| refusal("42704", format!("unknown number sequence {name}")))?
        .into_iter();
    let sequence: String = column(&mut row)?;
    let pattern: String = column(&mut row)?;
    let timezone: Option<String> = column(&mut row)?;
    let timezone_type: Option<String> = column(&mut row)?;
    let timezone_valid: bool = column(&mut row)?;
    let pattern = wamn_number_pattern::parse(&pattern)
        .map_err(|error| refusal("22023", error.to_string()))?;
    for field in pattern.fields() {
        match fields.get(field) {
            Some(Some(value)) if !value.is_empty() => {}
            Some(None) => return Err(refusal("22004", format!("key {field} is null"))),
            _ => return Err(refusal("22023", format!("key {field} is missing or empty"))),
        }
    }
    let formats = date_formats(&pattern).collect::<Vec<_>>();
    let mut dates = BTreeMap::new();
    if !formats.is_empty() {
        if !timezone_valid {
            return Err(refusal("22023", "invalid time zone"));
        }
        let timezone = match timezone_type.as_deref() {
            None => "UTC",
            Some("string") => timezone
                .as_deref()
                .ok_or_else(|| refusal("22023", "invalid time zone"))?,
            _ => return Err(refusal("22023", "invalid time zone")),
        };
        let iso_year = formats.contains(&DateFormat::Week);
        let mut formats = formats;
        formats.sort_unstable();
        formats.dedup();
        let tokens = formats
            .iter()
            .map(|format| match format {
                DateFormat::Year if iso_year => "IYYY",
                DateFormat::ShortYear if iso_year => "IY",
                DateFormat::Year => "YYYY",
                DateFormat::ShortYear => "YY",
                DateFormat::Month => "MM",
                DateFormat::Day => "DD",
                DateFormat::Week => "IW",
                DateFormat::Quarter => "Q",
            })
            .collect::<Vec<_>>()
            .join("|");
        let rows = transaction
            .query(
                "SELECT to_char(transaction_timestamp() AT TIME ZONE $1, $2)",
                vec![SqlValue::Text(timezone.to_owned()), SqlValue::Text(tokens)],
            )
            .await?;
        let mut row = rows
            .rows
            .into_iter()
            .next()
            .ok_or_else(|| {
                StatementError::invalid_result("number sequence", "missing transaction date")
            })?
            .into_iter();
        let text: String = column(&mut row)?;
        dates.extend(formats.into_iter().zip(text.split('|').map(str::to_owned)));
    }

    let mut counters = BTreeMap::<&str, String>::new();
    for counter in counter_order(&pattern) {
        let key = match counter.policy().key() {
            None => "",
            Some(Key::Field(field)) => fields[&**field].expect("keys were checked"),
            Some(Key::Counter(other)) => counters[&**other].as_str(),
            Some(Key::Date(format)) => dates[format].as_str(),
        };
        let value = counter_value(transaction, &sequence, counter, key, advance)
            .await?
            .ok_or_else(|| {
                refusal(
                    "P0002",
                    format!("counter {} has issued no number", counter.name()),
                )
            })?;
        counters.insert(counter.name(), render_counter(counter, value));
    }
    Ok(pattern
        .parts()
        .iter()
        .map(|part| match part {
            Part::Literal(text) => text.as_ref(),
            Part::Counter(counter) => counters[counter.name()].as_str(),
            Part::Field(field) => fields[&**field].expect("keys were checked"),
            Part::Date(format) => dates[format].as_str(),
        })
        .collect())
}

async fn counter_value(
    transaction: &mut Transaction,
    sequence: &str,
    counter: &Counter,
    key: &str,
    advance: bool,
) -> Result<Option<i64>, StatementError> {
    let binds = || {
        vec![
            SqlValue::Text(sequence.to_owned()),
            SqlValue::Text(counter.name().to_owned()),
            SqlValue::Text(key.to_owned()),
        ]
    };
    let per = matches!(counter.policy(), Policy::Per(_));
    let sql = if advance && !per {
        "SELECT platform.bump_counter($1::text::uuid, $2, $3)"
    } else {
        COUNTER_SQL
    };
    let rows = transaction.query(sql, binds()).await?;
    let value = rows
        .rows
        .into_iter()
        .next()
        .map(|row| column::<i64>(&mut row.into_iter()))
        .transpose()?;
    match (advance, value) {
        (true, None) => {
            let rows = transaction
                .query(
                    "SELECT platform.assign_counter($1::text::uuid, $2, $3)",
                    binds(),
                )
                .await?;
            let row = rows.rows.into_iter().next().ok_or_else(|| {
                StatementError::invalid_result("number sequence", "missing counter value")
            })?;
            Ok(Some(column(&mut row.into_iter())?))
        }
        (false, value) if !per => Ok(value.filter(|next| *next > 1).map(|next| next - 1)),
        (_, value) => Ok(value),
    }
}

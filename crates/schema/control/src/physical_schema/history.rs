use super::{NAME_BYTE_LIMIT, PhysicalSchemaError, RelationColumn};
use std::fmt::Write as _;

/// The `CREATE OR REPLACE FUNCTION` text of the stamp function of one relation.
///
/// `stamped` lists the selected stamp columns, and `others` every other column.
/// An insert takes fresh stamps. A changed row keeps the created pair and moves
/// the updated pair. A true no-op keeps every old stamp. The change test casts
/// each other column to text, so a change of numeric scale alone is a change,
/// and a type without `=` compares too.
pub fn stamp_function_sql(name: &str, stamped: &[&str], others: &[&str]) -> String {
    let assign = |source: fn(&str) -> String| {
        stamped.iter().fold(String::new(), |mut text, column| {
            writeln!(
                text,
                "        NEW.{} := {};",
                postgres_protocol::escape::escape_identifier(column),
                source(column)
            )
            .expect("writing to a String cannot fail");
            text
        })
    };
    let fresh = |column: &str| match column {
        "created_by" | "updated_by" => "actor".to_owned(),
        _ => "transaction_timestamp()".to_owned(),
    };
    let moved = |column: &str| match column {
        "updated_at" => "transaction_timestamp()".to_owned(),
        "updated_by" => "actor".to_owned(),
        _ => format!(
            "OLD.{}",
            postgres_protocol::escape::escape_identifier(column)
        ),
    };
    let kept = |column: &str| {
        format!(
            "OLD.{}",
            postgres_protocol::escape::escape_identifier(column)
        )
    };
    let insert = assign(fresh);
    let changed_assign = assign(moved);
    let unchanged = assign(kept);
    let changed = changed_test(others);
    format!(
        "CREATE OR REPLACE FUNCTION wamn_history.{name}()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $stamp$
DECLARE
    actor uuid := NULLIF(current_setting('app.user_id', true), '')::uuid;
BEGIN
    IF actor IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'actor-required';
    END IF;
    IF TG_OP = 'INSERT' THEN
{insert}    ELSIF {changed} THEN
{changed_assign}    ELSE
{unchanged}    END IF;
    RETURN NEW;
END
$stamp$;
REVOKE ALL ON FUNCTION wamn_history.{name}() FROM PUBLIC;"
    )
}

/// The `CREATE OR REPLACE FUNCTION` text of the log function of one relation.
///
/// The entry keys the row by its primary key columns. A relation with no
/// primary key raises `history-key-required`. An update under the same key
/// records the old and the new value of each changed column. An insert, a
/// delete, and a primary key change record the full row. The images keep the
/// `to_jsonb` spelling, and the readers canonicalize it. A key column of type
/// `timestamptz` keeps the canonical spelling, so every entry of a row carries
/// one key text.
pub fn log_function_sql(name: &str, history: &str, columns: &[RelationColumn]) -> String {
    let all = columns
        .iter()
        .map(|column| column.name.as_str())
        .collect::<Vec<_>>();
    let keys = columns
        .iter()
        .filter(|column| column.primary_key)
        .collect::<Vec<_>>();
    let no_key = if keys.is_empty() {
        "    RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'history-key-required';\n"
    } else {
        ""
    };
    let key = |row: &str| {
        let members = keys
            .iter()
            .map(|column| {
                let value = format!(
                    "{row}.{}",
                    postgres_protocol::escape::escape_identifier(&column.name)
                );
                let value = if column.timestamptz {
                    format!("wamn_history.timestamptz_image({value})")
                } else {
                    value
                };
                format!(
                    "{}, {value}",
                    postgres_protocol::escape::escape_literal(&column.name)
                )
            })
            .collect::<Vec<_>>();
        format!("jsonb_build_object({})", members.join(", "))
    };
    let same_key = if keys.is_empty() {
        "true".to_owned()
    } else {
        keys.iter()
            .map(|column| {
                let column = postgres_protocol::escape::escape_identifier(&column.name);
                format!("to_jsonb(OLD.{column}) = to_jsonb(NEW.{column})")
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let diff = all.iter().fold(String::new(), |mut text, column| {
        let name = postgres_protocol::escape::escape_literal(column);
        let column = postgres_protocol::escape::escape_identifier(column);
        writeln!(
            text,
            "            IF NEW.{column}::text IS DISTINCT FROM OLD.{column}::text THEN
                changed_before := changed_before || jsonb_build_object({name}, OLD.{column});
                changed_after := changed_after || jsonb_build_object({name}, NEW.{column});
            END IF;"
        )
        .expect("writing to a String cannot fail");
        text
    });
    let changed = changed_test(&all);
    let (old_key, new_key) = (key("OLD"), key("NEW"));
    let insert = format!(
        "INSERT INTO {history} (row_key, type, operation, changed_by, changed_at, \
         transaction_id, before, after)"
    );
    format!(
        "CREATE OR REPLACE FUNCTION wamn_history.{name}()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $log$
DECLARE
    actor uuid := NULLIF(current_setting('app.user_id', true), '')::uuid;
    operation text := NULLIF(current_setting('app.operation', true), '');
    changed_before jsonb := '{{}}';
    changed_after jsonb := '{{}}';
BEGIN
    IF actor IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'actor-required';
    END IF;
    IF operation IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'operation-required';
    END IF;
{no_key}    IF TG_OP = 'UPDATE' THEN
        -- A true no-op writes no entry.
        IF NOT {changed} THEN
            RETURN NULL;
        END IF;
        IF {same_key} THEN
{diff}            {insert}
            VALUES ({new_key}, 'update', operation, actor, transaction_timestamp(),
                    pg_current_xact_id()::text::bigint, changed_before, changed_after);
            RETURN NULL;
        END IF;
    END IF;
    IF TG_OP <> 'INSERT' THEN
        {insert}
        VALUES ({old_key}, 'delete', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, to_jsonb(OLD), '{{}}');
    END IF;
    IF TG_OP <> 'DELETE' THEN
        {insert}
        VALUES ({new_key}, 'insert', operation, actor, transaction_timestamp(),
                pg_current_xact_id()::text::bigint, '{{}}', to_jsonb(NEW));
    END IF;
    RETURN NULL;
END
$log$;
REVOKE ALL ON FUNCTION wamn_history.{name}() FROM PUBLIC;"
    )
}

/// Whether any of `columns` differs between `OLD` and `NEW`, as SQL text.
pub fn changed_test(columns: &[&str]) -> String {
    if columns.is_empty() {
        return "false".to_owned();
    }
    let tests = columns
        .iter()
        .map(|column| {
            let column = postgres_protocol::escape::escape_identifier(column);
            format!("NEW.{column}::text IS DISTINCT FROM OLD.{column}::text")
        })
        .collect::<Vec<_>>();
    format!("({})", tests.join("\n            OR "))
}

/// The body that PostgreSQL stores for a generated function: the text between
/// its two dollar-quote tags.
pub fn function_body<'a>(statement: &'a str, tag: &str) -> &'a str {
    let start = statement.find(tag).map_or(0, |start| start + tag.len());
    let end = statement[start..]
        .find(tag)
        .map_or(statement.len(), |end| start + end);
    &statement[start..end]
}

/// The name of the generated function of one relation, refused at 64 bytes or more.
pub fn generated_function_name(
    schema: &str,
    relation: &str,
    suffix: &str,
) -> Result<String, PhysicalSchemaError> {
    let name = format!("{schema}__{relation}_{suffix}");
    if name.len() >= NAME_BYTE_LIMIT {
        return Err(PhysicalSchemaError::invalid(format!(
            "record-history-function-name-too-long: wamn_history.{name} has {} bytes; limit is {}",
            name.len(),
            NAME_BYTE_LIMIT - 1
        )));
    }
    Ok(name)
}

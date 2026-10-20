//! The state of one row at a per-row position, from the rows of a history read.
//!
//! A history read returns the entries of one row in ascending position order.
//! Each result row carries one entry, the current row image, and the head
//! position. [`state_at`] folds those rows backward from the current row. A
//! row that ends with a delete folds backward from the `before` image of that
//! delete.
//!
//! An image is the JSONB text that `wamn_history.row_image` renders. The fold
//! keeps each column value as raw JSON text and replaces whole values, so
//! numeric scale and every other spelling stay as the database holds them.
//!
//! Every position before the oldest retained entry is unavailable. A read with
//! no rows is unavailable at every position. The fold does not compare the
//! current row with the newest entry, so it does not see a change that the log
//! did not record.
//!
//! The crate also holds the names of the record history platform fixtures.
//! Every component recognizes a fixture by these names.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

/// The suffix of every history table name. Naming reserves it.
pub const HISTORY_TABLE_SUFFIX: &str = "_history";

/// The trigger that stamps the selected columns of a relation.
pub const STAMP_TRIGGER: &str = "wamn_record_history_stamp";

/// The trigger that writes one history entry for each changed row of a logged relation.
pub const LOG_TRIGGER: &str = "wamn_record_history_log";

/// The NOLOGIN role that holds the history retention grants of record history.
pub const AUDIT_RETENTION_ROLE: &str = "wamn_audit_retention";

/// The retention of a relation that keeps no log.
pub const NO_LOG_RETENTION: &str = "none";

/// The identity column that orders the entries of one row.
pub const POSITION_COLUMN: &str = "position";

/// The column that names one entry. PostgreSQL generates a random UUID for it.
pub const ENTRY_ID_COLUMN: &str = "id";

/// The history table columns in creation order, each with its schema column type.
///
/// The history table of a package relation has no `tenant_id` column. Every
/// column is `NOT NULL`. The log trigger writes every column except
/// [`POSITION_COLUMN`] and [`ENTRY_ID_COLUMN`], which PostgreSQL generates.
pub const HISTORY_COLUMNS: [(&str, &str); 10] = [
    (POSITION_COLUMN, "int64"),
    (ENTRY_ID_COLUMN, "uuid"),
    ("row_key", "json"),
    ("type", "text"),
    ("operation", "text"),
    ("changed_by", "uuid"),
    ("changed_at", "timestamptz"),
    ("transaction_id", "int64"),
    ("before", "json"),
    ("after", "json"),
];

/// PostgreSQL truncates an object name of 64 bytes or more.
const NAME_BYTE_LIMIT: usize = 64;

/// The history table name of a relation.
pub fn history_table_name(relation: &str) -> String {
    format!("{relation}{HISTORY_TABLE_SUFFIX}")
}

/// Whether a relation name ends with the reserved history suffix.
pub fn is_history_table_name(name: &str) -> bool {
    name.ends_with(HISTORY_TABLE_SUFFIX)
}

/// Whether every object name that the history table of `relation` derives fits in a PostgreSQL name.
///
/// The names are the NOT NULL constraint of each column, the primary key, the
/// CHECK constraints of `type` and `operation`, and the identity sequence.
pub fn history_object_names_fit(relation: &str) -> bool {
    let history = history_table_name(relation);
    HISTORY_COLUMNS
        .iter()
        .map(|(column, _)| format!("{history}_{column}_not_null"))
        .chain([
            format!("{history}_pkey"),
            format!("{history}_type_check"),
            format!("{history}_operation_check"),
            format!("{history}_{POSITION_COLUMN}_seq"),
        ])
        .all(|name| name.len() < NAME_BYTE_LIMIT)
}

/// Whether `value` is the retention of a relation that keeps a log.
///
/// The value is `unlimited` or `P<n>D`, where n is a positive whole number of
/// days with no leading zero.
pub fn is_log_retention(value: &str) -> bool {
    if value == "unlimited" {
        return true;
    }
    value
        .strip_prefix('P')
        .and_then(|rest| rest.strip_suffix('D'))
        .is_some_and(|days| {
            days.bytes().next().is_some_and(|first| first != b'0')
                && days.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// One result row of a history read, with the fields that the fold reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryRow<'a> {
    /// The per-row position of the entry.
    pub position: i64,
    /// The entry type: `insert`, `update`, or `delete`.
    pub type_: &'a str,
    /// The JSONB text of the entry `before` image.
    pub before: &'a str,
    /// The JSONB text of the current row image, `{}` for a deleted row.
    pub current: &'a str,
    /// The newest position of the row when the read ran.
    pub head_position: i64,
}

/// The state of a row at a per-row position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowState {
    /// The row exists with this image.
    Present(RowImage),
    /// No row exists under the key.
    Absent,
    /// The retained entries cannot show the state.
    Unavailable,
}

/// A row image: each column name with its raw JSON value text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowImage {
    columns: BTreeMap<String, String>,
}

impl RowImage {
    /// Each column name and its raw JSON value text, in column name order.
    pub fn columns(&self) -> impl Iterator<Item = (&str, &str)> {
        self.columns
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }
}

/// The reason that the fold refuses the rows of a history read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldErrorType {
    /// The rows carry different head positions, so they come from different reads of a changed row.
    HeadMismatch,
    /// The positions of the rows do not rise.
    PositionOrder,
    /// The newest row is not at the head position, so pages are missing.
    IncompleteRead,
    /// A row carries a kind other than `insert`, `update`, or `delete`.
    UnknownKind,
    /// An image is not a JSON object.
    MalformedImage,
    /// An entry does not follow from the state after it.
    BrokenChain,
}

/// A history read that the fold refuses, with the position of the row at fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldError {
    type_: FoldErrorType,
    position: i64,
}

impl FoldError {
    const fn new(kind: FoldErrorType, position: i64) -> Self {
        Self {
            type_: kind,
            position,
        }
    }

    /// The reason for the refusal.
    pub const fn error_type(&self) -> FoldErrorType {
        self.type_
    }

    /// The position of the row at fault.
    pub const fn position(&self) -> i64 {
        self.position
    }
}

impl fmt::Display for FoldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the history read cannot fold at position {}: {:?}",
            self.position, self.type_
        )
    }
}

impl Error for FoldError {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Insert,
    Update,
    Delete,
}

/// The state of the row after the newest entry at or before `position`.
///
/// `rows` holds every row of every page of one history read, in page order.
/// The fold refuses rows whose head positions differ, whose positions do not
/// rise, or whose newest position is not the head position.
pub fn state_at(rows: &[HistoryRow<'_>], position: i64) -> Result<RowState, FoldError> {
    let (Some(oldest), Some(newest)) = (rows.first(), rows.last()) else {
        return Ok(RowState::Unavailable);
    };
    let mut kinds = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        if row.head_position != newest.head_position {
            return Err(FoldError::new(FoldErrorType::HeadMismatch, row.position));
        }
        if index > 0 && rows[index - 1].position >= row.position {
            return Err(FoldError::new(FoldErrorType::PositionOrder, row.position));
        }
        kinds.push(row_type(row)?);
    }
    if newest.position != newest.head_position {
        return Err(FoldError::new(
            FoldErrorType::IncompleteRead,
            newest.position,
        ));
    }
    if position < oldest.position {
        return Ok(RowState::Unavailable);
    }

    let mut state = match kinds.last() {
        Some(Kind::Delete) => None,
        _ => Some(image(newest.current, newest.position)?),
    };
    for (row, kind) in rows.iter().zip(kinds).rev() {
        if row.position <= position {
            break;
        }
        state = match (kind, state) {
            (Kind::Insert, Some(_)) => None,
            (Kind::Delete, None) => Some(image(row.before, row.position)?),
            (Kind::Update, Some(mut after)) => {
                after
                    .columns
                    .extend(image(row.before, row.position)?.columns);
                Some(after)
            }
            _ => return Err(FoldError::new(FoldErrorType::BrokenChain, row.position)),
        };
    }
    Ok(state.map_or(RowState::Absent, RowState::Present))
}

fn row_type(row: &HistoryRow<'_>) -> Result<Kind, FoldError> {
    match row.type_ {
        "insert" => Ok(Kind::Insert),
        "update" => Ok(Kind::Update),
        "delete" => Ok(Kind::Delete),
        _ => Err(FoldError::new(FoldErrorType::UnknownKind, row.position)),
    }
}

/// The image text with only the named top-level columns, or `None` when the
/// text is not one JSON object.
///
/// Each kept member keeps its raw name and value text in image order, so every
/// value keeps the spelling that the database holds. For the text of a JSONB
/// object, the result is the text of that object without the other columns. A
/// history read uses it to return only the columns that its projection
/// declares.
///
/// Each value of a column in `timestamptz` takes the canonical spelling: UTC
/// RFC 3339 with exactly six fractional digits and a `Z`, as
/// `wamn_history.timestamptz_image` spells it. A package entry keeps the
/// `to_jsonb` spelling with an offset, and an entry written before
/// `wamn-c8a5v.2` keeps the canonical one, so a reader sees one spelling for
/// both. A value in neither spelling, such as `infinity`, stays as it is.
pub fn retain_columns(text: &str, columns: &[&str], timestamptz: &[&str]) -> Option<String> {
    let mut kept = Vec::new();
    members(text, |name, member, value| {
        if columns.contains(&name.as_str()) {
            let canonical = timestamptz
                .contains(&name.as_str())
                .then(|| canonical_timestamptz(value))
                .flatten();
            kept.push(match canonical {
                Some(canonical) => format!("{}{canonical}", &member[..member.len() - value.len()]),
                None => member.to_owned(),
            });
        }
    })?;
    Some(format!("{{{}}}", kept.join(", ")))
}

/// The canonical spelling of one JSON string that holds a `timestamptz` in
/// RFC 3339 form, with a `Z` or a numeric offset, or `None`.
fn canonical_timestamptz(value: &str) -> Option<String> {
    let text = value.strip_prefix('"')?.strip_suffix('"')?;
    let (date, rest) = text.split_once('T')?;
    let mut date_parts = date.splitn(3, '-');
    let year = digits(date_parts.next()?, 4, 6)?;
    let month = digits(date_parts.next()?, 2, 2)?;
    let day = digits(date_parts.next()?, 2, 2)?;
    let clock = rest.get(..8)?;
    let (hour, minute, second) = (
        digits(clock.get(..2)?, 2, 2)?,
        digits(clock.get(3..5)?, 2, 2)?,
        digits(clock.get(6..8)?, 2, 2)?,
    );
    if clock.as_bytes()[2] != b':' || clock.as_bytes()[5] != b':' {
        return None;
    }
    let mut rest = &rest[8..];
    let mut micros = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let length = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if !(1..=6).contains(&length) {
            return None;
        }
        micros = digits(&fraction[..length], 1, 6)? * 10_i64.pow(u32::try_from(6 - length).ok()?);
        rest = &fraction[length..];
    }
    let offset = match rest.as_bytes().first()? {
        b'Z' if rest.len() == 1 => 0,
        sign @ (b'+' | b'-') => {
            let mut parts = rest[1..].split(':');
            let hours = digits(parts.next()?, 2, 2)?;
            let minutes = parts.next().map_or(Some(0), |part| digits(part, 2, 2))?;
            let seconds = parts.next().map_or(Some(0), |part| digits(part, 2, 2))?;
            if parts.next().is_some() {
                return None;
            }
            let offset = hours * 3600 + minutes * 60 + seconds;
            if *sign == b'-' { -offset } else { offset }
        }
        _ => return None,
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let seconds =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    let (days, clock) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    if year < 1 {
        return None;
    }
    Some(format!(
        "\"{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{micros:06}Z\"",
        clock / 3600,
        clock % 3600 / 60,
        clock % 60
    ))
}

/// The value of an ASCII decimal text of `min` to `max` digits.
fn digits(text: &str, min: usize, max: usize) -> Option<i64> {
    ((min..=max).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Split the top-level members of one JSON object without parsing the values.
fn image(text: &str, position: i64) -> Result<RowImage, FoldError> {
    let mut columns = BTreeMap::new();
    members(text, |name, _, value| {
        columns.insert(name, value.to_owned());
    })
    .ok_or(FoldError::new(FoldErrorType::MalformedImage, position))?;
    Ok(RowImage { columns })
}

/// Visit the decoded name, the raw member text, and the raw value text of each
/// top-level member of one JSON object. The member text runs from the opening
/// quote of the name to the end of the value.
fn members<'a>(text: &'a str, mut visit: impl FnMut(String, &'a str, &'a str)) -> Option<()> {
    let bytes = text.as_bytes();
    let mut cursor = space(bytes, 0);
    if bytes.get(cursor) != Some(&b'{') {
        return None;
    }
    cursor = space(bytes, cursor + 1);
    if bytes.get(cursor) == Some(&b'}') {
        cursor += 1;
    } else {
        loop {
            let member = cursor;
            let (name, end) = string(text, cursor)?;
            cursor = space(bytes, end);
            if bytes.get(cursor) != Some(&b':') {
                return None;
            }
            let start = space(bytes, cursor + 1);
            let end = value_end(bytes, start)?;
            visit(name, &text[member..end], &text[start..end]);
            cursor = space(bytes, end);
            match bytes.get(cursor) {
                Some(b',') => cursor = space(bytes, cursor + 1),
                Some(b'}') => {
                    cursor += 1;
                    break;
                }
                _ => return None,
            }
        }
    }
    (space(bytes, cursor) == bytes.len()).then_some(())
}

fn space(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
    {
        cursor += 1;
    }
    cursor
}

/// The decoded JSON string at `start`, and the offset after its closing quote.
fn string(text: &str, start: usize) -> Option<(String, usize)> {
    if text.as_bytes().get(start) != Some(&b'"') {
        return None;
    }
    let mut decoded = String::new();
    let mut chars = text[start + 1..].char_indices();
    while let Some((offset, character)) = chars.next() {
        let escaped = match character {
            '"' => return Some((decoded, start + offset + 2)),
            '\\' => chars.next()?.1,
            character => {
                decoded.push(character);
                continue;
            }
        };
        decoded.push(match escaped {
            '"' | '\\' | '/' => escaped,
            'b' => '\u{8}',
            'f' => '\u{c}',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'u' => {
                let mut units = vec![hex_unit(&mut chars)?];
                if (0xD800..0xDC00).contains(&units[0]) {
                    if chars.next()?.1 != '\\' || chars.next()?.1 != 'u' {
                        return None;
                    }
                    units.push(hex_unit(&mut chars)?);
                }
                char::decode_utf16(units).next()?.ok()?
            }
            _ => return None,
        });
    }
    None
}

fn hex_unit(chars: &mut std::str::CharIndices<'_>) -> Option<u16> {
    let digits = chars.take(4).map(|(_, digit)| digit).collect::<String>();
    if digits.len() != 4 || !digits.bytes().all(|digit| digit.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(&digits, 16).ok()
}

/// The offset after the JSON value that starts at `start`.
fn value_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut cursor = start;
    while let Some(&byte) = bytes.get(cursor) {
        cursor += 1;
        if in_string {
            match byte {
                b'\\' => cursor += 1,
                b'"' => {
                    in_string = false;
                    if depth == 0 {
                        return Some(cursor);
                    }
                }
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return Some(cursor);
                }
            }
            b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r' if depth == 0 => {
                return (cursor - 1 > start).then_some(cursor - 1);
            }
            _ => {}
        }
    }
    (depth == 0 && !in_string && cursor > start).then_some(cursor)
}

#[cfg(test)]
mod tests {
    use super::{canonical_timestamptz, history_object_names_fit, is_log_retention};

    #[test]
    fn a_timestamptz_takes_the_canonical_spelling_from_either_spelling() {
        for (stored, canonical) in [
            (
                r#""2026-10-20T13:32:00.621907+00:00""#,
                r#""2026-10-20T13:32:00.621907Z""#,
            ),
            (
                r#""2026-10-20T13:32:00.621907Z""#,
                r#""2026-10-20T13:32:00.621907Z""#,
            ),
            (
                r#""2026-10-20T09:32:00.6219-04:00""#,
                r#""2026-10-20T13:32:00.621900Z""#,
            ),
            (
                r#""2026-10-20T00:30:00+05:30""#,
                r#""2026-10-19T19:00:00.000000Z""#,
            ),
            (
                r#""2024-02-29T23:00:00-02:00""#,
                r#""2024-03-01T01:00:00.000000Z""#,
            ),
            (
                r#""1999-12-31T23:59:59.5-00:00:30""#,
                r#""2000-01-01T00:00:29.500000Z""#,
            ),
            (
                r#""0001-01-01T00:00:00+00:00""#,
                r#""0001-01-01T00:00:00.000000Z""#,
            ),
        ] {
            assert_eq!(
                canonical_timestamptz(stored).as_deref(),
                Some(canonical),
                "{stored}"
            );
        }
        for kept in [
            r#""infinity""#,
            r#""-infinity""#,
            r#""2026-10-20 13:32:00+00""#,
            r#""2026-10-20T13:32:00""#,
            r#""2026-10-20T13:32:00.1234567Z""#,
            r#""0044-03-15T12:00:00+00:00 BC""#,
            r#""0001-01-01T00:00:00+01:00""#,
            "null",
            "1",
        ] {
            assert_eq!(canonical_timestamptz(kept), None, "{kept}");
        }
    }

    #[test]
    fn log_retention_accepts_unlimited_and_whole_days_only() {
        for accepted in ["unlimited", "P1D", "P30D", "P90D", "P365D", "P1000D"] {
            assert!(is_log_retention(accepted), "{accepted}");
        }
        for refused in [
            "none",
            "",
            "P",
            "PD",
            "P0D",
            "P01D",
            "P-1D",
            "P+1D",
            "P1.5D",
            "P1W",
            "P1M",
            "P1Y",
            "PT1H",
            "P1DT1H",
            "p30d",
            "P30d",
            " P30D",
            "P30D ",
            "UNLIMITED",
        ] {
            assert!(!is_log_retention(refused), "{refused}");
        }
    }

    #[test]
    fn a_relation_name_of_32_bytes_overflows_its_history_names() {
        assert!(history_object_names_fit(&"r".repeat(31)));
        assert!(!history_object_names_fit(&"r".repeat(32)));
    }
}

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

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

/// One result row of a history read, with the fields that the fold reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryRow<'a> {
    /// The per-row position of the entry.
    pub position: i64,
    /// The entry kind: `insert`, `update`, or `delete`.
    pub kind: &'a str,
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
pub enum FoldErrorKind {
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
    kind: FoldErrorKind,
    position: i64,
}

impl FoldError {
    const fn new(kind: FoldErrorKind, position: i64) -> Self {
        Self { kind, position }
    }

    /// The reason for the refusal.
    pub const fn kind(&self) -> FoldErrorKind {
        self.kind
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
            self.position, self.kind
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
            return Err(FoldError::new(FoldErrorKind::HeadMismatch, row.position));
        }
        if index > 0 && rows[index - 1].position >= row.position {
            return Err(FoldError::new(FoldErrorKind::PositionOrder, row.position));
        }
        kinds.push(kind(row)?);
    }
    if newest.position != newest.head_position {
        return Err(FoldError::new(
            FoldErrorKind::IncompleteRead,
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
            _ => return Err(FoldError::new(FoldErrorKind::BrokenChain, row.position)),
        };
    }
    Ok(state.map_or(RowState::Absent, RowState::Present))
}

fn kind(row: &HistoryRow<'_>) -> Result<Kind, FoldError> {
    match row.kind {
        "insert" => Ok(Kind::Insert),
        "update" => Ok(Kind::Update),
        "delete" => Ok(Kind::Delete),
        _ => Err(FoldError::new(FoldErrorKind::UnknownKind, row.position)),
    }
}

/// Split the top-level members of one JSON object without parsing the values.
fn image(text: &str, position: i64) -> Result<RowImage, FoldError> {
    members(text).ok_or(FoldError::new(FoldErrorKind::MalformedImage, position))
}

fn members(text: &str) -> Option<RowImage> {
    let bytes = text.as_bytes();
    let mut cursor = space(bytes, 0);
    if bytes.get(cursor) != Some(&b'{') {
        return None;
    }
    cursor = space(bytes, cursor + 1);
    let mut columns = BTreeMap::new();
    if bytes.get(cursor) == Some(&b'}') {
        cursor += 1;
    } else {
        loop {
            let (name, end) = string(text, cursor)?;
            cursor = space(bytes, end);
            if bytes.get(cursor) != Some(&b':') {
                return None;
            }
            let start = space(bytes, cursor + 1);
            let end = value_end(bytes, start)?;
            columns.insert(name, text[start..end].to_owned());
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
    (space(bytes, cursor) == bytes.len()).then_some(RowImage { columns })
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

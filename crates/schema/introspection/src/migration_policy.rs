//! Pre-apply validation for package-owned PostgreSQL migration artifacts.
//!
//! The validator intentionally admits only the DDL shape currently demanded by
//! Receiving: schema-qualified ordinary `CREATE TABLE` statements. PostgreSQL
//! remains responsible for parsing table bodies, while post-apply catalog
//! introspection validates their resulting objects. This layer refuses
//! statement operations that the final catalog state cannot prove.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Stable internal class for a refused migration artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationPolicyErrorKind {
    NotSqlArtifact,
    ArtifactRead,
    EmptyArtifact,
    InvalidSql,
    SetRole,
    RoleOperation,
    GrantOperation,
    NontransactionalOperation,
    CrossSchemaMutation,
    UnnamedConstraint,
    ConstraintNameTooLong,
    RuledOperation,
    UnsupportedStatement,
}

/// Contextual refusal from the migration-artifact boundary.
#[derive(Debug)]
pub struct MigrationPolicyError {
    kind: MigrationPolicyErrorKind,
    path: PathBuf,
    statement_index: Option<usize>,
    detail: Box<str>,
    source: Option<io::Error>,
}

impl MigrationPolicyError {
    /// Stable class for callers that must not match display text.
    pub const fn kind(&self) -> MigrationPolicyErrorKind {
        self.kind
    }

    /// SQL artifact that was refused.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// One-based statement index when the refusal belongs to a statement.
    pub const fn statement_index(&self) -> Option<usize> {
        self.statement_index
    }
}

impl fmt::Display for MigrationPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "migration {}", self.path.display())?;
        if let Some(statement_index) = self.statement_index {
            write!(formatter, " statement {statement_index}")?;
        }
        write!(formatter, " refused ({:?}): {}", self.kind, self.detail)
    }
}

impl std::error::Error for MigrationPolicyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|source| source as _)
    }
}

/// Validate one SQL migration artifact before PostgreSQL executes it.
///
/// Reading by path is deliberate: callers cannot accidentally validate a Rust
/// source string or a generated Rust wrapper instead of the shipped migration.
pub fn validate_migration_file(
    path: impl AsRef<Path>,
    configured_schema: &str,
) -> Result<(), MigrationPolicyError> {
    let path = path.as_ref();
    if path.extension() != Some(OsStr::new("sql")) {
        return Err(policy_error(
            MigrationPolicyErrorKind::NotSqlArtifact,
            path,
            None,
            "migration artifacts must have the .sql extension",
        ));
    }

    let sql = fs::read_to_string(path).map_err(|source| MigrationPolicyError {
        kind: MigrationPolicyErrorKind::ArtifactRead,
        path: path.to_path_buf(),
        statement_index: None,
        detail: "could not read migration SQL as UTF-8".into(),
        source: Some(source),
    })?;
    let statements = lex_statements(&sql, path)?;
    if statements.is_empty() {
        return Err(policy_error(
            MigrationPolicyErrorKind::EmptyArtifact,
            path,
            None,
            "migration contains no SQL statement",
        ));
    }

    for (index, tokens) in statements.iter().enumerate() {
        validate_statement(tokens, configured_schema, path, index + 1)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token<'a> {
    Word(&'a str),
    QuotedIdentifier(&'a str),
    Opaque,
    Symbol(u8),
}

fn policy_error(
    kind: MigrationPolicyErrorKind,
    path: &Path,
    statement_index: Option<usize>,
    detail: impl Into<Box<str>>,
) -> MigrationPolicyError {
    MigrationPolicyError {
        kind,
        path: path.to_path_buf(),
        statement_index,
        detail: detail.into(),
        source: None,
    }
}

fn validate_statement(
    tokens: &[Token<'_>],
    configured_schema: &str,
    path: &Path,
    statement_index: usize,
) -> Result<(), MigrationPolicyError> {
    let refusal = if is_role_switch(tokens) {
        Some((
            MigrationPolicyErrorKind::SetRole,
            "session authorization changes are forbidden",
        ))
    } else if is_grant_operation(tokens) {
        Some((
            MigrationPolicyErrorKind::GrantOperation,
            "grant and privilege operations are forbidden",
        ))
    } else if is_role_operation(tokens) {
        Some((
            MigrationPolicyErrorKind::RoleOperation,
            "role operations are forbidden",
        ))
    } else if is_nontransactional_operation(tokens) {
        Some((
            MigrationPolicyErrorKind::NontransactionalOperation,
            "nontransactional SQL is forbidden",
        ))
    } else if is_ruled_operation(tokens) {
        Some((
            MigrationPolicyErrorKind::RuledOperation,
            "the statement operates on a refused object class",
        ))
    } else {
        None
    };
    if let Some((kind, detail)) = refusal {
        return refuse_statement(kind, path, statement_index, detail);
    }
    if words(tokens, 0, &["create", "table"]) {
        return validate_create_table(tokens, configured_schema, path, statement_index);
    }
    refuse_statement(
        MigrationPolicyErrorKind::UnsupportedStatement,
        path,
        statement_index,
        "only schema-qualified CREATE TABLE is admitted",
    )
}

fn validate_create_table(
    tokens: &[Token<'_>],
    configured_schema: &str,
    path: &Path,
    statement_index: usize,
) -> Result<(), MigrationPolicyError> {
    let (Some(Token::Word(schema_name)), Some(Token::Symbol(b'.')), Some(Token::Word(table_name))) = (
        tokens.get(2).copied(),
        tokens.get(3).copied(),
        tokens.get(4).copied(),
    ) else {
        return refuse_statement(
            MigrationPolicyErrorKind::CrossSchemaMutation,
            path,
            statement_index,
            "CREATE TABLE must use an unquoted schema-qualified target",
        );
    };
    if schema_name != configured_schema {
        return refuse_statement(
            MigrationPolicyErrorKind::CrossSchemaMutation,
            path,
            statement_index,
            format!("schema {schema_name:?} is outside configured schema {configured_schema:?}"),
        );
    }
    if !matches!(tokens.get(5), Some(Token::Symbol(b'('))) {
        return refuse_statement(
            MigrationPolicyErrorKind::UnsupportedStatement,
            path,
            statement_index,
            "CREATE TABLE must define an ordinary table body",
        );
    }

    let mut depth = 0_usize;
    let mut body_end = None;
    for (index, token) in tokens.iter().enumerate().skip(5) {
        match token {
            Token::Symbol(b'(') => depth += 1,
            Token::Symbol(b')') => {
                depth -= 1;
                if depth == 0 {
                    body_end = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }

    let Some(body_end) = body_end else {
        return refuse_statement(
            MigrationPolicyErrorKind::InvalidSql,
            path,
            statement_index,
            "unterminated CREATE TABLE body",
        );
    };
    if body_end == 6 {
        return refuse_statement(
            MigrationPolicyErrorKind::InvalidSql,
            path,
            statement_index,
            "CREATE TABLE body is empty",
        );
    }
    if contains_top_level_word(&tokens[6..body_end], "like") {
        return refuse_statement(
            MigrationPolicyErrorKind::UnsupportedStatement,
            path,
            statement_index,
            "CREATE TABLE LIKE is not admitted",
        );
    }
    validate_constraint_names(&tokens[6..body_end], table_name, path, statement_index)?;
    if body_end + 1 != tokens.len() {
        return refuse_statement(
            MigrationPolicyErrorKind::UnsupportedStatement,
            path,
            statement_index,
            "CREATE TABLE options outside the ordinary table body are not admitted",
        );
    }
    Ok(())
}

fn refuse_statement<T>(
    kind: MigrationPolicyErrorKind,
    path: &Path,
    statement_index: usize,
    detail: impl Into<Box<str>>,
) -> Result<T, MigrationPolicyError> {
    Err(policy_error(kind, path, Some(statement_index), detail))
}

fn is_role_switch(tokens: &[Token<'_>]) -> bool {
    words(tokens, 0, &["set", "role"])
        || words(tokens, 0, &["set", "local", "role"])
        || words(tokens, 0, &["set", "session", "role"])
        || words(tokens, 0, &["set", "session", "authorization"])
        || words(tokens, 0, &["reset", "role"])
        || words(tokens, 0, &["reset", "session", "authorization"])
}

fn is_grant_operation(tokens: &[Token<'_>]) -> bool {
    word(tokens, 0, "grant")
        || word(tokens, 0, "revoke")
        || words(tokens, 0, &["alter", "default", "privileges"])
}

fn is_role_operation(tokens: &[Token<'_>]) -> bool {
    (["create", "alter", "drop"]
        .into_iter()
        .any(|verb| word(tokens, 0, verb))
        && ["role", "user", "group"]
            .into_iter()
            .any(|object| word(tokens, 1, object)))
        || words(tokens, 0, &["reassign", "owned"])
        || words(tokens, 0, &["drop", "owned"])
        || words(tokens, 0, &["comment", "on", "role"])
        || words(tokens, 0, &["security", "label", "on", "role"])
}

fn is_nontransactional_operation(tokens: &[Token<'_>]) -> bool {
    word(tokens, 0, "vacuum")
        || word(tokens, 0, "cluster")
        || words(tokens, 0, &["alter", "system"])
        || (["create", "drop"]
            .into_iter()
            .any(|verb| word(tokens, 0, verb))
            && ["database", "tablespace"]
                .into_iter()
                .any(|object| word(tokens, 1, object)))
        || (words(tokens, 0, &["create", "index"]) && word(tokens, 2, "concurrently"))
        || (words(tokens, 0, &["create", "unique", "index"]) && word(tokens, 3, "concurrently"))
        || (words(tokens, 0, &["drop", "index"]) && word(tokens, 2, "concurrently"))
        || (word(tokens, 0, "reindex")
            && (contains_word(tokens, "concurrently")
                || word(tokens, 1, "database")
                || word(tokens, 1, "system")))
}

fn is_ruled_operation(tokens: &[Token<'_>]) -> bool {
    if word(tokens, 0, "do") {
        return true;
    }
    if words(tokens, 0, &["alter", "table"])
        && contains_words(tokens, &["row", "level", "security"])
    {
        return true;
    }

    let object_index = if words(tokens, 0, &["create", "or", "replace"]) {
        3
    } else if ["create", "alter", "drop"]
        .into_iter()
        .any(|verb| word(tokens, 0, verb))
    {
        1
    } else {
        return false;
    };

    [
        "extension",
        "function",
        "procedure",
        "trigger",
        "rule",
        "policy",
        "view",
        "language",
        "type",
        "domain",
    ]
    .into_iter()
    .any(|object| word(tokens, object_index, object))
        || words(tokens, object_index, &["event", "trigger"])
        || words(tokens, object_index, &["foreign", "table"])
        || words(tokens, object_index, &["materialized", "view"])
}

fn word(tokens: &[Token<'_>], index: usize, expected: &str) -> bool {
    matches!(tokens.get(index), Some(Token::Word(actual)) if actual.eq_ignore_ascii_case(expected))
}

fn words(tokens: &[Token<'_>], start: usize, expected: &[&str]) -> bool {
    expected
        .iter()
        .enumerate()
        .all(|(offset, expected)| word(tokens, start + offset, expected))
}

fn contains_word(tokens: &[Token<'_>], expected: &str) -> bool {
    tokens
        .iter()
        .any(|token| matches!(token, Token::Word(actual) if actual.eq_ignore_ascii_case(expected)))
}

fn contains_words(tokens: &[Token<'_>], expected: &[&str]) -> bool {
    tokens
        .windows(expected.len())
        .any(|window| words(window, 0, expected))
}

fn contains_top_level_word(tokens: &[Token<'_>], expected: &str) -> bool {
    let mut depth = 0_usize;
    for token in tokens {
        match token {
            Token::Symbol(b'(') => depth += 1,
            Token::Symbol(b')') => depth = depth.saturating_sub(1),
            Token::Word(actual) if depth == 0 && actual.eq_ignore_ascii_case(expected) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn validate_constraint_names(
    body: &[Token<'_>],
    table_name: &str,
    path: &Path,
    statement_index: usize,
) -> Result<(), MigrationPolicyError> {
    let mut segment_start = 0_usize;
    let mut depth = 0_usize;
    for index in 0..=body.len() {
        let at_segment_end = index == body.len()
            || (matches!(body.get(index), Some(Token::Symbol(b','))) && depth == 0);
        if at_segment_end {
            validate_segment_constraint_names(
                &body[segment_start..index],
                table_name,
                path,
                statement_index,
            )?;
            segment_start = index + 1;
            continue;
        }
        match body[index] {
            Token::Symbol(b'(') => depth += 1,
            Token::Symbol(b')') => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

fn validate_segment_constraint_names(
    segment: &[Token<'_>],
    table_name: &str,
    path: &Path,
    statement_index: usize,
) -> Result<(), MigrationPolicyError> {
    if let Some((name, byte_len)) = overlength_constraint_name(segment) {
        return refuse_statement(
            MigrationPolicyErrorKind::ConstraintNameTooLong,
            path,
            statement_index,
            format!(
                "table {table_name:?} constraint {name:?} is {byte_len} bytes; names must be shorter than 64 bytes"
            ),
        );
    }
    let has_table_foreign_key = top_level_words_at(segment, &["foreign", "key"]).is_some();
    for (constraint_kind, words) in [
        ("primary key", &["primary", "key"][..]),
        ("unique", &["unique"][..]),
        ("check", &["check"][..]),
        ("foreign key", &["foreign", "key"][..]),
    ] {
        if unnamed_top_level_constraint(segment, words).is_some() {
            return refuse_statement(
                MigrationPolicyErrorKind::UnnamedConstraint,
                path,
                statement_index,
                format!("table {table_name:?} has an unnamed {constraint_kind} constraint"),
            );
        }
    }
    if !has_table_foreign_key && unnamed_top_level_constraint(segment, &["references"]).is_some() {
        return refuse_statement(
            MigrationPolicyErrorKind::UnnamedConstraint,
            path,
            statement_index,
            format!("table {table_name:?} has an unnamed foreign key constraint"),
        );
    }
    Ok(())
}

fn overlength_constraint_name<'a>(tokens: &'a [Token<'a>]) -> Option<(&'a str, usize)> {
    let mut depth = 0_usize;
    for index in 0..tokens.len() {
        match tokens[index] {
            Token::Symbol(b'(') => depth += 1,
            Token::Symbol(b')') => depth = depth.saturating_sub(1),
            Token::Word(actual) if depth == 0 && actual.eq_ignore_ascii_case("constraint") => {
                let (name, byte_len) = match tokens.get(index + 1) {
                    Some(Token::Word(name)) => (*name, name.len()),
                    Some(Token::QuotedIdentifier(name)) => {
                        (*name, quoted_identifier_byte_len(name))
                    }
                    _ => continue,
                };
                if byte_len >= 64 {
                    return Some((name, byte_len));
                }
            }
            _ => {}
        }
    }
    None
}

fn quoted_identifier_byte_len(name: &str) -> usize {
    let bytes = name.as_bytes();
    let mut cursor = 0_usize;
    let mut byte_len = 0_usize;
    while cursor < bytes.len() {
        cursor += if bytes[cursor] == b'"' && bytes.get(cursor + 1) == Some(&b'"') {
            2
        } else {
            1
        };
        byte_len += 1;
    }
    byte_len
}

fn top_level_words_at(tokens: &[Token<'_>], expected: &[&str]) -> Option<usize> {
    let mut depth = 0_usize;
    for index in 0..tokens.len() {
        match tokens[index] {
            Token::Symbol(b'(') => depth += 1,
            Token::Symbol(b')') => depth = depth.saturating_sub(1),
            _ if depth == 0 && words(tokens, index, expected) => return Some(index),
            _ => {}
        }
    }
    None
}

fn unnamed_top_level_constraint(tokens: &[Token<'_>], expected: &[&str]) -> Option<usize> {
    let mut depth = 0_usize;
    for index in 0..tokens.len() {
        match tokens[index] {
            Token::Symbol(b'(') => depth += 1,
            Token::Symbol(b')') => depth = depth.saturating_sub(1),
            _ if depth == 0
                && words(tokens, index, expected)
                && !has_constraint_name(tokens, index) =>
            {
                return Some(index);
            }
            _ => {}
        }
    }
    None
}

fn has_constraint_name(tokens: &[Token<'_>], kind_index: usize) -> bool {
    kind_index >= 2
        && word(tokens, kind_index - 2, "constraint")
        && matches!(tokens[kind_index - 1], Token::Word(_))
}

fn lex_statements<'a>(
    sql: &'a str,
    path: &Path,
) -> Result<Vec<Vec<Token<'a>>>, MigrationPolicyError> {
    let bytes = sql.as_bytes();
    let mut cursor = 0_usize;
    let mut tokens = Vec::new();
    let mut statements = Vec::new();

    while cursor < bytes.len() {
        match bytes[cursor] {
            byte if byte.is_ascii_whitespace() => cursor += 1,
            b'-' if bytes.get(cursor + 1) == Some(&b'-') => {
                cursor += 2;
                while cursor < bytes.len() && bytes[cursor] != b'\n' {
                    cursor += 1;
                }
            }
            b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                cursor = scan_block_comment(bytes, cursor).ok_or_else(|| {
                    lexical_error(path, statements.len() + 1, "unterminated block comment")
                })?;
            }
            b'e' | b'E'
                if bytes.get(cursor + 1) == Some(&b'\'')
                    && (cursor == 0 || !is_word_continue(bytes[cursor - 1])) =>
            {
                cursor = scan_single_quote(bytes, cursor + 1, true).ok_or_else(|| {
                    lexical_error(path, statements.len() + 1, "unterminated escape string")
                })?;
                tokens.push(Token::Opaque);
            }
            b'\'' => {
                cursor = scan_single_quote(bytes, cursor, false).ok_or_else(|| {
                    lexical_error(path, statements.len() + 1, "unterminated quoted string")
                })?;
                tokens.push(Token::Opaque);
            }
            b'"' => {
                let start = cursor + 1;
                cursor = scan_double_quote(bytes, cursor).ok_or_else(|| {
                    lexical_error(path, statements.len() + 1, "unterminated quoted identifier")
                })?;
                tokens.push(Token::QuotedIdentifier(&sql[start..cursor - 1]));
            }
            b'$' if dollar_delimiter_end(bytes, cursor).is_some() => {
                cursor = scan_dollar_quote(sql, cursor).ok_or_else(|| {
                    lexical_error(
                        path,
                        statements.len() + 1,
                        "unterminated dollar-quoted string",
                    )
                })?;
                tokens.push(Token::Opaque);
            }
            b';' => {
                if !tokens.is_empty() {
                    statements.push(std::mem::take(&mut tokens));
                }
                cursor += 1;
            }
            byte if is_word_start(byte) => {
                let start = cursor;
                cursor += 1;
                while cursor < bytes.len() && is_word_continue(bytes[cursor]) {
                    cursor += 1;
                }
                tokens.push(Token::Word(&sql[start..cursor]));
            }
            symbol => {
                tokens.push(Token::Symbol(symbol));
                cursor += 1;
            }
        }
    }

    if !tokens.is_empty() {
        statements.push(tokens);
    }
    Ok(statements)
}

fn lexical_error(path: &Path, statement_index: usize, detail: &str) -> MigrationPolicyError {
    policy_error(
        MigrationPolicyErrorKind::InvalidSql,
        path,
        Some(statement_index),
        detail,
    )
}

fn scan_block_comment(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start + 2;
    let mut depth = 1_usize;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"/*") {
            depth += 1;
            cursor += 2;
        } else if bytes[cursor..].starts_with(b"*/") {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return Some(cursor);
            }
        } else {
            cursor += 1;
        }
    }
    None
}

fn scan_single_quote(bytes: &[u8], start: usize, backslash_escapes: bool) -> Option<usize> {
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        if backslash_escapes && bytes[cursor] == b'\\' {
            cursor += 2;
            continue;
        }
        if bytes[cursor] == b'\'' {
            if bytes.get(cursor + 1) == Some(&b'\'') {
                cursor += 2;
            } else {
                return Some(cursor + 1);
            }
        } else {
            cursor += 1;
        }
    }
    None
}

fn scan_double_quote(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"' {
            if bytes.get(cursor + 1) == Some(&b'"') {
                cursor += 2;
            } else {
                return Some(cursor + 1);
            }
        } else {
            cursor += 1;
        }
    }
    None
}

fn scan_dollar_quote(sql: &str, start: usize) -> Option<usize> {
    let delimiter_end = dollar_delimiter_end(sql.as_bytes(), start)?;
    let delimiter = &sql[start..delimiter_end];
    let body = &sql[delimiter_end..];
    body.find(delimiter)
        .map(|offset| delimiter_end + offset + delimiter.len())
}

fn dollar_delimiter_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start + 1;
    if bytes.get(cursor) == Some(&b'$') {
        return Some(cursor + 1);
    }
    if !bytes.get(cursor).is_some_and(|byte| is_word_start(*byte)) {
        return None;
    }
    cursor += 1;
    while bytes
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        cursor += 1;
    }
    (bytes.get(cursor) == Some(&b'$')).then_some(cursor + 1)
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_word_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

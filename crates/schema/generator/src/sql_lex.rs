//! Minimal SQL tokenization for package-corpus qualification checks.
//!
//! This is intentionally not a migration parser. It distinguishes identifiers
//! from comments and string bodies solely so corpus validation can refuse an
//! authored schema coordinate without matching inert text.

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Identifier(Box<str>),
    Dot,
    Other,
}

pub(crate) fn contains_schema_qualified_reference(sql: &[u8], schema: &str) -> bool {
    let tokens = tokens(sql);
    tokens.windows(3).any(|window| {
        matches!(
            window,
            [Token::Identifier(left), Token::Dot, Token::Identifier(_)] if left.as_ref() == schema
        )
    })
}

fn tokens(sql: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < sql.len() {
        match sql[cursor] {
            byte if byte.is_ascii_whitespace() => cursor += 1,
            b'-' if sql.get(cursor + 1) == Some(&b'-') => skip_line_comment(sql, &mut cursor),
            b'/' if sql.get(cursor + 1) == Some(&b'*') => skip_block_comment(sql, &mut cursor),
            b'\'' => skip_quoted(sql, &mut cursor, b'\''),
            b'"' => tokens.push(Token::Identifier(quoted_identifier(sql, &mut cursor))),
            b'$' if dollar_quote_end(sql, cursor).is_some() => skip_dollar_quote(sql, &mut cursor),
            b'.' => {
                tokens.push(Token::Dot);
                cursor += 1;
            }
            byte if identifier_start(byte) => {
                tokens.push(Token::Identifier(unquoted_identifier(sql, &mut cursor)));
            }
            _ => {
                tokens.push(Token::Other);
                cursor += 1;
            }
        }
    }
    tokens
}

fn skip_line_comment(sql: &[u8], cursor: &mut usize) {
    *cursor += 2;
    while *cursor < sql.len() && !matches!(sql[*cursor], b'\n' | b'\r') {
        *cursor += 1;
    }
}

fn skip_block_comment(sql: &[u8], cursor: &mut usize) {
    *cursor += 2;
    let mut depth = 1_u32;
    while *cursor < sql.len() && depth > 0 {
        if sql.get(*cursor..*cursor + 2) == Some(b"/*") {
            depth += 1;
            *cursor += 2;
        } else if sql.get(*cursor..*cursor + 2) == Some(b"*/") {
            depth -= 1;
            *cursor += 2;
        } else {
            *cursor += 1;
        }
    }
}

fn skip_quoted(sql: &[u8], cursor: &mut usize, quote: u8) {
    *cursor += 1;
    while *cursor < sql.len() {
        if sql[*cursor] != quote {
            *cursor += 1;
        } else if sql.get(*cursor + 1) == Some(&quote) {
            *cursor += 2;
        } else {
            *cursor += 1;
            return;
        }
    }
}

fn quoted_identifier(sql: &[u8], cursor: &mut usize) -> Box<str> {
    *cursor += 1;
    let mut identifier = String::new();
    while *cursor < sql.len() {
        if sql[*cursor] != b'"' {
            identifier.push(char::from(sql[*cursor]));
            *cursor += 1;
        } else if sql.get(*cursor + 1) == Some(&b'"') {
            identifier.push('"');
            *cursor += 2;
        } else {
            *cursor += 1;
            break;
        }
    }
    identifier.into_boxed_str()
}

fn dollar_quote_end(sql: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start + 1;
    while cursor < sql.len() && (sql[cursor].is_ascii_alphanumeric() || sql[cursor] == b'_') {
        cursor += 1;
    }
    (sql.get(cursor) == Some(&b'$')).then_some(cursor)
}

fn skip_dollar_quote(sql: &[u8], cursor: &mut usize) {
    let delimiter_end = dollar_quote_end(sql, *cursor).expect("caller identified dollar quote");
    let delimiter = &sql[*cursor..=delimiter_end];
    *cursor = delimiter_end + 1;
    while *cursor + delimiter.len() <= sql.len() {
        if &sql[*cursor..*cursor + delimiter.len()] == delimiter {
            *cursor += delimiter.len();
            return;
        }
        *cursor += 1;
    }
}

fn unquoted_identifier(sql: &[u8], cursor: &mut usize) -> Box<str> {
    let start = *cursor;
    *cursor += 1;
    while *cursor < sql.len() && identifier_continue(sql[*cursor]) {
        *cursor += 1;
    }
    String::from_utf8_lossy(&sql[start..*cursor])
        .to_ascii_lowercase()
        .into_boxed_str()
}

const fn identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

const fn identifier_continue(byte: u8) -> bool {
    identifier_start(byte) || byte.is_ascii_digit() || byte == b'$'
}

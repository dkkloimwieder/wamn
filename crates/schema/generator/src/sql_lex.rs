//! Minimal SQL tokenization for package-corpus qualification checks.
//!
//! This is intentionally not a migration parser. It distinguishes identifiers
//! from comments and string bodies so the corpus loader can validate the
//! closed SQL subset without matching inert text.

use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod grammar;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RelationAccess {
    pub(crate) select_fields: BTreeSet<String>,
    pub(crate) insert_fields: BTreeSet<String>,
    pub(crate) update_fields: BTreeSet<String>,
    pub(crate) lock: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Identifier(Box<str>),
    Dot,
    LeftParen,
    RightParen,
    Comma,
    Equals,
    Star,
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

/// Derive relation/column privileges from one authored command SQL artifact.
///
/// The parser intentionally admits only the finite statement forms emitted or
/// authored by the Receiving POC: SELECT, INSERT, UPDATE, CTEs, qualified
/// relation references, and explicit row-lock clauses. Bind and result shapes
/// remain owned by the two-sibling verifier; this pass joins the verified SQL
/// to the manifest's relation authority declaration.
pub(crate) fn relation_access(
    sql: &[u8],
    relations: &BTreeMap<String, BTreeSet<String>>,
) -> Result<BTreeMap<String, RelationAccess>, &'static str> {
    let tokens = tokens(sql);
    let depths = depths(&tokens)?;
    refuse_unsupported_effects(&tokens)?;
    let mut access = relations
        .keys()
        .map(|relation| (relation.clone(), RelationAccess::default()))
        .collect::<BTreeMap<_, _>>();
    let aliases = relation_aliases(&tokens, relations);
    let mut excluded_select_tokens = BTreeSet::new();

    for index in 0..tokens.len() {
        if identifier(&tokens[index]) == Some("insert") {
            parse_insert(
                &tokens,
                &depths,
                index,
                relations,
                &mut access,
                &mut excluded_select_tokens,
            )?;
        }
        if identifier(&tokens[index]) == Some("update") && !is_lock_update(&tokens, &depths, index)
        {
            parse_update(
                &tokens,
                &depths,
                index,
                relations,
                &aliases,
                &mut access,
                &mut excluded_select_tokens,
            )?;
        }
    }

    for index in 0..tokens.len().saturating_sub(2) {
        let Some(left) = identifier(&tokens[index]) else {
            continue;
        };
        if tokens[index + 1] != Token::Dot {
            continue;
        }
        let Some(field) = identifier(&tokens[index + 2]) else {
            continue;
        };
        let Some(relation) = aliases
            .get(left)
            .map(String::as_str)
            .or_else(|| relations.contains_key(left).then_some(left))
        else {
            continue;
        };
        if relations
            .get(relation)
            .is_some_and(|fields| fields.contains(field))
            && !excluded_select_tokens.contains(&(index + 2))
        {
            access
                .get_mut(relation)
                .expect("known relation has an access row")
                .select_fields
                .insert(field.to_owned());
        }
    }

    for index in 0..tokens.len() {
        if identifier(&tokens[index]) == Some("select") {
            parse_select(
                &tokens,
                &depths,
                index,
                relations,
                &aliases,
                &excluded_select_tokens,
                &mut access,
            )?;
        }
    }

    access.retain(|_, privileges| privileges != &RelationAccess::default());
    Ok(access)
}

fn depths(tokens: &[Token]) -> Result<Vec<usize>, &'static str> {
    let mut depth = 0_usize;
    let mut depths = Vec::with_capacity(tokens.len());
    for token in tokens {
        match token {
            Token::LeftParen => {
                depths.push(depth);
                depth += 1;
            }
            Token::RightParen => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("SQL has an unmatched closing parenthesis")?;
                depths.push(depth);
            }
            _ => depths.push(depth),
        }
    }
    (depth == 0)
        .then_some(depths)
        .ok_or("SQL has an unmatched opening parenthesis")
}

fn relation_aliases(
    tokens: &[Token],
    relations: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    for index in 0..tokens.len() {
        let Some(keyword) = identifier(&tokens[index]) else {
            continue;
        };
        if !matches!(keyword, "from" | "join" | "update" | "into") {
            continue;
        }
        let Some(relation) = tokens.get(index + 1).and_then(identifier) else {
            continue;
        };
        if !relations.contains_key(relation) {
            continue;
        }
        aliases.insert(relation.to_owned(), relation.to_owned());
        let alias_index = if tokens.get(index + 2).and_then(identifier) == Some("as") {
            index + 3
        } else {
            index + 2
        };
        if let Some(alias) = tokens.get(alias_index).and_then(identifier)
            && !is_keyword(alias)
        {
            aliases.insert(alias.to_owned(), relation.to_owned());
        }
    }
    aliases
}

fn parse_insert(
    tokens: &[Token],
    depths: &[usize],
    insert: usize,
    relations: &BTreeMap<String, BTreeSet<String>>,
    access: &mut BTreeMap<String, RelationAccess>,
    excluded: &mut BTreeSet<usize>,
) -> Result<(), &'static str> {
    if tokens.get(insert + 1).and_then(identifier) != Some("into") {
        return Err("INSERT must use INSERT INTO");
    }
    let relation_index = insert + 2;
    let relation = tokens
        .get(relation_index)
        .and_then(identifier)
        .ok_or("INSERT target is missing")?;
    let Some(fields) = relations.get(relation) else {
        return Err("INSERT references an undeclared relation");
    };
    if tokens.get(relation_index + 1) != Some(&Token::LeftParen) {
        return Err("INSERT must declare its target columns");
    }
    let list_depth = depths[relation_index + 1] + 1;
    let mut cursor = relation_index + 2;
    let mut closed = false;
    while cursor < tokens.len() {
        if tokens[cursor] == Token::RightParen && depths[cursor] + 1 == list_depth {
            closed = true;
            break;
        }
        if depths[cursor] == list_depth
            && let Some(field) = identifier(&tokens[cursor])
        {
            if !fields.contains(field) {
                return Err("INSERT names an unknown target column");
            }
            access
                .get_mut(relation)
                .expect("known relation has an access row")
                .insert_fields
                .insert(field.to_owned());
            excluded.insert(cursor);
        }
        cursor += 1;
    }
    if !closed {
        return Err("INSERT target column list is not closed");
    }
    parse_returning(tokens, depths, insert, relation, fields, access)?;
    Ok(())
}

fn parse_update(
    tokens: &[Token],
    depths: &[usize],
    update: usize,
    relations: &BTreeMap<String, BTreeSet<String>>,
    aliases: &BTreeMap<String, String>,
    access: &mut BTreeMap<String, RelationAccess>,
    excluded: &mut BTreeSet<usize>,
) -> Result<(), &'static str> {
    let relation = tokens
        .get(update + 1)
        .and_then(identifier)
        .ok_or("UPDATE target is missing")?;
    let Some(fields) = relations.get(relation) else {
        return Err("UPDATE references an undeclared relation");
    };
    let depth = depths[update];
    let end = statement_end(depths, update, depth);
    let set = (update + 2..end)
        .find(|index| depths[*index] == depth && identifier(&tokens[*index]) == Some("set"))
        .ok_or("UPDATE is missing SET")?;
    let set_end = (set + 1..end)
        .find(|index| {
            depths[*index] == depth
                && matches!(
                    identifier(&tokens[*index]),
                    Some("from" | "where" | "returning")
                )
        })
        .unwrap_or(end);
    if (set + 1..set_end).any(|index| {
        depths[index] == depth
            && tokens[index] == Token::LeftParen
            && (index == set + 1 || tokens.get(index - 1) == Some(&Token::Comma))
    }) {
        return Err("UPDATE tuple assignments are outside the admitted SQL subset");
    }
    let mut assignments = 0_usize;
    for index in set + 1..set_end.saturating_sub(1) {
        let assignment_head =
            index == set + 1 || (depths[index - 1] == depth && tokens[index - 1] == Token::Comma);
        if depths[index] != depth || !assignment_head || tokens[index + 1] != Token::Equals {
            continue;
        }
        let Some(field) = identifier(&tokens[index]) else {
            continue;
        };
        if !fields.contains(field) {
            return Err("UPDATE names an unknown target column");
        }
        access
            .get_mut(relation)
            .expect("known relation has an access row")
            .update_fields
            .insert(field.to_owned());
        excluded.insert(index);
        assignments += 1;
    }
    if assignments == 0 {
        return Err("UPDATE must use simple column assignments");
    }
    collect_unqualified_fields(
        tokens,
        depths,
        update,
        end,
        &[relation],
        relations,
        aliases,
        excluded,
        access,
    );
    parse_returning(tokens, depths, update, relation, fields, access)?;
    Ok(())
}

fn parse_returning(
    tokens: &[Token],
    depths: &[usize],
    statement: usize,
    relation: &str,
    fields: &BTreeSet<String>,
    access: &mut BTreeMap<String, RelationAccess>,
) -> Result<(), &'static str> {
    let depth = depths[statement];
    let end = statement_end(depths, statement, depth);
    let Some(returning) = (statement + 1..end)
        .find(|index| depths[*index] == depth && identifier(&tokens[*index]) == Some("returning"))
    else {
        return Ok(());
    };
    for index in returning + 1..end {
        if depths[index] == depth && tokens[index] == Token::Star {
            return Err("RETURNING must name returned columns");
        }
        if let Some(field) = identifier(&tokens[index])
            && fields.contains(field)
            && !is_qualified_left(tokens, index)
        {
            access
                .get_mut(relation)
                .expect("known relation has an access row")
                .select_fields
                .insert(field.to_owned());
        }
    }
    Ok(())
}

fn refuse_unsupported_effects(tokens: &[Token]) -> Result<(), &'static str> {
    for (index, token) in tokens.iter().enumerate() {
        let Some(keyword) = identifier(token) else {
            continue;
        };
        if keyword == "delete" && tokens.get(index + 1).and_then(identifier) == Some("from") {
            return Err("DELETE authority is not admitted by the command manifest");
        }
        if matches!(
            keyword,
            "alter"
                | "call"
                | "copy"
                | "create"
                | "drop"
                | "grant"
                | "merge"
                | "revoke"
                | "truncate"
        ) {
            return Err("authored command SQL contains an unsupported effect");
        }
    }
    Ok(())
}

fn parse_select(
    tokens: &[Token],
    depths: &[usize],
    select: usize,
    relations: &BTreeMap<String, BTreeSet<String>>,
    aliases: &BTreeMap<String, String>,
    excluded: &BTreeSet<usize>,
    access: &mut BTreeMap<String, RelationAccess>,
) -> Result<(), &'static str> {
    let depth = depths[select];
    let end = statement_end(depths, select, depth);
    let physical = physical_select_relations(tokens, depths, select, end, depth, relations);
    if physical.is_empty() {
        return Ok(());
    }
    if (select + 1..end).any(|index| depths[index] == depth && tokens[index] == Token::Star) {
        return Err("authored command SELECT must name consumed columns");
    }
    let physical = physical.iter().map(String::as_str).collect::<Vec<_>>();
    collect_unqualified_fields(
        tokens, depths, select, end, &physical, relations, aliases, excluded, access,
    );
    apply_lock_clause(
        tokens, depths, select, end, depth, &physical, aliases, access,
    )?;
    Ok(())
}

fn physical_select_relations(
    tokens: &[Token],
    depths: &[usize],
    select: usize,
    end: usize,
    depth: usize,
    relations: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut physical = BTreeSet::new();
    for index in select + 1..end.saturating_sub(1) {
        if depths[index] != depth || !matches!(identifier(&tokens[index]), Some("from" | "join")) {
            continue;
        }
        if let Some(relation) = tokens.get(index + 1).and_then(identifier)
            && relations.contains_key(relation)
        {
            physical.insert(relation.to_owned());
        }
    }
    physical
}

#[allow(
    clippy::too_many_arguments,
    reason = "the SQL scope and output sets stay explicit"
)]
fn collect_unqualified_fields(
    tokens: &[Token],
    depths: &[usize],
    start: usize,
    end: usize,
    physical: &[&str],
    relations: &BTreeMap<String, BTreeSet<String>>,
    aliases: &BTreeMap<String, String>,
    excluded: &BTreeSet<usize>,
    access: &mut BTreeMap<String, RelationAccess>,
) {
    let scope_depth = depths[start];
    for index in start + 1..end {
        if depths[index] < scope_depth || excluded.contains(&index) || is_qualified(tokens, index) {
            continue;
        }
        let Some(field) = identifier(&tokens[index]) else {
            continue;
        };
        if is_keyword(field) || aliases.contains_key(field) || relations.contains_key(field) {
            continue;
        }
        let matching = physical
            .iter()
            .copied()
            .filter(|relation| relations[*relation].contains(field))
            .collect::<Vec<_>>();
        if let [relation] = matching.as_slice() {
            access
                .get_mut(*relation)
                .expect("known relation has an access row")
                .select_fields
                .insert(field.to_owned());
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the token scope and relation/alias outputs stay explicit"
)]
fn apply_lock_clause(
    tokens: &[Token],
    depths: &[usize],
    select: usize,
    end: usize,
    depth: usize,
    physical: &[&str],
    aliases: &BTreeMap<String, String>,
    access: &mut BTreeMap<String, RelationAccess>,
) -> Result<(), &'static str> {
    for index in select + 1..end {
        if depths[index] != depth || identifier(&tokens[index]) != Some("for") {
            continue;
        }
        let locking = matches!(
            identifier(tokens.get(index + 1).unwrap_or(&Token::Other)),
            Some("update" | "share" | "no" | "key")
        );
        if !locking {
            continue;
        }
        let of = (index + 1..end).find(|candidate| {
            depths[*candidate] == depth && identifier(&tokens[*candidate]) == Some("of")
        });
        let targets = if let Some(of) = of {
            let mut targets = Vec::new();
            for candidate in of + 1..end {
                if depths[candidate] < depth {
                    break;
                }
                if depths[candidate] != depth {
                    continue;
                }
                let Some(name) = identifier(&tokens[candidate]) else {
                    continue;
                };
                if matches!(name, "nowait" | "skip" | "locked" | "for") {
                    break;
                }
                if let Some(relation) = aliases.get(name) {
                    targets.push(relation.as_str());
                }
            }
            targets
        } else {
            physical.to_vec()
        };
        if targets.is_empty() {
            return Err("row-lock clause has no declared physical relation");
        }
        for relation in targets {
            access
                .get_mut(relation)
                .expect("known relation has an access row")
                .lock = true;
        }
    }
    Ok(())
}

fn is_lock_update(tokens: &[Token], depths: &[usize], update: usize) -> bool {
    let depth = depths[update];
    (update.saturating_sub(3)..update)
        .any(|index| depths[index] == depth && identifier(&tokens[index]) == Some("for"))
}

fn statement_end(depths: &[usize], start: usize, depth: usize) -> usize {
    (start + 1..depths.len())
        .find(|index| depths[*index] < depth)
        .unwrap_or(depths.len())
}

fn is_qualified(tokens: &[Token], index: usize) -> bool {
    is_qualified_left(tokens, index) || tokens.get(index + 1) == Some(&Token::Dot)
}

fn is_qualified_left(tokens: &[Token], index: usize) -> bool {
    index > 0 && tokens.get(index - 1) == Some(&Token::Dot)
}

fn identifier(token: &Token) -> Option<&str> {
    match token {
        Token::Identifier(value) => Some(value),
        _ => None,
    }
}

fn is_keyword(value: &str) -> bool {
    matches!(
        value,
        "all"
            | "and"
            | "as"
            | "asc"
            | "by"
            | "case"
            | "conflict"
            | "constraint"
            | "desc"
            | "do"
            | "else"
            | "end"
            | "false"
            | "for"
            | "from"
            | "in"
            | "insert"
            | "into"
            | "is"
            | "join"
            | "key"
            | "left"
            | "limit"
            | "locked"
            | "materialized"
            | "no"
            | "not"
            | "nothing"
            | "null"
            | "of"
            | "on"
            | "or"
            | "order"
            | "returning"
            | "select"
            | "set"
            | "share"
            | "then"
            | "true"
            | "update"
            | "values"
            | "when"
            | "where"
            | "with"
    )
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
            b'(' => {
                tokens.push(Token::LeftParen);
                cursor += 1;
            }
            b')' => {
                tokens.push(Token::RightParen);
                cursor += 1;
            }
            b',' => {
                tokens.push(Token::Comma);
                cursor += 1;
            }
            b'=' => {
                tokens.push(Token::Equals);
                cursor += 1;
            }
            b'*' => {
                tokens.push(Token::Star);
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

#[cfg(test)]
mod tests {
    use proptest::prelude::ProptestConfig;
    use proptest::{prop_assert, prop_assert_eq, proptest};

    use super::grammar::{Forbidden, Piece, Statement, Style, catalog, render};
    use super::*;

    /// Derive the access of one rendered piece list against the grammar catalog.
    fn access(
        pieces: &[Piece],
        style: &Style,
    ) -> Result<BTreeMap<String, RelationAccess>, &'static str> {
        relation_access(render(pieces, style).as_bytes(), &catalog())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// LEX-1: a derived privilege names only a declared relation and only
        /// that relation's declared fields. This is the permission property:
        /// the parser may under-report, but it may never invent authority.
        #[test]
        fn derived_access_names_only_declared_relations_and_fields(
            statement: Statement,
            style: Style,
        ) {
            let declared = catalog();
            let Ok(derived) = access(&statement.pieces(), &style) else {
                return Ok(());
            };
            for (relation, privileges) in &derived {
                let Some(fields) = declared.get(relation) else {
                    return Err(proptest::test_runner::TestCaseError::fail(format!(
                        "undeclared relation {relation}"
                    )));
                };
                for named in privileges
                    .select_fields
                    .iter()
                    .chain(&privileges.insert_fields)
                    .chain(&privileges.update_fields)
                {
                    prop_assert!(fields.contains(named), "{relation}.{named} is undeclared");
                }
            }
        }

        /// LEX-2: the parser recovers exactly the access the statement was
        /// built from -- no more, and no less.
        #[test]
        fn the_parser_recovers_the_authored_access(statement: Statement) {
            let expected = statement.expected();
            let derived = access(&statement.pieces(), &Style::canonical());
            prop_assert!(
                derived.is_ok(),
                "admitted statement refused: {derived:?} for {}",
                render(&statement.pieces(), &Style::canonical())
            );
            let derived = derived.expect("the assertion above returned on refusal");
            prop_assert_eq!(
                derived.keys().map(String::as_str).collect::<Vec<_>>(),
                vec![expected.relation]
            );
            let observed = &derived[expected.relation];
            prop_assert_eq!(&observed.select_fields, &expected.select_fields);
            prop_assert_eq!(&observed.insert_fields, &expected.insert_fields);
            prop_assert_eq!(&observed.update_fields, &expected.update_fields);
            prop_assert_eq!(observed.lock, expected.lock);
        }

        /// LEX-3: comments, string bodies, dollar-quoted bodies, keyword case
        /// and identifier quoting are not code. None of them may move the
        /// derived authority, however hostile the inert text reads.
        #[test]
        fn inert_text_and_rendering_style_never_change_authority(
            statement: Statement,
            style: Style,
        ) {
            let pieces = statement.pieces();
            prop_assert_eq!(access(&pieces, &Style::canonical()), access(&pieces, &style));
        }

        /// LEX-4: a refusal is a property of the tokens, so no rendering of a
        /// known-bad statement is ever admitted.
        #[test]
        fn known_bad_statements_are_refused_under_every_rendering(
            forbidden: Forbidden,
            style: Style,
        ) {
            let pieces = forbidden.pieces();
            prop_assert!(
                access(&pieces, &style).is_err(),
                "admitted {forbidden:?}: {}",
                render(&pieces, &style)
            );
        }

        /// LEX-5: a schema name that appears only inside inert text is not a
        /// schema-qualified reference. No generated statement names `sched`,
        /// and every inert body does.
        #[test]
        fn schema_qualification_ignores_inert_text(statement: Statement, style: Style) {
            let sql = render(&statement.pieces(), &style);
            prop_assert!(!contains_schema_qualified_reference(sql.as_bytes(), "sched"), "{sql}");
        }
    }

    /// The refusal paths as committed cases, so the set is enumerated rather
    /// than sampled.
    #[test]
    fn every_known_bad_shape_is_refused() {
        for forbidden in Forbidden::all() {
            let sql = render(&forbidden.pieces(), &Style::canonical());
            assert!(
                relation_access(sql.as_bytes(), &catalog()).is_err(),
                "admitted {forbidden:?}: {sql}"
            );
        }
    }

    #[test]
    fn schema_qualification_reads_tokens_not_text() {
        assert!(contains_schema_qualified_reference(
            b"SELECT sched.item.id FROM sched.item",
            "sched"
        ));
        assert!(!contains_schema_qualified_reference(
            b"-- sched.item\nSELECT id FROM item",
            "sched"
        ));
        assert!(!contains_schema_qualified_reference(
            b"/* sched.item */ SELECT id FROM item",
            "sched"
        ));
        assert!(!contains_schema_qualified_reference(
            b"SELECT id FROM item WHERE status = 'sched.item'",
            "sched"
        ));
        assert!(!contains_schema_qualified_reference(
            b"SELECT id FROM item WHERE status = $q$ sched.item $q$",
            "sched"
        ));
    }

    fn relations() -> BTreeMap<String, BTreeSet<String>> {
        BTreeMap::from([(
            "item".to_owned(),
            ["id".to_owned(), "status".to_owned()].into_iter().collect(),
        )])
    }

    #[test]
    fn access_is_derived_by_database_verb() {
        let observed = relation_access(
            b"SELECT item.id FROM item WHERE item.status = $1 FOR KEY SHARE OF item",
            &relations(),
        )
        .unwrap();
        assert_eq!(
            observed["item"],
            RelationAccess {
                select_fields: ["id".to_owned(), "status".to_owned()].into_iter().collect(),
                lock: true,
                ..RelationAccess::default()
            }
        );

        let observed = relation_access(
            b"UPDATE item SET status = CASE WHEN id = $1 THEN 'ready' ELSE 'held' END RETURNING status",
            &relations(),
        )
        .unwrap();
        assert_eq!(
            observed["item"],
            RelationAccess {
                select_fields: ["id".to_owned(), "status".to_owned()].into_iter().collect(),
                update_fields: ["status".to_owned()].into_iter().collect(),
                ..RelationAccess::default()
            }
        );
    }

    #[test]
    fn unsupported_effect_and_opaque_returning_refuse() {
        assert!(
            relation_access(
                b"WITH removed AS (DELETE FROM item RETURNING id) SELECT removed.id FROM removed",
                &relations(),
            )
            .is_err()
        );
        assert!(
            relation_access(
                b"INSERT INTO item (id, status) VALUES ($1, $2) RETURNING *",
                &relations(),
            )
            .is_err()
        );
        assert!(
            relation_access(
                b"UPDATE item SET (id, status) = ($1, $2) WHERE item.id = $3",
                &relations(),
            )
            .is_err()
        );
    }
}

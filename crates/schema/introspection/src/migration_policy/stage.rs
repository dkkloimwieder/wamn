//! Dynamic-SQL refusals and typed inspection for staged expressions and conditions.

use std::path::Path;

use crate::ir::POSTGRES_SERVER_EXPRESSIONS;

use super::{
    MigrationPolicyError, MigrationPolicyErrorType, Token, artifact_statements,
    dollar_delimiter_end, lex_statements, refuse_statement, word, words,
};

mod expression;
mod synchronization;
pub use expression::{
    StageRelation, StageUniqueKey, validate_stage_batch, validate_stage_condition,
    validate_stage_expression, validate_stage_trigger_body,
};
pub use synchronization::{
    StageSynchronization, inspect_stage_synchronization, inspect_stage_synchronization_declaration,
};

/// Refuse dynamic SQL and elevated routines, including directives inside routine bodies.
///
/// Success is not SQL admission. Callers must separately establish ownership,
/// expression authority, and the complete stage execution policy. Whole-source
/// expression diagnostics are partial: they name `now()`, `coalesce()`, and
/// binary arithmetic. Complete typed expressions require `validate_stage_expression`.
pub fn refuse_dynamic_stage_sql(
    path: impl AsRef<Path>,
    bytes: &[u8],
) -> Result<(), MigrationPolicyError> {
    let path = path.as_ref();
    // This sentinel satisfies the artifact reader, which does not inspect schemas.
    // This preflight does not establish schema or relation authority.
    let statements = artifact_statements(path, bytes, &["stage_preflight"])?;
    for (index, tokens) in statements.iter().enumerate() {
        inspect_statement(tokens, path, index + 1, 0)?;
    }
    Ok(())
}

fn inspect_statement(
    tokens: &[Token<'_>],
    path: &Path,
    statement_index: usize,
    depth: usize,
) -> Result<(), MigrationPolicyError> {
    let object = if words(tokens, 0, &["create", "or", "replace"]) {
        3
    } else {
        1
    };
    let routine = word(tokens, 0, "create")
        && (word(tokens, object, "function") || word(tokens, object, "procedure"));
    let trigger = word(tokens, 0, "create")
        && (word(tokens, object, "trigger") || words(tokens, object, &["constraint", "trigger"]));

    for index in 0..tokens.len() {
        // Whole statements lack destination types. These negative checks are
        // diagnostics only; typed expression admission remains a separate step.
        if matches!(tokens.get(index + 1), Some(Token::Symbol(b'(')))
            && !routine_declaration(tokens, index, object, routine, trigger)
            && (word(tokens, index, "now") || word(tokens, index, "coalesce"))
            && let Token::Word(name) = tokens[index]
            && !POSTGRES_SERVER_EXPRESSIONS
                .iter()
                .any(|(_, expression, _)| {
                    expression
                        .strip_suffix("()")
                        .is_some_and(|allowed| allowed == name)
                        && !index
                            .checked_sub(1)
                            .is_some_and(|previous| tokens[previous] == Token::Symbol(b'.'))
                })
        {
            return refuse_statement(
                MigrationPolicyErrorType::UnsupportedStatement,
                path,
                statement_index,
                format!("stage expression function {name}() is outside the column-default policy"),
            );
        }
        if let Some(operator) = binary_arithmetic(tokens, index) {
            return refuse_statement(
                MigrationPolicyErrorType::UnsupportedStatement,
                path,
                statement_index,
                format!(
                    "stage arithmetic operator {operator} is outside the column-default policy"
                ),
            );
        }
        let directive = if word(tokens, index, "do")
            && !word(tokens, index + 1, "update")
            && !word(tokens, index + 1, "nothing")
        {
            Some("DO")
        } else if words(tokens, index, &["security", "definer"]) {
            Some("SECURITY DEFINER")
        } else if word(tokens, index, "execute")
            && !(trigger
                && (word(tokens, index + 1, "function") || word(tokens, index + 1, "procedure")))
        {
            Some("EXECUTE")
        } else {
            None
        };
        if let Some(directive) = directive {
            return refuse_statement(
                MigrationPolicyErrorType::RuledOperation,
                path,
                statement_index,
                format!("stage SQL forbids {directive}"),
            );
        }
        if routine && word(tokens, index, "as") {
            if matches!(
                tokens.get(index + 2),
                Some(Token::StringLiteral(_) | Token::Opaque(_))
            ) {
                return refuse_statement(
                    MigrationPolicyErrorType::UnsupportedStatement,
                    path,
                    statement_index,
                    "concatenated stage routine bodies cannot be inspected for EXECUTE, DO, or SECURITY DEFINER",
                );
            }
            inspect_body(tokens.get(index + 1), path, statement_index, depth)?;
        }
    }
    Ok(())
}

fn routine_declaration(
    tokens: &[Token<'_>],
    index: usize,
    object: usize,
    routine: bool,
    trigger: bool,
) -> bool {
    let start = if index >= 2 && tokens[index - 1] == Token::Symbol(b'.') {
        index - 2
    } else {
        index
    };
    (routine && start == object + 1)
        || (trigger && start >= 2 && word(tokens, start - 2, "execute"))
}

fn binary_arithmetic(tokens: &[Token<'_>], index: usize) -> Option<char> {
    let Token::Symbol(operator @ (b'+' | b'-' | b'*' | b'/' | b'%' | b'^')) = tokens[index] else {
        return None;
    };
    let previous = tokens.get(index.checked_sub(1)?)?;
    let operand = matches!(
        previous,
        Token::StringLiteral(_) | Token::QuotedIdentifier(_) | Token::Symbol(b'0'..=b'9' | b')')
    ) || matches!(previous, Token::Word(value) if !matches!(value.to_ascii_lowercase().as_str(),
            "select" | "default" | "set" | "then" | "else" | "return" | "when" | "and" | "or" | "not"));
    operand.then_some(char::from(operator))
}

fn inspect_body(
    token: Option<&Token<'_>>,
    path: &Path,
    statement_index: usize,
    depth: usize,
) -> Result<(), MigrationPolicyError> {
    // Bound recursion for untrusted artifacts with nested routine definitions.
    if depth == 32 {
        return refuse_statement(
            MigrationPolicyErrorType::UnsupportedStatement,
            path,
            statement_index,
            "stage routine body nesting exceeds the inspection limit",
        );
    }
    let body = match token {
        Some(Token::StringLiteral(value)) => value[1..value.len() - 1].replace("''", "'"),
        Some(Token::Opaque(value)) if value.starts_with('$') => {
            let delimiter_length = dollar_delimiter_end(value.as_bytes(), 0)
                .expect("lexer establishes a dollar delimiter");
            value[delimiter_length..value.len() - delimiter_length].to_owned()
        }
        _ => {
            return refuse_statement(
                MigrationPolicyErrorType::UnsupportedStatement,
                path,
                statement_index,
                "stage routine body encoding cannot be inspected for EXECUTE, DO, or SECURITY DEFINER",
            );
        }
    };
    let statements = lex_statements(&body, path).map_err(|mut error| {
        error.statement_index = Some(statement_index);
        error
    })?;
    for tokens in statements {
        inspect_statement(&tokens, path, statement_index, depth + 1)?;
    }
    Ok(())
}

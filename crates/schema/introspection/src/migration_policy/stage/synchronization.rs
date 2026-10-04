//! Closed declaration shape for an owned synchronization trigger.

use std::path::Path;

use super::super::{
    MigrationPolicyError, MigrationPolicyErrorType, Token, dollar_delimiter_end, lex_statements,
    policy_error, words,
};
use super::expression::{StageRelation, validate_stage_trigger_body};
use super::refuse_dynamic_stage_sql;

/// Inspected SQL identities, not evidence that the database objects are owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageSynchronization {
    pub schema: String,
    pub relation: String,
    pub function: String,
    pub trigger: String,
    pub body: String,
}

/// Inspect one invoker function followed by its row synchronization trigger.
///
/// The caller supplies owned relation facts and fields that this stage can write.
/// This does not authorize execution, replacement, or catalog exclusion. Those
/// operations must establish ownership and compare the actual database objects.
pub fn inspect_stage_synchronization(
    path: impl AsRef<Path>,
    bytes: &[u8],
    relation: &StageRelation,
    writable_columns: &[String],
) -> Result<StageSynchronization, MigrationPolicyError> {
    let path = path.as_ref();
    let inspected = inspect_stage_synchronization_declaration(path, bytes)?;
    if inspected.schema != relation.schema || inspected.relation != relation.name {
        return Err(policy_error(
            MigrationPolicyErrorType::UnsupportedStatement,
            path,
            Some(2),
            format!(
                "trigger {} targets unowned relation {}.{}",
                inspected.trigger, inspected.schema, inspected.relation
            ),
        ));
    }
    validate_stage_trigger_body(path, &inspected.body, relation, writable_columns).map_err(
        |mut error| {
            error.statement_index = Some(1);
            error
        },
    )?;
    Ok(inspected)
}

/// Read the closed declaration shape without admitting its body or ownership.
///
/// Before execution, call `inspect_stage_synchronization` with actual owned
/// relation and writable-column facts. This function alone is not admission.
pub fn inspect_stage_synchronization_declaration(
    path: impl AsRef<Path>,
    bytes: &[u8],
) -> Result<StageSynchronization, MigrationPolicyError> {
    let path = path.as_ref();
    refuse_dynamic_stage_sql(path, bytes)?;
    let sql = std::str::from_utf8(bytes).map_err(|_| {
        policy_error(
            MigrationPolicyErrorType::InvalidSql,
            path,
            None,
            "synchronization SQL is not UTF-8",
        )
    })?;
    let statements = lex_statements(sql, path)?;
    if statements.len() != 2 {
        return Err(policy_error(
            MigrationPolicyErrorType::UnsupportedStatement,
            path,
            None,
            "synchronization requires one CREATE FUNCTION followed by one CREATE TRIGGER",
        ));
    }
    if words(&statements[0], 0, &["create", "or", "replace"]) {
        return Err(policy_error(
            MigrationPolicyErrorType::UnsupportedStatement,
            path,
            Some(1),
            "synchronization declaration forbids CREATE OR REPLACE FUNCTION",
        ));
    }
    let mut declaration = Declaration {
        path,
        tokens: &statements[0],
        position: 0,
        statement: 1,
    };
    declaration.words(&["create", "function"])?;
    let (schema, function) = declaration.qualified()?;
    declaration.symbol(b'(')?;
    declaration.symbol(b')')?;
    declaration.words(&[
        "returns", "trigger", "language", "plpgsql", "security", "invoker", "as",
    ])?;
    let body = match declaration.tokens.get(declaration.position) {
        Some(Token::Opaque(value)) if value.starts_with('$') => {
            let length = dollar_delimiter_end(value.as_bytes(), 0)
                .expect("lexer establishes dollar quoting");
            value[length..value.len() - length].to_owned()
        }
        _ => return Err(declaration.error("a dollar-quoted synchronization body is required")),
    };
    declaration.position += 1;
    declaration.end()?;
    let mut declaration = Declaration {
        path,
        tokens: &statements[1],
        position: 0,
        statement: 2,
    };
    declaration.words(&["create", "trigger"])?;
    let trigger = declaration.identifier()?;
    if [
        wamn_record_history::STAMP_TRIGGER,
        wamn_record_history::LOG_TRIGGER,
        wamn_catalog::VERSION_NOTE_TRIGGER,
        wamn_catalog::VERSION_BUMP_TRIGGER,
    ]
    .contains(&trigger.as_str())
    {
        return Err(declaration.error(format!("trigger {trigger} is reserved for the platform")));
    }
    declaration.words(&["before", "insert", "or", "update", "on"])?;
    let (trigger_schema, trigger_relation) = declaration.qualified()?;
    if trigger_schema != schema {
        return Err(declaration.error(format!(
            "function {schema}.{function} is outside trigger schema {trigger_schema}"
        )));
    }
    declaration.words(&["for", "each", "row", "execute", "function"])?;
    let (invoked_schema, invoked_function) = declaration.qualified()?;
    declaration.symbol(b'(')?;
    declaration.symbol(b')')?;
    declaration.end()?;
    if invoked_schema != schema || invoked_function != function {
        return Err(declaration.error(format!("trigger {trigger} invokes {invoked_schema}.{invoked_function}, not declared function {schema}.{function}")));
    }
    Ok(StageSynchronization {
        schema,
        relation: trigger_relation,
        function,
        trigger,
        body,
    })
}

struct Declaration<'a> {
    path: &'a Path,
    tokens: &'a [Token<'a>],
    position: usize,
    statement: usize,
}

impl Declaration<'_> {
    fn error(&self, detail: impl Into<String>) -> MigrationPolicyError {
        policy_error(
            MigrationPolicyErrorType::UnsupportedStatement,
            self.path,
            Some(self.statement),
            format!("synchronization declaration: {}", detail.into()),
        )
    }

    fn words(&mut self, expected: &[&str]) -> Result<(), MigrationPolicyError> {
        for word in expected {
            if !matches!(self.tokens.get(self.position), Some(Token::Word(value)) if value.eq_ignore_ascii_case(word))
            {
                return Err(self.error(format!(
                    "expected {word}, found {:?}",
                    self.tokens.get(self.position)
                )));
            }
            self.position += 1;
        }
        Ok(())
    }

    fn symbol(&mut self, symbol: u8) -> Result<(), MigrationPolicyError> {
        if self.tokens.get(self.position) != Some(&Token::Symbol(symbol)) {
            return Err(self.error(format!(
                "expected {}, found {:?}",
                char::from(symbol),
                self.tokens.get(self.position)
            )));
        }
        self.position += 1;
        Ok(())
    }

    fn identifier(&mut self) -> Result<String, MigrationPolicyError> {
        let name = match self.tokens.get(self.position) {
            Some(Token::Word(value)) => value.to_ascii_lowercase(),
            Some(Token::QuotedIdentifier(value)) => value.replace("\"\"", "\""),
            token => return Err(self.error(format!("expected an identifier, found {token:?}"))),
        };
        if name.is_empty() || name.len() > 63 {
            return Err(self.error(format!(
                "identifier {name:?} exceeds the PostgreSQL identifier boundary"
            )));
        }
        self.position += 1;
        Ok(name)
    }

    fn qualified(&mut self) -> Result<(String, String), MigrationPolicyError> {
        let schema = self.identifier()?;
        self.symbol(b'.')?;
        Ok((schema, self.identifier()?))
    }

    fn end(&self) -> Result<(), MigrationPolicyError> {
        if self.position != self.tokens.len() {
            return Err(self.error(format!(
                "unsupported trailing declaration {:?}",
                &self.tokens[self.position..]
            )));
        }
        Ok(())
    }
}

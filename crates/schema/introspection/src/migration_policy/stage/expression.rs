//! Closed, typed grammar for package-owned stage expressions and conditions.

use std::collections::BTreeMap;
use std::path::Path;

use crate::ir::{ColumnType, POSTGRES_SERVER_EXPRESSIONS, postgres_default, postgres_type};

use super::super::{
    MigrationPolicyError, MigrationPolicyErrorType, Token, lex_statements, policy_error,
};

mod batch;
pub use batch::{StageUniqueKey, validate_stage_batch};
mod trigger;
pub use trigger::validate_stage_trigger_body;

/// A relation and columns whose package ownership the caller established.
#[derive(Debug, Clone)]
pub struct StageRelation {
    pub schema: String,
    pub name: String,
    pub columns: BTreeMap<String, ColumnType>,
}

/// Validate an expression with owned columns and numbered, typed parameters.
///
/// This admits comparisons, Boolean logic, owned EXISTS queries, and same-type
/// casts. Functions and literal forms retain the column-default policy.
pub fn validate_stage_expression(
    path: impl AsRef<Path>,
    expected: ColumnType,
    expression: &str,
    relations: &[StageRelation],
    parameters: &[ColumnType],
) -> Result<(), MigrationPolicyError> {
    validate(
        path.as_ref(),
        expression,
        expected,
        relations,
        parameters,
        false,
    )
    .map_err(|mut error| {
        error.detail = format!("{}; expression {expression:?}", error.detail).into();
        error
    })
}

/// Validate one SELECT that returns a Boolean stage condition.
pub fn validate_stage_condition(
    path: impl AsRef<Path>,
    bytes: &[u8],
    relations: &[StageRelation],
    parameters: &[ColumnType],
) -> Result<(), MigrationPolicyError> {
    let path = path.as_ref();
    let sql = std::str::from_utf8(bytes).map_err(|_| {
        policy_error(
            MigrationPolicyErrorType::InvalidSql,
            path,
            None,
            "stage condition is not UTF-8 SQL",
        )
    })?;
    validate(path, sql, ColumnType::Boolean, relations, parameters, true)
}

fn validate(
    path: &Path,
    sql: &str,
    expected: ColumnType,
    relations: &[StageRelation],
    parameters: &[ColumnType],
    select: bool,
) -> Result<(), MigrationPolicyError> {
    let statements = lex_statements(sql, path)?;
    if statements.len() != 1 {
        return Err(policy_error(
            MigrationPolicyErrorType::UnsupportedStatement,
            path,
            None,
            "stage expression or condition requires exactly one statement",
        ));
    }
    let mut parser = Parser {
        tokens: &statements[0],
        position: 0,
        path,
        relations,
        parameters,
        bindings: vec![
            relations
                .iter()
                .enumerate()
                .map(|(index, relation)| Binding {
                    alias: relation.name.clone(),
                    relation: index,
                })
                .collect(),
        ],
        depth: 0,
        ctes: BTreeMap::new(),
        row_reference: None,
    };
    if select {
        parser.require_word("select")?;
    }
    let value = parser.expression()?;
    if parser.position != parser.tokens.len() {
        return Err(parser.error(format!("unsupported stage construct {}", parser.current())));
    }
    parser.require_type(&value, expected)?;
    Ok(())
}

#[derive(Debug)]
enum Value {
    Typed(ColumnType),
    Literal(String),
    Null,
}

struct Binding {
    alias: String,
    relation: usize,
}

struct Parser<'a, 'sql> {
    tokens: &'a [Token<'sql>],
    position: usize,
    path: &'a Path,
    relations: &'a [StageRelation],
    parameters: &'a [ColumnType],
    bindings: Vec<Vec<Binding>>,
    depth: usize,
    ctes: BTreeMap<String, usize>,
    row_reference: Option<&'static str>,
}

impl Parser<'_, '_> {
    fn expression(&mut self) -> Result<Value, MigrationPolicyError> {
        if self.depth == 64 {
            return Err(self.error("stage expression nesting exceeds the inspection limit"));
        }
        self.depth += 1;
        let value = self.or_expression();
        self.depth -= 1;
        value
    }

    fn or_expression(&mut self) -> Result<Value, MigrationPolicyError> {
        let mut value = self.and_expression()?;
        while self.eat_word("or") {
            self.require_type(&value, ColumnType::Boolean)?;
            let right = self.and_expression()?;
            self.require_type(&right, ColumnType::Boolean)?;
            value = Value::Typed(ColumnType::Boolean);
        }
        Ok(value)
    }

    fn and_expression(&mut self) -> Result<Value, MigrationPolicyError> {
        let mut value = self.not_expression()?;
        while self.eat_word("and") {
            self.require_type(&value, ColumnType::Boolean)?;
            let right = self.not_expression()?;
            self.require_type(&right, ColumnType::Boolean)?;
            value = Value::Typed(ColumnType::Boolean);
        }
        Ok(value)
    }

    fn not_expression(&mut self) -> Result<Value, MigrationPolicyError> {
        if self.eat_word("not") {
            // Parse recursively through the bounded expression entry point.
            self.depth += 1;
            if self.depth >= 64 {
                return Err(self.error("stage expression nesting exceeds the inspection limit"));
            }
            let value = self.not_expression()?;
            self.depth -= 1;
            self.require_type(&value, ColumnType::Boolean)?;
            return Ok(Value::Typed(ColumnType::Boolean));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Value, MigrationPolicyError> {
        let left = self.atom()?;
        if self.eat_word("is") {
            self.eat_word("not");
            self.require_word("null")?;
            return Ok(Value::Typed(ColumnType::Boolean));
        }
        let operation = match self.tokens.get(self.position) {
            Some(Token::Symbol(b'=')) => "=",
            Some(Token::Symbol(b'<')) => "<",
            Some(Token::Symbol(b'>')) => ">",
            Some(Token::Symbol(b'!')) => "!",
            _ => return Ok(left),
        };
        self.position += 1;
        if operation == "!" {
            self.require_symbol(b'=')?;
        } else if operation == "<" {
            if !self.eat_symbol(b'=') {
                self.eat_symbol(b'>');
            }
        } else if operation == ">" {
            self.eat_symbol(b'=');
        }
        let right = self.atom()?;
        self.comparable(&left, &right)?;
        Ok(Value::Typed(ColumnType::Boolean))
    }

    fn atom(&mut self) -> Result<Value, MigrationPolicyError> {
        let value = if self.eat_symbol(b'(') {
            let value = self.expression()?;
            self.require_symbol(b')')?;
            value
        } else if self.eat_word("exists") {
            self.exists()?
        } else if self.eat_word("null") {
            Value::Null
        } else if self.eat_word("true") || self.eat_word("false") {
            Value::Typed(ColumnType::Boolean)
        } else if self.eat_symbol(b'$') {
            let index = self
                .digits()?
                .parse::<usize>()
                .map_err(|_| self.error("invalid bound parameter number"))?;
            let column_type = index
                .checked_sub(1)
                .and_then(|index| self.parameters.get(index))
                .ok_or_else(|| {
                    self.error(format!("bound parameter ${index} has no declared type"))
                })?;
            Value::Typed(*column_type)
        } else if let Some(Token::StringLiteral(value)) = self.tokens.get(self.position) {
            self.position += 1;
            Value::Literal((*value).to_owned())
        } else if matches!(
            self.tokens.get(self.position),
            Some(Token::Symbol(b'0'..=b'9' | b'+' | b'-'))
        ) {
            let mut literal = String::new();
            if self.eat_symbol(b'-') {
                literal.push('-');
            } else if self.eat_symbol(b'+') {
                literal.push('+');
            }
            literal.push_str(&self.digits()?);
            if self.eat_symbol(b'.') {
                literal.push('.');
                literal.push_str(&self.digits()?);
            }
            Value::Literal(literal)
        } else {
            self.reference()?
        };
        if self.eat_symbol(b':') {
            self.require_symbol(b':')?;
            let name = self.cast_name()?;
            let destination = postgres_type(&name).map_err(|_| {
                self.error(format!(
                    "cast to {name} is outside the column-default type policy"
                ))
            })?;
            self.require_type(&value, destination)?;
            return Ok(Value::Typed(destination));
        }
        Ok(value)
    }

    fn reference(&mut self) -> Result<Value, MigrationPolicyError> {
        let keyword = matches!(self.tokens.get(self.position), Some(Token::Word(_)));
        let mut names = vec![self.identifier()?];
        while self.eat_symbol(b'.') {
            names.push(self.identifier()?);
        }
        if self.eat_symbol(b'(') {
            let spelling = format!("{}()", names.join("."));
            let admitted = POSTGRES_SERVER_EXPRESSIONS
                .iter()
                .find(|(_, name, _)| *name == spelling);
            let Some((column_type, _, _)) = admitted else {
                return Err(self.error(format!(
                    "stage function {}() is outside the column-default policy",
                    names.join(".")
                )));
            };
            self.require_symbol(b')')?;
            return Ok(Value::Typed(*column_type));
        }
        if keyword && names.len() == 1 && names[0] == "current_timestamp" {
            let (column_type, _, _) = POSTGRES_SERVER_EXPRESSIONS
                .iter()
                .find(|(_, name, _)| *name == "CURRENT_TIMESTAMP")
                .expect("shared expression policy declares CURRENT_TIMESTAMP");
            return Ok(Value::Typed(*column_type));
        }
        if let Some(row) = self.row_reference
            && !matches!(names.as_slice(), [qualifier, _] if qualifier == row)
        {
            return Err(self.error(format!(
                "trigger expression must read {row}.<owned column>, found {}",
                names.join(".")
            )));
        }
        self.column(&names).map(Value::Typed)
    }

    fn exists(&mut self) -> Result<Value, MigrationPolicyError> {
        if self.row_reference.is_some() {
            return Err(self.error(
                "EXISTS and external data reads are outside synchronization trigger bodies",
            ));
        }
        self.require_symbol(b'(')?;
        self.require_word("select")?;
        // EXISTS ignores its projection. A fixed projection avoids admitting
        // executable expressions that are unrelated to the condition.
        if self.digits()? != "1" {
            return Err(self.error("stage EXISTS requires SELECT 1"));
        }
        self.require_word("from")?;
        let binding = self.relation()?;
        self.bindings.push(vec![binding]);
        if self.eat_word("where") {
            let predicate = self.expression()?;
            self.require_type(&predicate, ColumnType::Boolean)?;
        }
        self.require_symbol(b')')?;
        self.bindings.pop();
        Ok(Value::Typed(ColumnType::Boolean))
    }

    fn relation(&mut self) -> Result<Binding, MigrationPolicyError> {
        let schema = self.identifier()?;
        if !self.eat_symbol(b'.') {
            if let Some(relation) = self.ctes.get(&schema).copied() {
                return Ok(Binding {
                    alias: schema,
                    relation,
                });
            }
            return Err(self.error(format!(
                "stage relation {schema} must be schema-qualified to establish package ownership"
            )));
        }
        let name = self.identifier()?;
        let matching = self
            .relations
            .iter()
            .enumerate()
            .filter(|(_, relation)| relation.name == name && relation.schema == schema)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let [relation] = matching.as_slice() else {
            return Err(self.error(format!(
                "relation {schema}.{name} is outside unambiguous package ownership"
            )));
        };
        let alias = if self.eat_word("as") {
            self.identifier()?
        } else {
            name
        };
        Ok(Binding {
            alias,
            relation: *relation,
        })
    }

    fn column(&self, names: &[String]) -> Result<ColumnType, MigrationPolicyError> {
        for frame in self.bindings.iter().rev() {
            let applicable = frame
                .iter()
                .filter(|binding| {
                    let relation = &self.relations[binding.relation];
                    match names {
                        [field] => {
                            !self.ctes.values().any(|index| *index == binding.relation)
                                || relation.columns.contains_key(field)
                        }
                        [qualifier, _] => *qualifier == binding.alias,
                        [schema, table, _] => {
                            *schema == relation.schema
                                && *table == relation.name
                                && binding.alias == relation.name
                        }
                        _ => false,
                    }
                })
                .collect::<Vec<_>>();
            if applicable.is_empty() {
                continue;
            }
            // Never fall through an inner binding to an outer owned field. The
            // inner table can contain columns owned by another package.
            let [binding] = applicable.as_slice() else {
                return Err(self.error(format!("ambiguous owned column {}", names.join("."))));
            };
            let field = names.last().expect("a column reference has a name");
            return self.relations[binding.relation]
                .columns
                .get(field)
                .copied()
                .ok_or_else(|| {
                    self.error(format!(
                        "column {} is outside package ownership",
                        names.join(".")
                    ))
                });
        }
        Err(self.error(format!(
            "column {} is outside package ownership",
            names.join(".")
        )))
    }

    fn comparable(&self, left: &Value, right: &Value) -> Result<(), MigrationPolicyError> {
        match (left, right) {
            (Value::Typed(column_type), other) | (other, Value::Typed(column_type)) => {
                self.require_type(other, *column_type)
            }
            (Value::Null, _) | (_, Value::Null) => Ok(()),
            (Value::Literal(left), Value::Literal(right)) => {
                if [
                    ColumnType::Int32,
                    ColumnType::Int64,
                    ColumnType::Numeric,
                    ColumnType::Text,
                    ColumnType::Boolean,
                ]
                .iter()
                .any(|column_type| {
                    postgres_default(*column_type, left).is_ok()
                        && postgres_default(*column_type, right).is_ok()
                }) {
                    Ok(())
                } else {
                    Err(self.error("comparison literals have no shared admitted type"))
                }
            }
        }
    }

    fn require_type(
        &self,
        value: &Value,
        expected: ColumnType,
    ) -> Result<(), MigrationPolicyError> {
        let accepted = match value {
            Value::Typed(actual) => *actual == expected,
            Value::Literal(literal) => postgres_default(expected, literal).is_ok(),
            Value::Null => true,
        };
        if accepted {
            Ok(())
        } else {
            Err(self.error(format!("stage expression {value:?} does not have type {expected:?}; cross-type conversion is refused")))
        }
    }

    fn cast_name(&mut self) -> Result<String, MigrationPolicyError> {
        if !matches!(self.tokens.get(self.position), Some(Token::Word(_))) {
            return Err(
                self.error("stage casts require an unquoted type from the column-default policy")
            );
        }
        let name = self.identifier()?;
        match name.as_str() {
            "double" => {
                self.require_word("precision")?;
                Ok("double precision".into())
            }
            "timestamp" => {
                self.require_word("with")?;
                self.require_word("time")?;
                self.require_word("zone")?;
                Ok("timestamp with time zone".into())
            }
            _ => Ok(name),
        }
    }

    fn identifier(&mut self) -> Result<String, MigrationPolicyError> {
        let value = match self.tokens.get(self.position) {
            Some(Token::Word(value)) => value.to_ascii_lowercase(),
            Some(Token::QuotedIdentifier(value)) => value.replace("\"\"", "\""),
            _ => return Err(self.error(format!("unsupported stage construct {}", self.current()))),
        };
        self.position += 1;
        Ok(value)
    }

    fn digits(&mut self) -> Result<String, MigrationPolicyError> {
        let mut value = String::new();
        while let Some(Token::Symbol(digit @ b'0'..=b'9')) = self.tokens.get(self.position) {
            value.push(char::from(*digit));
            self.position += 1;
        }
        if value.is_empty() {
            Err(self.error(format!(
                "expected a numeric literal or parameter number, found {}",
                self.current()
            )))
        } else {
            Ok(value)
        }
    }

    fn eat_word(&mut self, expected: &str) -> bool {
        if super::super::word(self.tokens, self.position, expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn require_word(&mut self, expected: &str) -> Result<(), MigrationPolicyError> {
        if self.eat_word(expected) {
            Ok(())
        } else {
            Err(self.error(format!("expected {expected}, found {}", self.current())))
        }
    }

    fn eat_symbol(&mut self, symbol: u8) -> bool {
        if self.tokens.get(self.position) == Some(&Token::Symbol(symbol)) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn require_symbol(&mut self, symbol: u8) -> Result<(), MigrationPolicyError> {
        if self.eat_symbol(symbol) {
            Ok(())
        } else {
            Err(self.error(format!(
                "expected {}, found {}",
                char::from(symbol),
                self.current()
            )))
        }
    }

    fn current(&self) -> String {
        match self.tokens.get(self.position) {
            Some(
                Token::Word(value) | Token::QuotedIdentifier(value) | Token::StringLiteral(value),
            ) => (*value).to_owned(),
            Some(Token::Symbol(value)) => char::from(*value).to_string(),
            Some(Token::Opaque(_)) => "opaque SQL literal".into(),
            None => "end of expression".into(),
        }
    }

    fn error(&self, detail: impl Into<Box<str>>) -> MigrationPolicyError {
        policy_error(
            MigrationPolicyErrorType::UnsupportedStatement,
            self.path,
            Some(1),
            detail,
        )
    }
}

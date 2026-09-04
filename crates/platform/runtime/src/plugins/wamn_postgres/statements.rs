//! Host-owned resolution and contract checks for verified SQL statements.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

use super::{
    ContractMismatch, ContractPart, RowSet, SqlValue, StatementError, ValueShape, WamnPostgres,
};

/// A type in the frozen `wamn:postgres/types.sql-value` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementValueType {
    Boolean,
    Int32,
    Int64,
    Float64,
    Text,
    Bytes,
    Numeric,
    Timestamptz,
    Json,
    Uuid,
}

impl StatementValueType {
    /// Return the canonical WIT vocabulary spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Int32 => "int32",
            Self::Int64 => "int64",
            Self::Float64 => "float64",
            Self::Text => "text",
            Self::Bytes => "bytes",
            Self::Numeric => "numeric",
            Self::Timestamptz => "timestamptz",
            Self::Json => "json",
            Self::Uuid => "uuid",
        }
    }
}

/// One parameter or result-column contract slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatementField {
    pub value_type: StatementValueType,
    pub nullable: bool,
}

/// One statement whose exact bytes and value shape passed native verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedStatement {
    pub exact_sql: Box<str>,
    pub binds: Box<[StatementField]>,
    pub columns: Box<[StatementField]>,
    /// PostgreSQL's build-time verdict: this statement writes or takes a row
    /// lock, so it must run inside a transaction. A statement that does neither
    /// runs autocommit -- one flight, no claim binding, no COMMIT.
    pub transactional: bool,
}

/// The verified statements admitted for one operation.
pub type VerifiedStatementSet = BTreeMap<String, VerifiedStatement>;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct BoundStatementSet(BTreeMap<String, Arc<VerifiedStatement>>);

#[derive(Debug, Default)]
pub(super) struct StatementScopes {
    bindings: HashMap<String, HashMap<String, Arc<BoundStatementSet>>>,
    active: HashMap<String, Arc<BoundStatementSet>>,
}

impl WamnPostgres {
    /// Bind one admitted operation's immutable statement set to a component.
    ///
    /// Binding verifies every map key against the exact SQL bytes. Rebinding the
    /// same facts is a no-op; changing facts under a live component/operation
    /// identity is refused.
    pub fn bind_statement_operation(
        &self,
        component_id: &str,
        operation: &str,
        statements: VerifiedStatementSet,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!component_id.is_empty(), "statement-component-id-empty");
        anyhow::ensure!(!operation.is_empty(), "statement-operation-empty");
        for (digest, statement) in &statements {
            let observed = statement_digest(&statement.exact_sql);
            anyhow::ensure!(
                digest == &observed,
                "statement-digest-mismatch: expected {digest}, observed {observed}"
            );
        }

        let statements = Arc::new(BoundStatementSet(
            statements
                .into_iter()
                .map(|(digest, statement)| (digest, Arc::new(statement)))
                .collect(),
        ));
        let mut scopes = self
            .statement_scopes
            .write()
            .expect("statement scopes lock poisoned");
        let operations = scopes.bindings.entry(component_id.to_owned()).or_default();
        if let Some(existing) = operations.get(operation) {
            anyhow::ensure!(
                existing == &statements,
                "statement-operation-rebind-conflict: {operation}"
            );
            return Ok(());
        }
        operations.insert(operation.to_owned(), statements);
        Ok(())
    }

    /// Activate exactly one bound operation's statement set for an invocation.
    ///
    /// A failed activation first clears any prior scope, so a pooled component
    /// cannot retain another operation's SQL authority.
    pub fn activate_statement_operation(
        &self,
        component_id: &str,
        operation: &str,
    ) -> anyhow::Result<()> {
        let mut scopes = self
            .statement_scopes
            .write()
            .expect("statement scopes lock poisoned");
        let statements = scopes
            .bindings
            .get(component_id)
            .and_then(|operations| operations.get(operation))
            .cloned();
        scopes.active.remove(component_id);
        let statements = statements.ok_or_else(|| {
            anyhow::anyhow!("statement-operation-unbound: {component_id}::{operation}")
        })?;
        scopes.active.insert(component_id.to_owned(), statements);
        Ok(())
    }

    /// Revoke the invocation's active statement authority.
    pub fn revoke_statement_operation(&self, component_id: &str) {
        self.statement_scopes
            .write()
            .expect("statement scopes lock poisoned")
            .active
            .remove(component_id);
    }

    /// Remove every verified-statement binding for one exact component scope.
    ///
    /// This is the instance-lifecycle counterpart to
    /// [`bind_statement_operation`](Self::bind_statement_operation). It removes
    /// both the immutable operation facts and any invocation-active view without
    /// treating the component id as a prefix.
    pub fn clear_statement_scope(&self, component_id: &str) {
        let mut scopes = self
            .statement_scopes
            .write()
            .expect("statement scopes lock poisoned");
        scopes.bindings.remove(component_id);
        scopes.active.remove(component_id);
    }

    pub(super) fn active_statement_set(
        &self,
        component_id: &str,
    ) -> Option<Arc<BoundStatementSet>> {
        self.statement_scopes
            .read()
            .expect("statement scopes lock poisoned")
            .active
            .get(component_id)
            .cloned()
    }

    pub(super) fn clear_statement_bindings(&self, workload_id: &str) {
        let mut scopes = self
            .statement_scopes
            .write()
            .expect("statement scopes lock poisoned");
        scopes
            .bindings
            .retain(|component_id, _| !component_id.starts_with(workload_id));
        scopes
            .active
            .retain(|component_id, _| !component_id.starts_with(workload_id));
    }
}

pub(super) fn resolve_statement(
    statements: Option<&BoundStatementSet>,
    digest: &str,
    binds: &[SqlValue],
) -> Result<Arc<VerifiedStatement>, StatementError> {
    let statement = statements
        .and_then(|statements| statements.0.get(digest))
        .cloned()
        .ok_or_else(|| StatementError::UnknownStatement(digest.to_owned()))?;
    if !binds_match(&statement.binds, binds) {
        return Err(contract_mismatch(
            digest,
            ContractPart::Binds,
            expected_shape(&statement.binds),
            observed_bind_shape(&statement.binds, binds),
        ));
    }
    Ok(statement)
}

pub(super) fn validate_statement_result(
    digest: &str,
    statement: &VerifiedStatement,
    result: RowSet,
) -> Result<RowSet, StatementError> {
    let mut observed_types: Vec<String> = result
        .columns
        .iter()
        .map(|column| {
            statement_type_for_postgres(&column.type_name).map_or_else(
                || column.type_name.clone(),
                |value_type| value_type.as_str().into(),
            )
        })
        .collect();
    let columns_match = result.columns.len() == statement.columns.len()
        && result
            .columns
            .iter()
            .zip(&statement.columns)
            .all(|(column, expected)| {
                statement_type_for_postgres(&column.type_name) == Some(expected.value_type)
            });
    let values_match = result.rows.iter().all(|row| {
        row.len() == statement.columns.len()
            && row
                .iter()
                .zip(&statement.columns)
                .all(|(value, expected)| !matches!(value, SqlValue::Null) || expected.nullable)
    });
    if columns_match && values_match {
        return Ok(result);
    }

    for row in &result.rows {
        for (index, (value, expected)) in row.iter().zip(&statement.columns).enumerate() {
            if matches!(value, SqlValue::Null) && !expected.nullable {
                if let Some(observed) = observed_types.get_mut(index) {
                    *observed = "null".to_owned();
                }
            }
        }
    }
    Err(contract_mismatch(
        digest,
        ContractPart::Columns,
        expected_shape(&statement.columns),
        ValueShape {
            count: bounded_count(result.columns.len()),
            types: observed_types,
        },
    ))
}

pub(super) fn validate_prepared_statement(
    digest: &str,
    statement: &VerifiedStatement,
    prepared: &tokio_postgres::Statement,
) -> Result<(), StatementError> {
    let bind_types: Vec<String> = prepared
        .params()
        .iter()
        .map(|value_type| normalized_postgres_name(value_type.name()))
        .collect();
    if !postgres_types_match(
        &statement.binds,
        prepared.params().iter().map(|ty| ty.name()),
    ) {
        return Err(contract_mismatch(
            digest,
            ContractPart::Binds,
            expected_shape(&statement.binds),
            ValueShape {
                count: bounded_count(bind_types.len()),
                types: bind_types,
            },
        ));
    }

    let column_types: Vec<String> = prepared
        .columns()
        .iter()
        .map(|column| normalized_postgres_name(column.type_().name()))
        .collect();
    if !postgres_types_match(
        &statement.columns,
        prepared
            .columns()
            .iter()
            .map(|column| column.type_().name()),
    ) {
        return Err(contract_mismatch(
            digest,
            ContractPart::Columns,
            expected_shape(&statement.columns),
            ValueShape {
                count: bounded_count(column_types.len()),
                types: column_types,
            },
        ));
    }
    Ok(())
}

fn statement_digest(sql: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(sql.as_bytes())))
}

fn binds_match(expected: &[StatementField], observed: &[SqlValue]) -> bool {
    expected.len() == observed.len()
        && expected
            .iter()
            .zip(observed)
            .all(|(expected, observed)| value_matches(*expected, observed))
}

fn value_matches(expected: StatementField, observed: &SqlValue) -> bool {
    match statement_type_for_value(observed) {
        Some(observed) => observed == expected.value_type,
        None => expected.nullable,
    }
}

fn statement_type_for_value(value: &SqlValue) -> Option<StatementValueType> {
    Some(match value {
        SqlValue::Null => return None,
        SqlValue::Boolean(_) => StatementValueType::Boolean,
        SqlValue::Int32(_) => StatementValueType::Int32,
        SqlValue::Int64(_) => StatementValueType::Int64,
        SqlValue::Float64(_) => StatementValueType::Float64,
        SqlValue::Text(_) => StatementValueType::Text,
        SqlValue::Bytes(_) => StatementValueType::Bytes,
        SqlValue::Numeric(_) => StatementValueType::Numeric,
        SqlValue::Timestamptz(_) => StatementValueType::Timestamptz,
        SqlValue::Json(_) => StatementValueType::Json,
        SqlValue::Uuid(_) => StatementValueType::Uuid,
    })
}

fn statement_type_for_postgres(type_name: &str) -> Option<StatementValueType> {
    Some(match type_name {
        "bool" => StatementValueType::Boolean,
        "int2" | "int4" => StatementValueType::Int32,
        "int8" => StatementValueType::Int64,
        "float4" | "float8" => StatementValueType::Float64,
        "text" | "varchar" | "bpchar" | "name" | "unknown" => StatementValueType::Text,
        "bytea" => StatementValueType::Bytes,
        "numeric" => StatementValueType::Numeric,
        "timestamptz" => StatementValueType::Timestamptz,
        "json" | "jsonb" => StatementValueType::Json,
        "uuid" => StatementValueType::Uuid,
        _ => return None,
    })
}

fn normalized_postgres_name(type_name: &str) -> String {
    statement_type_for_postgres(type_name).map_or_else(
        || type_name.to_owned(),
        |value_type| value_type.as_str().to_owned(),
    )
}

fn postgres_types_match<'a>(
    expected: &[StatementField],
    observed: impl ExactSizeIterator<Item = &'a str>,
) -> bool {
    expected.len() == observed.len()
        && expected.iter().zip(observed).all(|(expected, observed)| {
            statement_type_for_postgres(observed) == Some(expected.value_type)
        })
}

fn expected_shape(fields: &[StatementField]) -> ValueShape {
    ValueShape {
        count: bounded_count(fields.len()),
        types: fields
            .iter()
            .map(|field| field.value_type.as_str().to_owned())
            .collect(),
    }
}

fn observed_bind_shape(expected: &[StatementField], observed: &[SqlValue]) -> ValueShape {
    ValueShape {
        count: bounded_count(observed.len()),
        types: observed
            .iter()
            .enumerate()
            .map(|(index, value)| {
                statement_type_for_value(value).map_or_else(
                    || {
                        expected
                            .get(index)
                            .filter(|field| field.nullable)
                            .map_or("null", |field| field.value_type.as_str())
                            .to_owned()
                    },
                    |value_type| value_type.as_str().to_owned(),
                )
            })
            .collect(),
    }
}

fn bounded_count(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn contract_mismatch(
    digest: &str,
    part: ContractPart,
    expected: ValueShape,
    observed: ValueShape,
) -> StatementError {
    StatementError::StatementContractMismatch(ContractMismatch {
        statement_digest: digest.to_owned(),
        part,
        expected,
        observed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::wamn_postgres::StaticCredentialProvider;

    fn plugin() -> WamnPostgres {
        WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::default_only(None)))
    }

    fn field(value_type: StatementValueType, nullable: bool) -> StatementField {
        StatementField {
            value_type,
            nullable,
        }
    }

    fn verified(sql: &str) -> VerifiedStatement {
        VerifiedStatement {
            exact_sql: sql.into(),
            binds: [field(StatementValueType::Uuid, false)].into(),
            columns: [field(StatementValueType::Text, false)].into(),
        }
    }

    #[test]
    fn activation_exposes_only_the_selected_operation_and_revoke_closes_it() {
        let plugin = plugin();
        let first = verified("SELECT $1::uuid, 'first'::text");
        let first_digest = statement_digest(&first.exact_sql);
        let second = verified("SELECT $1::uuid, 'second'::text");
        let second_digest = statement_digest(&second.exact_sql);
        plugin
            .bind_statement_operation(
                "component",
                "first/get",
                BTreeMap::from([(first_digest.clone(), first)]),
            )
            .expect("bind first operation");
        plugin
            .bind_statement_operation(
                "component",
                "second/get",
                BTreeMap::from([(second_digest.clone(), second)]),
            )
            .expect("bind second operation");

        plugin
            .activate_statement_operation("component", "first/get")
            .expect("activate first operation");
        let active = plugin
            .active_statement_set("component")
            .expect("first operation is active");
        assert!(
            resolve_statement(Some(&active), &first_digest, &[SqlValue::Uuid("id".into())]).is_ok()
        );
        assert!(matches!(
            resolve_statement(Some(&active), &second_digest, &[SqlValue::Uuid("id".into())]),
            Err(StatementError::UnknownStatement(digest)) if digest == second_digest
        ));

        plugin
            .activate_statement_operation("component", "second/get")
            .expect("activate second operation");
        let active = plugin
            .active_statement_set("component")
            .expect("second operation is active");
        assert!(matches!(
            resolve_statement(Some(&active), &first_digest, &[SqlValue::Uuid("id".into())]),
            Err(StatementError::UnknownStatement(digest)) if digest == first_digest
        ));
        assert!(
            resolve_statement(
                Some(&active),
                &second_digest,
                &[SqlValue::Uuid("id".into())]
            )
            .is_ok()
        );

        plugin.revoke_statement_operation("component");
        assert!(plugin.active_statement_set("component").is_none());
    }

    #[test]
    fn exact_scope_cleanup_preserves_a_scope_with_the_same_prefix() {
        let plugin = plugin();
        let first = verified("SELECT $1::uuid, 'first'::text");
        let first_digest = statement_digest(&first.exact_sql);
        let second = verified("SELECT $1::uuid, 'second'::text");
        let second_digest = statement_digest(&second.exact_sql);
        plugin
            .bind_statement_operation(
                "workload",
                "orders/get",
                BTreeMap::from([(first_digest, first)]),
            )
            .expect("bind exact scope");
        plugin
            .bind_statement_operation(
                "workload-child",
                "orders/get",
                BTreeMap::from([(second_digest.clone(), second)]),
            )
            .expect("bind prefix-sharing scope");
        plugin
            .activate_statement_operation("workload", "orders/get")
            .expect("activate exact scope");
        plugin
            .activate_statement_operation("workload-child", "orders/get")
            .expect("activate prefix-sharing scope");

        plugin.clear_statement_scope("workload");

        assert!(plugin.active_statement_set("workload").is_none());
        assert!(
            plugin
                .activate_statement_operation("workload", "orders/get")
                .is_err(),
            "the exact scope's immutable binding must also be gone"
        );
        let active = plugin
            .active_statement_set("workload-child")
            .expect("prefix-sharing scope remains active");
        assert!(
            resolve_statement(
                Some(&active),
                &second_digest,
                &[SqlValue::Uuid("id".into())]
            )
            .is_ok()
        );
    }

    #[test]
    fn binding_refuses_a_digest_that_does_not_name_the_exact_sql_bytes() {
        let error = plugin()
            .bind_statement_operation(
                "component",
                "orders/get",
                BTreeMap::from([("sha256:wrong".to_owned(), verified("SELECT $1::uuid"))]),
            )
            .expect_err("digest drift must fail closed");
        assert!(error.to_string().starts_with("statement-digest-mismatch:"));
    }

    #[test]
    fn bind_mismatch_reports_expected_and_observed_shapes() {
        let statement = verified("SELECT $1::uuid");
        let digest = statement_digest(&statement.exact_sql);
        let set = BoundStatementSet(BTreeMap::from([(digest.clone(), Arc::new(statement))]));
        let error = resolve_statement(Some(&set), &digest, &[SqlValue::Text("id".into())])
            .expect_err("wrong carrier type must fail closed");
        let StatementError::StatementContractMismatch(mismatch) = error else {
            panic!("expected statement-contract-mismatch");
        };
        assert_eq!(mismatch.statement_digest, digest);
        assert_eq!(mismatch.part, ContractPart::Binds);
        assert_eq!(mismatch.expected.types, vec!["uuid".to_owned()]);
        assert_eq!(mismatch.observed.types, vec!["text".to_owned()]);
    }

    #[test]
    fn result_mismatch_reports_the_column_contract() {
        let statement = verified("SELECT $1::uuid");
        let digest = statement_digest(&statement.exact_sql);
        let result = RowSet {
            columns: vec![super::super::Column {
                name: "value".into(),
                type_name: "int8".into(),
            }],
            rows: vec![vec![SqlValue::Int64(1)]],
        };
        let error = validate_statement_result(&digest, &statement, result)
            .expect_err("wrong result type must fail closed");
        let StatementError::StatementContractMismatch(mismatch) = error else {
            panic!("expected statement-contract-mismatch");
        };
        assert_eq!(mismatch.part, ContractPart::Columns);
        assert_eq!(mismatch.expected.types, vec!["text".to_owned()]);
        assert_eq!(mismatch.observed.types, vec!["int64".to_owned()]);
    }
}

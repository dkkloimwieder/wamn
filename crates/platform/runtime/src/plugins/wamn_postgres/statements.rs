//! Host-owned resolution and contract checks for verified SQL statements.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::{
    Column, ContractMismatch, ContractPart, SqlValue, StatementError, ValueShape, WamnPostgres,
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
pub(super) struct BoundStatementSet {
    statements: BTreeMap<String, Arc<VerifiedStatement>>,
    /// The operation is a read (get, query or projection).
    read: bool,
}

/// One operation's statement set, keyed by each statement's package-relative
/// path and sealed -- the per-component half of the statement bind.
///
/// Nothing in here is per request: the admitted facts it was lowered from name
/// the component digest, so one of these serves every scope that binds the
/// operation. Opaque, so a set can be bound only as it was prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedStatementSet(Arc<BoundStatementSet>);

/// The statement set of each named `statements` import of one component,
/// keyed by import name -- the per-member half of the statement bind.
///
/// Each member of a composed component imports the interface under a name of
/// its own, and its set is its own operations' statements. Prepared once per
/// component, like [`PreparedStatementSet`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparedStatementMembers(Arc<BTreeMap<Box<str>, Arc<BoundStatementSet>>>);

/// The statements a scope may run while its operation is active.
#[derive(Debug)]
struct ActiveStatements {
    /// The set of the unnamed import: the active operation's own set.
    operation: Arc<BoundStatementSet>,
    /// The set of each named import.
    members: PreparedStatementMembers,
}

#[derive(Debug, Default)]
pub(super) struct StatementScopes {
    bindings: HashMap<String, HashMap<String, Arc<BoundStatementSet>>>,
    members: HashMap<String, PreparedStatementMembers>,
    active: HashMap<String, ActiveStatements>,
}

impl PreparedStatementSet {
    pub(super) fn is_read(&self) -> bool {
        self.0.read
    }
}

impl WamnPostgres {
    /// Seal one admitted operation's immutable statement set, once per
    /// component, so a scope can bind it. `read` is true for a get, query or
    /// projection operation.
    #[must_use]
    pub fn prepare_statement_set(
        statements: VerifiedStatementSet,
        read: bool,
    ) -> PreparedStatementSet {
        PreparedStatementSet(Arc::new(BoundStatementSet {
            statements: statements
                .into_iter()
                .map(|(path, statement)| (path, Arc::new(statement)))
                .collect(),
            read,
        }))
    }

    /// Seal the statement set of each named import of one component.
    #[must_use]
    pub fn prepare_statement_members(
        members: impl IntoIterator<Item = (String, PreparedStatementSet)>,
    ) -> PreparedStatementMembers {
        PreparedStatementMembers(Arc::new(
            members
                .into_iter()
                .map(|(name, statements)| (name.into_boxed_str(), statements.0))
                .collect(),
        ))
    }

    /// Bind the named-import sets of a component under a scope. Once per
    /// request, before [`Self::activate_statement_operation`].
    pub fn bind_prepared_statement_members(
        &self,
        component_id: &str,
        members: &PreparedStatementMembers,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!component_id.is_empty(), "statement-component-id-empty");
        let mut scopes = self
            .statement_scopes
            .write()
            .expect("statement scopes lock poisoned");
        if let Some(existing) = scopes.members.get(component_id) {
            anyhow::ensure!(
                existing == members,
                "statement-members-rebind-conflict: {component_id}"
            );
            return Ok(());
        }
        scopes
            .members
            .insert(component_id.to_owned(), members.clone());
        Ok(())
    }

    /// Bind a prepared set under a scope. Once per request.
    ///
    /// The rebind-conflict refusal stays here: a scope may not hold two
    /// different sets for one operation.
    pub fn bind_prepared_statement_operation(
        &self,
        component_id: &str,
        operation: &str,
        statements: &PreparedStatementSet,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!component_id.is_empty(), "statement-component-id-empty");
        anyhow::ensure!(!operation.is_empty(), "statement-operation-empty");
        let mut scopes = self
            .statement_scopes
            .write()
            .expect("statement scopes lock poisoned");
        let operations = scopes.bindings.entry(component_id.to_owned()).or_default();
        if let Some(existing) = operations.get(operation) {
            anyhow::ensure!(
                existing == &statements.0,
                "statement-operation-rebind-conflict: {operation}"
            );
            return Ok(());
        }
        operations.insert(operation.to_owned(), Arc::clone(&statements.0));
        Ok(())
    }

    /// Both halves in order, for a caller with one workload and one scope.
    pub fn bind_statement_operation(
        &self,
        component_id: &str,
        operation: &str,
        statements: VerifiedStatementSet,
    ) -> anyhow::Result<()> {
        let prepared = Self::prepare_statement_set(statements, false);
        self.bind_prepared_statement_operation(component_id, operation, &prepared)
    }

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
        let members = scopes
            .members
            .get(component_id)
            .cloned()
            .unwrap_or_default();
        scopes.active.insert(
            component_id.to_owned(),
            ActiveStatements {
                operation: statements,
                members,
            },
        );
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
        scopes.members.remove(component_id);
        scopes.active.remove(component_id);
    }

    /// The statements that the `statements` import named `instance` may run
    /// in `component_id` now. `None` names the unnamed import, which runs the
    /// active operation's set; a named import runs its member's set only.
    pub(super) fn active_statement_set(
        &self,
        component_id: &str,
        instance: Option<&str>,
    ) -> Option<Arc<BoundStatementSet>> {
        let scopes = self
            .statement_scopes
            .read()
            .expect("statement scopes lock poisoned");
        let active = scopes.active.get(component_id)?;
        match instance {
            None => Some(Arc::clone(&active.operation)),
            Some(instance) => active.members.0.get(instance).cloned(),
        }
    }

    /// Whether the operation active in `component_id` is a read.
    pub(super) fn active_read(&self, component_id: &str) -> bool {
        self.statement_scopes
            .read()
            .expect("statement scopes lock poisoned")
            .active
            .get(component_id)
            .is_some_and(|active| active.operation.read)
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
            .members
            .retain(|component_id, _| !component_id.starts_with(workload_id));
        scopes
            .active
            .retain(|component_id, _| !component_id.starts_with(workload_id));
    }
}

/// Resolve one admitted statement. A read operation (`read`) has no write
/// authority, so a statement that writes or locks rows is unknown to it.
pub(super) fn resolve_statement(
    statements: Option<&BoundStatementSet>,
    read: bool,
    statement_path: &str,
    binds: &[SqlValue],
) -> Result<Arc<VerifiedStatement>, StatementError> {
    let statement = statements
        .and_then(|statements| statements.statements.get(statement_path))
        .filter(|statement| !(read && statement.transactional))
        .cloned()
        .ok_or_else(|| StatementError::UnknownStatement(statement_path.to_owned()))?;
    if !binds_match(&statement.binds, binds) {
        return Err(contract_mismatch(
            statement_path,
            ContractPart::Binds,
            expected_shape(&statement.binds),
            observed_bind_shape(&statement.binds, binds),
        ));
    }
    Ok(statement)
}

pub(super) fn validate_statement_result(
    statement_path: &str,
    statement: &VerifiedStatement,
    columns: &[Column],
    rows: &[Vec<SqlValue>],
) -> Result<(), StatementError> {
    let columns_match = columns.len() == statement.columns.len()
        && columns
            .iter()
            .zip(&statement.columns)
            .all(|(column, expected)| {
                statement_type_for_postgres(&column.type_name) == Some(expected.value_type)
            });
    let values_match = rows.iter().all(|row| {
        row.len() == statement.columns.len()
            && row
                .iter()
                .zip(&statement.columns)
                .all(|(value, expected)| !matches!(value, SqlValue::Null) || expected.nullable)
    });
    if columns_match && values_match {
        return Ok(());
    }

    let mut observed_types: Vec<String> = columns
        .iter()
        .map(|column| {
            statement_type_for_postgres(&column.type_name).map_or_else(
                || column.type_name.clone(),
                |value_type| value_type.as_str().into(),
            )
        })
        .collect();
    for row in rows {
        for (index, (value, expected)) in row.iter().zip(&statement.columns).enumerate() {
            if matches!(value, SqlValue::Null)
                && !expected.nullable
                && let Some(observed) = observed_types.get_mut(index)
            {
                "null".clone_into(observed);
            }
        }
    }
    Err(contract_mismatch(
        statement_path,
        ContractPart::Columns,
        expected_shape(&statement.columns),
        ValueShape {
            count: bounded_count(columns.len()),
            types: observed_types,
        },
    ))
}

pub(super) fn validate_prepared_statement(
    statement_path: &str,
    statement: &VerifiedStatement,
    prepared: &tokio_postgres::Statement,
) -> Result<(), StatementError> {
    if !postgres_types_match(
        &statement.binds,
        prepared
            .params()
            .iter()
            .map(tokio_postgres::types::Type::name),
    ) {
        return Err(contract_mismatch(
            statement_path,
            ContractPart::Binds,
            expected_shape(&statement.binds),
            ValueShape {
                count: bounded_count(prepared.params().len()),
                types: prepared
                    .params()
                    .iter()
                    .map(|value_type| normalized_postgres_name(value_type.name()))
                    .collect(),
            },
        ));
    }

    if !postgres_types_match(
        &statement.columns,
        prepared
            .columns()
            .iter()
            .map(|column| column.type_().name()),
    ) {
        return Err(contract_mismatch(
            statement_path,
            ContractPart::Columns,
            expected_shape(&statement.columns),
            ValueShape {
                count: bounded_count(prepared.columns().len()),
                types: prepared
                    .columns()
                    .iter()
                    .map(|column| normalized_postgres_name(column.type_().name()))
                    .collect(),
            },
        ));
    }
    Ok(())
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
    statement_path: &str,
    part: ContractPart,
    expected: ValueShape,
    observed: ValueShape,
) -> StatementError {
    StatementError::StatementContractMismatch(ContractMismatch {
        statement: statement_path.to_owned(),
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
            // These fixtures are SELECTs, which is what PostgreSQL classifies as
            // needing no transaction.
            transactional: false,
        }
    }

    #[test]
    fn activation_exposes_only_the_selected_operation_and_revoke_closes_it() {
        let plugin = plugin();
        let first = verified("SELECT $1::uuid, 'first'::text");
        let first_path = "sql/first.sql".to_owned();
        let second = verified("SELECT $1::uuid, 'second'::text");
        let second_path = "sql/second.sql".to_owned();
        plugin
            .bind_statement_operation(
                "component",
                "first/get",
                BTreeMap::from([(first_path.clone(), first)]),
            )
            .expect("bind first operation");
        plugin
            .bind_statement_operation(
                "component",
                "second/get",
                BTreeMap::from([(second_path.clone(), second)]),
            )
            .expect("bind second operation");

        plugin
            .activate_statement_operation("component", "first/get")
            .expect("activate first operation");
        let active = plugin
            .active_statement_set("component", None)
            .expect("first operation is active");
        assert!(
            resolve_statement(
                Some(&active),
                false,
                &first_path,
                &[SqlValue::Uuid("id".into())]
            )
            .is_ok()
        );
        assert!(matches!(
            resolve_statement(Some(&active), false, &second_path, &[SqlValue::Uuid("id".into())]),
            Err(StatementError::UnknownStatement(path)) if path == second_path
        ));

        plugin
            .activate_statement_operation("component", "second/get")
            .expect("activate second operation");
        let active = plugin
            .active_statement_set("component", None)
            .expect("second operation is active");
        assert!(matches!(
            resolve_statement(Some(&active), false, &first_path, &[SqlValue::Uuid("id".into())]),
            Err(StatementError::UnknownStatement(path)) if path == first_path
        ));
        assert!(
            resolve_statement(
                Some(&active),
                false,
                &second_path,
                &[SqlValue::Uuid("id".into())]
            )
            .is_ok()
        );

        plugin.revoke_statement_operation("component");
        assert!(plugin.active_statement_set("component", None).is_none());
    }

    #[test]
    fn each_named_import_runs_only_its_member_set_while_the_operation_is_active() {
        let plugin = plugin();
        let id = [SqlValue::Uuid("id".into())];
        let entry = "sql/entry.sql";
        let base = "sql/base.sql";
        let prepared = |path: &str| {
            WamnPostgres::prepare_statement_set(
                BTreeMap::from([(path.to_owned(), verified("SELECT $1::uuid, 'x'::text"))]),
                false,
            )
        };
        let members = WamnPostgres::prepare_statement_members([
            ("sql-0".to_owned(), prepared(base)),
            ("sql-1".to_owned(), prepared(entry)),
        ]);
        plugin
            .bind_prepared_statement_members("component", &members)
            .expect("bind the member sets");
        plugin
            .bind_statement_operation(
                "component",
                "entry/run",
                BTreeMap::from([
                    (entry.to_owned(), verified("SELECT $1::uuid, 'x'::text")),
                    (base.to_owned(), verified("SELECT $1::uuid, 'x'::text")),
                ]),
            )
            .expect("bind the folded operation set");
        assert!(
            plugin
                .active_statement_set("component", Some("sql-0"))
                .is_none(),
            "a member set is not active before its operation"
        );
        plugin
            .activate_statement_operation("component", "entry/run")
            .expect("activate the operation");

        for (instance, own, other) in [("sql-0", base, entry), ("sql-1", entry, base)] {
            let active = plugin
                .active_statement_set("component", Some(instance))
                .expect("the member set is active");
            assert!(resolve_statement(Some(&active), false, own, &id).is_ok());
            assert!(matches!(
                resolve_statement(Some(&active), false, other, &id),
                Err(StatementError::UnknownStatement(path)) if path == other
            ));
        }
        assert!(
            plugin
                .active_statement_set("component", Some("sql-2"))
                .is_none(),
            "an import with no member runs nothing"
        );

        plugin.revoke_statement_operation("component");
        assert!(
            plugin
                .active_statement_set("component", Some("sql-0"))
                .is_none()
        );
    }

    #[test]
    fn exact_scope_cleanup_preserves_a_scope_with_the_same_prefix() {
        let plugin = plugin();
        let first = verified("SELECT $1::uuid, 'first'::text");
        let first_path = "sql/first.sql".to_owned();
        let second = verified("SELECT $1::uuid, 'second'::text");
        let second_path = "sql/second.sql".to_owned();
        plugin
            .bind_statement_operation(
                "workload",
                "orders/get",
                BTreeMap::from([(first_path, first)]),
            )
            .expect("bind exact scope");
        plugin
            .bind_statement_operation(
                "workload-child",
                "orders/get",
                BTreeMap::from([(second_path.clone(), second)]),
            )
            .expect("bind prefix-sharing scope");
        plugin
            .activate_statement_operation("workload", "orders/get")
            .expect("activate exact scope");
        plugin
            .activate_statement_operation("workload-child", "orders/get")
            .expect("activate prefix-sharing scope");

        plugin.clear_statement_scope("workload");

        assert!(plugin.active_statement_set("workload", None).is_none());
        assert!(
            plugin
                .activate_statement_operation("workload", "orders/get")
                .is_err(),
            "the exact scope's immutable binding must also be gone"
        );
        let active = plugin
            .active_statement_set("workload-child", None)
            .expect("prefix-sharing scope remains active");
        assert!(
            resolve_statement(
                Some(&active),
                false,
                &second_path,
                &[SqlValue::Uuid("id".into())]
            )
            .is_ok()
        );
    }

    /// A read operation refuses an admitted statement that writes, and runs a
    /// read-only one; a command still resolves the writing statement
    /// (`wamn-zckip`).
    #[test]
    fn a_read_operation_refuses_a_writing_statement_and_a_command_runs_it() {
        let plugin = plugin();
        let id = [SqlValue::Uuid("id".into())];
        let select = "sql/select.sql";
        let update = "sql/update.sql";
        let statements = || {
            BTreeMap::from([
                (select.to_owned(), verified("SELECT $1::uuid, 'x'::text")),
                (
                    update.to_owned(),
                    VerifiedStatement {
                        transactional: true,
                        ..verified("UPDATE t SET v = 'x' WHERE id = $1 RETURNING v")
                    },
                ),
            ])
        };
        for (operation, read) in [("orders/get", true), ("orders/record", false)] {
            plugin
                .bind_prepared_statement_operation(
                    "component",
                    operation,
                    &WamnPostgres::prepare_statement_set(statements(), read),
                )
                .expect("bind the operation");
        }

        plugin
            .activate_statement_operation("component", "orders/get")
            .expect("activate the read");
        let read = plugin.active_read("component");
        assert!(read);
        let active = plugin
            .active_statement_set("component", None)
            .expect("the read is active");
        assert!(resolve_statement(Some(&active), read, select, &id).is_ok());
        assert!(matches!(
            resolve_statement(Some(&active), read, update, &id),
            Err(StatementError::UnknownStatement(path)) if path == update
        ));

        plugin
            .activate_statement_operation("component", "orders/record")
            .expect("activate the command");
        let read = plugin.active_read("component");
        assert!(!read);
        let active = plugin
            .active_statement_set("component", None)
            .expect("the command is active");
        assert!(resolve_statement(Some(&active), read, update, &id).is_ok());
    }

    #[test]
    fn bind_mismatch_reports_expected_and_observed_shapes() {
        let statement = verified("SELECT $1::uuid");
        let path = "sql/statement.sql".to_owned();
        let set = BoundStatementSet {
            statements: BTreeMap::from([(path.clone(), Arc::new(statement))]),
            read: false,
        };
        let error = resolve_statement(Some(&set), false, &path, &[SqlValue::Text("id".into())])
            .expect_err("wrong carrier type must fail closed");
        let StatementError::StatementContractMismatch(mismatch) = error else {
            panic!("expected statement-contract-mismatch");
        };
        assert_eq!(mismatch.statement, path);
        assert_eq!(mismatch.part, ContractPart::Binds);
        assert_eq!(mismatch.expected.types, vec!["uuid".to_owned()]);
        assert_eq!(mismatch.observed.types, vec!["text".to_owned()]);
    }

    #[test]
    fn result_mismatch_reports_the_column_contract() {
        let statement = verified("SELECT $1::uuid");
        let path = "sql/statement.sql".to_owned();
        let result = super::super::RowSet {
            wal_position: None,
            columns: vec![super::super::Column {
                name: "value".into(),
                type_name: "int8".into(),
            }],
            rows: vec![vec![SqlValue::Int64(1)]],
        };
        let error = validate_statement_result(&path, &statement, &result.columns, &result.rows)
            .expect_err("wrong result type must fail closed");
        let StatementError::StatementContractMismatch(mismatch) = error else {
            panic!("expected statement-contract-mismatch");
        };
        assert_eq!(mismatch.part, ContractPart::Columns);
        assert_eq!(mismatch.expected.types, vec!["text".to_owned()]);
        assert_eq!(mismatch.observed.types, vec!["int64".to_owned()]);
    }
}

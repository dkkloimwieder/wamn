//! The gateway router: parse an HTTP request against a [`Catalog`], validate
//! every identifier against it, and compile an injection-safe parameterized
//! query.
//!
//! Catalog cross-references are by **id** (`Reference{entity}`,
//! `Relation.from/to/through` are entity ids; `from_field` is a field id), while
//! the physical SQL identifiers the DDL compiler (3.2) emits are the **names**
//! (`CREATE TABLE "<entity.name>"`, column `"<field.name>"`). So the router
//! resolves by id and emits by name, quoting every name with
//! [`quote_ident`](wamn_ddl::sql::quote_ident).

use std::collections::HashMap;

use serde_json::Value;
use wamn_catalog::{Catalog, Entity, Field, FieldType};
use wamn_ddl::sql::quote_ident;

use crate::error::ApiError;
use crate::value::SqlValue;

/// Default REST base path; a route is `<base>/{entity}[/{id}]`.
pub const DEFAULT_BASE_PATH: &str = "/api/rest";
/// Hard cap on a page's size (the real limiter is 4.6).
pub const DEFAULT_MAX_PAGE_SIZE: u32 = 100;
/// Page size when the request omits `limit`.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// An HTTP method the gateway understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    /// Parse an HTTP method name (case-insensitive). Unknown methods → `None`.
    pub fn from_http(s: &str) -> Option<Method> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Some(Method::Get),
            "POST" => Some(Method::Post),
            "PUT" => Some(Method::Put),
            "PATCH" => Some(Method::Patch),
            "DELETE" => Some(Method::Delete),
            _ => None,
        }
    }
}

/// A compiled statement: the SQL template, its ordered `$n` parameters, and the
/// projected column names (in the same order the row cells come back), which
/// shaping keys the response object on.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub(crate) sql: String,
    pub(crate) params: Vec<SqlValue>,
    pub(crate) columns: Vec<String>,
}

impl Compiled {
    /// The primary SQL statement (with `$n` placeholders).
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// The bound parameters, in `$n` order.
    pub fn params(&self) -> &[SqlValue] {
        &self.params
    }

    /// The projected column names, in order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }
}

/// The direction of a one-level relation expansion relative to the resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpandDir {
    /// The resource holds the foreign key → embed the single parent it points at.
    ToOne,
    /// The resource is the parent → embed the array of children pointing at it.
    ToMany,
}

/// A resolved one-level expansion, ready to be executed once the primary rows
/// are known: run `SELECT columns FROM target_table WHERE match_column IN (…)`
/// over the distinct `key_column` values of the primary rows, then attach.
#[derive(Debug, Clone, PartialEq)]
pub struct Expand {
    /// Relation name — the response key the embedded record(s) land under.
    pub(crate) name: String,
    pub(crate) dir: ExpandDir,
    /// The primary-row column that supplies the join keys.
    pub(crate) key_column: String,
    /// The related table to read from.
    pub(crate) target_table: String,
    /// The related-table column that echoes the join key.
    pub(crate) match_column: String,
    /// The projected columns of the related table.
    pub(crate) columns: Vec<String>,
}

impl Expand {
    /// Relation name — the response key the embedded record(s) land under.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The direction of the expansion relative to the resource.
    pub fn dir(&self) -> ExpandDir {
        self.dir
    }

    /// The primary-row column that supplies the join keys.
    pub fn key_column(&self) -> &str {
        &self.key_column
    }

    /// The related table to read from.
    pub fn target_table(&self) -> &str {
        &self.target_table
    }

    /// The related-table column that echoes the join key.
    pub fn match_column(&self) -> &str {
        &self.match_column
    }

    /// The projected columns of the related table.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }
}

/// What kind of operation a [`Plan`] carries (drives response cardinality:
/// list → array, the singular kinds → object / 404 / 204).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlanKind {
    List,
    GetOne,
    CreateOne,
    UpdateOne,
    DeleteOne,
}

/// A fully compiled request: the primary statement, any expansions to run
/// afterward, and the HTTP status a success returns.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub(crate) kind: PlanKind,
    pub(crate) query: Compiled,
    pub(crate) expands: Vec<Expand>,
    pub(crate) status: u16,
}

impl Plan {
    /// The kind of operation (drives response cardinality).
    pub fn kind(&self) -> PlanKind {
        self.kind
    }

    /// The primary compiled statement.
    pub fn query(&self) -> &Compiled {
        &self.query
    }

    /// The one-level expansions to run after the primary query.
    pub fn expands(&self) -> &[Expand] {
        &self.expands
    }

    /// The HTTP status a success returns.
    pub fn status(&self) -> u16 {
        self.status
    }
}

/// Builds the `$n` parameter list, keeping placeholder numbers in lockstep with
/// the params vector so an interpolation gap is structurally impossible.
struct ParamBuilder {
    params: Vec<SqlValue>,
}

impl ParamBuilder {
    fn new() -> Self {
        Self { params: Vec::new() }
    }

    /// Bind a value and return its `$n` placeholder.
    fn bind(&mut self, v: SqlValue) -> String {
        self.params.push(v);
        format!("${}", self.params.len())
    }
}

/// Compiles requests for one catalog. Cheap to construct (borrows the catalog,
/// precomputes name/id indexes); rebuild it when the catalog snapshot changes.
pub struct Router<'a> {
    catalog: &'a Catalog,
    by_name: HashMap<&'a str, &'a Entity>,
    by_id: HashMap<&'a str, &'a Entity>,
    base_path: String,
    max_page_size: u32,
    default_page_size: u32,
}

impl<'a> Router<'a> {
    /// Build a router over a catalog snapshot.
    pub fn new(catalog: &'a Catalog) -> Self {
        let mut by_name = HashMap::new();
        let mut by_id = HashMap::new();
        for e in &catalog.entities {
            by_name.insert(e.name.as_str(), e);
            by_id.insert(e.id.as_str(), e);
        }
        Self {
            catalog,
            by_name,
            by_id,
            base_path: DEFAULT_BASE_PATH.to_string(),
            max_page_size: DEFAULT_MAX_PAGE_SIZE,
            default_page_size: DEFAULT_PAGE_SIZE,
        }
    }

    /// Override the REST base path (default `/api/rest`).
    pub fn with_base_path(mut self, p: impl Into<String>) -> Self {
        self.base_path = p.into();
        self
    }

    /// Override the max page size (v1's only paging limiter; 4.6 hardens it).
    pub fn with_max_page_size(mut self, n: u32) -> Self {
        self.max_page_size = n.max(1);
        if self.default_page_size > self.max_page_size {
            self.default_page_size = self.max_page_size;
        }
        self
    }

    /// Compile a request into a [`Plan`], or reject it with an [`ApiError`].
    ///
    /// `query` is the decoded query-string pairs; `body` the parsed JSON body
    /// (for writes). No SQL is produced until every identifier has resolved
    /// against the catalog and every value has typed against its field.
    pub fn compile(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Plan, ApiError> {
        let (segment, id) = self.split_route(path)?;
        let entity = self.entity_by_name(segment)?;
        match (method, id) {
            (Method::Get, None) => self.compile_list(entity, query),
            (Method::Get, Some(id)) => self.compile_get(entity, id, query),
            (Method::Post, None) => {
                let body = body.ok_or(ApiError::PayloadRequired)?;
                self.compile_create(entity, body)
            }
            (Method::Put | Method::Patch, Some(id)) => {
                let body = body.ok_or(ApiError::PayloadRequired)?;
                // PUT is a full replace, PATCH a partial merge (see compile_update).
                self.compile_update(entity, id, body, matches!(method, Method::Put))
            }
            (Method::Delete, Some(id)) => self.compile_delete(entity, id),
            _ => Err(ApiError::MethodNotAllowed),
        }
    }

    /// Build the SQL for one expansion given the distinct primary-side keys.
    /// The keys are bound as `$n` parameters (an `IN` list); an empty key set
    /// yields a never-matching query so callers can run it uniformly.
    pub fn build_expand(&self, ex: &Expand, keys: &[SqlValue]) -> Compiled {
        let proj = quote_all(&ex.columns);
        let tbl = quote_ident(&ex.target_table);
        let mcol = quote_ident(&ex.match_column);
        if keys.is_empty() {
            return Compiled {
                sql: format!("SELECT {proj} FROM {tbl} WHERE 1 = 0"),
                params: Vec::new(),
                columns: ex.columns.clone(),
            };
        }
        let placeholders = (1..=keys.len())
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(", ");
        Compiled {
            sql: format!("SELECT {proj} FROM {tbl} WHERE {mcol} IN ({placeholders})"),
            params: keys.to_vec(),
            columns: ex.columns.clone(),
        }
    }

    // ---- route / entity / field resolution -------------------------------

    /// Split `<base>/{entity}[/{id}]`. The base must match on a segment
    /// boundary (so `/api/restaurants` does not match base `/api/rest`).
    fn split_route<'p>(&self, path: &'p str) -> Result<(&'p str, Option<&'p str>), ApiError> {
        let rest = path
            .strip_prefix(&self.base_path)
            .ok_or(ApiError::NotFound)?;
        if !rest.is_empty() && !rest.starts_with('/') {
            return Err(ApiError::NotFound);
        }
        let rest = rest.trim_matches('/');
        if rest.is_empty() {
            return Err(ApiError::NotFound);
        }
        let mut it = rest.splitn(3, '/');
        let entity = it.next().unwrap_or("");
        let id = it.next().filter(|s| !s.is_empty());
        if it.next().is_some() {
            return Err(ApiError::NotFound); // more than {entity}/{id}
        }
        Ok((entity, id))
    }

    fn entity_by_name(&self, name: &str) -> Result<&'a Entity, ApiError> {
        self.by_name
            .get(name)
            .copied()
            .ok_or_else(|| ApiError::UnknownEntity(name.to_string()))
    }

    fn field_by_name<'e>(&self, entity: &'e Entity, name: &str) -> Result<&'e Field, ApiError> {
        entity
            .fields
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| ApiError::UnknownField {
                entity: entity.name.clone(),
                field: name.to_string(),
            })
    }

    // ---- per-operation compilers -----------------------------------------

    fn compile_list(&self, entity: &Entity, query: &[(String, String)]) -> Result<Plan, ApiError> {
        let mut pb = ParamBuilder::new();
        let mut where_clauses: Vec<String> = Vec::new();
        let mut order_by: Vec<String> = Vec::new();
        let mut expand_names: Vec<String> = Vec::new();
        let mut limit: Option<u32> = None;
        let mut offset: u64 = 0;

        for (key, raw) in query {
            match key.as_str() {
                "sort" => {
                    for part in raw.split(',') {
                        let part = part.trim();
                        if part.is_empty() {
                            continue;
                        }
                        let (name, desc) = match part.strip_prefix('-') {
                            Some(r) => (r, true),
                            None => (part.strip_prefix('+').unwrap_or(part), false),
                        };
                        let field = self.field_by_name(entity, name)?;
                        order_by.push(format!(
                            "{} {}",
                            quote_ident(&field.name),
                            if desc { "DESC" } else { "ASC" }
                        ));
                    }
                }
                "limit" => {
                    let n: u32 = raw.trim().parse().map_err(|_| {
                        ApiError::InvalidRequest("limit must be a non-negative integer".into())
                    })?;
                    limit = Some(n);
                }
                "offset" => {
                    offset = raw.trim().parse().map_err(|_| {
                        ApiError::InvalidRequest("offset must be a non-negative integer".into())
                    })?;
                }
                "expand" => collect_names(raw, &mut expand_names),
                _ => {
                    let field = self.field_by_name(entity, key)?;
                    let (op, value) = split_op(raw);
                    where_clauses.push(self.filter_clause(&mut pb, field, op, value)?);
                }
            }
        }

        let mut sql = format!(
            "SELECT {} FROM {}",
            projection_sql(entity),
            quote_ident(&entity.name)
        );
        if !where_clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&where_clauses.join(" AND "));
        }
        if order_by.is_empty() {
            order_by.push(format!("{} ASC", quote_ident("id")));
        }
        sql.push_str(" ORDER BY ");
        sql.push_str(&order_by.join(", "));

        let limit = limit
            .unwrap_or(self.default_page_size)
            .min(self.max_page_size);
        // Parsed as u64 (rejects negatives); reject a value past i64::MAX rather
        // than letting `as i64` wrap to a negative OFFSET the database refuses.
        let offset = i64::try_from(offset)
            .map_err(|_| ApiError::InvalidRequest("offset out of range".into()))?;
        let lp = pb.bind(SqlValue::Int64(i64::from(limit)));
        let op = pb.bind(SqlValue::Int64(offset));
        sql.push_str(&format!(" LIMIT {lp} OFFSET {op}"));

        let expands = self.resolve_expands(entity, &expand_names)?;
        Ok(Plan {
            kind: PlanKind::List,
            query: Compiled {
                sql,
                params: pb.params,
                columns: projection(entity),
            },
            expands,
            status: 200,
        })
    }

    fn compile_get(
        &self,
        entity: &Entity,
        id: &str,
        query: &[(String, String)],
    ) -> Result<Plan, ApiError> {
        require_uuid(id)?;
        let mut expand_names = Vec::new();
        for (key, raw) in query {
            if key == "expand" {
                collect_names(raw, &mut expand_names);
            }
        }
        let mut pb = ParamBuilder::new();
        let idp = pb.bind(SqlValue::Uuid(id.to_string()));
        let sql = format!(
            "SELECT {} FROM {} WHERE {} = {idp}",
            projection_sql(entity),
            quote_ident(&entity.name),
            quote_ident("id"),
        );
        let expands = self.resolve_expands(entity, &expand_names)?;
        Ok(Plan {
            kind: PlanKind::GetOne,
            query: Compiled {
                sql,
                params: pb.params,
                columns: projection(entity),
            },
            expands,
            status: 200,
        })
    }

    fn compile_create(&self, entity: &Entity, body: &Value) -> Result<Plan, ApiError> {
        let obj = body
            .as_object()
            .ok_or_else(|| ApiError::InvalidRequest("request body must be a JSON object".into()))?;
        self.reject_unknown_keys(entity, obj.keys(), "set")?;
        // Every required (non-nullable, un-defaulted) field must be present.
        for f in &entity.fields {
            if !f.nullable && f.default.is_none() && !obj.contains_key(&f.name) {
                return Err(ApiError::InvalidValue {
                    field: f.name.clone(),
                    message: "required".into(),
                });
            }
        }

        let mut pb = ParamBuilder::new();
        // tenant_id is set from the session claim server-side — never from the
        // request — so the 3.2 floor's WITH CHECK is satisfied without a param.
        let mut cols = vec![quote_ident("tenant_id")];
        let mut vals = vec!["current_setting('app.tenant', true)".to_string()];
        for f in &entity.fields {
            if let Some(v) = obj.get(&f.name) {
                cols.push(quote_ident(&f.name));
                vals.push(pb.bind(value_for_field(f, v)?));
            }
        }
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING {}",
            quote_ident(&entity.name),
            cols.join(", "),
            vals.join(", "),
            projection_sql(entity),
        );
        Ok(Plan {
            kind: PlanKind::CreateOne,
            query: Compiled {
                sql,
                params: pb.params,
                columns: projection(entity),
            },
            expands: Vec::new(),
            status: 201,
        })
    }

    /// Compile an update. `replace` selects the semantics:
    ///
    /// * **PATCH** (`replace = false`) is a partial merge — only the fields
    ///   present in the body are `SET`; a body with no writable fields is a 400.
    /// * **PUT** (`replace = true`) is a full replace — *every* writable field is
    ///   `SET`. A present field is bound as a `$n` parameter; an omitted one is
    ///   reset to its column `DEFAULT` (NULL for a nullable no-default column,
    ///   else the declared default), and an omitted **required** field (not
    ///   nullable, no default) is rejected exactly as create rejects it. `DEFAULT`
    ///   is a SQL keyword, not user input, so this stays injection-safe.
    fn compile_update(
        &self,
        entity: &Entity,
        id: &str,
        body: &Value,
        replace: bool,
    ) -> Result<Plan, ApiError> {
        require_uuid(id)?;
        let obj = body
            .as_object()
            .ok_or_else(|| ApiError::InvalidRequest("request body must be a JSON object".into()))?;
        self.reject_unknown_keys(entity, obj.keys(), "update")?;
        if !replace && obj.is_empty() {
            return Err(ApiError::InvalidRequest("no fields to update".into()));
        }

        let mut pb = ParamBuilder::new();
        let mut sets = Vec::new();
        for f in &entity.fields {
            match obj.get(&f.name) {
                Some(v) => sets.push(format!(
                    "{} = {}",
                    quote_ident(&f.name),
                    pb.bind(value_for_field(f, v)?)
                )),
                // PATCH leaves an omitted field untouched; PUT resets it.
                None if replace => {
                    if !f.nullable && f.default.is_none() {
                        return Err(ApiError::InvalidValue {
                            field: f.name.clone(),
                            message: "required".into(),
                        });
                    }
                    sets.push(format!("{} = DEFAULT", quote_ident(&f.name)));
                }
                None => {}
            }
        }
        let idp = pb.bind(SqlValue::Uuid(id.to_string()));
        let sql = format!(
            "UPDATE {} SET {} WHERE {} = {idp} RETURNING {}",
            quote_ident(&entity.name),
            sets.join(", "),
            quote_ident("id"),
            projection_sql(entity),
        );
        Ok(Plan {
            kind: PlanKind::UpdateOne,
            query: Compiled {
                sql,
                params: pb.params,
                columns: projection(entity),
            },
            expands: Vec::new(),
            status: 200,
        })
    }

    fn compile_delete(&self, entity: &Entity, id: &str) -> Result<Plan, ApiError> {
        require_uuid(id)?;
        let mut pb = ParamBuilder::new();
        let idp = pb.bind(SqlValue::Uuid(id.to_string()));
        let sql = format!(
            "DELETE FROM {} WHERE {} = {idp} RETURNING {}",
            quote_ident(&entity.name),
            quote_ident("id"),
            quote_ident("id"),
        );
        Ok(Plan {
            kind: PlanKind::DeleteOne,
            query: Compiled {
                sql,
                params: pb.params,
                columns: vec!["id".to_string()],
            },
            expands: Vec::new(),
            status: 204,
        })
    }

    // ---- helpers ----------------------------------------------------------

    /// Reject a write body key that names no field, or a managed column.
    fn reject_unknown_keys<'k>(
        &self,
        entity: &Entity,
        keys: impl Iterator<Item = &'k String>,
        verb: &str,
    ) -> Result<(), ApiError> {
        for k in keys {
            if k == "id" || k == "tenant_id" {
                return Err(ApiError::InvalidValue {
                    field: k.clone(),
                    message: format!("managed column cannot be {verb}"),
                });
            }
            if !entity.fields.iter().any(|f| &f.name == k) {
                return Err(ApiError::UnknownField {
                    entity: entity.name.clone(),
                    field: k.clone(),
                });
            }
        }
        Ok(())
    }

    /// Build one filter clause, binding its value(s) as parameters.
    fn filter_clause(
        &self,
        pb: &mut ParamBuilder,
        field: &Field,
        op: &str,
        raw: &str,
    ) -> Result<String, ApiError> {
        let col = quote_ident(&field.name);
        match op {
            "in" => {
                let mut placeholders = Vec::new();
                for part in raw.split(',') {
                    placeholders.push(pb.bind(value_for_field_str(field, part.trim())?));
                }
                if placeholders.is_empty() {
                    Ok("1 = 0".to_string())
                } else {
                    Ok(format!("{col} IN ({})", placeholders.join(", ")))
                }
            }
            "like" => match &field.field_type {
                FieldType::Text { .. } | FieldType::Enum { .. } => {
                    let p = pb.bind(SqlValue::Text(raw.to_string()));
                    Ok(format!("{col} LIKE {p}"))
                }
                _ => Err(ApiError::InvalidValue {
                    field: field.name.clone(),
                    message: "like is only supported on text fields".into(),
                }),
            },
            _ => {
                // split_op only ever yields an operator from the allowlist.
                let sqlop = sql_operator(op).unwrap_or("=");
                let p = pb.bind(value_for_field_str(field, raw)?);
                Ok(format!("{col} {sqlop} {p}"))
            }
        }
    }

    /// Resolve `?expand=` relation names into executable [`Expand`]s.
    fn resolve_expands(&self, entity: &Entity, names: &[String]) -> Result<Vec<Expand>, ApiError> {
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let unknown = || ApiError::UnknownRelation {
                entity: entity.name.clone(),
                relation: name.clone(),
            };
            let rel = self
                .catalog
                .relations
                .iter()
                .find(|r| &r.name == name && (r.from == entity.id || r.to == entity.id))
                .ok_or_else(unknown)?;
            let from_field_id = rel.from_field.as_ref().ok_or_else(unknown)?;

            if rel.from == entity.id {
                // This entity holds the FK → embed the single parent (to-one).
                let target = self
                    .by_id
                    .get(rel.to.as_str())
                    .copied()
                    .ok_or_else(unknown)?;
                let fk = entity
                    .fields
                    .iter()
                    .find(|f| &f.id == from_field_id)
                    .ok_or_else(unknown)?;
                out.push(Expand {
                    name: name.clone(),
                    dir: ExpandDir::ToOne,
                    key_column: fk.name.clone(),
                    target_table: target.name.clone(),
                    match_column: "id".to_string(),
                    columns: projection(target),
                });
            } else {
                // Children point at this entity → embed the array (to-many).
                let child = self
                    .by_id
                    .get(rel.from.as_str())
                    .copied()
                    .ok_or_else(unknown)?;
                let fk = child
                    .fields
                    .iter()
                    .find(|f| &f.id == from_field_id)
                    .ok_or_else(unknown)?;
                out.push(Expand {
                    name: name.clone(),
                    dir: ExpandDir::ToMany,
                    key_column: "id".to_string(),
                    target_table: child.name.clone(),
                    match_column: fk.name.clone(),
                    columns: projection(child),
                });
            }
        }
        Ok(out)
    }
}

// ---- free helpers ---------------------------------------------------------

/// The projected columns for an entity: the managed `id` plus every user field,
/// in field order. `tenant_id` is deliberately never exposed.
fn projection(entity: &Entity) -> Vec<String> {
    let mut cols = Vec::with_capacity(entity.fields.len() + 1);
    cols.push("id".to_string());
    cols.extend(entity.fields.iter().map(|f| f.name.clone()));
    cols
}

fn projection_sql(entity: &Entity) -> String {
    quote_all(&projection(entity))
}

fn quote_all(cols: &[String]) -> String {
    cols.iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Map a filter operator token to its SQL. `in` / `like` are special-cased by
/// the caller; this covers the comparison operators.
fn sql_operator(op: &str) -> Option<&'static str> {
    match op {
        "eq" => Some("="),
        "neq" => Some("<>"),
        "lt" => Some("<"),
        "lte" => Some("<="),
        "gt" => Some(">"),
        "gte" => Some(">="),
        "like" => Some("LIKE"),
        "in" => Some("IN"),
        _ => None,
    }
}

/// Split a filter value into `(operator, value)`. A leading `op.` is only
/// honored when `op` is a real operator, so a bare value (including one that
/// contains a dot, like `12.50`) is treated as `eq`.
fn split_op(v: &str) -> (&str, &str) {
    if let Some((maybe_op, rest)) = v.split_once('.')
        && sql_operator(maybe_op).is_some()
    {
        return (maybe_op, rest);
    }
    ("eq", v)
}

/// Push comma-separated names from a query value onto `out`.
fn collect_names(raw: &str, out: &mut Vec<String>) {
    for part in raw.split(',') {
        let p = part.trim();
        if !p.is_empty() {
            out.push(p.to_string());
        }
    }
}

fn require_uuid(id: &str) -> Result<(), ApiError> {
    if is_uuid(id) {
        Ok(())
    } else {
        Err(ApiError::InvalidValue {
            field: "id".into(),
            message: "not a valid uuid".into(),
        })
    }
}

/// Cheap **format** check (8-4-4-4-12 hex) — it accepts any hex in the
/// version/variant nibbles, so it is not a variant/version-valid UUID check.
/// The database column type is the real backstop that rejects a malformed
/// value; this only screens the request shape at the edge. Avoids a `uuid`
/// dependency.
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => c.is_ascii_hexdigit(),
    })
}

/// Type a JSON body value against a field, or reject it. This is where the
/// no-float rule, enum-membership, uuid-format and length checks live for
/// writes.
fn value_for_field(field: &Field, v: &Value) -> Result<SqlValue, ApiError> {
    let err = |m: String| ApiError::InvalidValue {
        field: field.name.clone(),
        message: m,
    };
    if v.is_null() {
        return if field.nullable {
            Ok(SqlValue::Null)
        } else {
            Err(err("null not allowed for a non-nullable field".into()))
        };
    }
    match &field.field_type {
        FieldType::Text { max_len } => {
            let s = v.as_str().ok_or_else(|| err("expected a string".into()))?;
            check_len(s, *max_len).map_err(err)?;
            Ok(SqlValue::Text(s.to_string()))
        }
        FieldType::Int => {
            let n = v
                .as_i64()
                .ok_or_else(|| err("expected an integer".into()))?;
            let n: i32 = n
                .try_into()
                .map_err(|_| err("out of range for int".into()))?;
            Ok(SqlValue::Int32(n))
        }
        FieldType::BigInt => Ok(SqlValue::Int64(
            v.as_i64()
                .ok_or_else(|| err("expected an integer".into()))?,
        )),
        FieldType::Bool => Ok(SqlValue::Bool(
            v.as_bool()
                .ok_or_else(|| err("expected a boolean".into()))?,
        )),
        FieldType::Uuid => {
            let s = v
                .as_str()
                .ok_or_else(|| err("expected a uuid string".into()))?;
            uuid_value(s).map_err(err)
        }
        FieldType::Json => Ok(SqlValue::Json(v.to_string())),
        FieldType::Date => {
            let s = v
                .as_str()
                .ok_or_else(|| err("expected a date string".into()))?;
            reject_nonfinite_timestamp(s).map_err(err)?;
            Ok(SqlValue::Text(s.to_string()))
        }
        FieldType::Timestamptz => {
            let s = v
                .as_str()
                .ok_or_else(|| err("expected a timestamp string".into()))?;
            reject_nonfinite_timestamp(s).map_err(err)?;
            Ok(SqlValue::Timestamptz(s.to_string()))
        }
        FieldType::Enum { variants } => {
            let s = v.as_str().ok_or_else(|| err("expected a string".into()))?;
            enum_value(s, variants).map_err(err)
        }
        FieldType::Numeric {
            precision, scale, ..
        } => {
            let s = numeric_string_from_json(v).map_err(err)?;
            Ok(SqlValue::Numeric(
                validate_decimal(&s, *precision, *scale).map_err(err)?,
            ))
        }
        FieldType::Reference { .. } => {
            let s = v
                .as_str()
                .ok_or_else(|| err("expected a uuid reference".into()))?;
            uuid_value(s).map_err(err)
        }
    }
}

/// Type a query-string filter value against a field, or reject it.
fn value_for_field_str(field: &Field, s: &str) -> Result<SqlValue, ApiError> {
    let err = |m: String| ApiError::InvalidValue {
        field: field.name.clone(),
        message: m,
    };
    match &field.field_type {
        FieldType::Text { max_len } => {
            check_len(s, *max_len).map_err(err)?;
            Ok(SqlValue::Text(s.to_string()))
        }
        FieldType::Int => Ok(SqlValue::Int32(
            s.parse().map_err(|_| err("expected an integer".into()))?,
        )),
        FieldType::BigInt => Ok(SqlValue::Int64(
            s.parse().map_err(|_| err("expected an integer".into()))?,
        )),
        FieldType::Bool => match s {
            "true" => Ok(SqlValue::Bool(true)),
            "false" => Ok(SqlValue::Bool(false)),
            _ => Err(err("expected true or false".into())),
        },
        FieldType::Uuid | FieldType::Reference { .. } => uuid_value(s).map_err(err),
        FieldType::Json => Ok(SqlValue::Json(s.to_string())),
        FieldType::Date => {
            reject_nonfinite_timestamp(s).map_err(err)?;
            Ok(SqlValue::Text(s.to_string()))
        }
        FieldType::Timestamptz => {
            reject_nonfinite_timestamp(s).map_err(err)?;
            Ok(SqlValue::Timestamptz(s.to_string()))
        }
        FieldType::Enum { variants } => enum_value(s, variants).map_err(err),
        FieldType::Numeric {
            precision, scale, ..
        } => Ok(SqlValue::Numeric(
            validate_decimal(s, *precision, *scale).map_err(err)?,
        )),
    }
}

/// Reject the infinite instants a `date`/`timestamptz` column can otherwise
/// hold. Postgres accepts `[+-]infinity` (and the `inf` abbreviation) as a
/// valid instant, but `to_jsonb` serializes it as the JSON **string**
/// `"infinity"` — so a row-event outbox payload's field would silently change
/// JSON type from instant to string. Rejecting it at the gateway edge (a clean
/// 400) complements the generated-table floor CHECK (the DB-level backstop that
/// also covers flow-authored SQL). `NaN` is not reachable here: it is invalid
/// for `date`/`timestamptz` in Postgres.
fn reject_nonfinite_timestamp(s: &str) -> Result<(), String> {
    let t = s.trim().to_ascii_lowercase();
    let bare = t.strip_prefix(['+', '-']).unwrap_or(&t);
    if bare == "infinity" || bare == "inf" {
        return Err("infinite timestamps are not allowed".into());
    }
    Ok(())
}

fn check_len(s: &str, max_len: Option<u32>) -> Result<(), String> {
    if let Some(n) = max_len
        && s.chars().count() > n as usize
    {
        return Err(format!("exceeds max length {n}"));
    }
    Ok(())
}

fn uuid_value(s: &str) -> Result<SqlValue, String> {
    if is_uuid(s) {
        Ok(SqlValue::Uuid(s.to_string()))
    } else {
        Err("not a valid uuid".into())
    }
}

fn enum_value(s: &str, variants: &[String]) -> Result<SqlValue, String> {
    if variants.iter().any(|x| x == s) {
        Ok(SqlValue::Text(s.to_string()))
    } else {
        Err(format!(
            "not one of the allowed values: {}",
            variants.join(", ")
        ))
    }
}

/// Extract a numeric literal string from a JSON value, enforcing the no-float
/// rule: a JSON float is rejected outright; only an integer or a decimal string
/// is accepted (validated for exactness by [`validate_decimal`]).
fn numeric_string_from_json(v: &Value) -> Result<String, String> {
    if let Some(s) = v.as_str() {
        return Ok(s.to_string());
    }
    if v.is_i64() || v.is_u64() {
        return Ok(v.to_string());
    }
    if v.is_f64() {
        return Err("numeric must be an exact-decimal string, not a float".into());
    }
    Err("expected a decimal string or integer".into())
}

/// Validate that `s` is an exact decimal fitting `numeric(precision, scale)`:
/// optional sign, digits, optional single `.` and fractional digits, no
/// exponent; fractional digits ≤ `scale`; significant integer digits ≤
/// `precision - scale`. Returns the trimmed literal (Postgres re-parses it).
fn validate_decimal(s: &str, precision: u32, scale: u32) -> Result<String, String> {
    let t = s.trim();
    let body = t
        .strip_prefix('-')
        .or_else(|| t.strip_prefix('+'))
        .unwrap_or(t);
    if body.is_empty() {
        return Err("not a number".into());
    }
    if body.contains(['e', 'E']) {
        return Err("exponent notation is not allowed".into());
    }
    let (int_part, frac_part) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err("not a number".into());
    }
    if !int_part.bytes().all(|c| c.is_ascii_digit())
        || !frac_part.bytes().all(|c| c.is_ascii_digit())
    {
        return Err("not a number".into());
    }
    if frac_part.len() as u32 > scale {
        return Err(format!("more than {scale} fractional digits"));
    }
    let significant_int = int_part.trim_start_matches('0').len() as u32;
    if significant_int > precision.saturating_sub(scale) {
        return Err(format!("does not fit numeric({precision},{scale})"));
    }
    Ok(t.to_string())
}

use serde_json::Value;
use wamn_entity_access::{
    CompareOp, EntityAccessError, EntityOperation, EntityPlan, EntityRequest, Filter, ListOptions,
    Planner, Sort, SortDirection, UpdateMode,
};
use wamn_schema_model::Catalog;

use crate::ApiError;

pub use wamn_entity_access::{Expansion as Expand, ExpansionDirection as ExpandDir, PlanKind};
pub use wamn_pg_core::Statement as Compiled;

const DEFAULT_BASE_PATH: &str = "/api/rest";

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
    pub fn from_http(value: &str) -> Option<Self> {
        match value.to_ascii_uppercase().as_str() {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PUT" => Some(Self::Put),
            "PATCH" => Some(Self::Patch),
            "DELETE" => Some(Self::Delete),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    inner: EntityPlan,
    status: u16,
}

impl Plan {
    pub fn kind(&self) -> PlanKind {
        self.inner.kind()
    }
    pub fn query(&self) -> &Compiled {
        self.inner.statement()
    }
    pub fn expands(&self) -> &[Expand] {
        self.inner.expands()
    }
    pub fn status(&self) -> u16 {
        self.status
    }
}

pub struct Router<'a> {
    planner: Planner<'a>,
    base_path: String,
}

impl<'a> Router<'a> {
    pub fn new(catalog: &'a Catalog) -> Self {
        Self {
            planner: Planner::new(catalog),
            base_path: DEFAULT_BASE_PATH.to_string(),
        }
    }

    pub fn with_base_path(mut self, value: impl Into<String>) -> Self {
        self.base_path = value.into();
        self
    }

    pub fn with_max_page_size(mut self, value: u32) -> Self {
        self.planner = self.planner.with_max_page_size(value);
        self
    }

    pub fn compile(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Plan, ApiError> {
        let (entity, id) = split_route(&self.base_path, path)?;
        let (operation, status) = match (method, id) {
            (Method::Get, None) => (EntityOperation::List(parse_list(query)?), 200),
            (Method::Get, Some(id)) => (
                EntityOperation::Get {
                    id: id.to_string(),
                    expand: expand_names(query),
                },
                200,
            ),
            (Method::Post, None) => (
                EntityOperation::Create {
                    fields: body.cloned().ok_or(ApiError::PayloadRequired)?,
                },
                201,
            ),
            (Method::Put | Method::Patch, Some(id)) => (
                EntityOperation::Update {
                    id: id.to_string(),
                    fields: body.cloned().ok_or(ApiError::PayloadRequired)?,
                    mode: if method == Method::Put {
                        UpdateMode::Replace
                    } else {
                        UpdateMode::Merge
                    },
                },
                200,
            ),
            (Method::Delete, Some(id)) => (EntityOperation::Delete { id: id.to_string() }, 204),
            _ => return Err(ApiError::MethodNotAllowed),
        };
        let request = EntityRequest {
            entity: entity.to_string(),
            operation,
        };
        Ok(Plan {
            inner: self.planner.plan(&request).map_err(ApiError::from)?,
            status,
        })
    }

    pub fn build_expand(&self, expand: &Expand, keys: &[wamn_pg_core::SqlValue]) -> Compiled {
        self.planner.build_expansion(expand, keys)
    }
}

fn split_route<'a>(base: &str, path: &'a str) -> Result<(&'a str, Option<&'a str>), ApiError> {
    let rest = path.strip_prefix(base).ok_or(ApiError::NotFound)?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return Err(ApiError::NotFound);
    }
    let mut parts = rest.trim_matches('/').split('/');
    let entity = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or(ApiError::NotFound)?;
    let id = parts.next().filter(|value| !value.is_empty());
    if parts.next().is_some() {
        return Err(ApiError::NotFound);
    }
    Ok((entity, id))
}

fn parse_list(query: &[(String, String)]) -> Result<ListOptions, ApiError> {
    let mut options = ListOptions::default();
    for (key, raw) in query {
        match key.as_str() {
            "sort" => options.sort.extend(raw.split(',').filter_map(parse_sort)),
            "limit" => {
                options.limit = Some(raw.parse().map_err(|_| {
                    ApiError::InvalidRequest("limit must be a non-negative integer".into())
                })?);
            }
            "offset" => {
                options.offset = raw.parse().map_err(|_| {
                    ApiError::InvalidRequest("offset must be a non-negative integer".into())
                })?;
            }
            "expand" => {
                for name in names(raw) {
                    if !options.expand.contains(&name) {
                        options.expand.push(name);
                    }
                }
            }
            field => options.filters.push(parse_filter(field, raw)),
        }
    }
    Ok(options)
}

fn parse_sort(part: &str) -> Option<Sort> {
    let part = part.trim();
    if part.is_empty() {
        return None;
    }
    let (field, direction) = part.strip_prefix('-').map_or(
        (part.trim_start_matches('+'), SortDirection::Asc),
        |field| (field, SortDirection::Desc),
    );
    Some(Sort {
        field: field.to_string(),
        direction,
    })
}

fn parse_filter(field: &str, raw: &str) -> Filter {
    let (op, value) = raw
        .split_once('.')
        .filter(|(op, _)| {
            matches!(
                *op,
                "eq" | "neq" | "lt" | "lte" | "gt" | "gte" | "like" | "in"
            )
        })
        .unwrap_or(("eq", raw));
    match op {
        "like" => Filter::Like {
            field: field.into(),
            pattern: value.into(),
        },
        "in" => Filter::In {
            field: field.into(),
            values: value.split(',').map(|v| v.trim().to_string()).collect(),
        },
        _ => Filter::Compare {
            field: field.into(),
            op: match op {
                "neq" => CompareOp::NotEq,
                "lt" => CompareOp::Lt,
                "lte" => CompareOp::Lte,
                "gt" => CompareOp::Gt,
                "gte" => CompareOp::Gte,
                _ => CompareOp::Eq,
            },
            value: value.into(),
        },
    }
}

fn names(raw: &str) -> impl Iterator<Item = String> + '_ {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn expand_names(query: &[(String, String)]) -> Vec<String> {
    let mut result = Vec::new();
    for name in query
        .iter()
        .filter(|(key, _)| key == "expand")
        .flat_map(|(_, value)| names(value))
    {
        if !result.contains(&name) {
            result.push(name);
        }
    }
    result
}

impl From<EntityAccessError> for ApiError {
    fn from(error: EntityAccessError) -> Self {
        match error {
            EntityAccessError::UnknownEntity(value) => Self::UnknownEntity(value),
            EntityAccessError::UnknownField { entity, field } => {
                Self::UnknownField { entity, field }
            }
            EntityAccessError::UnknownRelation { entity, relation } => {
                Self::UnknownRelation { entity, relation }
            }
            EntityAccessError::UnsupportedExpansion {
                entity,
                relation,
                cardinality,
            } => Self::UnsupportedExpansion {
                entity,
                relation,
                cardinality,
            },
            EntityAccessError::UnservableRelation { entity, relation } => {
                Self::UnservableRelation { entity, relation }
            }
            EntityAccessError::InvalidValue { field, message } => {
                Self::InvalidValue { field, message }
            }
            EntityAccessError::InvalidRequest(message) => Self::InvalidRequest(message),
            _ => Self::InvalidRequest("unsupported entity-access error".to_string()),
        }
    }
}

//! How a package operation names its token, its route path and its route
//! attachment. Generation derives the routes of an application package with
//! these, and the host routes of the control contract use the same functions.

/// The sealed operation token `<package>:<model>/<action>@<version>`. The
/// package, model and action are written in kebab case.
pub fn operation_token(package: &str, version: &str, model: &str, action: &str) -> String {
    format!(
        "{}:{}/{}@{version}",
        package.replace('_', "-"),
        model.replace('_', "-"),
        action.replace('_', "-"),
    )
}

/// A sealed operation id split at its last `@` into its reference and its
/// version, or `None` when the id carries no version.
pub fn split_sealed_operation(sealed: &str) -> Option<(&str, &str)> {
    sealed.rsplit_once('@')
}

/// The reference of a sealed operation id: the id without its `@version`.
pub fn sealed_operation_reference(sealed: &str) -> &str {
    split_sealed_operation(sealed).map_or(sealed, |(reference, _)| reference)
}

/// The route path `<path_prefix>/<model>/<action>`.
pub fn route_path(path_prefix: &str, model: &str, action: &str) -> String {
    format!("{path_prefix}/{model}/{action}")
}

/// The attachment id `<id_prefix>-<model>-<action>-http`, or
/// `<model>-<action>-http` when the prefix is empty. The model is written in
/// kebab case.
pub fn route_attachment_id(id_prefix: &str, model: &str, action: &str) -> String {
    let model = model.replace('_', "-");
    if id_prefix.is_empty() {
        format!("{model}-{action}-http")
    } else {
        format!("{id_prefix}-{model}-{action}-http")
    }
}

/// The compiler's description of one operation in the compiled package document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationDescription {
    pub operation: OperationContract,
    #[serde(default)]
    pub input: serde_json::Value,
    #[serde(default)]
    pub result: serde_json::Value,
    #[serde(default)]
    pub errors: serde_json::Value,
}

/// Borrow one operation's compiled facts. Old documents carry no descriptions.
pub fn operation_contract<'a>(
    operations: &'a std::collections::BTreeMap<String, OperationDescription>,
    local: &str,
) -> Option<&'a OperationDescription> {
    operations.get(local)
}

/// Build facts for one exported operation. Each consumer borrows this view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationContract {
    pub operation: String,
    #[serde(rename = "type")]
    pub type_: crate::OperationType,
    #[serde(default)]
    pub visibility: Option<String>,
    pub permission_token: Option<String>,
    pub grant: Option<String>,
    #[serde(default)]
    pub statements: Vec<StatementDescription>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub lists: Option<serde_json::Value>,
    #[serde(default)]
    pub sync: bool,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub transaction: Option<String>,
    #[serde(default)]
    pub idempotent_by: Option<serde_json::Value>,
    #[serde(default)]
    pub relations: Vec<serde_json::Value>,
    #[serde(default)]
    pub record: Option<serde_json::Value>,
}

/// The described database interface of one statement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatementDescription {
    pub name: String,
    pub path: String,
    pub binds: Vec<StatementValue>,
    pub columns: Vec<StatementValue>,
    pub relations: Vec<crate::profile::RelationProfile>,
    pub transactional: bool,
}

/// A parameter or selected column, in database order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatementValue {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: wamn_scalar::SqlValueType,
    pub nullable: bool,
}

impl crate::OperationType {
    /// Frozen operation type literal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Query => "query",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Command => "command",
            Self::Projection => "projection",
            Self::EventHandler => "event_handler",
        }
    }
}

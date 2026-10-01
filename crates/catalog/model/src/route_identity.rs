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

/// The reference of a sealed operation id: the id without its `@version`.
pub fn sealed_operation_reference(sealed: &str) -> &str {
    sealed
        .rsplit_once('@')
        .map_or(sealed, |(reference, _)| reference)
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

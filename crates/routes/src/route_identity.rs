//! Application route paths and attachment names.

/// The route path `<path_prefix>/<model>/<action>`.
pub fn route_path(path_prefix: &str, model: &str, action: &str) -> String {
    format!("{path_prefix}/{model}/{action}")
}

/// The attachment id `<id_prefix>-<model>-<action>-http`, or
/// `<model>-<action>-http` when the prefix is empty. The model is written in
/// kebab case.
pub fn route_attachment_id(
    id_prefix: &str,
    model: &str,
    action: &str,
) -> wamn_package::canonical_name::CanonicalName {
    let model = model.replace('_', "-");
    let text = if id_prefix.is_empty() {
        format!("{model}-{action}-http")
    } else {
        format!("{id_prefix}-{model}-{action}-http")
    };
    text.parse()
        .expect("validated route parts make a canonical attachment name")
}

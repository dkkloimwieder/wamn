use super::{
    COMPONENT_DECLARATION_PLACEHOLDER, ComponentDeclarationError, ComponentDeclarationErrorType,
    Path, Value,
};

pub(super) fn render_palette_declaration(
    template: &Path,
    mut document: Value,
    target: &wamn_catalog::ComponentPackageCoordinate,
    store_alias: Option<&str>,
) -> Result<Value, ComponentDeclarationError> {
    let invalid = |detail: String| {
        ComponentDeclarationError::invalid(
            ComponentDeclarationErrorType::TemplateInvalid,
            template,
            detail,
        )
    };
    for (field, placeholder, value) in [
        (
            "project_env_key",
            COMPONENT_DECLARATION_PLACEHOLDER,
            &target.project_env_key,
        ),
        ("package-id", "__PACKAGE_ID__", &target.package_id),
        (
            "package-version",
            "__PACKAGE_VERSION__",
            &target.package_version,
        ),
    ] {
        let slot = document
            .pointer_mut(&format!("/target/{field}"))
            .ok_or_else(|| invalid(format!("has no target.{field}")))?;
        if slot.as_str() != Some(placeholder) {
            return Err(invalid(format!(
                "must leave target.{field} as the placeholder {placeholder}"
            )));
        }
        *slot = Value::String(value.clone());
    }
    for connection in document
        .get_mut("connections")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        let Some(alias) = connection.get_mut("store-alias") else {
            continue;
        };
        if !alias.is_object() {
            continue;
        }
        let selected = store_alias
            .or_else(|| (alias["parameter"] == "requirement").then_some("upstream"))
            .ok_or_else(|| invalid("configuration alias has no binding selection".to_owned()))?;
        *alias = Value::String(selected.to_owned());
    }
    Ok(document)
}

use super::{
    COMPONENT_DECLARATION_PLACEHOLDER, COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER,
    ComponentDeclarationError, ComponentDeclarationErrorType, Path, Value,
};

pub(super) fn render_palette_declaration(
    template: &Path,
    mut document: Value,
    scope: &wamn_catalog::ComponentPackageScope,
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
        ("env_key", COMPONENT_DECLARATION_PLACEHOLDER, &scope.env_key),
        ("package-id", "__PACKAGE_ID__", &scope.package_id),
        (
            "package-version",
            "__PACKAGE_VERSION__",
            &scope.package_version,
        ),
    ] {
        let slot = document
            .pointer_mut(&format!("/scope/{field}"))
            .ok_or_else(|| invalid(format!("has no scope.{field}")))?;
        if slot.as_str() != Some(placeholder) {
            return Err(invalid(format!(
                "must leave scope.{field} as the placeholder {placeholder}"
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
        if alias.as_str() != Some(COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER) {
            continue;
        }
        let store_alias = store_alias.ok_or_else(|| {
            invalid(format!(
                "leaves a store alias as {COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER}, and no \
                 binding selection names a store alias for it"
            ))
        })?;
        *alias = Value::String(store_alias.to_owned());
    }
    Ok(document)
}

pub(super) fn render_declaration_document(
    template: &Path,
    document: &mut Value,
    env_key: &str,
    owners: &wamn_catalog::manifest::OperationOwners,
) -> Result<(), ComponentDeclarationError> {
    let invalid = |detail: String| {
        ComponentDeclarationError::invalid(
            ComponentDeclarationErrorType::TemplateInvalid,
            template,
            detail,
        )
    };
    wamn_schema_generator::operation_reference::resolve_declaration_document(document, owners)
        .map_err(|error| invalid(error.context().to_owned()))?;
    let slot = document
        .pointer_mut("/scope/env_key")
        .ok_or_else(|| invalid("has no scope.env_key".to_owned()))?;
    if slot.as_str() != Some(COMPONENT_DECLARATION_PLACEHOLDER) {
        return Err(invalid(
            "must leave scope.env_key as the deployment placeholder".to_owned(),
        ));
    }
    *slot = Value::String(env_key.to_owned());
    Ok(())
}

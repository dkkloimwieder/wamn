use super::{
    AdmittedComponent, BTreeMap, BTreeSet, ComponentConnection, ComponentConnectionRequirement,
    ComponentConnectionType, ComponentProjectionError, ComponentProjectionErrorType,
    ConnectionTypeDescriptor, ExpectedOperationContract, GeneratedStatementContract, declared_sql,
};

/// Bind result signing only from the package's verified typed result fields.
pub(super) fn bind_signed_file_results(
    component: &mut AdmittedComponent,
    manifest: &wamn_catalog::manifest::PackageManifest,
) -> Result<(), ComponentProjectionError> {
    use wamn_catalog::manifest::ResultClass;
    use wamn_catalog::{
        SIGNED_FILE_READS, SignedFileField, SignedFileReads, SignedFileResultClass,
    };

    let invalid = |detail: String| {
        ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementContractInvalid,
            detail,
        )
    };
    // Re-run the same vocabulary gate for callers that construct a manifest.
    wamn_catalog::manifest::validate_operation_vocabulary(manifest)
        .map_err(|error| invalid(error.to_string()))?;
    for (export, operation) in &mut component.operations {
        let result = manifest
            .custom_operations
            .iter()
            .find_map(|(name, declaration)| {
                let canonical =
                    wamn_catalog::manifest::canonical_operation_identity(&manifest.package, name)
                        .ok()?;
                (canonical == *export)
                    .then_some(declaration.result.as_ref())
                    .flatten()
            });
        let reads = result
            .filter(|result| !result.signed_reads.is_empty())
            .map(|result| SignedFileReads {
                class: match result.class {
                    ResultClass::One => SignedFileResultClass::One,
                    ResultClass::OptionalOne => SignedFileResultClass::OptionalOne,
                    ResultClass::BoundedList => SignedFileResultClass::BoundedList,
                    ResultClass::Page => SignedFileResultClass::Page,
                },
                fields: result
                    .signed_reads
                    .iter()
                    .map(|(path, read)| SignedFileField {
                        path: path.clone(),
                        nullable: result
                            .fields
                            .iter()
                            .find(|field| field.path == *path)
                            .expect("the package gate requires the signed result field")
                            .nullable,
                        read: read.clone(),
                    })
                    .collect(),
            });
        for port in &operation.output_ports {
            if port.schema.schema.get(SIGNED_FILE_READS).is_some() {
                return Err(invalid(format!(
                    "{export} must declare signed_reads in its package result, not in its component schema"
                )));
            }
        }
        if let Some(reads) = reads {
            let main = operation
                .output_ports
                .iter_mut()
                .find(|port| port.name == "main")
                .ok_or_else(|| {
                    invalid(format!(
                        "{export} signed result requires a main output port"
                    ))
                })?;
            let schema = main.schema.schema.as_object_mut().ok_or_else(|| {
                invalid(format!("{export} signed result requires an object schema"))
            })?;
            schema.insert(
                SIGNED_FILE_READS.into(),
                serde_json::to_value(reads).expect("verified signed file metadata serializes"),
            );
            wamn_catalog::signed_file_reads(operation)
                .map_err(|error| invalid(format!("{export}: {error}")))?;
        }
    }
    Ok(())
}

/// Translate one admitted connection into its platform-owned portable record.
///
/// This is the single place a declared alias becomes connection SEMANTICS. The
/// descriptor is minted from the platform's own constructor, never authored, so
/// field ownership and credential injection cannot be widened by a declaration.
pub(crate) fn portable_requirement(
    component_digest: &str,
    connection: &ComponentConnection,
) -> ComponentConnectionRequirement {
    let descriptor = match connection.requirement_type {
        ComponentConnectionType::Http => ConnectionTypeDescriptor::http_v1(),
        ComponentConnectionType::Blobstore => ConnectionTypeDescriptor::blobstore_v1(),
    };
    ComponentConnectionRequirement::new(component_digest, &connection.store_alias, descriptor)
}

/// The exports among `selected` whose request element transaction the host begins, as
/// the package manifest declares them.
pub(super) fn host_transaction_exports<T>(
    manifest: &wamn_catalog::manifest::PackageManifest,
    selected: &BTreeMap<String, T>,
) -> Result<BTreeSet<String>, ComponentProjectionError> {
    let mut exports = BTreeSet::new();
    for local_operation in manifest.host_transaction_operations() {
        let operation = wamn_catalog::manifest::canonical_operation_identity(
            &manifest.package,
            &local_operation,
        )
        .map_err(|error| {
            ComponentProjectionError::new(
                ComponentProjectionErrorType::StatementOperationMismatch,
                format!("operation {local_operation:?} is not canonical: {error}"),
            )
        })?;
        if selected.contains_key(&operation) {
            exports.insert(operation);
        }
    }
    Ok(exports)
}

/// The manifest operations one pushed component is expected to serve, or the
/// refusal when its exports and the manifest disagree.
///
/// The manifest is the package's own declaration of ITS components -- the
/// data component groups whose operations it registers. A platform palette
/// node (`label-render`, `blob-put`: one `wamn:node` handler, no registered
/// operation) is not the package's to declare, so a component ABSENT from the
/// manifest is admitted iff it registers no manifest operation, with an empty
/// expected set (RULED `wamn-362o.40`). A component that claims a registered
/// operation must be a manifest member, as before: that is the fence against
/// a stranger serving the package's operations.
///
/// ABSENT FROM THE MANIFEST DOES NOT MEAN UNEXAMINED. The palette node's
/// identity and posture are governed on the same push: its imports pass the
/// tenant allowlist and the capability registry's posture
/// (`wamn_component_policy::analyze_tenant`), and its declared exports are
/// checked against the component bytes (`wamn_engine::component_admission`)
/// before this function runs. This branch only
/// declines to look for manifest operations that, by construction, it does
/// not serve; it is not a bypass of admission.
pub(super) fn validate_component_operation_assignment(
    manifest: &wamn_catalog::manifest::PackageManifest,
    component: &AdmittedComponent,
    expected: &BTreeMap<String, ExpectedOperationContract>,
) -> Result<BTreeSet<String>, ComponentProjectionError> {
    if !manifest.components.contains_key(&component.component) {
        let registered = component
            .operations
            .iter()
            .filter_map(|(export, declared)| {
                declared
                    .registered_operation
                    .as_deref()
                    .map(|registered| format!("{export} -> {registered}"))
            })
            .collect::<Vec<_>>();
        if registered.is_empty() {
            return Ok(BTreeSet::new());
        }
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementComponentMismatch,
            format!(
                "component {:?} is absent from the package manifest yet registers manifest operations: {registered:?}",
                component.component
            ),
        ));
    }
    let misassigned = component
        .operations
        .keys()
        .filter(|export| {
            expected
                .get(*export)
                .is_some_and(|operation| operation.component != component.component)
        })
        .collect::<Vec<_>>();
    if !misassigned.is_empty() {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementComponentMismatch,
            format!(
                "component {:?} exports manifest operations assigned elsewhere: {misassigned:?}",
                component.component
            ),
        ));
    }

    let expected_component_operations = expected
        .iter()
        .filter(|(_, operation)| operation.component == component.component)
        .map(|(operation, _)| operation.clone())
        .collect::<BTreeSet<_>>();
    let exported_operations = component
        .operations
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let missing = expected_component_operations
        .difference(&exported_operations)
        .collect::<Vec<_>>();
    let unexpected_public = component
        .operations
        .iter()
        .filter_map(|(export, declared)| {
            declared
                .registered_operation
                .as_ref()
                .filter(|_| !expected_component_operations.contains(export))
                .map(|_| export)
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() || !unexpected_public.is_empty() {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementOperationMismatch,
            format!(
                "component {:?} differs from its manifest operation set: missing={missing:?}, unexpected-public={unexpected_public:?}",
                component.component
            ),
        ));
    }
    for operation in &expected_component_operations {
        let declared = &component.operations[operation];
        let expected_registration = expected[operation].public.then_some(operation.as_str());
        if declared.registered_operation.as_deref() != expected_registration {
            return Err(ComponentProjectionError::new(
                ComponentProjectionErrorType::StatementOperationMismatch,
                format!(
                    "component {:?} export {operation:?} registered-operation is {:?}, expected {expected_registration:?}",
                    component.component, declared.registered_operation,
                ),
            ));
        }
    }
    Ok(expected_component_operations)
}

pub(super) fn expected_operation_contracts(
    manifest: &wamn_catalog::manifest::PackageManifest,
) -> Result<BTreeMap<String, ExpectedOperationContract>, ComponentProjectionError> {
    let default_component = (manifest.components.len() == 1).then(|| {
        manifest
            .components
            .keys()
            .next()
            .expect("one component exists")
            .as_str()
    });
    let mut expected = BTreeMap::new();
    for (model, declaration) in &manifest.models {
        for (action, operation) in &declaration.operations {
            let local_operation = format!("{model}.{}", action.as_str());
            insert_expected_operation(
                manifest,
                &mut expected,
                &local_operation,
                operation.component.as_deref().or(default_component),
                true,
            )?;
        }
    }
    for (local_operation, operation) in &manifest.custom_operations {
        insert_expected_operation(
            manifest,
            &mut expected,
            local_operation,
            operation.component.as_deref().or(default_component),
            operation.visibility() == wamn_catalog::manifest::OperationVisibility::Public,
        )?;
    }
    Ok(expected)
}

pub(super) fn insert_expected_operation(
    manifest: &wamn_catalog::manifest::PackageManifest,
    expected: &mut BTreeMap<String, ExpectedOperationContract>,
    local_operation: &str,
    component: Option<&str>,
    public: bool,
) -> Result<(), ComponentProjectionError> {
    let component = component.ok_or_else(|| {
        ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementOperationMismatch,
            format!("operation {local_operation:?} has no component assignment"),
        )
    })?;
    let operation =
        wamn_catalog::manifest::canonical_operation_identity(&manifest.package, local_operation)
            .map_err(|error| {
                ComponentProjectionError::new(
                    ComponentProjectionErrorType::StatementOperationMismatch,
                    format!("operation {local_operation:?} is not canonical: {error}"),
                )
            })?;
    let (module, action) = local_operation.split_once('.').ok_or_else(|| {
        ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementOperationMismatch,
            format!("operation {local_operation:?} has no module/action boundary"),
        )
    })?;
    let relative_path = format!("generated/contracts/{module}/{action}.operation.json");
    if expected
        .insert(
            operation.clone(),
            ExpectedOperationContract {
                relative_path,
                component: component.to_owned(),
                public,
            },
        )
        .is_some()
    {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementOperationMismatch,
            format!("operation {operation:?} occurs more than once in the package manifest"),
        ));
    }
    Ok(())
}

/// Refuse a declared statement that the parsed check refuses
/// ([`declared_sql`]).
pub(super) fn check_statement_sql(
    operation: &str,
    statement: &str,
    sql: &str,
) -> Result<(), ComponentProjectionError> {
    declared_sql::check_declared_statement(sql).map_err(|refusal| {
        ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementRefused,
            format!("operation {operation:?} statement {statement:?} is refused: {refusal}"),
        )
    })
}

/// Refuse a stored component whose declared SQL the parsed check refuses.
///
/// Publish reads the statements from the pushed descriptor, not from the
/// package files, so it checks them again here.
pub fn check_admitted_statements(
    component: &AdmittedComponent,
) -> Result<(), ComponentProjectionError> {
    for (operation, admitted) in &component.operations {
        for statement in admitted.statements.values() {
            check_statement_sql(operation, &statement.name, &statement.sql)?;
        }
    }
    Ok(())
}

pub(super) fn validate_generated_statement_shape(
    operation: &str,
    statement: &GeneratedStatementContract,
) -> Result<(), ComponentProjectionError> {
    if !canonical_text(&statement.name) {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorType::StatementContractInvalid,
            format!("operation {operation:?} carries an empty or non-canonical statement name"),
        ));
    }
    for (kind, fields) in [("bind", &statement.binds), ("column", &statement.columns)] {
        let mut names = BTreeSet::new();
        for field in fields {
            if !canonical_text(&field.name) || !names.insert(field.name.as_str()) {
                return Err(ComponentProjectionError::new(
                    ComponentProjectionErrorType::StatementContractInvalid,
                    format!(
                        "operation {operation:?} statement {:?} carries an empty, non-canonical, or duplicate {kind}",
                        statement.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn canonical_text(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.as_bytes().contains(&0)
}

pub(super) fn ensure_component_package_matches(
    component: &AdmittedComponent,
    package: &wamn_catalog::PackageCoordinate,
) -> anyhow::Result<()> {
    if component.scope.package_id != package.package_id()
        || component.scope.package_version != package.package_version()
    {
        return Err(ComponentProjectionError::new(
            ComponentProjectionErrorType::PackageCoordinateMismatch,
            format!(
                "declaration names {}@{} but package directory names {}@{}",
                component.scope.package_id,
                component.scope.package_version,
                package.package_id(),
                package.package_version()
            ),
        )
        .into());
    }
    Ok(())
}

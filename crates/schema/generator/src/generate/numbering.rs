//! Build refusals of the package's numbering and caller-filled columns
//! (`docs/plan/platform-config.md`, "Declaration in wamn.k" and "Numbering").
//!
//! A sequence's pattern parses with the one parser, [`wamn_number_pattern`].
//! A `number` field and an `actor` field are filled by generated SQL, so no
//! write may supply either. A sequence is used when a model field numbers from
//! it or when it is declared `callable`, which gives the package's commands
//! the generated `sequences::<name>::next`; a sequence with neither is a
//! refusal. A package with models runs on `cloud` only.

use wamn_number_pattern::{Key, Pattern, Scope};

use super::{
    BTreeMap, CatalogIr, ColumnType, ConstraintType, CrudAction, GenerateError, GenerateErrorType,
    ModelDeclaration, PackageManifest, Table, column, relation, validate_identifier,
};
use crate::manifest::Target;

/// Refuse what the package's numbering declarations cannot mean.
///
/// Validation of every model ran first, so each model resolves its relation.
pub(super) fn validate_numbering(
    catalog: &CatalogIr,
    manifest: &PackageManifest,
) -> Result<(), GenerateError> {
    if !manifest.models.is_empty()
        && manifest
            .targets
            .iter()
            .any(|target| *target != Target::Cloud)
    {
        return Err(GenerateError::new(
            GenerateErrorType::InvalidManifest,
            "a package with models runs on cloud only: set targets = [\"cloud\"] or move the models to a cloud package",
        ));
    }
    let patterns = sequence_patterns(manifest)?;
    for (model_name, model) in &manifest.models {
        let table = relation(catalog, model).expect("validation resolved every relation");
        validate_model_fields(manifest, model_name, model, table)?;
        if let Some((field, sequence)) = model.number_field() {
            validate_numbered_field(
                model_name,
                model,
                table,
                field,
                sequence,
                &patterns[sequence],
            )?;
        }
    }
    for (sequence, declaration) in &manifest.sequences {
        let bound = manifest.models.values().any(|model| {
            model
                .number_field()
                .is_some_and(|(_, used)| used == sequence)
        });
        if !bound && !declaration.callable {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidManifest,
                format!(
                    "sequence {sequence} numbers no field and no command calls it: bind it with number = \"{sequence}\" on a model field, set callable = True, or remove it"
                ),
            ));
        }
    }
    refuse_supplied_fields(manifest)
}

/// Each sequence's parsed pattern, by name.
fn sequence_patterns(manifest: &PackageManifest) -> Result<BTreeMap<&str, Pattern>, GenerateError> {
    manifest
        .sequences
        .iter()
        .map(|(name, declaration)| {
            validate_identifier(name, "sequence")?;
            let pattern = wamn_number_pattern::parse(&declaration.pattern).map_err(|source| {
                GenerateError::new(
                    GenerateErrorType::InvalidManifest,
                    format!("sequence {name} has an invalid pattern: {source}"),
                )
            })?;
            Ok((name.as_str(), pattern))
        })
        .collect()
}

fn validate_model_fields(
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
) -> Result<(), GenerateError> {
    let invalid = |detail: String| GenerateError::new(GenerateErrorType::InvalidModel, detail);
    let mut numbered = None;
    for (field, declaration) in &model.fields {
        if column(table, field).is_none() {
            return Err(GenerateError::for_object(
                GenerateErrorType::UnknownColumn,
                format!("{model_name} references unknown field {field}"),
                field.as_str(),
            ));
        }
        let Some(sequence) = &declaration.number else {
            continue;
        };
        if declaration.role.is_some() {
            return Err(invalid(format!(
                "{model_name}.{field} is numbered and has a role; a field is filled one way"
            )));
        }
        if !manifest.sequences.contains_key(sequence) {
            return Err(invalid(format!(
                "{model_name}.{field} numbers from undeclared sequence {sequence}"
            )));
        }
        if let Some(other) = numbered.replace(field) {
            return Err(invalid(format!(
                "{model_name} numbers two fields, {other} and {field}; number one"
            )));
        }
    }
    Ok(())
}

/// The key rules of one numbered field (`docs/plan/platform-config.md`,
/// "Sequence as the record id").
fn validate_numbered_field(
    model_name: &str,
    model: &ModelDeclaration,
    table: &Table,
    field: &str,
    sequence: &str,
    pattern: &Pattern,
) -> Result<(), GenerateError> {
    let invalid = |detail: String| GenerateError::new(GenerateErrorType::InvalidModel, detail);
    let key = primary_key(table);
    if key == Some(field) {
        if pattern
            .counters()
            .any(|counter| counter.scope().key().is_some())
        {
            return Err(invalid(format!(
                "{model_name}.{field} is the key and numbers from sequence {sequence}, whose pattern uses reset= or per=; a key's counters may not, because two records would collide"
            )));
        }
        let ty = column(table, field)
            .expect("model fields were validated")
            .column_type();
        if matches!(ty, ColumnType::Int32 | ColumnType::Int64) && !pattern.is_bare_counter() {
            return Err(invalid(format!(
                "{model_name}.{field} is an integer key, so sequence {sequence} takes a bare {{seq:<counter>}} pattern only"
            )));
        }
    }
    for counter in pattern.counters() {
        if let Scope::Per(Key::Field(name)) = counter.scope()
            && key == Some(&**name)
        {
            return Err(invalid(format!(
                "sequence {sequence} counter {} is per= the key field {model_name}.{name}; the same key would mean the same number",
                counter.name()
            )));
        }
    }
    let writable = model
        .operations
        .get(&CrudAction::Create)
        .map(|create| create.writable_fields.as_slice());
    for name in pattern.fields() {
        let Some(named) = column(table, name) else {
            return Err(invalid(format!(
                "sequence {sequence} names {name}, which is not a field of {model_name}, another counter of its pattern, or date:FMT; it must be a field of every model that uses the sequence"
            )));
        };
        if matches!(named.column_type(), ColumnType::Bytes | ColumnType::Json) {
            return Err(invalid(format!(
                "sequence {sequence} names {model_name}.{name}, a {} field, which has no text to render or key by",
                named.column_type().as_str()
            )));
        }
        if writable.is_some_and(|fields| !fields.iter().any(|candidate| candidate == name)) {
            return Err(invalid(format!(
                "{model_name}.create does not write {name}, which sequence {sequence} names; the generated insert passes it to next-number, so make it writable"
            )));
        }
    }
    Ok(())
}

/// Refuse a write that supplies a `number` or an `actor` field: generated SQL
/// fills both, so the caller never does.
fn refuse_supplied_fields(manifest: &PackageManifest) -> Result<(), GenerateError> {
    for (model_name, model) in &manifest.models {
        let filled = model
            .number_field()
            .map(|(field, _)| field)
            .into_iter()
            .chain(model.actor_fields())
            .collect::<Vec<_>>();
        if filled.is_empty() {
            continue;
        }
        for (action, operation) in &model.operations {
            if let Some(field) = operation
                .writable_fields
                .iter()
                .find(|field| filled.contains(&field.as_str()))
            {
                return Err(supplied(
                    &format!("{model_name}.{}", action.as_str()),
                    model_name,
                    field,
                ));
            }
        }
        for (operation_name, operation) in &manifest.custom_operations {
            for written in operation
                .relations
                .iter()
                .filter(|written| written.schema == model.schema && written.table == model.table)
            {
                if let Some(field) = written
                    .insert_fields
                    .iter()
                    .chain(&written.update_fields)
                    .find(|field| filled.contains(&field.as_str()))
                {
                    return Err(supplied(operation_name, model_name, field));
                }
            }
        }
    }
    Ok(())
}

fn supplied(operation: &str, model_name: &str, field: &str) -> GenerateError {
    GenerateError::new(
        GenerateErrorType::InvalidOperation,
        format!(
            "{operation} writes {model_name}.{field}, which generated SQL fills; remove it from the write"
        ),
    )
}

/// The model's one-column primary key.
fn primary_key(table: &Table) -> Option<&str> {
    table
        .constraints()
        .iter()
        .find_map(|constraint| match constraint.constraint_type() {
            ConstraintType::PrimaryKey { columns } => match &**columns {
                [column] => Some(&**column),
                _ => None,
            },
            _ => None,
        })
}

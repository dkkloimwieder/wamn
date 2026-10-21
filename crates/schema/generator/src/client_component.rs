//! Emits wfe descriptions for the roles in the client screen plan.
//!
//! Descriptions contain presentation only. Application bindings own requests,
//! validation, navigation and conversion between form drafts and wire values.
//! Field keys retain contract paths. Repeated rows use paths relative to their
//! group. JSON and byte inputs use text drafts, as do exact wire numbers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::client_ir::FieldIr;
use crate::client_plan::{ClientPlan, Role, ScreenPlan};
use crate::generate::GeneratedFile;

/// Why a screen description could not be emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientComponentError {
    type_: ClientComponentErrorType,
    detail: String,
}

/// What went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientComponentErrorType {
    /// A screen has a shape that wfe cannot describe.
    UnwrittenRole,
    /// A table column that a table definition cannot state: it is not a member
    /// of its row, or its type is not one of the frozen `sql-value` names.
    UnwrittenColumn,
    /// A revision input of a form that no declared path supplies: no read is
    /// bound to it, and it names no reference input whose record it guards.
    UnsuppliedRevision,
}

impl ClientComponentErrorType {
    /// Stable wire code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnwrittenRole => "unwritten_role",
            Self::UnwrittenColumn => "unwritten_column",
            Self::UnsuppliedRevision => "unsupplied_revision",
        }
    }
}

impl ClientComponentError {
    fn new(error_type: ClientComponentErrorType, detail: impl Into<String>) -> Self {
        Self {
            type_: error_type,
            detail: detail.into(),
        }
    }

    /// What went wrong.
    #[must_use]
    pub const fn error_type(&self) -> ClientComponentErrorType {
        self.type_
    }
}

impl core::fmt::Display for ClientComponentError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}: {}", self.type_.code(), self.detail)
    }
}

impl std::error::Error for ClientComponentError {}

/// The directory that holds the emitted descriptions.
pub const COMPONENT_DIRECTORY: &str = "generated/client-ts/components";

/// Emit one description per served screen, grouped into model modules.
///
/// # Errors
///
/// Refuses repeated input shapes that wfe cannot describe.
pub fn emit_ts_components(
    plan: &ClientPlan<'_>,
) -> Result<Vec<GeneratedFile>, ClientComponentError> {
    let header = "// @generated from the client-contract IR; do not edit.\n";
    let mut files = Vec::new();
    let mut index = String::from(header);
    let mut labels = String::from(header);
    index.push_str("export * from \"./labels.js\";\n");
    for model in &plan.models {
        let mut body = String::new();
        let mut imports = BTreeSet::new();
        for screen in model
            .screens
            .iter()
            .filter(|screen| screen.role.is_supported() && screen.contract.route.is_some())
        {
            let role = match screen.role {
                Role::Table => "Table",
                Role::Form => "Form",
                Role::Detail => "Detail",
                Role::Delete => "Delete",
                Role::Unsupported(_) => unreachable!("only supported roles are selected"),
            };
            let stem = crate::client_ts::type_stem(screen.model, screen.name);
            let name = format!("{stem}{role}");
            let title = screen
                .contract
                .label
                .as_deref()
                .unwrap_or(&screen.contract.operation);
            writeln!(labels, "export const {name}Label = {};", quote(title)).expect("write");
            imports.insert(format!("{role}Description"));
            let mut description = json!({"id": screen.contract.operation, "title": title});
            let type_name = match screen.role {
                Role::Table => {
                    description["columns"] = Value::Object(
                        screen
                            .columns
                            .iter()
                            .map(|field| (field.path.clone(), presentation(field)))
                            .collect(),
                    );
                    "TableDescription".to_owned()
                }
                Role::Detail => {
                    description["fields"] = Value::Array(
                        screen
                            .columns
                            .iter()
                            .map(|field| {
                                let mut value = presentation(field);
                                value["field"] = json!(field.path);
                                value
                            })
                            .collect(),
                    );
                    let keys = screen
                        .columns
                        .iter()
                        .map(|field| quote(&field.path))
                        .collect::<Vec<_>>();
                    let keys = if keys.is_empty() {
                        "never".to_owned()
                    } else {
                        keys.join(" | ")
                    };
                    format!("DetailDescription<Record<{keys}, unknown>>")
                }
                Role::Delete => {
                    description["confirmation"] = json!(format!("Confirm {title}?"));
                    "DeleteDescription".to_owned()
                }
                Role::Form => {
                    let (fields, values) = form(screen)?;
                    writeln!(body, "\nexport interface {name}Values {{\n{values}}}")
                        .expect("write");
                    description["fields"] = json!(fields);
                    description["submit"] =
                        json!({"id": screen.contract.operation, "label": title});
                    format!("FormDescription<{name}Values>")
                }
                Role::Unsupported(_) => unreachable!("only supported roles are selected"),
            };
            writeln!(
                body,
                "\nexport const {name} = {} as const satisfies {type_name};",
                serde_json::to_string_pretty(&description).expect("description serializes")
            )
            .expect("write");
        }
        if body.is_empty() {
            continue;
        }
        let source = format!(
            "{header}import type {{ {} }} from \"@wfe/generator\";\n{body}",
            imports.into_iter().collect::<Vec<_>>().join(", ")
        );
        files.push(GeneratedFile::new(
            format!("{COMPONENT_DIRECTORY}/{}.ts", model.model.name).into_boxed_str(),
            source.into_bytes().into_boxed_slice(),
        ));
        writeln!(index, "export * from \"./{}.js\";", model.model.name).expect("write");
    }
    for (operation, reason) in plan.unsupported() {
        writeln!(index, "// No description for {operation}: {reason}").expect("write");
    }
    for operation in plan.unserved() {
        writeln!(index, "// No description for {operation}: no HTTP route").expect("write");
    }
    for (name, source) in [("labels", labels), ("index", index)] {
        files.push(GeneratedFile::new(
            format!("{COMPONENT_DIRECTORY}/{name}.ts").into_boxed_str(),
            source.into_bytes().into_boxed_slice(),
        ));
    }
    Ok(files)
}

fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("text serializes")
}

fn presentation(field: &FieldIr) -> Value {
    let mut value = json!({"label": field.label.as_deref().unwrap_or(&field.path)});
    let format = match field.type_name.as_str() {
        "boolean" => Some("boolean-symbol"),
        "timestamptz" => Some("datetime"),
        "date" => Some("date"),
        // wfe's plain format preserves exact decimal strings without coercion.
        "int32" | "int64" | "numeric" | "float64" => Some("number-plain"),
        _ => None,
    };
    if let Some(format) = format {
        value["format"] = json!(format);
    }
    value
}

fn scalar(field: &FieldIr, key: &str) -> (Value, String) {
    let mut value = presentation(field);
    value["field"] = json!(key);
    value["required"] = json!(field.required && !field.nullable);
    let ty = if field.values.is_empty() {
        match field.type_name.as_str() {
            "boolean" => "boolean".to_owned(),
            "int32" | "float64" => "number".to_owned(),
            _ => "string".to_owned(),
        }
    } else {
        field
            .values
            .iter()
            .map(|value| quote(value))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let control = if ty == "number" {
        if !field.required {
            value["empty"] = json!("undefined");
        } else if field.nullable {
            value["empty"] = json!("null");
        }
        "number"
    } else if !field.required || field.nullable {
        "custom"
    } else if !field.values.is_empty() {
        "select"
    } else {
        match field.type_name.as_str() {
            "boolean" => "checkbox",
            "timestamptz" => "datetime",
            "date" => "date",
            "json" => "textarea",
            _ => "text",
        }
    };
    value["control"] = json!(control);
    let mut ty = ty;
    if field.nullable {
        ty.push_str(" | null");
    }
    if !field.required {
        ty.push_str(" | undefined");
    }
    (value, format!("  {}: {ty};\n", quote(key)))
}

fn find_field<'a>(fields: &'a [FieldIr], path: &str) -> Option<&'a FieldIr> {
    fields.iter().find_map(|field| {
        if field.path == path {
            Some(field)
        } else {
            find_field(&field.children, path)
        }
    })
}

fn form(screen: &ScreenPlan<'_>) -> Result<(Vec<Value>, String), ClientComponentError> {
    let mut fields = Vec::new();
    let mut values = String::new();
    let mut groups: BTreeMap<&str, Vec<&FieldIr>> = BTreeMap::new();
    let bound = screen.revision.map(|binding| binding.command_key_input);
    for field in &screen.inputs {
        if bound == Some(field.path.as_str()) {
            continue;
        }
        if let Some((group, member)) = field.path.split_once("[].") {
            if member.contains("[]") {
                return Err(ClientComponentError::new(
                    ClientComponentErrorType::UnwrittenRole,
                    format!(
                        "{}: wfe does not support nested repeated input {}",
                        screen.contract.operation, field.path
                    ),
                ));
            }
            groups.entry(group).or_default().push(field);
        } else {
            let (description, ty) = scalar(field, &field.path);
            fields.push(description);
            values.push_str(&ty);
        }
    }
    for (group, members) in groups {
        let path = format!("{group}[]");
        let declared = find_field(&screen.contract.input_fields, &path)
            .expect("a repeated leaf has a group in the IR");
        let mut rows = Vec::new();
        writeln!(values, "  {}: {{", quote(&path)).expect("write");
        for field in members {
            let key = field
                .path
                .strip_prefix(&format!("{path}."))
                .expect("group prefix");
            let (description, ty) = scalar(field, key);
            rows.push(description);
            values.push_str("  ");
            values.push_str(&ty);
        }
        values.push_str("  }[];\n");
        fields.push(
            json!({"field": path, "label": declared.label.as_deref().unwrap_or(&declared.path),
            "control": "repeat", "fields": rows}),
        );
    }
    Ok((fields, values))
}

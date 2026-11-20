//! Read custom operation contracts from the authored WIT packages.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Map, Value, json};
use wit_parser::{Interface, Resolve, Type, TypeDefKind};

pub(crate) const DEPS: &str = "component/wit/deps";

#[derive(Debug)]
pub(crate) struct Sources {
    namespace: String,
    groups: BTreeMap<String, PathBuf>,
}

impl Sources {
    pub(crate) fn read(root: &Path, paths: &[PathBuf], document: &mut Value) -> Result<Self> {
        let namespace = document["package"]["id"]
            .as_str()
            .context("package id")?
            .replace('_', "-");
        let version = document["package"]["version"]
            .as_str()
            .context("package version")?;
        let mut groups = BTreeMap::new();
        for path in paths.iter().filter_map(|path| path.strip_prefix(DEPS).ok()) {
            let directory = path
                .parent()
                .and_then(Path::to_str)
                .context("WIT package directory")?;
            let group = directory
                .strip_prefix(&namespace)
                .and_then(|suffix| suffix.strip_prefix('-'))
                .with_context(|| {
                    format!("WIT package directory {directory} needs the {namespace} prefix")
                })?;
            ensure!(
                path.file_name().is_some_and(|name| name != "package.wit")
                    && path.components().count() == 2,
                "write custom WIT under {DEPS}/{directory} beside the generated package.wit"
            );
            ensure!(
                groups.insert(group.to_owned(), path.to_owned()).is_none(),
                "WIT group {group} needs one authored file"
            );
        }
        let sources = Self { namespace, groups };
        let mut world = Resolve::default();
        if !sources.groups.is_empty() {
            world.push_source(
                "wamn:node",
                include_str!("../../../wit/wamn-node/package.wit"),
            )?;
        }
        let version = version.to_owned();
        let mut matched = BTreeSet::new();
        for (group, path) in &sources.groups {
            let mut files = wit_parser::SourceMap::new();
            files.push_str("package.wit", sources.package_line(group, &version)?);
            let path = root.join(DEPS).join(path);
            let text = fs::read_to_string(&path)?;
            ensure!(
                !text
                    .lines()
                    .any(|line| line.trim_start().starts_with("package ")),
                "{} declares a package. The build supplies the package name and version",
                path.display()
            );
            files.push(&path, text);
            let package = world.push_group(files.parse().map_err(|(files, error)| {
                anyhow::anyhow!("{}", wit_parser::render_anyhow_error(&error.into(), &files))
            })?)?;
            for (name, interface) in &world.packages[package].interfaces {
                let interface_name = format!("{group}.{name}");
                let mut candidates = document
                    .get_mut("custom_operations")
                    .and_then(Value::as_object_mut)
                    .into_iter()
                    .flatten()
                    .filter(|(name, _)| name.replace('_', "-") == interface_name);
                let (local, operation) = candidates.next().with_context(|| {
                    format!("authored WIT interface {interface_name} has no KCL operation")
                })?;
                ensure!(
                    candidates.next().is_none(),
                    "more than one KCL operation matches {interface_name}"
                );
                read_operation(&world, &world.interfaces[*interface], name, operation)
                    .with_context(|| format!("read WIT contract for {local}"))?;
                matched.insert(local.to_owned());
            }
        }
        if let Some(operations) = document["custom_operations"].as_object() {
            for local in operations.keys() {
                ensure!(
                    matched.contains(local),
                    "KCL operation {local} has no authored WIT interface"
                );
            }
        }
        Ok(sources)
    }

    fn package_line(&self, group: &str, version: &str) -> Result<String> {
        let name = wit_encoder::PackageName::new(
            self.namespace.as_str(),
            group.to_owned(),
            Some(version.parse()?),
        );
        Ok(wit_encoder::Package::new(name).to_string())
    }

    pub(crate) fn stage(
        &self,
        root: &Path,
        version: &str,
        output: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) -> Result<()> {
        for (group, path) in &self.groups {
            let directory = format!("wit/deps/{}-{group}", self.namespace);
            if let std::collections::btree_map::Entry::Vacant(file) =
                output.entry(Path::new(&directory).join("package.wit"))
            {
                file.insert(self.package_line(group, version)?.into_bytes());
            }
            output.insert(
                Path::new("wit/deps").join(path),
                fs::read(root.join(DEPS).join(path))?,
            );
        }
        Ok(())
    }
}

fn read_operation(
    world: &Resolve,
    interface: &Interface,
    name: &str,
    operation: &mut Value,
) -> Result<()> {
    let request = named_type(interface, name, "request").context("missing request type")?;
    let result = named_type(interface, name, "result");
    let event = operation["type"] == "event_handler";
    let run = interface
        .functions
        .get("run")
        .context("missing run function")?;
    let input = if event {
        request
    } else {
        named_type(interface, name, "item").context("missing request item")?
    };
    let input_matches = run.params.iter().any(|param| {
        param.name == "input"
            && if event {
                param.ty == input
            } else {
                matches!(definition(world, param.ty), Ok(TypeDefKind::List(ty)) if *ty == input)
            }
    });
    ensure!(
        input_matches && event == result.is_none(),
        "run must use the request and result for this operation"
    );
    read_input(world, interface, name, operation, request)?;
    read_result(world, interface, name, operation, result)?;
    read_errors(world, interface, name, operation)
}

fn read_input(
    world: &Resolve,
    interface: &Interface,
    name: &str,
    operation: &mut Value,
    request: Type,
) -> Result<()> {
    let wrapped = operation["type"] == "command" && operation["idempotent_by"].as_str().is_some();
    let prefix = if wrapped { "value." } else { "" };
    let feed = operation["feed"] == true;
    let input = operation["input"].as_object_mut().context("input policy")?;
    let key = input.remove("key");
    let guards = take_map(input, "revision_guards");
    let mut request_fields = Vec::new();
    if let Some(correlation) = input.remove("request_id") {
        let item = named_type(interface, name, "item").context("missing WIT request item")?;
        ensure!(
            correlation == "request_id"
                && matches!(definition(world, item)?,
                    TypeDefKind::Record(record) if record.fields.iter().any(|field| {
                        field.name == "request-id" && member_type(world, field.ty) == (false, Type::String)
                    })
                ),
            "request correlation needs the WIT request-id field"
        );
        request_fields.push(json!({"path": "request_id", "type": "text", "nullable": false}));
    }
    match definition(world, request)? {
        TypeDefKind::Record(record) => {
            for field in &record.fields {
                if !feed || field.name != "feed" {
                    read_fields(
                        world,
                        field.ty,
                        &format!("{prefix}{}", public_name(&field.name)),
                        &mut request_fields,
                    )?;
                }
            }
        }
        TypeDefKind::Enum(empty)
            if empty
                .cases
                .iter()
                .map(|case| case.name.as_str())
                .eq(["request"]) => {}
        _ => bail!("request must be a record or the empty request enum"),
    }
    if let Some(key) = key {
        let key = key.as_str().context("idempotency key field name")?;
        let path = format!("value.{key}");
        ensure!(
            request_fields.iter().any(|field| field["path"] == path
                && field["type"] == "text"
                && field["nullable"] == false),
            "idempotency key {key} needs a non-null text input field"
        );
        ensure!(
            key == "idempotency_key",
            "the claim key field must be idempotency_key"
        );
    } else {
        ensure!(
            operation["idempotent_by"] != "claim",
            "declare the WIT idempotency key field in KCL"
        );
    }
    let input = operation["input"].as_object_mut().context("input policy")?;
    apply_labels(input, &mut request_fields, prefix)?;
    for (field, target) in guards {
        let path = format!("{prefix}{field}");
        let member = request_fields
            .iter_mut()
            .find(|member| member["path"] == path)
            .with_context(|| format!("revision guard {field} has no WIT field"))?;
        ensure!(
            member["revision"] == true,
            "guard {field} needs a WIT revision type"
        );
        let target = target.as_str().context("revision guard target field")?;
        member["revision"] = format!("{prefix}{target}").into();
    }
    input.insert("fields".to_owned(), request_fields.into());

    Ok(())
}

fn read_result(
    world: &Resolve,
    interface: &Interface,
    name: &str,
    operation: &mut Value,
    result: Option<Type>,
) -> Result<()> {
    let feed = operation["feed"] == true;
    if let Some(result) = result {
        let row = named_type(interface, name, "row");
        let (class, members) = match (definition(world, result)?, row) {
            (TypeDefKind::Variant(_), Some(row)) if feed => ("bounded_list", row),
            (TypeDefKind::Record(record), Some(row)) => {
                let [carrier] = record.fields.as_slice() else {
                    bail!("list result needs one row carrier");
                };
                match (carrier.name.as_str(), carrier.ty) {
                    ("rows", Type::Id(id)) if matches!(world.types[id].kind, TypeDefKind::List(ty) if ty == row) => {
                        ("bounded_list", row)
                    }
                    ("value", Type::Id(id)) if matches!(world.types[id].kind, TypeDefKind::Option(ty) if ty == row) => {
                        ("optional_one", row)
                    }
                    _ => bail!("result needs a rows list or an optional value"),
                }
            }
            (TypeDefKind::Record(_), None) => ("one", result),
            _ => bail!("unsupported operation result"),
        };
        let mut fields = Vec::new();
        let TypeDefKind::Record(record) = definition(world, members)? else {
            bail!("result fields must be in a record");
        };
        for field in &record.fields {
            if feed && field.name == "row-version" {
                ensure!(field.ty == Type::S64, "feed row-version must be s64");
            } else {
                read_fields(world, field.ty, &public_name(&field.name), &mut fields)?;
            }
        }
        if operation["result"].is_null() {
            operation["result"] = json!({});
        }
        let output = operation["result"]
            .as_object_mut()
            .context("result policy")?;
        apply_labels(output, &mut fields, "")?;
        output.insert("fields".to_owned(), fields.into());
        output.insert("class".to_owned(), class.into());
    } else {
        ensure!(
            operation["result"].is_null(),
            "operation has result policy but no WIT result"
        );
    }
    Ok(())
}

fn read_errors(
    world: &Resolve,
    interface: &Interface,
    name: &str,
    operation: &mut Value,
) -> Result<()> {
    let errors = named_type(interface, name, "error").context("missing error variant")?;
    let TypeDefKind::Variant(errors) = definition(world, errors)? else {
        bail!("errors must be a WIT variant");
    };
    let operation = operation.as_object_mut().context("operation policy")?;
    let mut text = take_map(operation, "error_text");
    let mut details = Map::new();
    let mut names = Vec::new();
    for case in &errors.cases {
        let literal = public_name(&case.name);
        let mut required = Vec::new();
        let mut optional = Vec::new();
        if let Some(payload) = case.ty {
            let TypeDefKind::Record(record) = definition(world, payload)? else {
                bail!("error {literal} needs a record payload");
            };
            for field in &record.fields {
                let (nullable, ty) = member_type(world, field.ty);
                let fields = if nullable {
                    &mut optional
                } else {
                    &mut required
                };
                ensure!(
                    if matches!(
                        field.name.as_str(),
                        "expected-row-version" | "observed-row-version"
                    ) {
                        ty == Type::S32 || ty == Type::S64
                    } else {
                        ty == Type::String
                    },
                    "error {literal} field {} has an unsupported payload type",
                    field.name
                );
                fields.push(public_name(&field.name));
            }
        }
        let mut detail = json!({"required": required, "optional": optional});
        if let Some(text) = text.remove(&literal) {
            detail["text"] = text;
        }
        details.insert(literal.clone(), detail);
        names.push(literal);
    }
    ensure!(
        text.is_empty(),
        "error display text names unknown WIT errors: {:?}",
        text.keys()
    );
    operation.insert("errors".to_owned(), json!(names));
    operation.insert("error_details".to_owned(), details.into());
    Ok(())
}

fn take_map(object: &mut Map<String, Value>, name: &str) -> Map<String, Value> {
    match object.remove(name) {
        Some(Value::Object(entries)) => entries,
        _ => Map::new(),
    }
}

fn apply_labels(object: &mut Map<String, Value>, fields: &mut [Value], prefix: &str) -> Result<()> {
    let mut labels = take_map(object, "labels");
    for field in fields {
        let path = field["path"].as_str().expect("field path");
        let name = path.strip_prefix(prefix).unwrap_or(path);
        if let Some(Value::Object(mut label)) = labels.remove(name) {
            if let Some(reference) = label.get_mut("references")
                && let Some(name) = reference["narrowed_by"].as_str()
            {
                reference["narrowed_by"] = format!("{prefix}{name}").into();
            }
            field.as_object_mut().expect("field object").extend(label);
        }
    }
    ensure!(
        labels.is_empty(),
        "presentation names unknown WIT fields: {:?}",
        labels.keys()
    );
    Ok(())
}

fn named_type(interface: &Interface, name: &str, suffix: &str) -> Option<Type> {
    interface
        .types
        .get(&format!("{name}-{suffix}"))
        .copied()
        .map(Type::Id)
}

fn definition(world: &Resolve, ty: Type) -> Result<&TypeDefKind> {
    if let Type::Id(id) = ty {
        match world.types[id].kind {
            TypeDefKind::Type(ty) => definition(world, ty),
            ref other => Ok(other),
        }
    } else {
        bail!("expected a named WIT type");
    }
}

fn member_type(world: &Resolve, ty: Type) -> (bool, Type) {
    match ty {
        Type::Id(id) => match world.types[id].kind {
            TypeDefKind::Option(ty) => (true, member_type(world, ty).1),
            TypeDefKind::Type(ty) => member_type(world, ty),
            _ => (false, ty),
        },
        _ => (false, ty),
    }
}

fn public_name(name: &str) -> String {
    name.replace('-', "_")
}

fn read_fields(world: &Resolve, ty: Type, path: &str, fields: &mut Vec<Value>) -> Result<()> {
    read_field(world, ty, path, false, fields)
}

fn read_field(
    world: &Resolve,
    ty: Type,
    path: &str,
    nullable: bool,
    fields: &mut Vec<Value>,
) -> Result<()> {
    let mut field = json!({"path": path, "nullable": nullable});
    field["type"] = match ty {
        Type::Bool => "boolean",
        Type::S32 => "int32",
        Type::S64 => "int64",
        Type::F64 => "float64",
        Type::String => "text",
        Type::Id(id) => {
            let defined = &world.types[id];
            match (defined.name.as_deref(), &defined.kind) {
                (Some("uuid"), TypeDefKind::Type(Type::String)) => "uuid",
                (Some("numeric"), TypeDefKind::Type(Type::String)) => "numeric",
                (Some("timestamp"), TypeDefKind::Type(Type::String)) => "timestamptz",
                (
                    Some("revision" | "revision-long"),
                    TypeDefKind::Type(ty @ (Type::S32 | Type::S64)),
                ) => {
                    field["revision"] = true.into();
                    if *ty == Type::S32 { "int32" } else { "int64" }
                }
                (Some("json-value"), _) => "json",
                (_, TypeDefKind::Type(ty)) => {
                    return read_field(world, *ty, path, nullable, fields);
                }
                (_, TypeDefKind::Option(ty)) => return read_field(world, *ty, path, true, fields),
                (_, TypeDefKind::List(Type::U8)) => "bytes",
                (_, TypeDefKind::List(ty)) if !nullable => {
                    return read_fields(world, *ty, &format!("{path}[]"), fields);
                }
                (_, TypeDefKind::Record(record)) if !nullable => {
                    for member in &record.fields {
                        read_fields(
                            world,
                            member.ty,
                            &format!("{path}.{}", public_name(&member.name)),
                            fields,
                        )?;
                    }
                    return Ok(());
                }
                (_, TypeDefKind::Enum(cases)) => {
                    field["values"] = json!(
                        cases
                            .cases
                            .iter()
                            .map(|case| public_name(&case.name))
                            .collect::<Vec<_>>()
                    );
                    "text"
                }
                _ => bail!("unsupported WIT field {path}"),
            }
        }
        _ => bail!("unsupported WIT field {path}"),
    }
    .into();
    fields.push(field);
    Ok(())
}

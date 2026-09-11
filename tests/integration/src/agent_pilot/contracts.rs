//! Read the contract paths used by the retained pilot grader.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::{Value, json};

use super::{read_json, text};

pub(super) fn files(directory: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if !directory.is_dir() {
        return Ok(paths);
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            paths.extend(files(&entry.path())?);
        } else if kind.is_file() {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

pub(super) fn contract(root: &Path, operation: &str, side: &str) -> anyhow::Result<Option<Value>> {
    let Some((domain, action)) = operation.split_once('.') else {
        return Ok(None);
    };
    let suffix = PathBuf::from("contracts")
        .join(domain)
        .join(format!("{action}.{side}.json"));
    files(root)?
        .into_iter()
        .find(|path| path.ends_with(&suffix))
        .map(|path| read_json(&path))
        .transpose()
}

pub(super) fn fields(document: &Value) -> Vec<Value> {
    let mut fields = Vec::new();
    if let Some(declared) = document["fields"].as_array() {
        fields.extend(
            declared
                .iter()
                .map(|field| json!({"path":field["path"],"type":field["type"]})),
        );
    } else {
        if let Some(members) = document.as_object() {
            fields.extend(
                members
                    .iter()
                    .filter(|(_, value)| {
                        value.get("type").is_some() && value.get("required").is_some()
                    })
                    .map(|(name, value)| json!({"path":name,"type":value["type"]})),
            );
        }
        fields.extend(
            document["writable_fields"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|field| json!({"path":field["field"],"type":field["type"]})),
        );
    }
    for field in &mut fields {
        field["name"] = json!(
            text(&field["path"])
                .replace("[]", "")
                .rsplit('.')
                .next()
                .unwrap_or_default()
        );
    }
    fields
}

pub(super) fn path_map(document: &Value, input: bool) -> BTreeMap<String, String> {
    let paths = if input {
        fields(document)
            .into_iter()
            .map(|field| text(&field["path"]).to_owned())
            .collect::<Vec<_>>()
    } else {
        document["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|field| text(&field["path"]).to_owned())
            .collect()
    };
    let mut map = BTreeMap::<String, String>::new();
    for path in paths {
        let name = path
            .replace("[]", "")
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_owned();
        if map
            .get(&name)
            .is_none_or(|prior| path.split('.').count() < prior.split('.').count())
        {
            map.insert(name, path);
        }
    }
    map
}

pub(super) fn route(root: &Path, operation: &str) -> anyhow::Result<String> {
    let attachment = files(root)?
        .into_iter()
        .find(|path| path.ends_with("publication/attachments.json"));
    let Some(attachment) = attachment else {
        return Ok(String::new());
    };
    let document = read_json(&attachment)?;
    let mut objects = Vec::new();
    visit_objects(&document, &mut objects);
    let wiring = operation.replace('.', "_");
    for object in &objects {
        if object["wiring-id"] == wiring || object["operation"] == operation {
            for value in [
                &object["definition"]["route"]["path"],
                &object["route"]["path"],
                &object["path"],
            ] {
                if let Some(path) = value.as_str() {
                    return Ok(path.to_owned());
                }
            }
        }
    }
    let tail = format!("/{}", operation.replace('.', "/"));
    Ok(objects
        .into_iter()
        .filter_map(|object| object["route"]["path"].as_str())
        .find(|path| path.ends_with(&tail))
        .unwrap_or_default()
        .to_owned())
}

pub(super) fn visit_objects<'a>(document: &'a Value, output: &mut Vec<&'a Value>) {
    match document {
        Value::Object(members) => {
            output.push(document);
            for value in members.values() {
                visit_objects(value, output);
            }
        }
        Value::Array(values) => {
            for value in values {
                visit_objects(value, output);
            }
        }
        _ => (),
    }
}

fn word(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

fn quoted(value: &str) -> Option<&str> {
    value.strip_prefix('`')?.strip_suffix('`')
}

fn entries(cell: &str) -> Vec<Value> {
    cell.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|entry| {
            if let Some((name, kind)) = entry.split_once(' ')
                && let Some(name) = quoted(name)
                && word(name)
                && !kind.is_empty()
                && kind.bytes().all(|byte| byte.is_ascii_lowercase())
            {
                return json!({"name":name,"type":kind});
            }
            json!({"unreadable":entry})
        })
        .collect()
}

fn brief_table(brief: &str) -> (Vec<Value>, Vec<Value>) {
    let mut inside = false;
    let mut operations = Vec::new();
    let mut scalars = Vec::new();
    for line in brief.lines() {
        if line.starts_with("## The data contract") {
            inside = true;
            continue;
        }
        if inside
            && line
                .strip_prefix("## ")
                .and_then(|rest| rest.bytes().next())
                .is_some_and(|byte| byte.is_ascii_uppercase())
        {
            break;
        }
        if !inside || !line.starts_with('|') {
            continue;
        }
        let cells = line
            .split('|')
            .map(str::trim)
            .filter(|cell| !cell.is_empty())
            .collect::<Vec<_>>();
        if cells.len() == 3
            && let Some(operation) = quoted(cells[0])
            && let Some((domain, action)) = operation.split_once('.')
            && word(domain)
            && word(action)
        {
            operations.push(
                json!({"operation":operation,"input":entries(cells[1]),"result":entries(cells[2])}),
            );
        }
        if cells.len() == 2
            && let (Some(scalar), Some(form)) = (quoted(cells[0]), quoted(cells[1]))
            && word(scalar)
            && !form.is_empty()
            && form
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            scalars.push(json!({"scalar":scalar,"form":form}));
        }
    }
    (operations, scalars)
}

fn contract_token(kind: &str) -> &str {
    match kind {
        "text" | "uuid" | "list" => kind,
        "timestamp" => "timestamptz",
        "date" => "text",
        _ => "",
    }
}

fn assert_field(
    operation: &str,
    side: &str,
    declared: Option<&[Value]>,
    name: &str,
    kind: &str,
) -> Value {
    let wanted = contract_token(kind);
    let fields = declared.unwrap_or_default();
    let named = fields
        .iter()
        .filter(|field| field["name"] == name)
        .collect::<Vec<_>>();
    let listed = fields
        .iter()
        .find(|field| text(&field["path"]).starts_with(&format!("{name}[]")));
    let mut row = json!({"operation":operation,"side":side,"field":name,"type":kind});
    let mut types = named
        .iter()
        .map(|field| text(&field["type"]).to_owned())
        .collect::<Vec<_>>();
    types.sort();
    types.dedup();
    let verdict = if declared.is_none() {
        json!({"ok":false,"evidence":format!("the package publishes no {side} contract")})
    } else if wanted.is_empty() {
        json!({"ok":false,"evidence":"the brief names a type the platform does not publish"})
    } else if wanted == "list" {
        match listed {
            Some(field) => json!({"ok":true,"declared":field["path"]}),
            None => json!({"ok":false,"evidence":format!("no declared path begins with {name}[]")}),
        }
    } else if named.is_empty() {
        let mut paths = fields
            .iter()
            .map(|field| text(&field["path"]))
            .collect::<Vec<_>>();
        paths.sort();
        json!({"ok":false,"evidence":format!("the package declares no {side} field named {name}. it declares {}",paths.join(", "))})
    } else if types != [wanted] {
        json!({"ok":false,"declared":named[0]["path"],"evidence":format!("declared {}, the brief says {kind}, which the platform publishes as {wanted}",types.join(", "))})
    } else {
        json!({"ok":true,"declared":format!("{} {}",text(&named[0]["path"]),text(&named[0]["type"]))})
    };
    row.as_object_mut()
        .expect("constructed object")
        .extend(verdict.as_object().expect("constructed object").clone());
    row
}

pub(super) fn grade(root: &Path, brief: &Path) -> anyhow::Result<Vec<Value>> {
    let (operations, scalars) = brief_table(
        &fs::read_to_string(brief).with_context(|| format!("read {}", brief.display()))?,
    );
    anyhow::ensure!(
        !operations.is_empty(),
        "the data contract in {} names no operation",
        brief.display()
    );
    let mut rows = Vec::new();
    for operation in &operations {
        let name = text(&operation["operation"]);
        for side in ["input", "result"] {
            let declared = contract(root, name, side)?.map(|document| fields(&document));
            for entry in operation[side].as_array().into_iter().flatten() {
                if let Some(field) = entry["name"].as_str() {
                    rows.push(assert_field(
                        name,
                        side,
                        declared.as_deref(),
                        field,
                        text(&entry["type"]),
                    ));
                } else {
                    rows.push(json!({"operation":name,"side":side,"ok":false,"evidence":format!("the entry {} is not a `name` type pair",text(&entry["unreadable"]))}));
                }
            }
        }
    }
    for scalar in scalars {
        let scalar_name = text(&scalar["scalar"]);
        let form = text(&scalar["form"]);
        let key = contract_token(scalar_name);
        let mut declared = Vec::new();
        let mut wrong = Vec::new();
        for operation in &operations {
            let name = text(&operation["operation"]);
            if let Some(document) = contract(root, name, "input")? {
                let got = text(&document["canonicalization"][key]);
                if got.is_empty() {
                    continue;
                }
                declared.push(name);
                if got != form {
                    wrong.push(format!("{name}={got}"));
                }
            }
        }
        let mut row = json!({"scalar":scalar_name,"key":key,"form":form});
        let result = if declared.is_empty() {
            json!({"ok":false,"evidence":format!("no input contract publishes a canonicalization for {key}")})
        } else if !wrong.is_empty() {
            json!({"ok":false,"evidence":format!("declared otherwise by {}",wrong.join(" "))})
        } else {
            json!({"ok":true,"declared":declared.join(" ")})
        };
        row.as_object_mut()
            .expect("constructed object")
            .extend(result.as_object().expect("constructed object").clone());
        rows.push(row);
    }
    Ok(rows)
}

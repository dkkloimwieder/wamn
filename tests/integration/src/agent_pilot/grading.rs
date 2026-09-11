//! Apply the pilot's retained response and field-placement predicates.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::Context as _;
use serde_json::{Value, json};

use super::{contracts, text};

#[derive(Debug)]
pub(super) struct Grading {
    pub directory: PathBuf,
    pub root: PathBuf,
    pub overlay: String,
    pub steps: Vec<Value>,
    pub results: Vec<Value>,
}

pub(super) fn at<'a>(value: &'a Value, path: &str) -> &'a Value {
    path.split('.')
        .fold(value, |value, key| value.get(key).unwrap_or(&Value::Null))
}

pub(super) fn string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
}

fn optional(value: &Value) -> String {
    if value.is_null() || value == false {
        String::new()
    } else {
        string(value)
    }
}

fn put(value: &mut Value, path: &str, replacement: Value) -> anyhow::Result<()> {
    let (head, tail) = path
        .split_once('.')
        .map_or((path, None), |(head, tail)| (head, Some(tail)));
    let object = value
        .as_object_mut()
        .context("published input paths must place fields in objects")?;
    if let Some(tail) = tail {
        let child = object.entry(head).or_insert_with(|| json!({}));
        put(child, tail, replacement)
    } else {
        object.insert(head.to_owned(), replacement);
        Ok(())
    }
}

impl Grading {
    fn result_map(&self, operation: &str) -> anyhow::Result<BTreeMap<String, String>> {
        Ok(contracts::contract(&self.root, operation, "result")?
            .map(|document| contracts::path_map(&document, false))
            .unwrap_or_default())
    }

    fn read_result(&self, from: &str, path: &str) -> anyhow::Result<String> {
        let operation = self
            .steps
            .iter()
            .find(|step| step["id"] == from)
            .map(|step| text(&step["route"]["operation"]))
            .unwrap_or_default();
        let map = self.result_map(operation)?;
        let name = path.rsplit('.').next().unwrap_or_default();
        let resolved = map.get(name).map(|declared| {
            format!(
                "{}.{declared}",
                path.rsplit_once('.').map_or(path, |(prefix, _)| prefix)
            )
        });
        let result = self
            .results
            .iter()
            .find(|result| result["id"] == from)
            .unwrap_or(&Value::Null);
        Ok(optional(at(result, resolved.as_deref().unwrap_or(path))))
    }

    pub fn body(&self, step: &Value, operation: &str, request_id: &str) -> anyhow::Result<Value> {
        let contract = contracts::contract(&self.root, operation, "input")?.with_context(|| {
            format!(
                "no published input contract for {operation} under {}",
                self.overlay
            )
        })?;
        let map = contracts::path_map(&contract, true);
        let mut values = step.get("body").cloned().unwrap_or_else(|| json!({}));
        let values = values
            .as_object_mut()
            .context("a fixture body is an object")?;
        if let Some(reuse) = step["reuse"].as_object() {
            for (field, source) in reuse {
                let source = text(source);
                let (prior, path) = source.split_once('.').unwrap_or((source, source));
                values.insert(field.clone(), json!(self.read_result(prior, path)?));
            }
        }
        values.insert("request_id".to_owned(), json!(request_id));
        if map.contains_key("idempotency_key") && !values.contains_key("idempotency_key") {
            values.insert("idempotency_key".to_owned(), step["id"].clone());
        }
        let missing = values
            .keys()
            .filter(|name| !map.contains_key(*name))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            let mut declared = map.values().cloned().collect::<Vec<_>>();
            declared.sort();
            anyhow::bail!(
                "fixture field {} matches no declared path for {operation}. the contract declares {}",
                missing.join(", "),
                declared.join(", ")
            );
        }
        let mut body = json!({});
        for (name, value) in values {
            put(&mut body, &map[name], value.clone())?;
        }
        Ok(body)
    }

    pub fn remember(&mut self, id: &str, payload: &str) {
        let parsed = serde_json::from_str::<Value>(payload)
            .ok()
            .filter(|value| !value.is_null() && value != &false)
            .unwrap_or_else(|| json!({}));
        let value = if let Some(array) = parsed.as_array() {
            array
                .first()
                .and_then(|first| first.get("value"))
                .cloned()
                .filter(|value| !value.is_null() && value != &false)
                .unwrap_or_else(|| json!({}))
        } else {
            parsed
        };
        self.results.push(json!({"id":id,"value":value}));
    }

    pub fn verdict(
        &self,
        step: &Value,
        status: &str,
        payload: &str,
        operation: &str,
    ) -> anyhow::Result<(bool, String)> {
        let map = self.result_map(operation)?;
        let result_path = |name: &str| map.get(name).cloned().unwrap_or_else(|| name.to_owned());
        let expected_status = step["expect"]
            .get("status")
            .map(string)
            .unwrap_or_else(|| "200".to_owned());
        let mut evidence = format!("status={status}");
        if status != expected_status {
            return Ok((false, evidence));
        }
        let parsed = serde_json::from_str::<Value>(payload).unwrap_or_else(|_| json!({}));
        let first = parsed
            .as_array()
            .map_or(&parsed, |values| values.first().unwrap_or(&Value::Null));
        if step["expect"]["item"] == "error" {
            let got = optional(&first["error"]["code"]);
            evidence.push_str(&format!(" error_code={got}"));
            return Ok((got == optional(&step["expect"]["error_code"]), evidence));
        }
        let Some(value) = first.get("value") else {
            return Ok((false, format!("{evidence} no value item")));
        };
        let mut pass = true;
        for path in step["expect"]["present"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let path = result_path(path);
            let got = optional(at(value, &path));
            evidence.push_str(&format!(
                " present:{path}={}",
                if got.is_empty() { "<empty>" } else { &got }
            ));
            if got.is_empty() || got == "null" {
                pass = false;
            }
        }
        if let Some(values) = step["expect"]["value"].as_object() {
            for (path, wanted) in values {
                let resolved = result_path(path);
                let actual = string(at(value, &resolved));
                let wanted = string(wanted);
                evidence.push_str(&format!(" {resolved}={actual} want={wanted}"));
                if actual != wanted {
                    pass = false;
                }
            }
        }
        if let Some(equal) = step["expect"]["equals"].as_object() {
            for (path, source) in equal {
                let resolved = result_path(path);
                let source = text(source);
                let (prior, path) = source.split_once('.').unwrap_or((source, source));
                let mine = string(at(value, &resolved));
                let theirs = self.read_result(prior, path)?;
                evidence.push_str(&format!(" {resolved}={mine} vs {theirs}"));
                if mine != theirs || mine.is_empty() || mine == "null" {
                    pass = false;
                }
            }
        }
        if let Some(count) = step["expect"].get("count") {
            let path = result_path(text(&count["path"]));
            let wanted = string(&count["is"]);
            let got = match at(value, &path) {
                Value::Null => "0".to_owned(),
                Value::Array(values) => values.len().to_string(),
                Value::Object(values) => values.len().to_string(),
                Value::String(value) => value.chars().count().to_string(),
                Value::Number(value) => value
                    .as_f64()
                    .context("count is numeric")?
                    .abs()
                    .to_string(),
                Value::Bool(_) => anyhow::bail!("the count predicate cannot read a boolean"),
            };
            evidence.push_str(&format!(" count:{path}={got} want={wanted}"));
            if got != wanted {
                pass = false;
            }
        }
        if let Some(sorted) = step["expect"].get("sorted_by") {
            let mut path = result_path(text(&sorted["path"]));
            let field = text(&sorted["field"]);
            let declared = map.get(field).map(String::as_str).unwrap_or_default();
            if let Some((list, _)) = declared.split_once("[]") {
                path = list.trim_end_matches('.').to_owned();
            }
            let keys = at(value, &path)
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| row[field].clone())
                .collect::<Vec<_>>();
            let mut ordered = keys.clone();
            ordered.sort_by(compare);
            if sorted["direction"].as_str().unwrap_or("asc") != "asc" {
                ordered.reverse();
            }
            let verdict = if keys.is_empty() {
                "empty"
            } else if keys == ordered {
                "true"
            } else {
                "false"
            };
            evidence.push_str(&format!(" sorted_by:{path}.{field}={verdict}"));
            if verdict != "true" {
                pass = false;
            }
        }
        Ok((pass, evidence))
    }

    pub fn replay(
        &mut self,
        records: &[Value],
        loop_pass: bool,
        base_url: &str,
    ) -> anyhow::Result<Vec<Value>> {
        let mut output = Vec::new();
        for step in self.steps.clone() {
            let id = text(&step["id"]);
            let mut payload = String::new();
            let (pass, evidence) = if !loop_pass || base_url.is_empty() {
                (false, "not run: the loop served no release".to_owned())
            } else if step.get("sql").is_some() {
                let rows = records
                    .iter()
                    .find(|row| row["id"] == id)
                    .map(|row| optional(&row["rows"]))
                    .filter(|rows| !rows.is_empty())
                    .unwrap_or_else(|| "unrecorded".to_owned());
                let expected = optional(&step["expect"]["rows"]);
                (
                    rows == expected,
                    format!("sql rows={rows} expected={expected}"),
                )
            } else if let Some(concurrent) = step["concurrent"].as_array() {
                let code = text(&step["expect"]["exactly_one"]["error_code"]);
                let mut refusals = 0;
                for target in concurrent {
                    let path = self
                        .directory
                        .join("grade")
                        .join(format!("{id}-{}.out", text(target)));
                    if fs::read_to_string(path)
                        .unwrap_or_default()
                        .contains(&format!("\"{code}\""))
                    {
                        refusals += 1;
                    }
                }
                (
                    refusals == 1,
                    format!("concurrent refusals={refusals} expected=1 code={code}"),
                )
            } else if let Some(record) = records.iter().find(|row| row["id"] == id) {
                payload = optional(&record["response"]);
                self.verdict(
                    &step,
                    &optional(&record["status"]),
                    &payload,
                    text(&step["route"]["operation"]),
                )?
            } else {
                (
                    false,
                    format!("not replayable: the run recorded no response for {id}"),
                )
            };
            output.push(json!({"id":id,"must":step["must"].as_bool().unwrap_or(false),"invariant":text(&step["invariant"]),"proves":text(&step["proves"]),"pass":pass,"evidence":evidence}));
            if loop_pass && !base_url.is_empty() {
                self.remember(id, &payload);
            }
        }
        Ok(output)
    }

    pub fn placement(&mut self, records: &[Value]) -> anyhow::Result<Vec<Value>> {
        let mut output = Vec::new();
        for step in self.steps.clone() {
            let id = text(&step["id"]);
            let operation = text(&step["route"]["operation"]);
            if !operation.is_empty() {
                output.push(match self.body(&step,operation,id) {Ok(body)=>json!({"id":id,"operation":operation,"ok":true,"body":body}),Err(error)=>json!({"id":id,"operation":operation,"ok":false,"evidence":error.to_string()})});
            }
            let payload = records
                .iter()
                .find(|row| row["id"] == id)
                .map(|row| optional(&row["response"]))
                .unwrap_or_default();
            self.remember(id, &payload);
        }
        Ok(output)
    }
}

fn compare(left: &Value, right: &Value) -> Ordering {
    let rank = |value: &Value| match value {
        Value::Null => 0,
        Value::Bool(false) => 1,
        Value::Bool(true) => 2,
        Value::Number(_) => 3,
        Value::String(_) => 4,
        Value::Array(_) => 5,
        Value::Object(_) => 6,
    };
    let order = rank(left).cmp(&rank(right));
    if order != Ordering::Equal {
        return order;
    }
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left
            .as_f64()
            .partial_cmp(&right.as_f64())
            .unwrap_or(Ordering::Equal),
        (Value::String(left), Value::String(right)) => left.cmp(right),
        (Value::Array(left), Value::Array(right)) => left
            .iter()
            .zip(right)
            .map(|(left, right)| compare(left, right))
            .find(|order| *order != Ordering::Equal)
            .unwrap_or_else(|| left.len().cmp(&right.len())),
        (Value::Object(left), Value::Object(right)) => {
            left.keys().cmp(right.keys()).then_with(|| {
                left.values()
                    .zip(right.values())
                    .map(|(left, right)| compare(left, right))
                    .find(|order| *order != Ordering::Equal)
                    .unwrap_or(Ordering::Equal)
            })
        }
        _ => Ordering::Equal,
    }
}

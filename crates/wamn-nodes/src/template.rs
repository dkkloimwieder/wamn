//! `{{expression}}` string templating over the input payload — used by
//! `http-request` (url, header values) and the `postgres` list filters. The
//! text between `{{` and `}}` is a JMESPath expression; its result is
//! stringified: a string verbatim, a number/bool via `to_string` (numbers pass
//! through `serde_json::Number` — exact), `null` as the empty string, and an
//! array/object as compact JSON.

use serde_json::Value;
use wamn_node_sdk::{ErrorDetail, NodeError};

use crate::expr::eval_to_value;

/// Expand every `{{expr}}` span in `template` against `input`.
pub(crate) fn expand(template: &str, input: &Value) -> Result<String, NodeError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(NodeError::Terminal(ErrorDetail::coded(
                "invalid-template",
                format!("unclosed {{{{...}}}} in template {template:?}"),
            )));
        };
        let expr = after[..end].trim();
        out.push_str(&stringify(&eval_to_value(expr, input)?));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn stringify(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

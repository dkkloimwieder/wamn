use serde_json::{Value, json};

use super::fixture;

/// The fixture with `widget.archive` as a command idempotent by claim: the
/// codec claims its key in the write log, and the command locks and reads one
/// widget.
pub(super) fn manifest() -> Value {
    let mut value = fixture::manifest();
    value["custom_operations"]["widget.archive"] = json!({
        "type": "command", "visibility": "public", "permission": "widget.archive",
        "connection": "postgres", "transaction": "explicit_per_input",
        "automatic_retry": false, "idempotent_by": "claim",
        "canonicalization": {"excluded_fields": ["idempotency_key"]},
        "input": {"fields": [
            {"path": "idempotency_key", "type": "text", "nullable": false},
            {"path": "id", "type": "uuid", "nullable": false},
            {"path": "payload", "type": "text", "nullable": false}]},
        "result": {"class": "one", "fields": [
            {"path": "id", "type": "uuid", "nullable": false}]},
        "errors": ["invalid_input", "idempotency_conflict", "retry", "timeout",
            "permission_denied", "internal_error"],
        "error_details": {"idempotency_conflict": {"required": ["field"]}},
        "constraint_errors": {},
        "relations": [{"schema": "inventory", "table": "widget",
            "select_fields": ["id", "edit_version", "note"],
            "insert_fields": [], "update_fields": [], "lock": true,
            "constraints": []}],
        "statements": {
            "archive": {"path": "command/widget/archive.sql", "fetch": "one",
                "parameters": [{"name": "id", "type": "uuid", "nullable": false}],
                "row": [
                    {"name": "id", "type": "uuid", "nullable": false},
                    {"name": "edit_version", "type": "int64", "nullable": false},
                    {"name": "note", "type": "text", "nullable": true}]}
        }
    });
    value
}

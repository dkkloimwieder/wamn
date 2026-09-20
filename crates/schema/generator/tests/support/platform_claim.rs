use serde_json::{Value, json};

use super::fixture;

pub(super) fn manifest() -> Value {
    let mut value = fixture::manifest();
    value["custom_operations"]["widget.archive"] = json!({
        "kind": "command", "visibility": "public", "permission": "widget.archive",
        "connection": "postgres", "transaction": "explicit_per_input",
        "automatic_retry": false, "idempotent_by": "claim",
        "claim": {"table": "widget_command", "identities": {"id": "widget_id"},
            "claim": "claim", "replay": "replay", "finalize": "finalize"},
        "canonicalization": {"excluded_fields": ["idempotency_key"]},
        "input": {"fields": [
            {"path": "idempotency_key", "type": "text", "nullable": false},
            {"path": "payload", "type": "text", "nullable": false}]},
        "result": {"class": "one", "fields": [
            {"path": "id", "type": "uuid", "nullable": false}]},
        "errors": ["invalid_input", "idempotency_conflict", "retry", "timeout",
            "permission_denied", "internal_error"],
        "error_details": {"idempotency_conflict": {"required": ["field"]}},
        "constraint_errors": {},
        "relations": [{"schema": "inventory", "table": "widget_command",
            "select_fields": ["canonical_command", "idempotency_key", "widget_id"],
            "insert_fields": ["canonical_command", "idempotency_key"],
            "update_fields": [], "lock": false,
            "constraints": ["widget_command_pkey", "widget_command_widget_id_key"]}],
        "statements": {
            "claim": {"path": "command/widget/claim.sql", "fetch": "optional_one",
                "parameters": [
                    {"name": "canonical_command", "type": "bytes", "nullable": false},
                    {"name": "idempotency_key", "type": "text", "nullable": false}],
                "row": [{"name": "widget_id", "type": "uuid", "nullable": false}]},
            "replay": {"path": "command/widget/replay.sql", "fetch": "optional_one",
                "parameters": [{"name": "idempotency_key", "type": "text", "nullable": false}],
                "row": [
                    {"name": "canonical_command", "type": "bytes", "nullable": false},
                    {"name": "widget_id", "type": "uuid", "nullable": false}]},
            "finalize": {"path": "command/widget/finalize.sql", "fetch": "one",
                "parameters": [{"name": "idempotency_key", "type": "text", "nullable": false}],
                "row": [{"name": "widget_id", "type": "uuid", "nullable": false}]}
        }
    });
    value
}

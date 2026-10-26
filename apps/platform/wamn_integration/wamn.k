import manifest

manifest.Package {
    package = {id = "wamn_integration", version = "0.1.0"}
    required_platform_policy_contract = {id = "wamn_integration_data_access", state = "satisfied"}
    models = {
        setting = manifest.Model("setting", "wamn_integration") {
            table = "setting"
            owner = "wamn_integration"
            audit_log = {columns = []}
            operations = {}
        }
        log = manifest.Model("log", "wamn_integration") {
            table = "log"
            owner = "wamn_integration"
            audit_log = {columns = []}
            operations = {}
        }
    }
    custom_operations = {
        "setting.read" = manifest.Projection("setting.read") {
            permission = None
            visibility = "private"
            input.fields = [{path = "key", type = "text"}, {path = "carry", type = "json", nullable = True}]
            result = {class = "one", fields = [{path = "value", type = "json", nullable = True}, {path = "carry", type = "json", nullable = True}]}
            errors = ["invalid_input", "retry", "timeout", "internal_error"]
            statements = {
                read_setting = manifest.Statement("command/setting", "read") {
                    fetch = "optional_one"
                    parameters = [manifest.text("key")]
                    row = [manifest.json("value")]
                }
            }
        }
        "setting.write" = manifest.Command("setting.write") {
            permission = None
            visibility = "private"
            idempotent_by = "claim"
            input: {fields = [manifest.request_id, manifest.idempotency_key,
                {path = "value.key", type = "text"}, {path = "value.value", type = "json"}]}
            result = {class = "one", fields = [{path = "key", type = "text"}]}
            errors = ["invalid_input", "idempotency_conflict", "retry", "timeout", "internal_error"]
            statements = {
                write_setting = manifest.Statement("command/setting", "write") {
                    fetch = "one"
                    parameters = [manifest.text("key"), manifest.json("value")]
                    row = [manifest.text("key")]
                }
            }
        }
        "log.append" = manifest.Command("log.append") {
            permission = None
            visibility = "private"
            idempotent_by = "claim"
            input: {fields = [manifest.request_id, manifest.idempotency_key,
                {path = "value.delivery_id", type = "text"},
                {path = "value.record_id", type = "text"},
                {path = "value.outcome", type = "text", values = ["pushed", "refused"]},
                {path = "value.detail", type = "json", nullable = True}]}
            result = {class = "one", fields = [{path = "record_id", type = "text"}]}
            errors = ["invalid_input", "idempotency_conflict", "retry", "timeout", "internal_error"]
            statements = {
                append_log = manifest.Statement("command/log", "append") {
                    fetch = "optional_one"
                    parameters = [manifest.text("delivery_id"), manifest.text("record_id"), manifest.text("outcome"), manifest.nullable(manifest.json("detail"))]
                    row = [manifest.text("delivery_id"), manifest.text("record_id"), manifest.text("outcome")]
                }
            }
        }
    }
    connections = {}
    components = {wamn_integration = {connections = ["postgres"]}}
}

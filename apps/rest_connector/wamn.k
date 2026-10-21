import manifest

manifest.Package {
    package = {id = "rest_connector", version = "0.1.0"}
    required_platform_policy_contract = {id = "rest_connector_data_access", state = "satisfied"}
    models = {
        item = manifest.Model("item", "rest_connector") {
            table = "item"
            owner = "rest_connector"
            audit_log = {columns = []}
            operations = {}
        }
    }
    custom_operations = {
        "item.record" = manifest.Command("item.record") {
            permission = None
            visibility = "private"
            idempotent_by = "claim"
            input: {fields = [manifest.request_id, manifest.idempotency_key,
                {path = "value.key", type = "text"}, {path = "value.name", type = "text"}]}
            result = {class = "one", fields = [{path = "key", type = "text"}]}
            errors = ["invalid_input", "idempotency_conflict", "retry", "timeout", "internal_error"]
            statements = {
                record_item = manifest.Statement("command/item", "record") {
                    fetch = "optional_one"
                    parameters = [manifest.text("key"), manifest.text("name")]
                    row = [manifest.text("key")]
                }
            }
        }
    }
    connections = ["postgres"]
    components = {rest_connector = {connections = ["postgres"]}}
    workflows = {item = {wiring = "rest_item", schedule = "* * * * *"}}
}

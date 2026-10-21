import manifest

manifest.Package {
    package = {id = "rest_consumer_fixture", version = "0.1.0"}
    required_platform_policy_contract = {id = "rest_consumer_fixture_data_access", state = "satisfied"}
    models = {
        item = manifest.Model("item", "rest_consumer_fixture") {
            table = "item"
            owner = "rest_consumer_fixture"
            audit_log = {columns = []}
            operations = {}
        }
    }
    custom_operations = {
        "item.record" = manifest.Command("item.record") {
            idempotent_by = "claim"
            input: {fields = [manifest.request_id, manifest.idempotency_key,
                {path = "value.items", type = "json"}]}
            result = {class = "one", fields = [{path = "keys", type = "json"}]}
            errors = ["invalid_input", "idempotency_conflict", "retry", "timeout", "permission_denied", "internal_error"]
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
    components = {rest_consumer_fixture = {connections = ["postgres"]}}
}

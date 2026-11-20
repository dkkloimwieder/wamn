import manifest

manifest.Package {
    package = {id = "rest_consumer_fixture", version = "0.1.1"}
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
            input: {
                request_id = "request_id"
                key = "idempotency_key"
            }
            statements = {
                record_item = {fetch = "optional_one", binds = ["key", "name", "external_system_id"]}
            }
        }
    }
    connections = {}
    components = {rest_consumer_fixture = {}}
}

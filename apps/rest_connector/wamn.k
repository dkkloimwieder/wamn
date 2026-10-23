import manifest

manifest.Package {
    package = {id = "rest_connector", version = "0.1.0"}
    required_platform_policy_contract = {id = "rest_connector_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.0"
            digest = "sha256:c1bcd48915962a0390cae051384dcc002dfc1174b9914549a1b400c1c27349ab"
            operations = ["setting.read", "setting.write", "log.append"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.0"
            digest = "sha256:541aa98bacab6b29e123dc2986585c79dae17a65dda31caf5f48225635dc3e14"
            operations = ["item.record"]
        }
    }
    models = {}
    custom_operations = {
        "sync.finish" = manifest.Command("sync.finish") {
            permission = None
            visibility = "private"
            connection = None
            idempotent_by = "stateless"
            input.fields = [manifest.request_id,
                {path = "value.keys", type = "json"}]
            result = {class = "one", fields = [{path = "due", type = "timestamptz"}]}
            errors = ["invalid_input", "retry", "timeout", "internal_error"]
        }
        "sync.pushed" = manifest.Command("sync.pushed") {
            permission = None
            visibility = "private"
            connection = None
            idempotent_by = "stateless"
            input.fields = [manifest.request_id, {path = "value.record_id", type = "text"}]
            result = {class = "one", fields = [{path = "record_id", type = "text"}]}
            errors = ["invalid_input", "retry", "timeout", "internal_error"]
        }
    }
    connections = ["postgres"]
    components = {rest_connector = {connections = ["postgres"]}}
    workflows = {
        item = {wiring = "rest_item", schedule = "* * * * *"}
        pushed = {
            wiring = "rest_item_pushed"
            registration = {source_package = "rest_consumer_fixture", entity = "item", ops = ["insert"]}
        }
    }
}

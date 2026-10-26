import manifest

manifest.Package {
    package = {id = "rest_connector", version = "0.1.0"}
    required_platform_policy_contract = {id = "rest_connector_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.0"
            digest = "sha256:fafdff0f47f054b080fe12fd0cda93e1a4c6f939fdecdeebfde621551147e503"
            operations = ["setting.read", "setting.write", "log.append"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.0"
            digest = "sha256:ecaf9bddbb6930cea16f211640d79419169d709b8236bbc8163700c5f99f0ec7"
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
    connections = {upstream = {integration = "http", mode = "write"}}
    components = {rest_connector = {connections = ["postgres"]}}
}

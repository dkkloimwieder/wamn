import manifest

manifest.Package {
    package = {id = "netsuite", version = "0.1.0"}
    required_platform_policy_contract = {id = "netsuite_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.0"
            digest = "sha256:fafdff0f47f054b080fe12fd0cda93e1a4c6f939fdecdeebfde621551147e503"
            operations = ["setting.read", "setting.write"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.0"
            digest = "sha256:2b1a37f131831233c51c708fd3b3d329cc0fa11fed80e97f255d74b7002f23b9"
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
            input.fields = [manifest.request_id, {path = "value.due", type = "timestamptz"}]
            result = {class = "one", fields = [{path = "due", type = "timestamptz"}]}
            errors = ["invalid_input", "retry", "timeout", "internal_error"]
        }
    }
    connections = ["postgres"]
    components = {netsuite = {connections = ["postgres"]}}
}

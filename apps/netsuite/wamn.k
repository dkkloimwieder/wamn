import manifest

manifest.Package {
    package = {id = "netsuite", version = "0.1.0"}
    required_platform_policy_contract = {id = "netsuite_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.0"
            digest = "sha256:69391db4fe2dded040495fe65122e2d7158d664a94146d735c453de2c7975fac"
            operations = ["setting.read", "setting.write"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.0"
            digest = "sha256:20398f5918c0f5e6cc255fde8cb0d6e9b930662372071156e1cf62c6646a7a34"
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
    connections = {upstream = {integration = "http", mode = "write"}}
    components = {netsuite = {connections = ["postgres"]}}
}

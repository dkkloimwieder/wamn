import manifest

manifest.Package {
    package = {id = "netsuite", version = "0.1.1"}
    required_platform_policy_contract = {id = "netsuite_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.1"
            digest = "sha256:98ce4057d870804b451cd11f43538b92acd478bf55978287d30cec8dfed16238"
            operations = ["setting.read", "setting.write"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.1"
            digest = "sha256:6a8dde4704b0881a68b7267b54cff822476c1646adb4b98bbef420312c18288a"
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
    connections = {upstream = {interface = "wamn:connection/http@0.1.0", mode = "write"}}
    components = {netsuite = {}}
}

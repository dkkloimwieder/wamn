import manifest

manifest.Package {
    package = {id = "netsuite", version = "0.1.1"}
    required_platform_policy_contract = {id = "netsuite_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.1"
            digest = "sha256:604b655d6b35d344e7c32356183638f2089933defa8d134ba1faec323685a994"
            operations = ["setting.read", "setting.write"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.1"
            digest = "sha256:3a69753ec17c7dd4c67f98baef5ef61cee524513562042b8ef9f450c1531c8be"
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

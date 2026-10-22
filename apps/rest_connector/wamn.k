import manifest

manifest.Package {
    package = {id = "rest_connector", version = "0.1.0"}
    required_platform_policy_contract = {id = "rest_connector_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.0"
            digest = "sha256:2df464eb7657e95c45560240dd8675e02e707b84b013ca5048e1e5cdcb732b95"
            operations = ["setting.read", "setting.write", "log.append"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.0"
            digest = "sha256:f23bb9e087bcf2e050c3fba3843dfa1537c56a68b33a42608336a7954dfbb638"
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

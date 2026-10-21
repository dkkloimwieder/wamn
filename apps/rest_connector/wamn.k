import manifest

manifest.Package {
    package = {id = "rest_connector", version = "0.1.0"}
    required_platform_policy_contract = {id = "rest_connector_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.0"
            digest = "sha256:5583919648b64a8c854cf9cef8856eef006c1afa2445c8ca6b4ef6368fdad8d8"
            operations = ["setting.read", "setting.write", "log.append"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.0"
            digest = "sha256:1cb7500569259b4260a5ac2179bcec57b3e114523b3819d23a928c1e0570407d"
            operations = ["item.record"]
        }
    }
    models = {
        setting = manifest.Model("setting", "wamn_integration") {
            table = "setting"
            owner = "wamn_integration"
            operations = {}
        }
        log = manifest.Model("log", "wamn_integration") {
            table = "log"
            owner = "wamn_integration"
            operations = {}
        }
    }
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

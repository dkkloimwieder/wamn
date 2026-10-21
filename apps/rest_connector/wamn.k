import manifest

manifest.Package {
    package = {id = "rest_connector", version = "0.1.0"}
    required_platform_policy_contract = {id = "rest_connector_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.0"
            digest = "sha256:272c7d2cf405ca1d0f5be30735a7151a8a6bd995fc5f6aa108e48310a98bc7ec"
            operations = ["setting.read", "setting.write", "log.append"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.0"
            digest = "sha256:b19b12e909cc9918058aab18e426c28d14e8e8c70c8ed4aca48c8b85d85a9663"
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

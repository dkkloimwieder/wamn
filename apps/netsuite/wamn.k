import manifest

manifest.Package {
    package = {id = "netsuite", version = "0.1.0"}
    required_platform_policy_contract = {id = "netsuite_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.0"
            digest = "sha256:63f934e9e07e7cbd134b9defc0526773ae853ccfe75a3b3d30d56867c6a1805a"
            operations = ["setting.read", "setting.write"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.0"
            digest = "sha256:2f5e67f1a237877a1cce14d6d40390df2954c6a06f80007209183267dde2089a"
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
    workflows = {
        pull = {wiring = "netsuite_pull", schedule = "* * * * *"}
        push = {
            wiring = "netsuite_push"
            registration = {source_package = "rest_consumer_fixture", entity = "item", ops = ["insert"]}
        }
    }
}

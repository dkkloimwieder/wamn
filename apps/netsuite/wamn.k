import manifest

manifest.Package {
    package = {id = "netsuite", version = "0.1.0"}
    required_platform_policy_contract = {id = "netsuite_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.0"
            digest = "sha256:72709913ba56bce9e30ef3bb9a2dd53d2f485c7a44bde0cce1b615926f8e2e17"
            operations = ["setting.read", "setting.write"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.0"
            digest = "sha256:0991e390d37b584b238043e5d3219f862f8258f87e2c5d80282ba1395fd6c8e4"
            operations = ["item.record"]
        }
    }
    models = {}
    custom_operations = {}
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

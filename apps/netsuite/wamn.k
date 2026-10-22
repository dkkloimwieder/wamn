import manifest

manifest.Package {
    package = {id = "netsuite", version = "0.1.0"}
    required_platform_policy_contract = {id = "netsuite_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.0"
            digest = "sha256:2df464eb7657e95c45560240dd8675e02e707b84b013ca5048e1e5cdcb732b95"
            operations = ["setting.read", "setting.write"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.0"
            digest = "sha256:f23bb9e087bcf2e050c3fba3843dfa1537c56a68b33a42608336a7954dfbb638"
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

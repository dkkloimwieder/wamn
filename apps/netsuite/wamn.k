import manifest

manifest.Package {
    package = {id = "netsuite", version = "0.1.1"}
    required_platform_policy_contract = {id = "netsuite_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.1"
            digest = "sha256:5187d38942b34ed36cb0b3fe5e0c2b029831d77d4ce0379bc433043aa29117d1"
            operations = ["setting.read", "setting.write"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.1"
            digest = "sha256:e884d10935bee6fd3dacdc729392855c7c5ec5fa2f9cf6ccb9ed45997348f32c"
            operations = ["item.record"]
        }
    }
    # The composed base owns these relations. They select its SQL schema.
    models = {
        setting = manifest.Model("setting", "wamn_integration") {
            table = "setting"
            owner = "wamn_integration"
            operations = {}
        }
    }
    custom_operations = {
        "sync.finish" = manifest.Command("sync.finish") {
            visibility = "private"
            permission = None
            connection = None
            idempotent_by = "stateless"
            input: {
                request_id = "request_id"
            }
        }
    }
    connections = {upstream = {interface = "wamn:connection/http@0.1.0", mode = "write"}}
    components = {netsuite = {}}
}

import manifest

manifest.Package {
    package = {id = "netsuite", version = "0.1.1"}
    required_platform_policy_contract = {id = "netsuite_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.1"
            digest = "sha256:1b69d4013f9a346eddfa66cbfa6d2394ff5bb1d649bf5a0a5f3192865e6100ed"
            operations = ["setting.read", "setting.write"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.1"
            digest = "sha256:54604f355645fd4635ae9a52167d986efd3c33be7cf6dc6c2bf09871b2f850b5"
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

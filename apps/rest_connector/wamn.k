import manifest

manifest.Package {
    package = {id = "rest_connector", version = "0.1.1"}
    required_platform_policy_contract = {id = "rest_connector_data_access", state = "satisfied"}
    base_dependencies = {
        integration = {
            package = "wamn_integration"
            version = "0.1.1"
            digest = "sha256:db79352277729377a6d58ecb577a647960a5d312063371c195680ea843b5494a"
            operations = ["setting.read", "setting.write", "log.append"]
        }
        consumer = {
            package = "rest_consumer_fixture"
            version = "0.1.1"
            digest = "sha256:8e5501876b2fda97c871ed53f4c08572139cbaa7591d5d37f418c9de725c19f6"
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
        log = manifest.Model("log", "wamn_integration") {
            table = "log"
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
        "sync.pushed" = manifest.Command("sync.pushed") {
            visibility = "private"
            permission = None
            connection = None
            idempotent_by = "stateless"
            input: {
                request_id = "request_id"
            }
        }
    }
    connections = {upstream = {interface = "wamn:connection/http@0.1.0", mode = "read"}}
    components = {rest_connector = {exports = {"wamn:node/async-handler@0.2.0": {"input-ports": [{"name": "input", "schema": {}}], "output-ports": [{"name": "main", "schema": {}}], "parameters": []}}}}
}

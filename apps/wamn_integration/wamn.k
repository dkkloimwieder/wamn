import manifest

manifest.Package {
    package = {id = "wamn_integration", version = "0.1.1"}
    required_platform_policy_contract = {id = "wamn_integration_data_access", state = "satisfied"}
    models = {
        setting = manifest.Model("setting", "wamn_integration") {
            table = "setting"
            owner = "wamn_integration"
            audit_log = {columns = []}
            operations = {}
        }
        log = manifest.Model("log", "wamn_integration") {
            table = "log"
            owner = "wamn_integration"
            audit_log = {columns = []}
            operations = {}
        }
    }
    custom_operations = {
        "setting.read" = manifest.Projection("setting.read") {
            visibility = "private"
            permission = None
            statements = {
                read_setting = {fetch = "optional_one", binds = ["key"]}
            }
        }
        "setting.write" = manifest.Command("setting.write") {
            visibility = "private"
            permission = None
            idempotent_by = "claim"
            input: {
                request_id = "request_id"
                key = "idempotency_key"
            }
            statements = {
                write_setting = {fetch = "one", binds = ["key", "value"]}
            }
        }
        "log.append" = manifest.Command("log.append") {
            visibility = "private"
            permission = None
            idempotent_by = "claim"
            input: {
                request_id = "request_id"
                key = "idempotency_key"
            }
            statements = {
                append_log = {fetch = "optional_one", binds = ["delivery_id", "record_id", "outcome", "detail"]}
            }
        }
    }
    connections = {}
    components = {wamn_integration = {}}
}

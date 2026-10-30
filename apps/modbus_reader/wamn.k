import manifest

manifest.Package {
    package = {id = "modbus_reader", version = "0.1.0"}
    required_platform_policy_contract = {id = "modbus_reader_data_access", state = "satisfied"}
    models = {}
    custom_operations = {
        "register.read" = manifest.Command("register.read") {
            visibility = "private"
            permission = None
            connection = None
            transaction = None
            automatic_retry = None
            idempotent_by = "stateless"
            input.fields = [
                manifest.request_id
                {path = "value.unit", type = "int32"}
                {path = "value.address", type = "int32"}
            ]
            result = {class = "one", fields = [{path = "register", type = "int32"}]}
            errors = ["invalid_input", "unavailable", "retry", "timeout", "internal_error"]
            error_details = {unavailable = {}}
        }
    }
    connections = {registers = {interfaces = ["wamn:connector/reader@0.1.0"], mode = "read"}}
    components = {modbus_reader = {connections = ["registers"]}}
}

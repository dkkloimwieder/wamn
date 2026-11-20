import manifest

manifest.Package {
    package = {id = "modbus_reader", version = "0.1.1"}
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
            input: {
                request_id = "request_id"
            }
        }
    }
    connections = {registers = {interface = "wamn:modbus/read@1.0.0", mode = "read"}}
    components = {modbus_reader = {connections = ["registers"], parameters = {"register.read": [{"name": "unit", "schema": {"type": "integer", "minimum": 1, "maximum": 247}, "required": True}]}}}
}

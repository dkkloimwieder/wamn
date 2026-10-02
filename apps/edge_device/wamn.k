# The edge device package: the device operation that an edge box runs on each
# frame. The generator compiles this file to generated/wamn.json.
import manifest

manifest.Package {
    package = {id = "edge_device", version = "2.1.0", predecessor_version = "2.0.0"}
    required_platform_policy_contract = {id = "edge_device_data_access", state = "satisfied"}
    models = {}
    custom_operations = {
        "sample.read" = manifest.Command("sample.read") {
            # No SQL: no connection, transaction or retry.
            connection = None
            transaction = None
            automatic_retry = None
            label = "Read a sample"
            description = "The device operation of an edge box. It turns one frame from the serial device into the sample that the box stores and forwards. It declares no SQL."
            idempotent_by = "stateless"
            input.fields = [
                manifest.request_id
                {path = "value.frame", type = "text", label = "Frame", description = "The frame that the device sent, without its line ending."}
                {path = "value.captured_at", type = "timestamptz", label = "Captured at", description = "When the edge box read the frame."}
            ]
            result = {
                class = "one"
                fields = [{path = "frame", type = "text"}, {path = "captured_at", type = "timestamptz"}]
            }
            errors = ["invalid_input", "permission_denied", "internal_error"]
            error_details = {}
        }
    }
    connections = []
    components = {device = {connections = []}}
}

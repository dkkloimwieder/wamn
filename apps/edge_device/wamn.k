# The edge device package: the device operation that an edge box runs on each
# frame. The generator compiles this file to generated/wamn.json.
import manifest

manifest.Package {
    package = {id = "edge_device", version = "2.1.1"}
    required_platform_policy_contract = {id = "edge_device_data_access", state = "satisfied"}
    models = {}
    custom_operations = {
        "sample.read" = manifest.Command("sample.read") {
            connection = None
            transaction = None
            automatic_retry = None
            idempotent_by = "stateless"
            label = "Read a sample"
            description = "The device operation of an edge box. It turns one frame from the serial device into the sample that the box stores and forwards. It declares no SQL."
            input: {
                request_id = "request_id"
                labels = {
                    "frame" = {"label" = "Frame", "description" = "The frame that the device sent, without its line ending."}
                    "captured_at" = {"label" = "Captured at", "description" = "When the edge box read the frame."}
                }
            }
        }
    }
    connections = {}
    components = {device = {}}
    routes = {
        operations = {
            "sample.read" = {id = "sample-read-http", path = "/sample/read"}
        }
    }
}

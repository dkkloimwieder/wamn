# The edge samples package: the platform route that stores the samples edge
# boxes forward. The generator compiles this file to generated/wamn.json.
import manifest

_schema = "edge_samples"

manifest.Package {
    package = {id = "edge_samples", version = "2.1.1"}
    required_platform_policy_contract = {id = "edge_samples_data_access", state = "satisfied"}
    models = {
        sample = manifest.Model("sample", _schema) {
            table = "sample"
            owner = "edge_samples"
            server_owned_fields = []
            audit_log = {columns = ["created_at"]}
            operations: {
                get: {result = "one"}
            }
        }
    }
    custom_operations = {
        "sample.record" = manifest.Command("sample.record") {
            idempotent_by = "claim"
            label = "Record a sample"
            description = "One reading that an edge box forwards. The edge sample key is the idempotency key."
            input: {
                request_id = "request_id"
                key = "idempotency_key"
                labels = {
                    "frame" = {"label" = "Frame", "description" = "The frame that the device sent, without its line ending."}
                    "captured_at" = {"label" = "Captured at", "description" = "When the edge box read the frame."}
                }
            }
            statements = {
                record_sample = {fetch = "one", binds = ["frame", "captured_at"]}
            }
        }
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
    components = {samples = {}}
    routes = {
        operations = {
            "sample.record" = {id = "sample-record-http", path = "/sample/record"}
        }
    }
}

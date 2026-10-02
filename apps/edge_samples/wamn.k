# The edge samples package: the platform route that stores the samples edge
# boxes forward. The generator compiles this file to generated/wamn.json.
import manifest

_schema = "edge_samples"
_frame = {path = "value.frame", type = "text", label = "Frame", description = "The frame that the device sent, without its line ending."}
_captured_at = {path = "value.captured_at", type = "timestamptz", label = "Captured at", description = "When the edge box read the frame."}

manifest.Package {
    package = {id = "edge_samples", version = "2.1.0", predecessor_version = "2.0.0"}
    required_platform_policy_contract = {id = "edge_samples_data_access", state = "satisfied"}
    models = {
        sample = manifest.Model("sample", _schema) {
            table = "sample"
            owner = "edge_samples"
            server_owned_fields = ["id", "created_at"]
            enum_fields = {}
            audit_log = {columns = ["created_at"]}
            operations: {
                get: {result = "one"}
            }
        }
    }
    custom_operations = {
        "sample.record" = manifest.Command("sample.record") {
            label = "Record a sample"
            description = "One reading that an edge box forwards. The edge sample key is the idempotency key."
            idempotent_by = "claim"
            canonicalization = {excluded_fields = ["request_id", "value.idempotency_key"]}
            input = {
                envelope = {minimum = 1, maximum = 100}
                raw_body_maximum = 1048576
                fields = [manifest.request_id, manifest.idempotency_key, _frame, _captured_at]
            }
            result = {class = "one", fields = [{path = "sample_id", type = "uuid"}]}
            errors = ["invalid_input", "idempotency_conflict", "retry", "timeout", "permission_denied", "internal_error"]
            error_details = {idempotency_conflict = manifest.conflict_details}
            constraint_errors = {}
            relations = [{
                $schema = _schema
                table = "sample"
                insert_fields = ["captured_at", "frame"]
                select_fields = ["id"]
                update_fields = []
                constraints = ["sample_id_pkey"]
                lock = False
            }]
            statements = {
                record_sample = manifest.Statement("command/sample", "record") {
                    fetch = "one"
                    parameters = [manifest.text("frame"), manifest.timestamptz("captured_at")]
                    row = [manifest.uuid("id")]
                }
            }
        }
        "sample.read" = manifest.Command("sample.read") {
            # No SQL: no connection, transaction or retry.
            connection = None
            transaction = None
            automatic_retry = None
            label = "Read a sample"
            description = "The device operation of an edge box. It turns one frame from the serial device into the sample that the box stores and forwards. It declares no SQL."
            idempotent_by = "stateless"
            input.fields = [manifest.request_id, _frame, _captured_at]
            result = {
                class = "one"
                fields = [{path = "frame", type = "text"}, {path = "captured_at", type = "timestamptz"}]
            }
            errors = ["invalid_input", "permission_denied", "internal_error"]
            error_details = {}
        }
    }
    connections = ["postgres"]
    components = {samples = {connections = ["postgres"]}}
}

# The Acme overlay of Receiving: quality inspection on Acme purchase orders, and
# a receipt that Acme can hold for inspection. The generator compiles this file
# to generated/wamn.json.
import manifest

_schema = "receiving"
_errors_after_input = ["retry", "timeout", "permission_denied", "internal_error"]
_claim_errors = ["idempotency_conflict", *_errors_after_input]

manifest.Package {
    package = {id = "client_acme_receiving", version = "4.1.0", predecessor_version = "4.0.0"}
    # The pin is the contract with the base: its version and the digest of its
    # built component. Nothing here imports the base's wamn.k.
    base_dependencies = {
        base_receiving = {
            package = "wamn_receiving"
            version = "2.1.0"
            digest = "sha256:e34d0218af83ea540111cb9096f9b13ca5d2b9e5e3f14ddd9763b7b6ed71dba0"
            operations = ["receiving.record_receipt"]
        }
    }
    required_platform_policy_contract = {id = "receiving_data_access", state = "satisfied"}
    models = {
        purchase_order = manifest.Model("purchase_order", _schema) {
            table = "purchase_order"
            owner = "wamn_receiving"
            field_owners = {
                acme_inspection_required = "client_acme_receiving"
                acme_quality_status = "client_acme_receiving"
            }
            constraint_owners = {purchase_order_acme_quality_status_check = "client_acme_receiving"}
            server_owned_fields = ["id", "purchase_order_number", "supplier_id", "status", "row_version"]
            enum_fields = {
                status = ["open", "complete", "cancelled"]
                acme_quality_status = ["not_required", "pending", "approved"]
            }
            operations: {
                get: {result = "one"}
                update: {
                    writable_fields = ["acme_inspection_required", "acme_quality_status"]
                    revision_field = "row_version"
                    result = "one"
                }
            }
        }
        quality_inspection = manifest.Model("quality_inspection", _schema) {
            table = "quality_inspection"
            owner = "client_acme_receiving"
            server_owned_fields = ["receipt_id", "status", "row_version"]
            enum_fields = {status = ["pending", "approved"]}
            audit_log = {columns = ["created_at", "created_by", "updated_at", "updated_by"]}
            operations = {}
        }
    }
    custom_operations = {
        "receiving.record_receipt" = manifest.Command("receiving.record_receipt") {
            connection = None
            transaction = None
            automatic_retry = None
            idempotent_by = {inherited = {base = "base_receiving", operation = "receiving.record_receipt"}}
            participant = "receiving.record_receipt_participant"
            input = {
                raw_body_maximum = 1048576
                envelope = {minimum = 1, maximum = 100}
                line = {minimum = 1, maximum = 100}
                fields = [
                    manifest.request_id
                    manifest.idempotency_key
                    {path = "value.purchase_order_id", type = "uuid"}
                    {path = "value.receipt_reference", type = "text"}
                    manifest.occurred_at
                    {path = "value.line[].purchase_order_line_id", type = "uuid"}
                    {path = "value.line[].quantity", type = "numeric"}
                    {path = "value.line[].location_id", type = "uuid"}
                ]
            }
            result = {
                class = "one"
                fields = [
                    {path = "receipt_id", type = "uuid"}
                    {path = "purchase_order_id", type = "uuid"}
                    {path = "purchase_order_status", type = "text", values = ["open", "complete"]}
                    {path = "row_version", type = "int32", revision = True}
                ]
            }
            errors = [
                "invalid_input"
                "purchase_order_not_found"
                "purchase_order_not_open"
                "purchase_order_line_not_found"
                "purchase_order_line_mismatch"
                "location_not_found"
                "quantity_exceeds_remaining"
                "receipt_reference_conflict"
                *_claim_errors
            ]
            error_details = {
                purchase_order_not_found = {required = ["field"]}
                purchase_order_not_open = {required = ["field"]}
                purchase_order_line_not_found = manifest.not_found_details
                purchase_order_line_mismatch = {required = ["field", "id"]}
                location_not_found = manifest.not_found_details
                quantity_exceeds_remaining = {required = ["field", "id"]}
                receipt_reference_conflict = {required = ["constraint"]}
                idempotency_conflict = manifest.conflict_details
            }
            constraint_errors = {}
            relations = []
            statements = {}
        }
        "receiving.record_receipt_participant" = manifest.Command("receiving.record_receipt_participant") {
            automatic_retry = None
            transaction = "participant"
            idempotent_by = {inherited = {base = "base_receiving", operation = "receiving.record_receipt"}}
            input = {fields = [{path = "receipt_id", type = "uuid"}, {path = "purchase_order_id", type = "uuid"}]}
            errors = ["invalid_input", *_errors_after_input]
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "purchase_order"
                    select_fields = ["id", "acme_inspection_required", "acme_quality_status"]
                    insert_fields = []
                    update_fields = []
                    lock = True
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "quality_inspection"
                    select_fields = ["receipt_id"]
                    insert_fields = ["receipt_id", "status"]
                    update_fields = []
                    lock = False
                    constraints = ["quality_inspection_receipt_id_pkey"]
                }
            ]
            statements = {
                record_receipt_participant = manifest.Statement("command/record_receipt_participant", "record_receipt_participant") {
                    fetch = "one"
                    parameters = [manifest.uuid("receipt_id"), manifest.uuid("purchase_order_id")]
                    row = [manifest.nullable(manifest.text("outcome")), manifest.nullable(manifest.uuid("receipt_id"))]
                }
            }
        }
        "quality.load_purchase_order_detail" = manifest.Projection("quality.load_purchase_order_detail") {
            input = {fields = [{path = "purchase_order_id", type = "uuid"}]}
            result = {
                class = "one"
                fields = [
                    {path = "id", type = "uuid"}
                    {path = "purchase_order_number", type = "text"}
                    {path = "supplier_id", type = "uuid"}
                    {path = "status", type = "text", values = ["open", "complete", "cancelled"]}
                    {path = "row_version", type = "int32", revision = True}
                    {path = "acme_inspection_required", type = "boolean"}
                    {path = "acme_quality_status", type = "text", values = ["not_required", "pending", "approved"]}
                ]
            }
            errors = ["invalid_input", "not_found", *_errors_after_input]
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "purchase_order"
                    select_fields = [
                        "id"
                        "purchase_order_number"
                        "supplier_id"
                        "status"
                        "row_version"
                        "acme_inspection_required"
                        "acme_quality_status"
                    ]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
            ]
            statements = {
                load_purchase_order_detail = manifest.Statement("query", "quality_purchase_order_detail") {
                    fetch = "optional_one"
                    parameters = [manifest.uuid("purchase_order_id")]
                    row = [
                        manifest.uuid("id")
                        manifest.text("purchase_order_number")
                        manifest.uuid("supplier_id")
                        manifest.text("status")
                        manifest.int32("row_version")
                        {name = "acme_inspection_required", type = "boolean", nullable = False}
                        manifest.text("acme_quality_status")
                    ]
                }
            }
        }
        "quality.approve_inspection" = manifest.Command("quality.approve_inspection") {
            idempotent_by = {state = {guards = {quality_inspection = "expected_row_version"}}}
            input = {
                fields = [
                    manifest.request_id
                    {path = "receipt_id", type = "uuid"}
                    {path = "expected_row_version", type = "int32", revision = True}
                ]
            }
            result = {
                class = "one"
                fields = [
                    {path = "receipt_id", type = "uuid"}
                    {path = "status", type = "text", values = ["approved"]}
                    {path = "row_version", type = "int32", revision = True}
                    {path = "purchase_order_id", type = "uuid"}
                    {path = "purchase_order_row_version", type = "int32", revision = True}
                ]
            }
            errors = ["invalid_input", "not_found", "concurrency_conflict", *_errors_after_input]
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "quality_inspection"
                    select_fields = ["receipt_id", "status", "row_version"]
                    insert_fields = []
                    update_fields = ["status", "row_version"]
                    lock = True
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "receipt"
                    select_fields = ["id", "purchase_order_id"]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "purchase_order"
                    select_fields = ["id", "row_version"]
                    insert_fields = []
                    update_fields = ["acme_quality_status", "row_version"]
                    lock = False
                    constraints = []
                }
            ]
            statements = {
                approve_inspection = manifest.Statement("command/approve_inspection", "approve_inspection") {
                    fetch = "one"
                    parameters = [manifest.uuid("receipt_id"), manifest.int32("expected_row_version")]
                    row = [
                        manifest.nullable(manifest.text("outcome"))
                        manifest.nullable(manifest.int32("observed_row_version"))
                        manifest.nullable(manifest.uuid("receipt_id"))
                        manifest.nullable(manifest.text("status"))
                        manifest.nullable(manifest.int32("row_version"))
                        manifest.nullable(manifest.uuid("purchase_order_id"))
                        manifest.nullable(manifest.int32("purchase_order_row_version"))
                    ]
                }
            }
        }
        "quality.create_inspection" = manifest.EventHandler("quality.create_inspection") {
            permission = None
            visibility = "private"
            input = {fields = [{path = "event", type = "text", values = ["insert"]}, {path = "new.id", type = "uuid"}]}
            errors = ["invalid_input", "retry", "timeout", "internal_error"]
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "receipt"
                    select_fields = ["id", "purchase_order_id"]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "purchase_order"
                    select_fields = ["id", "acme_inspection_required"]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "quality_inspection"
                    select_fields = ["receipt_id"]
                    insert_fields = ["receipt_id"]
                    update_fields = []
                    lock = False
                    constraints = ["quality_inspection_receipt_id_pkey"]
                }
            ]
            registration = {source_package = "wamn_receiving", entity = "receipt", ops = ["insert"]}
            statements = {
                insert_inspection = manifest.Statement("command/create_inspection", "insert_inspection") {
                    fetch = "optional_one"
                    parameters = [manifest.uuid("receipt_id")]
                    row = [manifest.uuid("receipt_id")]
                }
                load_inspection = manifest.Statement("command/create_inspection", "load_inspection") {
                    fetch = "optional_one"
                    parameters = [manifest.uuid("receipt_id")]
                    row = [manifest.uuid("receipt_id")]
                }
            }
        }
    }
    connections = ["postgres"]
    components = {client_acme_receiving = {connections = ["postgres"]}}
    routes = {path_prefix = "/acme", id_prefix = "client-acme-receiving"}
}

# The Acme overlay of Receiving: quality inspection on Acme purchase orders, and
# a receipt that Acme can hold for inspection. The generator compiles this file
# to generated/wamn.json.
import manifest

_schema = "receiving"

manifest.Package {
    package = {id = "client_acme_receiving", version = "4.2.4"}
    # The pin is the contract with the base: its version and the digest of its
    # built component. Nothing here imports the base's wamn.k.
    base_dependencies = {
        base_receiving = {
            package = "wamn_receiving"
            version = "2.2.11"
            digest = "sha256:054035463ce1b51d1e3225c04b14f420f032b6a4b70d47a606969aacc8e3e232"
            operations = ["item.create", "location.create", "purchase_order.create", "purchase_order_line.create", "receiving.record_receipt", "supplier.create"]
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
            server_owned_fields = ["purchase_order_number", "supplier_id", "status", "row_version"]
            operations: {
                get: {result = "one"}
                update: {
                    writable_fields = ["acme_inspection_required", "acme_quality_status"]
                    revision_field = "row_version"
                    result = "one"
                }
            }
        }
        # The Data change handler reads Receiving's receipt inserts.
        receipt = manifest.Model("receipt", _schema) {
            table = "receipt"
            owner = "wamn_receiving"
            operations = {}
        }
        quality_inspection = manifest.Model("quality_inspection", _schema) {
            table = "quality_inspection"
            owner = "client_acme_receiving"
            server_owned_fields = ["receipt_id", "status", "row_version"]
            audit_log = {columns = ["created_at", "created_by", "updated_at", "updated_by"]}
            operations = {}
        }
    }
    custom_operations = {
        "acme.record_receipt" = manifest.Command("acme.record_receipt") {
            idempotent_by = "claim"
            input: {
                request_id = "request_id"
                line = {"minimum" = 1, "maximum" = 100}
                key = "idempotency_key"
            }
            statements = {
                record_inspection = {fetch = "one", binds = ["receipt_id", "purchase_order_id"]}
            }
        }
        "quality.load_purchase_order_detail" = manifest.Projection("quality.load_purchase_order_detail") {
            statements = {
                load_purchase_order_detail = {fetch = "optional_one", binds = ["purchase_order_id"]}
            }
        }
        "quality.approve_inspection" = manifest.Command("quality.approve_inspection") {
            idempotent_by = {"state" = {"guards" = {"quality_inspection" = "expected_row_version"}}}
            input: {
                request_id = "request_id"
            }
            statements = {
                approve_inspection = {fetch = "one", binds = ["receipt_id", "expected_row_version"]}
            }
        }
        "quality.create_inspection" = manifest.EventHandler("quality.create_inspection") {
            visibility = "private"
            permission = None
            statements = {
                insert_inspection = {fetch = "optional_one", binds = ["id"]}
                load_inspection = {fetch = "optional_one", binds = ["id"]}
            }
        }
    }
    connections = {}
    components = {client_acme_receiving = {data_change = {role = "admin", models = {receipt = ["insert"]}}}}
    routes = {
        operations = {
            "acme.record_receipt" = {id = "client-acme-receiving-acme-record-receipt-http", path = "/acme/acme/record_receipt"}
            "quality.load_purchase_order_detail" = {id = "client-acme-receiving-quality-load-purchase-order-detail-http", path = "/acme/quality/load_purchase_order_detail"}
            "quality.approve_inspection" = {id = "client-acme-receiving-quality-approve-inspection-http", path = "/acme/quality/approve_inspection"}
        }path_prefix = "/acme", id_prefix = "client-acme-receiving"}
}

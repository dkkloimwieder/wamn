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
            digest = "sha256:1d13ba86dbcc29d661cb12f22982df9081a0c63e7a5a18c417a2757abb0cb6f5"
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
    components = {client_acme_receiving = {}}
    routes = {
        operations = {
            "acme.record_receipt" = {id = "client-acme-receiving-acme-record-receipt-http", path = "/acme/acme/record_receipt"}
            "quality.load_purchase_order_detail" = {id = "client-acme-receiving-quality-load-purchase-order-detail-http", path = "/acme/quality/load_purchase_order_detail"}
            "quality.approve_inspection" = {id = "client-acme-receiving-quality-approve-inspection-http", path = "/acme/quality/approve_inspection"}
        }path_prefix = "/acme", id_prefix = "client-acme-receiving"}
}

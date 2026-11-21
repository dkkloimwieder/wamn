# The Receiving application: purchase orders, receipts and the commands that
# record what a dock receives. The generator compiles this file to
# generated/wamn.json.
import manifest

_schema = "receiving"

manifest.Package {
    package = {id = "wamn_receiving", version = "2.2.11"}
    client_package = {name = "@wamn/receiving-client"}
    required_platform_policy_contract = {id = "receiving_data_access", state = "satisfied"}
    sequences = {purchase_order = {pattern = "PO-{seq:po,width=6}"}}
    models = {
        item = manifest.Model("item", _schema) {
            table = "item"
            owner = "wamn_receiving"
            server_owned_fields = []
            field_text = {item_number = {label = "Item number", visible = True}}
            audit_log = {columns = [], retention = "P30D"}
            operations: {
                create: {writable_fields = ["item_number"], label = "Add an item", result = "one"}
            }
        }
        location = manifest.Model("location", _schema) {
            table = "location"
            owner = "wamn_receiving"
            server_owned_fields = []
            field_text = {location_code = {label = "Location code", visible = True}}
            audit_log = {columns = []}
            operations: {
                create: {writable_fields = ["location_code"], label = "Add a location", result = "one"}
            }
        }
        purchase_order = manifest.Model("purchase_order", _schema) {
            table = "purchase_order"
            owner = "wamn_receiving"
            client_field_extensible = True
            server_owned_fields = ["purchase_order_number", "status", "row_version"]
            field_text = {
                purchase_order_number = {label = "Order number", visible = True}
                supplier_id = {label = "Supplier", visible = True}
                status = {label = "Status", visible = True}
                row_version = {label = "Revision", visible = True}
                created_at = {label = "Created"}
                created_by = {label = "Created by"}
                updated_at = {label = "Updated"}
                updated_by = {label = "Updated by"}
            }
            fields = {
                purchase_order_number = {number = "purchase_order"}
                created_by = {role = "actor"}
                updated_by = {role = "actor"}
            }
            audit_log = {columns = ["created_at", "created_by", "updated_at", "updated_by"], retention = "unlimited"}
            operations: {
                create: {writable_fields = ["supplier_id"], label = "Add a purchase order", result = "one"}
                get: {label = "Purchase order", result = "one"}
                query: {
                    display_field = "purchase_order_number"
                    filters = [
                        {field = "supplier_id"}
                        {field = "status"}
                        {field = "purchase_order_number", match = "contains"}
                    ]
                    sort = {
                        fields = ["purchase_order_number", "status", "created_at"]
                        directions = ["ascending", "descending"]
                    }
                    label = "Purchase orders"
                    result = "page"
                }
                update: {
                    writable_fields = ["supplier_id"]
                    revision_field = "row_version"
                    label = "Change the supplier"
                    description = "The order must carry the revision the operator read, or the write is refused."
                    result = "one"
                }
            }
        }
        purchase_order_line = manifest.Model("purchase_order_line", _schema) {
            table = "purchase_order_line"
            owner = "wamn_receiving"
            server_owned_fields = []
            field_text = {
                line_number = {label = "Line", visible = True}
                item_id = {label = "Item", visible = True}
                ordered_quantity = {label = "Ordered", visible = True}
                received_quantity = {label = "Received", visible = True}
            }
            audit_log = {columns = [], retention = "P30D"}
            operations: {
                create: {
                    writable_fields = ["purchase_order_id", "line_number", "item_id", "ordered_quantity"]
                    label = "Add an order line"
                    result = "one"
                }
            }
        }
        receipt = manifest.Model("receipt", _schema) {
            table = "receipt"
            owner = "wamn_receiving"
            server_owned_fields = ["purchase_order_id", "receipt_reference", "occurred_at"]
            field_text = {
                receipt_reference = {label = "Receipt reference", visible = True}
                purchase_order_id = {label = "Purchase order", visible = True}
                occurred_at = {label = "Received at", visible = True}
                created_at = {label = "Recorded"}
                created_by = {label = "Recorded by"}
            }
            audit_log = {columns = ["created_at", "created_by"]}
            operations: {
                get: {label = "Receipt", result = "one"}
                query: {feed = False, display_field = "receipt_reference", label = "Receipts", result = "page"}
            }
        }
        receipt_line = manifest.Model("receipt_line", _schema) {
            table = "receipt_line"
            owner = "wamn_receiving"
            server_owned_fields = []
            field_text = {
                quantity = {label = "Quantity", visible = True}
                location_id = {label = "Location", visible = True}
                purchase_order_line_id = {label = "Order line", visible = True}
            }
            audit_log = {columns = []}
            operations = {}
        }
        supplier = manifest.Model("supplier", _schema) {
            table = "supplier"
            owner = "wamn_receiving"
            min_lengths = {name = 1}
            server_owned_fields = []
            field_text = {name = {label = "Supplier name", visible = True}, created_at = {label = "Added"}}
            audit_log = {columns = ["created_at"]}
            operations: {
                create: {writable_fields = ["name"], label = "Add a supplier", result = "one"}
                query: {feed = False, display_field = "name", label = "Suppliers", result = "page"}
            }
        }
    }
    custom_operations = {
        "receiving.record_receipt" = manifest.Command("receiving.record_receipt") {
            idempotent_by = "claim"
            label = "Record a receipt"
            description = "One submission records every line of one receipt against one purchase order."
            input: {
                request_id = "request_id"
                line = {"minimum" = 1, "maximum" = 100, "label" = "Receipt lines", "description" = "One line for each order line this receipt records."}
                key = "idempotency_key"
                labels = {
                    "purchase_order_id" = {"label" = "Purchase order", "references" = {"model" = "purchase_order"}}
                    "receipt_reference" = {"label" = "Receipt reference"}
                    "occurred_at" = {"label" = "Received at"}
                    "line[].purchase_order_line_id" = {"label" = "Order line", "references" = {"model" = "purchase_order_line", "narrowed_by" = "purchase_order_id"}}
                    "line[].quantity" = {"label" = "Quantity", "description" = "More than zero, and no more than the quantity the line still has remaining."}
                    "line[].location_id" = {"label" = "Location", "references" = {"model" = "location"}}
                }
            }
            result: {
                labels = {
                    "receipt_id" = {"label" = "Receipt"}
                    "purchase_order_id" = {"label" = "Purchase order"}
                    "purchase_order_status" = {"label" = "Status", "visible" = True}
                    "row_version" = {"label" = "Revision", "visible" = True}
                }
            }
            error_text = {"purchase_order_not_open" = "This purchase order is not open, so it takes no receipt."}
            constraint_errors = {"receipt_purchase_order_id_receipt_reference_key" = "receipt_reference_conflict"}
            statements = {
                lock_purchase_order = {fetch = "optional_one", binds = ["purchase_order_id"]}
                record_receipt = {fetch = "one", binds = ["purchase_order_id", "line", "receipt_reference", "occurred_at"]}
            }
        }
        "receiving.load_receipt_screen" = manifest.Projection("receiving.load_receipt_screen") {
            label = "Receiving screen"
            feed = True
            sync = True
            input: {
                labels = {
                    "purchase_order_id" = {"label" = "Purchase order", "references" = {"model" = "purchase_order"}}
                }
            }
            result: {
                labels = {
                    "purchase_order_number" = {"label" = "Order number", "visible" = True}
                    "purchase_order_status" = {"label" = "Status", "visible" = True}
                    "supplier_id" = {"label" = "Supplier"}
                    "purchase_order_row_version" = {"label" = "Revision", "visible" = True}
                    "line_number" = {"label" = "Line", "visible" = True}
                    "item_number" = {"label" = "Item", "visible" = True}
                    "ordered_quantity" = {"label" = "Ordered", "visible" = True}
                    "received_quantity" = {"label" = "Received", "visible" = True}
                    "remaining_quantity" = {"label" = "Remaining", "visible" = True}
                }
            }
            lists = {"model" = "purchase_order_line", "key_field" = "line_id", "display_field" = "item_number"}
            statements = {
                load_receipt_screen = {fetch = "bounded_list", binds = ["purchase_order_id"]}
            }
        }
        "receiving.load_open_receipt_lines" = manifest.Projection("receiving.load_open_receipt_lines") {
            label = "Open receipt lines"
            feed = True
            sync = True
            result: {
                labels = {
                    "purchase_order_number" = {"label" = "Order number", "visible" = True}
                    "purchase_order_status" = {"label" = "Status", "visible" = True}
                    "supplier_id" = {"label" = "Supplier"}
                    "purchase_order_row_version" = {"label" = "Revision", "visible" = True}
                    "line_number" = {"label" = "Line", "visible" = True}
                    "item_number" = {"label" = "Item", "visible" = True}
                    "ordered_quantity" = {"label" = "Ordered", "visible" = True}
                    "received_quantity" = {"label" = "Received", "visible" = True}
                    "remaining_quantity" = {"label" = "Remaining", "visible" = True}
                }
            }
            lists = {"model" = "purchase_order_line", "key_field" = "line_id", "display_field" = "item_number"}
            statements = {
                load_open_receipt_lines = {fetch = "bounded_list", binds = []}
            }
        }
        "receiving.load_purchase_order_history" = manifest.Projection("receiving.load_purchase_order_history") {
            label = "Purchase order history"
            input: {
                labels = {
                    "id" = {"label" = "Purchase order"}
                    "after_cursor" = {"label" = "Continue after", "description" = "The cursor of the last entry you read. Leave it empty for the first page."}
                    "limit" = {"label" = "Entries per page"}
                }
            }
            result: {
                labels = {
                    "id" = {"label" = "Entry"}
                    "cursor" = {"label" = "Position", "visible" = True}
                    "type" = {"label" = "Change", "visible" = True}
                    "operation" = {"label" = "Operation", "visible" = True}
                    "changed_by" = {"label" = "Changed by"}
                    "changed_at" = {"label" = "Changed at", "visible" = True}
                    "before" = {"label" = "Before", "visible" = True}
                    "after" = {"label" = "After", "visible" = True}
                    "current" = {"label" = "Current", "visible" = True}
                }
            }
            lists = {"key_field" = "id"}
            statements = {
                load_purchase_order_history = {fetch = "bounded_list", binds = ["id", "after_position", "limit"]}
            }
        }
        "receiving.purchase_order_summary" = manifest.Projection("receiving.purchase_order_summary") {
            label = "Purchase order receiving summary"
            input: {
                labels = {
                    "supplier_id" = {"label" = "Supplier", "references" = {"model" = "supplier"}}
                    "status" = {"label" = "Status"}
                }
            }
            lists = {"model" = "purchase_order", "key_field" = "purchase_order_id", "display_field" = "purchase_order_number"}
            statements = {
                purchase_order_summary = {fetch = "bounded_list", binds = ["supplier_id", "status"]}
            }
        }
        "location.list" = manifest.Projection("location.list") {
            label = "Locations"
            result: {
                labels = {
                    "location_code" = {"label" = "Location code", "visible" = True}
                }
            }
            lists = {"model" = "location", "key_field" = "id", "display_field" = "location_code"}
            statements = {
                list_locations = {fetch = "bounded_list", binds = []}
            }
        }
    }
    connections = {}
    components = {receiving = {}}
    routes = {
        operations = {
            "receiving.record_receipt" = {id = "receiving-record-receipt-http", path = "/receiving/record_receipt"}
            "receiving.load_receipt_screen" = {id = "receiving-load-receipt-screen-http", path = "/receiving/load_receipt_screen"}
            "receiving.load_open_receipt_lines" = {id = "receiving-load-open-receipt-lines-http", path = "/receiving/load_open_receipt_lines"}
            "receiving.load_purchase_order_history" = {id = "receiving-load-purchase-order-history-http", path = "/receiving/load_purchase_order_history"}
            "receiving.purchase_order_summary" = {id = "receiving-purchase-order-summary-http", path = "/receiving/purchase_order_summary"}
            "location.list" = {id = "location-list-http", path = "/location/list"}
        }
    }
}

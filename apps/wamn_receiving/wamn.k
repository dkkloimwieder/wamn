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
            field_text = {item_number = {label = "Item number"}}
            audit_log = {columns = [], retention = "P30D"}
            operations: {
                create: {writable_fields = ["item_number"], label = "Add an item", result = "one"}
            }
        }
        location = manifest.Model("location", _schema) {
            table = "location"
            owner = "wamn_receiving"
            server_owned_fields = []
            field_text = {location_code = {label = "Location code"}}
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
                purchase_order_number = {label = "Order number"}
                supplier_id = {label = "Supplier"}
                status = {label = "Status"}
                row_version = {label = "Revision"}
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
                line_number = {label = "Line"}
                item_id = {label = "Item"}
                ordered_quantity = {label = "Ordered"}
                received_quantity = {label = "Received"}
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
                receipt_reference = {label = "Receipt reference"}
                purchase_order_id = {label = "Purchase order"}
                occurred_at = {label = "Received at"}
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
                quantity = {label = "Quantity"}
                location_id = {label = "Location"}
                purchase_order_line_id = {label = "Order line"}
            }
            audit_log = {columns = []}
            operations = {}
        }
        supplier = manifest.Model("supplier", _schema) {
            table = "supplier"
            owner = "wamn_receiving"
            min_lengths = {name = 1}
            server_owned_fields = []
            field_text = {name = {label = "Supplier name"}, created_at = {label = "Added"}}
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
                    "purchase_order_status" = {"label" = "Status"}
                    "row_version" = {"label" = "Revision"}
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
                    "purchase_order_number" = {"label" = "Order number"}
                    "purchase_order_status" = {"label" = "Status"}
                    "supplier_id" = {"label" = "Supplier"}
                    "purchase_order_row_version" = {"label" = "Revision"}
                    "line_number" = {"label" = "Line"}
                    "item_number" = {"label" = "Item"}
                    "ordered_quantity" = {"label" = "Ordered"}
                    "received_quantity" = {"label" = "Received"}
                    "remaining_quantity" = {"label" = "Remaining"}
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
                    "purchase_order_number" = {"label" = "Order number"}
                    "purchase_order_status" = {"label" = "Status"}
                    "supplier_id" = {"label" = "Supplier"}
                    "purchase_order_row_version" = {"label" = "Revision"}
                    "line_number" = {"label" = "Line"}
                    "item_number" = {"label" = "Item"}
                    "ordered_quantity" = {"label" = "Ordered"}
                    "received_quantity" = {"label" = "Received"}
                    "remaining_quantity" = {"label" = "Remaining"}
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
                    "cursor" = {"label" = "Position"}
                    "type" = {"label" = "Change"}
                    "operation" = {"label" = "Operation"}
                    "changed_by" = {"label" = "Changed by"}
                    "changed_at" = {"label" = "Changed at"}
                    "before" = {"label" = "Before"}
                    "after" = {"label" = "After"}
                    "current" = {"label" = "Current"}
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
                    "location_code" = {"label" = "Location code"}
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

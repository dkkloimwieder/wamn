# The Receiving application: purchase orders, receipts and the commands that
# record what a dock receives. The generator compiles this file to
# generated/wamn.json.
import manifest

_schema = "receiving"
_errors_after_input = ["retry", "timeout", "permission_denied", "internal_error"]
_claim_errors = ["idempotency_conflict", *_errors_after_input]

manifest.Package {
    package = {id = "wamn_receiving", version = "2.1.0", predecessor_version = "2.0.0"}
    client_package = {name = "@wamn/receiving-client"}
    required_platform_policy_contract = {id = "receiving_data_access", state = "satisfied"}
    models = {
        item = manifest.Model("item", _schema) {
            table = "item"
            owner = "wamn_receiving"
            server_owned_fields = ["id"]
            enum_fields = {}
            field_text = {item_number = {label = "Item number"}}
            audit_log = {columns = []}
            operations = {}
        }
        location = manifest.Model("location", _schema) {
            table = "location"
            owner = "wamn_receiving"
            server_owned_fields = ["id"]
            enum_fields = {}
            field_text = {location_code = {label = "Location code"}}
            audit_log = {columns = []}
            operations = {}
        }
        purchase_order = manifest.Model("purchase_order", _schema) {
            table = "purchase_order"
            owner = "wamn_receiving"
            client_field_extensible = True
            server_owned_fields = ["id", "purchase_order_number", "status", "row_version", "created_at", "updated_at"]
            enum_fields = {status = ["open", "complete", "cancelled"]}
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
            audit_log = {columns = ["created_at", "created_by", "updated_at", "updated_by"], retention = "unlimited"}
            operations: {
                get: {label = "Purchase order", result = "one"}
                query: {
                    authored_sql = {
                        default = "query/open_purchase_order.sql"
                        variants = [
                            {
                                field = "purchase_order_number"
                                direction = "ascending"
                                path = "query/open_purchase_order_by_purchase_order_number_ascending.sql"
                            }
                            {
                                field = "purchase_order_number"
                                direction = "descending"
                                path = "query/open_purchase_order_by_purchase_order_number_descending.sql"
                            }
                            {
                                field = "status"
                                direction = "ascending"
                                path = "query/open_purchase_order_by_status_ascending.sql"
                            }
                            {
                                field = "status"
                                direction = "descending"
                                path = "query/open_purchase_order_by_status_descending.sql"
                            }
                            {field = "created_at", direction = "ascending", path = "query/open_purchase_order.sql"}
                            {
                                field = "created_at"
                                direction = "descending"
                                path = "query/open_purchase_order_by_created_at_descending.sql"
                            }
                        ]
                    }
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
            server_owned_fields = ["id"]
            enum_fields = {}
            field_text = {
                line_number = {label = "Line"}
                item_id = {label = "Item"}
                ordered_quantity = {label = "Ordered"}
                received_quantity = {label = "Received"}
            }
            audit_log = {columns = [], retention = "P30D"}
            operations = {}
        }
        receipt = manifest.Model("receipt", _schema) {
            table = "receipt"
            owner = "wamn_receiving"
            server_owned_fields = ["id", "purchase_order_id", "receipt_reference", "occurred_at", "created_at"]
            enum_fields = {}
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
                query: {label = "Receipts", result = "page"}
            }
        }
        receipt_line = manifest.Model("receipt_line", _schema) {
            table = "receipt_line"
            owner = "wamn_receiving"
            server_owned_fields = ["id"]
            enum_fields = {}
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
            server_owned_fields = ["id", "created_at"]
            enum_fields = {}
            field_text = {name = {label = "Supplier name"}, created_at = {label = "Added"}}
            audit_log = {columns = ["created_at"]}
            operations: {
                create: {writable_fields = ["name"], label = "Add a supplier", result = "one"}
                query: {label = "Suppliers", result = "page"}
            }
        }
    }
    custom_operations = {
        "receiving.record_receipt" = manifest.Command("receiving.record_receipt") {
            label = "Record a receipt"
            description = "One submission records every line of one receipt against one purchase order."
            idempotent_by = "claim"
            pre_commit = {fields = [{path = "receipt_id", type = "uuid"}, {path = "purchase_order_id", type = "uuid"}]}
            input = {
                raw_body_maximum = 1048576
                envelope = {minimum = 1, maximum = 100}
                line = {
                    minimum = 1
                    maximum = 100
                    label = "Receipt lines"
                    description = "One line for each order line this receipt records."
                }
                fields = [
                    manifest.request_id
                    manifest.idempotency_key
                    {
                        path = "value.purchase_order_id"
                        type = "uuid"
                        label = "Purchase order"
                        references = {model = "purchase_order"}
                    }
                    {path = "value.receipt_reference", type = "text", label = "Receipt reference"}
                    {path = "value.occurred_at", type = "timestamptz", label = "Received at"}
                    {
                        path = "value.line[].purchase_order_line_id"
                        type = "uuid"
                        label = "Order line"
                        references = {model = "purchase_order_line", narrowed_by = "value.purchase_order_id"}
                    }
                    {
                        path = "value.line[].quantity"
                        type = "numeric"
                        label = "Quantity"
                        description = "More than zero, and no more than the quantity the line still has remaining."
                    }
                    {
                        path = "value.line[].location_id"
                        type = "uuid"
                        label = "Location"
                        references = {model = "location"}
                    }
                ]
            }
            result = {
                class = "one"
                fields = [
                    {path = "receipt_id", type = "uuid", label = "Receipt"}
                    {path = "purchase_order_id", type = "uuid", label = "Purchase order"}
                    {path = "purchase_order_status", type = "text", values = ["open", "complete"], label = "Status"}
                    {path = "row_version", type = "int32", revision = True, label = "Revision"}
                ]
            }
            canonicalization = {
                excluded_fields = ["request_id", "value.idempotency_key"]
                line_order = {ascending_by = "purchase_order_line_id", positive_member = "quantity"}
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
                purchase_order_not_open = {
                    required = ["field"]
                    text = "This purchase order is not open, so it takes no receipt."
                }
                purchase_order_line_not_found = manifest.not_found_details
                purchase_order_line_mismatch = {required = ["field", "id"]}
                location_not_found = manifest.not_found_details
                quantity_exceeds_remaining = {required = ["field", "id"]}
                receipt_reference_conflict = {required = ["constraint"]}
                idempotency_conflict = manifest.conflict_details
            }
            constraint_errors = {receipt_purchase_order_id_receipt_reference_key = "receipt_reference_conflict"}
            relations = [
                {
                    $schema = _schema
                    table = "location"
                    select_fields = ["id"]
                    insert_fields = []
                    update_fields = []
                    lock = True
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "purchase_order"
                    select_fields = ["id", "status", "row_version"]
                    insert_fields = []
                    update_fields = ["status", "row_version"]
                    lock = True
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "purchase_order_line"
                    select_fields = ["id", "purchase_order_id", "ordered_quantity", "received_quantity"]
                    insert_fields = []
                    update_fields = ["received_quantity"]
                    lock = True
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "receipt"
                    select_fields = ["id"]
                    insert_fields = ["purchase_order_id", "receipt_reference", "occurred_at"]
                    update_fields = []
                    lock = False
                    constraints = ["receipt_purchase_order_id_receipt_reference_key"]
                }
                {
                    $schema = _schema
                    table = "receipt_line"
                    select_fields = ["id"]
                    insert_fields = ["receipt_id", "purchase_order_line_id", "quantity", "location_id"]
                    update_fields = []
                    lock = False
                    constraints = []
                }
            ]
            statements = {
                lock_purchase_order = manifest.Statement("command/record_receipt", "lock_purchase_order") {
                    fetch = "optional_one"
                    parameters = [manifest.uuid("purchase_order_id")]
                    row = [manifest.text("status")]
                }
                validate_receipt_line = manifest.Statement("command/record_receipt", "validate_receipt_line") {
                    fetch = "one"
                    parameters = [manifest.uuid("purchase_order_id"), manifest.json("line")]
                    row = [manifest.nullable(manifest.text("outcome")), manifest.nullable(manifest.uuid("id"))]
                }
                insert_receipt = manifest.Statement("command/record_receipt", "insert_receipt") {
                    fetch = "one"
                    parameters = [
                        manifest.uuid("purchase_order_id")
                        manifest.text("receipt_reference")
                        manifest.timestamptz("occurred_at")
                    ]
                    row = [manifest.uuid("id")]
                }
                insert_receipt_line = manifest.Statement("command/record_receipt", "insert_receipt_line") {
                    fetch = "bounded_list"
                    parameters = [manifest.uuid("receipt_id"), manifest.json("line")]
                    row = [manifest.uuid("id")]
                }
                update_purchase_order_line = manifest.Statement("command/record_receipt", "update_purchase_order_line") {
                    fetch = "bounded_list"
                    parameters = [manifest.uuid("purchase_order_id"), manifest.json("line")]
                    row = [manifest.uuid("id")]
                }
                finish_purchase_order = manifest.Statement("command/record_receipt", "finish_purchase_order") {
                    fetch = "one"
                    parameters = [manifest.uuid("purchase_order_id")]
                    row = [manifest.text("status"), manifest.int32("row_version")]
                }
            }
        }
        "receiving.load_receipt_screen" = manifest.Projection("receiving.load_receipt_screen") {
            label = "Receiving screen"
            lists = {model = "purchase_order_line", key_field = "line_id", display_field = "item_number"}
            input = {
                fields = [
                    {
                        path = "purchase_order_id"
                        type = "uuid"
                        label = "Purchase order"
                        references = {model = "purchase_order"}
                    }
                ]
            }
            result = {
                class = "bounded_list"
                fields = [
                    {path = "purchase_order_id", type = "uuid"}
                    {path = "purchase_order_number", type = "text", label = "Order number"}
                    {
                        path = "purchase_order_status"
                        type = "text"
                        values = ["open", "complete", "cancelled"]
                        label = "Status"
                    }
                    {path = "supplier_id", type = "uuid", label = "Supplier"}
                    {path = "row_version", type = "int32", revision = True, label = "Revision"}
                    {path = "line_id", type = "uuid", nullable = True}
                    {path = "line_number", type = "int32", nullable = True, label = "Line"}
                    {path = "item_id", type = "uuid", nullable = True}
                    {path = "item_number", type = "text", nullable = True, label = "Item"}
                    {path = "ordered_quantity", type = "numeric", nullable = True, label = "Ordered"}
                    {path = "received_quantity", type = "numeric", nullable = True, label = "Received"}
                    {path = "remaining_quantity", type = "numeric", nullable = True, label = "Remaining"}
                ]
            }
            errors = ["invalid_input", "not_found", *_errors_after_input]
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "purchase_order"
                    select_fields = ["id", "purchase_order_number", "status", "supplier_id", "row_version"]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "purchase_order_line"
                    select_fields = [
                        "id"
                        "purchase_order_id"
                        "line_number"
                        "item_id"
                        "ordered_quantity"
                        "received_quantity"
                    ]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "item"
                    select_fields = ["id", "item_number"]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
            ]
            statements = {
                load_receipt_screen = manifest.Statement("query", "load_receipt_screen") {
                    fetch = "bounded_list"
                    parameters = [manifest.uuid("purchase_order_id")]
                    row = [
                        manifest.uuid("purchase_order_id")
                        manifest.text("purchase_order_number")
                        manifest.text("purchase_order_status")
                        manifest.uuid("supplier_id")
                        manifest.int32("row_version")
                        manifest.nullable(manifest.uuid("line_id"))
                        manifest.nullable(manifest.int32("line_number"))
                        manifest.nullable(manifest.uuid("item_id"))
                        manifest.nullable(manifest.text("item_number"))
                        manifest.nullable(manifest.numeric("ordered_quantity"))
                        manifest.nullable(manifest.numeric("received_quantity"))
                        manifest.nullable(manifest.numeric("remaining_quantity"))
                    ]
                }
            }
        }
        "receiving.load_purchase_order_history" = manifest.Projection("receiving.load_purchase_order_history") {
            label = "Purchase order history"
            lists = {key_field = "id"}
            input = {
                fields = [
                    {path = "id", type = "uuid", label = "Purchase order"}
                    {
                        path = "after_cursor"
                        type = "text"
                        nullable = True
                        label = "Continue after"
                        description = "The cursor of the last entry you read. Leave it empty for the first page."
                    }
                    {path = "limit", type = "int32", label = "Entries per page"}
                ]
            }
            result = {
                class = "bounded_list"
                fields = [
                    {path = "id", type = "uuid", label = "Entry"}
                    {path = "cursor", type = "text", label = "Position"}
                    {path = "type", type = "text", values = ["insert", "update", "delete"], label = "Change"}
                    {path = "operation", type = "text", label = "Operation"}
                    {path = "changed_by", type = "uuid", label = "Changed by"}
                    {path = "changed_at", type = "timestamptz", label = "Changed at"}
                    {path = "before", type = "text", label = "Before"}
                    {path = "after", type = "text", label = "After"}
                    {path = "current", type = "text", label = "Current"}
                ]
            }
            errors = ["invalid_input", *_errors_after_input]
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
                        "created_at"
                        "created_by"
                        "updated_at"
                        "updated_by"
                    ]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "purchase_order_history"
                    select_fields = [
                        "id"
                        "position"
                        "row_key"
                        "type"
                        "operation"
                        "changed_by"
                        "changed_at"
                        "before"
                        "after"
                    ]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
            ]
            statements = {
                load_purchase_order_history = manifest.Statement("query", "load_purchase_order_history") {
                    fetch = "bounded_list"
                    parameters = [manifest.uuid("id"), manifest.int64("after_position"), manifest.int32("limit")]
                    row = [
                        manifest.uuid("id")
                        manifest.int64("position")
                        manifest.text("type")
                        manifest.text("operation")
                        manifest.uuid("changed_by")
                        manifest.timestamptz("changed_at")
                        manifest.nullable(manifest.text("before"))
                        manifest.nullable(manifest.text("after"))
                        manifest.nullable(manifest.text("current"))
                    ]
                }
            }
        }
        "location.list" = manifest.Projection("location.list") {
            label = "Locations"
            lists = {model = "location", key_field = "id"}
            input = {fields = []}
            result = {
                class = "bounded_list"
                fields = [
                    {path = "id", type = "uuid"}
                    {path = "location_code", type = "text", label = "Location code"}
                ]
            }
            errors = ["invalid_input", *_errors_after_input]
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "location"
                    select_fields = ["id", "location_code"]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
            ]
            statements = {
                list_locations = manifest.Statement("query", "location") {
                    fetch = "bounded_list"
                    parameters = []
                    row = [manifest.uuid("id"), manifest.text("location_code")]
                }
            }
        }
    }
    connections = ["postgres"]
    components = {receiving = {connections = ["postgres"]}}
}

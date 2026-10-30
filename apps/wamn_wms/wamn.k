# The WMS application: packaging, locations, products and the inventory commands
# that move, adjust, merge and split stock. The generator compiles this file to
# generated/wamn.json.
import manifest

_schema = "wms"
_errors_after_input = ["retry", "timeout", "permission_denied", "internal_error"]
_command_errors = ["concurrency_conflict", "idempotency_conflict", *_errors_after_input]

manifest.Package {
    package = {id = "wamn_wms", version = "2.1.5", predecessor_version = "2.1.4"}
    client_package = {name = "@wamn/wms-client"}
    required_platform_policy_contract = {id = "wms_data_access", state = "satisfied"}
    models = {
        inventory_transaction = manifest.Model("inventory_transaction", _schema) {
            table = "inventory_transaction"
            owner = "wamn_wms"
            server_owned_fields = []
            audit_log = {columns = ["created_at", "created_by"]}
            operations: {
                get: {result = "one"}
                query: {feed = False, result = "page"}
            }
        }
        location = manifest.Model("location", _schema) {
            table = "location"
            owner = "wamn_wms"
            min_lengths = {location_code = 1}
            server_owned_fields = ["row_version"]
            audit_log = {columns = ["created_at"], retention = "unlimited"}
            operations: {
                get: {result = "one"}
                query: {display_field = "location_code", filters = [{field = "location_code", match = "contains"}], result = "page"}
                create: {writable_fields = ["location_code"], result = "one"}
                update: {writable_fields = ["location_code"], revision_field = "row_version", result = "one"}
            }
        }
        packaging = manifest.Model("packaging", _schema) {
            table = "packaging"
            owner = "wamn_wms"
            min_lengths = {packaging_code = 1}
            server_owned_fields = ["row_version"]
            audit_log = {columns = ["created_at", "created_by", "updated_at", "updated_by"], retention = "unlimited"}
            operations: {
                get: {revision_field = "row_version", result = "one"}
                query: {
                    display_field = "packaging_code"
                    revision_field = "row_version"
                    filters = [
                        {field = "status"}
                        {field = "location_id"}
                        {field = "packaging_code", match = "contains"}
                    ]
                    sort = {
                        fields = ["packaging_code", "location_id", "updated_at", "created_at"]
                        directions = ["ascending", "descending"]
                    }
                    result = "page"
                }
                create: {
                    writable_fields = ["packaging_code", "type", "location_id", "status"]
                    values = {status = ["available", "held"]}
                    result = "one"
                }
            }
        }
        packaging_quantity = manifest.Model("packaging_quantity", _schema) {
            table = "packaging_quantity"
            owner = "wamn_wms"
            server_owned_fields = []
            audit_log = {columns = ["created_at"]}
            delete_mode = "hard"
            operations: {
                get: {result = "one"}
                query: {feed = False, result = "page"}
            }
        }
        product = manifest.Model("product", _schema) {
            table = "product"
            owner = "wamn_wms"
            min_lengths = {product_code = 1}
            server_owned_fields = ["row_version"]
            audit_log = {columns = ["created_at"], retention = "unlimited"}
            operations: {
                get: {result = "one"}
                query: {display_field = "product_code", filters = [{field = "product_code", match = "contains"}], result = "page"}
                create: {writable_fields = ["product_code"], result = "one"}
                update: {writable_fields = ["product_code"], revision_field = "row_version", result = "one"}
            }
        }
    }
    custom_operations = {
        "inventory.move" = manifest.Command("inventory.move") {
            idempotent_by = "claim"
            input: {
                fields = [
                    manifest.request_id
                    manifest.idempotency_key
                    {path = "value.packaging_id", type = "uuid", references = {model = "packaging"}}
                    {path = "value.to_location_id", type = "uuid", references = {model = "location"}}
                    {path = "value.expected_row_version", type = "int32", revision = "value.packaging_id"}
                    manifest.occurred_at
                ]
            }
            result = {
                class = "one"
                fields = [
                    {path = "packaging_id", type = "uuid"}
                    {path = "location_id", type = "uuid"}
                    {path = "row_version", type = "int32", revision = True}
                ]
            }
            errors = ["invalid_input", "packaging_not_found", "location_not_found", *_command_errors]
            error_details = {
                packaging_not_found = manifest.not_found_details
                location_not_found = manifest.not_found_details
            }
            statements = {
                lock_packaging = manifest.Statement("command/inventory_move", "lock_packaging") {
                    fetch = "optional_one"
                    parameters = [manifest.uuid("packaging_id")]
                    row = [manifest.uuid("location_id"), manifest.int32("row_version"), manifest.text("status")]
                }
                validate_location = manifest.Statement("command/inventory_move", "validate_location") {
                    fetch = "optional_one"
                    parameters = [manifest.uuid("location_id")]
                    row = [manifest.uuid("id")]
                }
                move_packaging = manifest.Statement("command/inventory_move", "move_packaging") {
                    fetch = "one"
                    parameters = [
                        manifest.uuid("packaging_id")
                        manifest.uuid("to_location_id")
                        manifest.timestamptz("occurred_at")
                    ]
                    row = [manifest.uuid("location_id"), manifest.int32("row_version"), manifest.text("status")]
                }
            }
        }
        "inventory.adjust" = manifest.Command("inventory.adjust") {
            idempotent_by = "claim"
            input: {
                fields = [
                    manifest.request_id
                    manifest.idempotency_key
                    {path = "value.packaging_id", type = "uuid", references = {model = "packaging"}}
                    {path = "value.product_id", type = "uuid", references = {model = "product"}}
                    {path = "value.status", type = "text", values = ["available", "held"]}
                    {path = "value.quantity", type = "numeric"}
                    {path = "value.reason_code", type = "text"}
                    {path = "value.expected_row_version", type = "int32", revision = "value.packaging_id"}
                    manifest.occurred_at
                ]
            }
            result = {
                class = "one"
                fields = [
                    {path = "transaction_ids[]", type = "uuid"}
                    {path = "packaging_id", type = "uuid"}
                    {path = "adjusted_quantity", type = "numeric"}
                    {path = "packaging_status", type = "text", values = ["available", "held", "consumed"]}
                    {path = "row_version", type = "int32", revision = True}
                ]
            }
            errors = ["invalid_input", "packaging_not_found", "quantity_not_found", *_command_errors]
            error_details = {
                packaging_not_found = manifest.not_found_details
                quantity_not_found = manifest.not_found_details
            }
            statements = {
                lock_packaging = manifest.Statement("command/inventory_adjust", "lock_packaging") {
                    fetch = "one"
                    parameters = [manifest.uuid("packaging_id"), manifest.int32("expected_row_version")]
                    row = [
                        manifest.Value {name = "outcome", type = "text", nullable = False}
                        manifest.Value {name = "detail", type = "json", nullable = False}
                    ]
                }
                adjust = manifest.Statement("command/inventory_adjust", "adjust") {
                    fetch = "one"
                    parameters = [
                        manifest.uuid("packaging_id")
                        manifest.uuid("product_id")
                        manifest.text("status")
                        manifest.numeric("quantity")
                        manifest.text("reason_code")
                        manifest.timestamptz("occurred_at")
                    ]
                    row = [
                        manifest.Value {name = "outcome", type = "text", nullable = False}
                        manifest.Value {name = "detail", type = "json", nullable = False}
                        manifest.Value {name = "transaction_id", type = "uuid", nullable = True}
                        manifest.Value {name = "adjusted_quantity", type = "numeric", nullable = True}
                        manifest.Value {name = "row_version", type = "int32", nullable = True}
                        manifest.Value {name = "status", type = "text", nullable = True}
                    ]
                }
            }
        }
        "inventory.merge" = manifest.Command("inventory.merge") {
            idempotent_by = "claim"
            input: {
                fields = [
                    manifest.request_id
                    manifest.idempotency_key
                    {path = "value.source_packaging_id", type = "uuid", references = {model = "packaging"}}
                    {path = "value.target_packaging_id", type = "uuid", references = {model = "packaging"}}
                    {path = "value.expected_row_version", type = "int32", revision = "value.target_packaging_id"}
                    manifest.occurred_at
                ]
            }
            result = {
                class = "one"
                fields = [
                    {path = "transaction_ids[]", type = "uuid"}
                    {path = "source_packaging_id", type = "uuid"}
                    {path = "target_packaging_id", type = "uuid"}
                    {path = "target_status", type = "text", values = ["available", "held", "consumed"]}
                    {path = "row_version", type = "int32", revision = True}
                ]
            }
            errors = ["invalid_input", "packaging_not_found", *_command_errors]
            error_details = {
                packaging_not_found = manifest.not_found_details
            }
            statements = {
                lock_both_packagings = manifest.Statement("command/inventory_merge", "lock_both_packagings") {
                    fetch = "one"
                    parameters = [manifest.uuid("source_packaging_id"), manifest.uuid("target_packaging_id"), manifest.int32("expected_row_version")]
                    row = [
                        manifest.Value {name = "outcome", type = "text", nullable = False}
                        manifest.Value {name = "detail", type = "json", nullable = False}
                    ]
                }
                merge = manifest.Statement("command/inventory_merge", "merge") {
                    fetch = "one"
                    parameters = [manifest.uuid("source_packaging_id"), manifest.uuid("target_packaging_id"), manifest.timestamptz("occurred_at")]
                    row = [
                        manifest.Value {name = "outcome", type = "text", nullable = False}
                        manifest.Value {name = "detail", type = "json", nullable = False}
                        manifest.Value {name = "transaction_ids", type = "json", nullable = False}
                        manifest.Value {name = "row_version", type = "int32", nullable = True}
                        manifest.Value {name = "status", type = "text", nullable = True}
                    ]
                }
            }
        }
        "inventory.split" = manifest.Command("inventory.split") {
            idempotent_by = "claim"
            input: {
                fields = [
                    manifest.request_id
                    manifest.idempotency_key
                    {path = "value.source_packaging_id", type = "uuid", references = {model = "packaging"}}
                    {path = "value.product_id", type = "uuid", references = {model = "product"}}
                    {path = "value.status", type = "text", values = ["available", "held"]}
                    {path = "value.quantity", type = "numeric"}
                    {path = "value.new_packaging_code", type = "text"}
                    {
                        path = "value.new_packaging_type"
                        type = "text"
                        values = ["pallet", "tote", "bin", "case", "loose"]
                    }
                    {path = "value.to_location_id", type = "uuid", references = {model = "location"}}
                    {path = "value.expected_row_version", type = "int32", revision = "value.source_packaging_id"}
                    manifest.occurred_at
                ]
            }
            result = {
                class = "one"
                fields = [
                    {path = "transaction_ids[]", type = "uuid"}
                    {path = "source_packaging_id", type = "uuid"}
                    {path = "new_packaging_id", type = "uuid"}
                    {path = "source_status", type = "text", values = ["available", "held", "consumed"]}
                    {path = "row_version", type = "int32", revision = True}
                ]
            }
            errors = [
                "invalid_input"
                "packaging_not_found"
                "location_not_found"
                "quantity_not_found"
                "insufficient_quantity"
                *_command_errors
            ]
            error_details = {
                packaging_not_found = manifest.not_found_details
                location_not_found = manifest.not_found_details
                quantity_not_found = manifest.not_found_details
                insufficient_quantity = {required = ["field"], optional = ["maximum", "observed"]}
            }
            statements = {
                lock_packaging = manifest.Statement("command/inventory_split", "lock_packaging") {
                    fetch = "one"
                    parameters = [manifest.uuid("source_packaging_id"), manifest.int32("expected_row_version")]
                    row = [
                        manifest.Value {name = "outcome", type = "text", nullable = False}
                        manifest.Value {name = "detail", type = "json", nullable = False}
                    ]
                }
                split = manifest.Statement("command/inventory_split", "split") {
                    fetch = "one"
                    parameters = [
                        manifest.uuid("source_packaging_id")
                        manifest.uuid("product_id")
                        manifest.text("status")
                        manifest.numeric("quantity")
                        manifest.text("new_packaging_code")
                        manifest.text("new_packaging_type")
                        manifest.uuid("to_location_id")
                        manifest.timestamptz("occurred_at")
                    ]
                    row = [
                        manifest.Value {name = "outcome", type = "text", nullable = False}
                        manifest.Value {name = "detail", type = "json", nullable = False}
                        manifest.Value {name = "transaction_id", type = "uuid", nullable = True}
                        manifest.Value {name = "new_packaging_id", type = "uuid", nullable = True}
                        manifest.Value {name = "row_version", type = "int32", nullable = True}
                        manifest.Value {name = "status", type = "text", nullable = True}
                    ]
                }
            }
        }
        "inventory.aggregate" = manifest.Projection("inventory.aggregate") {
            lists = {key_field = ["product_id", "location_id", "status"]}
            input: {fields = []}
            result = {
                class = "bounded_list"
                fields = [
                    {path = "product_id", type = "uuid"}
                    {path = "location_id", type = "uuid"}
                    {path = "status", type = "text", values = ["available", "held"]}
                    {path = "quantity", type = "numeric"}
                    {path = "packaging_count", type = "int32"}
                ]
            }
            errors = ["invalid_input", *_errors_after_input]
            statements = {
                inventory_aggregate = manifest.Statement("query", "inventory_aggregate") {
                    fetch = "bounded_list"
                    parameters = []
                    row = [
                        manifest.uuid("product_id")
                        manifest.uuid("location_id")
                        manifest.text("status")
                        manifest.nullable(manifest.numeric("quantity"))
                        manifest.nullable(manifest.int32("packaging_count"))
                    ]
                }
            }
        }
    }
    connections = {labels = {interfaces = ["wasmcloud:blobstore/blobstore@0.1.0"], mode = "write"}}
    components = {wms = {connections = ["postgres"]}}
}

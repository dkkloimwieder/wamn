# The WMS application: packaging, locations, products and the inventory commands
# that move, adjust, merge and split stock. The generator compiles this file to
# generated/wamn.json.
import manifest

_schema = "wms"
_errors_after_input = ["retry", "timeout", "permission_denied", "internal_error"]
_command_errors = ["concurrency_conflict", "idempotency_conflict", *_errors_after_input]

manifest.Package {
    package = {id = "wamn_wms", version = "2.1.0", predecessor_version = "2.0.0"}
    client_package = {name = "@wamn/wms-client"}
    required_platform_policy_contract = {id = "wms_data_access", state = "satisfied"}
    models = {
        inventory_transaction = manifest.Model("inventory_transaction", _schema) {
            table = "inventory_transaction"
            owner = "wamn_wms"
            server_owned_fields = ["id"]
            enum_fields = {}
            audit_log = {columns = ["created_at", "created_by"]}
            operations: {
                get: {result = "one"}
                query: {result = "page"}
            }
        }
        location = manifest.Model("location", _schema) {
            table = "location"
            owner = "wamn_wms"
            min_lengths = {location_code = 1}
            server_owned_fields = ["id", "row_version", "created_at"]
            enum_fields = {}
            audit_log = {columns = ["created_at"]}
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
            server_owned_fields = ["id", "row_version", "created_at", "updated_at"]
            enum_fields = {
                status = ["available", "held", "consumed"]
                type = ["pallet", "tote", "bin", "case", "loose"]
            }
            audit_log = {columns = ["created_at", "created_by", "updated_at", "updated_by"], retention = "unlimited"}
            operations: {
                get: {revision_field = "row_version", result = "one"}
                query: {
                    display_field = "packaging_code"
                    revision_field = "row_version"
                    authored_sql = {
                        default = "query/open_packaging.sql"
                        variants = [
                            {
                                field = "packaging_code"
                                direction = "ascending"
                                path = "query/open_packaging_by_packaging_code_ascending.sql"
                            }
                            {
                                field = "packaging_code"
                                direction = "descending"
                                path = "query/open_packaging_by_packaging_code_descending.sql"
                            }
                            {
                                field = "location_id"
                                direction = "ascending"
                                path = "query/open_packaging_by_location_id_ascending.sql"
                            }
                            {
                                field = "location_id"
                                direction = "descending"
                                path = "query/open_packaging_by_location_id_descending.sql"
                            }
                            {
                                field = "updated_at"
                                direction = "ascending"
                                path = "query/open_packaging_by_updated_at_ascending.sql"
                            }
                            {
                                field = "updated_at"
                                direction = "descending"
                                path = "query/open_packaging_by_updated_at_descending.sql"
                            }
                            {field = "created_at", direction = "ascending", path = "query/open_packaging.sql"}
                            {
                                field = "created_at"
                                direction = "descending"
                                path = "query/open_packaging_by_created_at_descending.sql"
                            }
                        ]
                    }
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
            server_owned_fields = ["id", "created_at"]
            enum_fields = {}
            audit_log = {columns = ["created_at"]}
            delete_mode = "hard"
            operations: {
                get: {result = "one"}
                query: {result = "page"}
            }
        }
        product = manifest.Model("product", _schema) {
            table = "product"
            owner = "wamn_wms"
            min_lengths = {product_code = 1}
            server_owned_fields = ["id", "row_version", "created_at"]
            enum_fields = {}
            audit_log = {columns = ["created_at"]}
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
            input = {
                raw_body_maximum = 1048576
                envelope = {minimum = 1, maximum = 100}
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
            canonicalization = {excluded_fields = ["request_id", "value.idempotency_key"]}
            errors = ["invalid_input", "packaging_not_found", "location_not_found", *_command_errors]
            error_details = {
                packaging_not_found = manifest.not_found_details
                location_not_found = manifest.not_found_details
                idempotency_conflict = manifest.conflict_details
            }
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "packaging"
                    select_fields = ["id", "location_id", "row_version", "status"]
                    insert_fields = []
                    update_fields = ["located_at", "location_id", "row_version"]
                    lock = True
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "location"
                    select_fields = ["id"]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
            ]
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
            input = {
                raw_body_maximum = 1048576
                envelope = {minimum = 1, maximum = 100}
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
            canonicalization = {excluded_fields = ["request_id", "value.idempotency_key"]}
            errors = ["invalid_input", "packaging_not_found", "quantity_not_found", *_command_errors]
            error_details = {
                packaging_not_found = manifest.not_found_details
                quantity_not_found = manifest.not_found_details
                idempotency_conflict = manifest.conflict_details
            }
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "inventory_transaction"
                    select_fields = ["id"]
                    insert_fields = [
                        "from_packaging_id"
                        "from_status"
                        "occurred_at"
                        "product_id"
                        "quantity"
                        "reason_code"
                        "to_packaging_id"
                        "to_status"
                    ]
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "packaging"
                    select_fields = ["id", "location_id", "row_version", "status"]
                    insert_fields = []
                    update_fields = ["row_version"]
                    lock = True
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "packaging_quantity"
                    select_fields = ["id", "packaging_id", "product_id", "quantity", "status"]
                    insert_fields = []
                    update_fields = ["quantity"]
                    delete = True
                    lock = False
                    constraints = []
                }
            ]
            statements = {
                lock_packaging = manifest.Statement("command/inventory_adjust", "lock_packaging") {
                    fetch = "optional_one"
                    parameters = [manifest.uuid("packaging_id")]
                    row = [manifest.uuid("location_id"), manifest.int32("row_version"), manifest.text("status")]
                }
                select_quantity = manifest.Statement("command/inventory_adjust", "select_quantity") {
                    fetch = "optional_one"
                    parameters = [manifest.uuid("packaging_id"), manifest.uuid("product_id"), manifest.text("status")]
                    row = [manifest.numeric("quantity")]
                }
                insert_transaction = manifest.Statement("command/inventory_adjust", "insert_transaction") {
                    fetch = "optional_one"
                    parameters = [
                        manifest.uuid("packaging_id")
                        manifest.uuid("product_id")
                        manifest.text("status")
                        manifest.numeric("quantity")
                        manifest.text("reason_code")
                        manifest.timestamptz("occurred_at")
                    ]
                    row = [manifest.uuid("id")]
                }
                set_quantity = manifest.Statement("command/inventory_adjust", "set_quantity") {
                    fetch = "optional_one"
                    parameters = [
                        manifest.uuid("packaging_id")
                        manifest.uuid("product_id")
                        manifest.text("status")
                        manifest.numeric("quantity")
                    ]
                    row = [manifest.uuid("id"), manifest.numeric("quantity")]
                }
                delete_quantity = manifest.Statement("command/inventory_adjust", "delete_quantity") {
                    fetch = "one"
                    parameters = [manifest.uuid("packaging_id"), manifest.uuid("product_id"), manifest.text("status")]
                    row = [manifest.uuid("id")]
                }
                touch_packaging = manifest.Statement("command/inventory_adjust", "touch_packaging") {
                    fetch = "one"
                    parameters = [manifest.uuid("packaging_id")]
                    row = [manifest.int32("row_version"), manifest.text("status")]
                }
            }
        }
        "inventory.merge" = manifest.Command("inventory.merge") {
            idempotent_by = "claim"
            input = {
                raw_body_maximum = 1048576
                envelope = {minimum = 1, maximum = 100}
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
            canonicalization = {excluded_fields = ["request_id", "value.idempotency_key"]}
            errors = ["invalid_input", "packaging_not_found", *_command_errors]
            error_details = {
                packaging_not_found = manifest.not_found_details
                idempotency_conflict = manifest.conflict_details
            }
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "inventory_transaction"
                    select_fields = ["id"]
                    insert_fields = [
                        "from_packaging_id"
                        "from_status"
                        "occurred_at"
                        "product_id"
                        "quantity"
                        "to_packaging_id"
                        "to_status"
                    ]
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "packaging"
                    select_fields = ["id", "location_id", "row_version", "status"]
                    insert_fields = []
                    update_fields = ["row_version", "status"]
                    lock = True
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "packaging_quantity"
                    select_fields = ["id", "packaging_id", "product_id", "quantity", "status"]
                    insert_fields = ["packaging_id", "product_id", "quantity", "status"]
                    update_fields = ["quantity"]
                    delete = True
                    lock = False
                    constraints = []
                }
            ]
            statements = {
                lock_both_packagings = manifest.Statement("command/inventory_merge", "lock_both_packagings") {
                    fetch = "bounded_list"
                    parameters = [manifest.uuid("source_packaging_id"), manifest.uuid("target_packaging_id")]
                    row = [
                        manifest.uuid("id")
                        manifest.uuid("location_id")
                        manifest.int32("row_version")
                        manifest.text("status")
                    ]
                }
                select_source_quantity = manifest.Statement("command/inventory_merge", "select_source_quantity") {
                    fetch = "bounded_list"
                    parameters = [manifest.uuid("source_packaging_id")]
                    row = [manifest.uuid("product_id"), manifest.numeric("quantity"), manifest.text("status")]
                }
                add_to_target = manifest.Statement("command/inventory_merge", "add_to_target") {
                    fetch = "optional_one"
                    parameters = [
                        manifest.uuid("target_packaging_id")
                        manifest.uuid("product_id")
                        manifest.text("status")
                        manifest.numeric("quantity")
                    ]
                    row = [manifest.uuid("id"), manifest.numeric("quantity")]
                }
                place_on_target = manifest.Statement("command/inventory_merge", "place_on_target") {
                    fetch = "one"
                    parameters = [
                        manifest.uuid("target_packaging_id")
                        manifest.uuid("product_id")
                        manifest.text("status")
                        manifest.numeric("quantity")
                    ]
                    row = [manifest.uuid("id"), manifest.numeric("quantity")]
                }
                insert_transaction = manifest.Statement("command/inventory_merge", "insert_transaction") {
                    fetch = "one"
                    parameters = [
                        manifest.uuid("product_id")
                        manifest.numeric("quantity")
                        manifest.uuid("source_packaging_id")
                        manifest.text("status")
                        manifest.uuid("target_packaging_id")
                        manifest.timestamptz("occurred_at")
                    ]
                    row = [manifest.uuid("id")]
                }
                delete_source_quantity = manifest.Statement("command/inventory_merge", "delete_source_quantity") {
                    fetch = "bounded_list"
                    parameters = [manifest.uuid("source_packaging_id")]
                    row = [manifest.uuid("id")]
                }
                consume_source = manifest.Statement("command/inventory_merge", "consume_source") {
                    fetch = "one"
                    parameters = [manifest.uuid("source_packaging_id")]
                    row = [manifest.int32("row_version")]
                }
                touch_target = manifest.Statement("command/inventory_merge", "touch_target") {
                    fetch = "one"
                    parameters = [manifest.uuid("target_packaging_id")]
                    row = [manifest.int32("row_version"), manifest.text("status")]
                }
            }
        }
        "inventory.split" = manifest.Command("inventory.split") {
            idempotent_by = "claim"
            input = {
                raw_body_maximum = 1048576
                envelope = {minimum = 1, maximum = 100}
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
            canonicalization = {excluded_fields = ["request_id", "value.idempotency_key"]}
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
                idempotency_conflict = manifest.conflict_details
            }
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "inventory_transaction"
                    select_fields = ["id"]
                    insert_fields = [
                        "from_packaging_id"
                        "from_status"
                        "occurred_at"
                        "product_id"
                        "quantity"
                        "to_packaging_id"
                        "to_status"
                    ]
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "location"
                    select_fields = ["id"]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "packaging"
                    select_fields = ["id", "location_id", "row_version", "status"]
                    insert_fields = ["located_at", "location_id", "packaging_code", "status", "type"]
                    update_fields = ["row_version"]
                    lock = True
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "packaging_quantity"
                    select_fields = ["id", "packaging_id", "product_id", "quantity", "status"]
                    insert_fields = ["packaging_id", "product_id", "quantity", "status"]
                    update_fields = ["quantity"]
                    lock = False
                    constraints = []
                }
            ]
            statements = {
                lock_packaging = manifest.Statement("command/inventory_split", "lock_packaging") {
                    fetch = "optional_one"
                    parameters = [manifest.uuid("source_packaging_id")]
                    row = [manifest.uuid("location_id"), manifest.int32("row_version"), manifest.text("status")]
                }
                select_quantity = manifest.Statement("command/inventory_split", "select_quantity") {
                    fetch = "optional_one"
                    parameters = [
                        manifest.uuid("source_packaging_id")
                        manifest.uuid("product_id")
                        manifest.text("status")
                    ]
                    row = [manifest.numeric("quantity")]
                }
                take_from_source = manifest.Statement("command/inventory_split", "take_from_source") {
                    fetch = "optional_one"
                    parameters = [
                        manifest.uuid("source_packaging_id")
                        manifest.uuid("product_id")
                        manifest.text("status")
                        manifest.numeric("quantity")
                    ]
                    row = [manifest.uuid("id"), manifest.numeric("quantity")]
                }
                validate_location = manifest.Statement("command/inventory_split", "validate_location") {
                    fetch = "optional_one"
                    parameters = [manifest.uuid("to_location_id")]
                    row = [manifest.uuid("id")]
                }
                create_packaging = manifest.Statement("command/inventory_split", "create_packaging") {
                    fetch = "one"
                    parameters = [
                        manifest.text("new_packaging_code")
                        manifest.text("new_packaging_type")
                        manifest.uuid("to_location_id")
                        manifest.timestamptz("occurred_at")
                        manifest.text("status")
                    ]
                    row = [manifest.uuid("id"), manifest.int32("row_version"), manifest.text("status")]
                }
                place_quantity = manifest.Statement("command/inventory_split", "place_quantity") {
                    fetch = "one"
                    parameters = [
                        manifest.uuid("new_packaging_id")
                        manifest.uuid("product_id")
                        manifest.text("status")
                        manifest.numeric("quantity")
                    ]
                    row = [manifest.uuid("id"), manifest.numeric("quantity")]
                }
                insert_transaction = manifest.Statement("command/inventory_split", "insert_transaction") {
                    fetch = "one"
                    parameters = [
                        manifest.uuid("product_id")
                        manifest.numeric("quantity")
                        manifest.uuid("source_packaging_id")
                        manifest.text("status")
                        manifest.uuid("new_packaging_id")
                        manifest.timestamptz("occurred_at")
                    ]
                    row = [manifest.uuid("id")]
                }
                touch_source = manifest.Statement("command/inventory_split", "touch_source") {
                    fetch = "one"
                    parameters = [manifest.uuid("source_packaging_id")]
                    row = [manifest.int32("row_version"), manifest.text("status")]
                }
            }
        }
        "inventory.aggregate" = manifest.Projection("inventory.aggregate") {
            lists = {key_field = ["product_id", "location_id", "status"]}
            input = {fields = []}
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
            constraint_errors = {}
            relations = [
                {
                    $schema = _schema
                    table = "packaging"
                    select_fields = ["id", "location_id", "status"]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
                {
                    $schema = _schema
                    table = "packaging_quantity"
                    select_fields = ["packaging_id", "product_id", "quantity", "status"]
                    insert_fields = []
                    update_fields = []
                    lock = False
                    constraints = []
                }
            ]
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
    connections = ["postgres"]
    components = {wms = {connections = ["postgres"]}}
    workflows = {
        movement_label = {
            wiring = "inventory_move_and_label"
            registration = {
                source_package = "wamn_wms"
                entity = "packaging"
                ops = ["update"]
                condition = "old.location_id != new.location_id"
            }
        }
    }
}

# The WMS application: packaging, locations, products and the inventory commands
# that move, adjust, merge and split stock. The generator compiles this file to
# generated/wamn.json.
import manifest

_schema = "wms"

manifest.Package {
    package = {id = "wamn_wms", version = "2.1.7"}
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
                request_id = "request_id"
                revision_guards = {"expected_row_version" = "packaging_id"}
                key = "idempotency_key"
                labels = {
                    "packaging_id" = {"references" = {"model" = "packaging"}}
                    "to_location_id" = {"references" = {"model" = "location"}}
                }
            }
            statements = {
                lock_packaging = {fetch = "optional_one", binds = ["packaging_id"]}
                validate_location = {fetch = "optional_one", binds = ["to_location_id"]}
                move_packaging = {fetch = "one", binds = ["packaging_id", "to_location_id", "occurred_at"]}
            }
        }
        "inventory.adjust" = manifest.Command("inventory.adjust") {
            idempotent_by = "claim"
            input: {
                request_id = "request_id"
                revision_guards = {"expected_row_version" = "packaging_id"}
                key = "idempotency_key"
                labels = {
                    "packaging_id" = {"references" = {"model" = "packaging"}}
                    "product_id" = {"references" = {"model" = "product"}}
                }
            }
            statements = {
                lock_packaging = {fetch = "one", binds = ["packaging_id", "expected_row_version"]}
                adjust = {fetch = "one", binds = ["packaging_id", "product_id", "status", "quantity", "reason_code", "occurred_at"]}
            }
        }
        "inventory.merge" = manifest.Command("inventory.merge") {
            idempotent_by = "claim"
            input: {
                request_id = "request_id"
                revision_guards = {"expected_row_version" = "target_packaging_id"}
                key = "idempotency_key"
                labels = {
                    "source_packaging_id" = {"references" = {"model" = "packaging"}}
                    "target_packaging_id" = {"references" = {"model" = "packaging"}}
                }
            }
            statements = {
                lock_both_packagings = {fetch = "one", binds = ["source_packaging_id", "target_packaging_id", "expected_row_version"]}
                merge = {fetch = "one", binds = ["source_packaging_id", "target_packaging_id", "occurred_at"]}
            }
        }
        "inventory.split" = manifest.Command("inventory.split") {
            idempotent_by = "claim"
            input: {
                request_id = "request_id"
                revision_guards = {"expected_row_version" = "source_packaging_id"}
                key = "idempotency_key"
                labels = {
                    "source_packaging_id" = {"references" = {"model" = "packaging"}}
                    "product_id" = {"references" = {"model" = "product"}}
                    "to_location_id" = {"references" = {"model" = "location"}}
                }
            }
            statements = {
                lock_packaging = {fetch = "one", binds = ["source_packaging_id", "expected_row_version"]}
                split = {fetch = "one", binds = ["source_packaging_id", "product_id", "status", "quantity", "new_packaging_code", "new_packaging_type", "to_location_id", "occurred_at"]}
            }
        }
        "inventory.aggregate" = manifest.Projection("inventory.aggregate") {
            lists = {"key_field" = ["product_id", "location_id", "status"]}
            statements = {
                inventory_aggregate = {fetch = "bounded_list", binds = []}
            }
        }
    }
    connections = {labels = {interface = "wasmcloud:blobstore/blobstore@0.1.0", mode = "write"}}
    components = {wms = {}}
    routes = {
        operations = {
            "inventory.move" = {id = "inventory-move-http", path = "/inventory/move"}
            "inventory.adjust" = {id = "inventory-adjust-http", path = "/inventory/adjust"}
            "inventory.merge" = {id = "inventory-merge-http", path = "/inventory/merge"}
            "inventory.split" = {id = "inventory-split-http", path = "/inventory/split"}
            "inventory.aggregate" = {id = "inventory-aggregate-http", path = "/inventory/aggregate"}
        }
    }
}

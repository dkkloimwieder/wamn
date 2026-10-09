# The platform fixture: the application that platform tests take in place of a
# real one. The generator compiles this file to generated/wamn.json.
import manifest

_schema = "inventory"
_errors_after_input = ["retry", "timeout", "permission_denied", "internal_error"]

manifest.Package {
    package = {id = "platform_fixture", version = "2.1.0", predecessor_version = "2.0.0"}
    required_platform_policy_contract = {id = "platform_fixture_data_access", state = "satisfied"}
    models = {
        widget = manifest.Model("widget", _schema) {
            table = "widget"
            owner = "platform_fixture"
            client_field_extensible = True
            server_owned_fields = ["id", "edit_version"]
            field_text = {
                code = {label = "Widget code", description = "The code an operator types to find one widget."}
                note = {label = "Operator note"}
            }
            audit_log = {columns = ["created_at"], retention = "P30D"}
            # A hard delete: a referenced widget refuses with foreign_key_violation.
            delete_mode = "hard"
            operations: {
                get: {revision_field = "edit_version", result = "one"}
                query: {
                    display_field = "code"
                    filters = [
                        {field = "code"}
                        {field = "note", match = "prefix"}
                        {field = "maker_id", match = "is_null"}
                    ]
                    sort = {fields = ["created_at"], directions = ["ascending", "descending"]}
                    label = "Find widgets"
                    result = "page"
                }
                create: {writable_fields = ["code", "maker_id", "note"], result = "one"}
                update: {writable_fields = ["code", "maker_id", "note"], revision_field = "edit_version", result = "one"}
                delete: {revision_field = "edit_version", result = "one"}
            }
        }
        widget_maker = manifest.Model("widget_maker", _schema) {
            table = "widget_maker"
            owner = "platform_fixture"
            server_owned_fields = ["id", "edit_version"]
            audit_log = {columns = ["created_at"]}
            operations: {
                get: {revision_field = "edit_version", result = "one"}
                query: {
                    display_field = "name"
                    revision_field = "edit_version"
                    filters = [
                        {field = "name", match = "contains"}
                        # A band: every read applies it, 30 days unless the request states one.
                        {field = "created_at", match = "range", required = True, default = {last_days = 30}}
                    ]
                    search = {fields = ["name"]}
                    sort = {fields = ["created_at"], directions = ["ascending", "descending"]}
                    result = "page"
                }
            }
        }
        widget_tag = manifest.Model("widget_tag", _schema) {
            table = "widget_tag"
            owner = "platform_fixture"
            server_owned_fields = ["id", "edit_version"]
            audit_log = {columns = [], retention = "P30D"}
            operations: {
                update: {writable_fields = ["label"], revision_field = "edit_version", result = "one"}
            }
        }
    }
    custom_operations = {
        "widget.archive" = manifest.Command("widget.archive") {
            input.fields = [
                {path = "id", type = "uuid", references = {model = "widget"}}
                {path = "expected_edit_version", type = "int64", revision = "id"}
            ]
            result = {
                class = "one"
                fields = [
                    {path = "id", type = "uuid"}
                    {path = "edit_version", type = "int64", revision = True}
                    {path = "note", type = "text", nullable = True}
                ]
            }
            errors = ["invalid_input", "already_archived", "concurrency_conflict", *_errors_after_input]
            error_details = {
                already_archived = {required = ["field"], text = "This widget is already archived."}
            }
            statements = {
                archive = manifest.Statement("command/widget", "archive") {
                    fetch = "one"
                    parameters = [manifest.uuid("id")]
                    row = [manifest.uuid("id"), manifest.int64("edit_version"), manifest.nullable(manifest.text("note"))]
                }
            }
            # No claim: the expected edit version guards the widget row.
            idempotent_by = {state.guards = {widget = "expected_edit_version"}}
        }
        "widget.list" = manifest.Projection("widget.list") {
            input.fields = [
                {path = "selector", type = "json"}
                {path = "maker_id", type = "uuid", nullable = True, references = {model = "widget_maker"}}
            ]
            result = {
                class = "bounded_list"
                fields = [
                    {path = "id", type = "uuid"}
                    {path = "code", type = "text"}
                    {path = "edit_version", type = "int64", revision = True}
                    {path = "attributes", type = "json", label = "Attributes", description = "Every key the widget carries, as the release stored it."}
                ]
            }
            errors = ["invalid_input", *_errors_after_input]
            statements = {
                list = manifest.Statement("query", "widget_list") {
                    fetch = "bounded_list"
                    parameters = []
                    row = [manifest.uuid("id"), manifest.text("code"), manifest.int64("edit_version"), manifest.json("attributes")]
                }
            }
            lists = {model = "widget", key_field = "id", display_field = "code"}
        }
        "widget.record_batch" = manifest.Command("widget.record_batch") {
            input: {
                line = {minimum = 1, maximum = 10, label = "Batch lines", description = "One line for each widget this batch records."}
                fields = [
                    manifest.request_id
                    manifest.idempotency_key
                    {path = "value.note", type = "text", nullable = True, label = "Batch note", description = "What the operator recorded about this batch."}
                    {path = "value.maker_id", type = "uuid", nullable = True, label = "Maker", references = {model = "widget_maker"}}
                    {path = "value.expected_edit_version", type = "int64", revision = "value.inspector_id", label = "Expected edit version"}
                    {path = "value.grade", type = "text", values = ["first", "second"], label = "Grade"}
                    {path = "value.inspector_id", type = "uuid", nullable = True, label = "Inspector", references = {model = "widget_maker"}}
                    # A line references a widget of the batch's maker.
                    {path = "value.line[].widget_id", type = "uuid", label = "Line", references = {model = "widget", narrowed_by = "value.maker_id"}}
                    {path = "value.line[].amount", type = "numeric", label = "Quantity received"}
                ]
            }
            result = {class = "one", fields = [{path = "widget_id", type = "uuid"}]}
            errors = ["invalid_input", "idempotency_conflict", *_errors_after_input]
            # No business error, so no detail: the platform derives each one.
            error_details = {}
            statements = {
                find_widget = manifest.Statement("command/widget", "record_batch") {
                    fetch = "one"
                    parameters = [manifest.uuid("widget_id")]
                    row = [manifest.uuid("widget_id")]
                }
            }
            idempotent_by = "claim"
            canonicalization = {
                line_order = {ascending_by = "widget_id", positive_member = "amount"}
            }
            label = "Record a batch"
            description = "One submission that records several lines at once."
        }
        "widget_maker.list" = manifest.Projection("widget_maker.list") {
            input.fields = []
            result = {
                class = "bounded_list"
                fields = [{path = "id", type = "uuid"}, {path = "name", type = "text"}]
            }
            errors = ["invalid_input", *_errors_after_input]
            statements = {
                list = manifest.Statement("query", "widget_maker_list") {
                    fetch = "bounded_list"
                    parameters = []
                    row = [manifest.uuid("id"), manifest.text("name")]
                }
            }
            lists = {key_field = "id"}
        }
    }
    connections = ["postgres"]
    components = {fixture = {connections = ["postgres"]}}
    routes = {auth_modes = ["pat"]}
}

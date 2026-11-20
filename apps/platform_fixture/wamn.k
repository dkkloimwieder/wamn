# The platform fixture: the application that platform tests take in place of a
# real one. The generator compiles this file to generated/wamn.json.
import manifest

_schema = "inventory"

manifest.Package {
    package = {id = "platform_fixture", version = "2.1.1"}
    required_platform_policy_contract = {id = "platform_fixture_data_access", state = "satisfied"}
    sequences = {
        label = {pattern = "L-{seq:n}"}
        record_label = {pattern = "{field:stock_id}-G{seq:n,reset=stock_id,width=4}"}
        pick_label = {pattern = "B{seq:batch,per=batch_id,width=4}-{seq:line,reset=batch,width=3}"}
    }
    models = {
        feed_line = manifest.Model("feed_line", _schema) {
            table = "feed_line"
            owner = "platform_fixture"
            audit_log = {columns = [], retention = "P30D"}
            operations = {}
        }
        feed_order = manifest.Model("feed_order", _schema) {
            table = "feed_order"
            owner = "platform_fixture"
            audit_log = {columns = [], retention = "P30D"}
            operations = {}
        }
        feed_item = manifest.Model("feed_item", _schema) {
            table = "feed_item"
            owner = "platform_fixture"
            audit_log = {columns = [], retention = "P30D"}
            operations = {}
        }
        counter = manifest.Model("counter", _schema) {
            table = "counter"
            owner = "platform_fixture"
            server_owned_fields = ["id", "edit_version"]
            audit_log = {columns = ["created_at"], retention = "P30D"}
            delete_mode = "hard"
            operations: {
                get: {revision_field = "edit_version", result = "one"}
                query: {
                    display_field = "label"
                    sort = {fields = ["created_at"], directions = ["ascending", "descending"]}
                    result = "page"
                }
                create: {writable_fields = ["label"], result = "one"}
                update: {writable_fields = ["label"], revision_field = "edit_version", result = "one"}
                delete: {revision_field = "edit_version", result = "one"}
            }
        }
        numbered_label = manifest.Model("numbered_label", _schema) {
            table = "numbered_label"
            owner = "platform_fixture"
            server_owned_fields = ["number", "edit_version"]
            fields = {
                number = {number = "record_label"}
                created_by = {role = "actor"}
                updated_by = {role = "actor"}
            }
            audit_log = {columns = ["created_at", "created_by", "updated_at", "updated_by"]}
            operations: {
                create: {writable_fields = ["stock_id"], result = "one"}
            }
        }
        widget = manifest.Model("widget", _schema) {
            table = "widget"
            owner = "platform_fixture"
            client_field_extensible = True
            server_owned_fields = ["edit_version"]
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
            server_owned_fields = ["edit_version"]
            audit_log = {columns = ["created_at"]}
            operations: {
                get: {revision_field = "edit_version", result = "one"}
                query: {
                    feed = False
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
            server_owned_fields = ["edit_version"]
            audit_log = {columns = [], retention = "P30D"}
            operations: {
                update: {writable_fields = ["label"], revision_field = "edit_version", result = "one"}
            }
        }
    }
    custom_operations = {
        "feed_line.screen" = manifest.Projection("feed_line.screen") {
            feed = True
            lists = {"model" = "feed_line", "key_field" = "id", "display_field" = "label"}
            statements = {
                screen = {fetch = "bounded_list", binds = ["order_id"]}
            }
        }
        "number.issue" = manifest.Command("number.issue") {
            idempotent_by = "claim"
            input: {
                request_id = "request_id"
                key = "idempotency_key"
            }
            statements = {
                count_widgets = {fetch = "one", binds = []}
            }
        }
        "widget.archive" = manifest.Command("widget.archive") {
            idempotent_by = {"state" = {"guards" = {"widget" = "expected_edit_version"}}}
            input: {
                revision_guards = {"expected_edit_version" = "id"}
                labels = {
                    "id" = {"references" = {"model" = "widget"}}
                }
            }
            error_text = {"already_archived" = "This widget is already archived."}
            statements = {
                archive = {fetch = "one", binds = ["id"]}
            }
        }
        "widget.list" = manifest.Projection("widget.list") {
            input: {
                labels = {
                    "maker_id" = {"references" = {"model" = "widget_maker"}}
                }
            }
            result: {
                labels = {
                    "attributes" = {"label" = "Attributes", "description" = "Every key the widget carries, as the release stored it."}
                }
            }
            lists = {"model" = "widget", "key_field" = "id", "display_field" = "code"}
            statements = {
                list = {fetch = "bounded_list", binds = []}
            }
        }
        "widget.record_batch" = manifest.Command("widget.record_batch") {
            idempotent_by = "claim"
            label = "Record a batch"
            description = "One submission that records several lines at once."
            input: {
                request_id = "request_id"
                line = {"minimum" = 1, "maximum" = 10, "label" = "Batch lines", "description" = "One line for each widget this batch records."}
                revision_guards = {"expected_edit_version" = "inspector_id"}
                key = "idempotency_key"
                labels = {
                    "note" = {"label" = "Batch note", "description" = "What the operator recorded about this batch."}
                    "maker_id" = {"label" = "Maker", "references" = {"model" = "widget_maker"}}
                    "expected_edit_version" = {"label" = "Expected edit version"}
                    "grade" = {"label" = "Grade"}
                    "inspector_id" = {"label" = "Inspector", "references" = {"model" = "widget_maker"}}
                    "line[].widget_id" = {"label" = "Line", "references" = {"model" = "widget", "narrowed_by" = "maker_id"}}
                    "line[].amount" = {"label" = "Quantity received"}
                }
            }
            statements = {
                find_widget = {fetch = "one", binds = ["widget_id"]}
            }
        }
        "widget_maker.list" = manifest.Projection("widget_maker.list") {
            lists = {"key_field" = "id"}
            statements = {
                list = {fetch = "bounded_list", binds = []}
            }
        }
    }
    connections = {}
    components = {fixture = {}}
    routes = {
        operations = {
            "widget.archive" = {id = "widget-archive-http", path = "/widget/archive"}
            "widget.list" = {id = "widget-list-http", path = "/widget/list"}
            "widget_maker.list" = {id = "widget-maker-list-http", path = "/widget_maker/list"}
            "widget.record_batch" = {id = "widget-record-batch-http", path = "/widget/record_batch"}
            "number.issue" = {id = "number-issue-http", path = "/number/issue"}
            "feed_line.screen" = {id = "feed-line-screen-http", path = "/feed_line/screen"}
        }auth_modes = ["pat"]}
}

# The minimal overlay above the platform fixture. The generator compiles this
# file to generated/wamn.json.
import manifest

manifest.Package {
    package = {id = "platform_fixture_overlay", version = "2.1.0", predecessor_version = "2.0.0"}
    # The pin is the contract with the base: its version and the digest of its
    # built component. Nothing here imports the base's wamn.k.
    base_dependencies = {
        base_fixture = {
            package = "platform_fixture"
            version = "2.1.0"
            digest = "sha256:33e4dcc80d975cb86b0a4aa5a1c721c821a55c0066f22b03f9cd517b4562963d"
            operations = ["widget.archive"]
        }
    }
    required_platform_policy_contract = {id = "platform_fixture_data_access", state = "satisfied"}
    models = {
        numbered_label = manifest.Model("numbered_label", "inventory") {
            table = "numbered_label"
            owner = "platform_fixture"
            server_owned_fields = ["number", "edit_version"]
            operations: {
                create: {writable_fields = ["stock_id"], result = "one"}
            }
        }
        # The base's widget, with one field the overlay owns.
        widget = manifest.Model("widget", "inventory") {
            table = "widget"
            owner = "platform_fixture"
            field_owners = {overlay_note = "platform_fixture_overlay"}
            server_owned_fields = ["edit_version"]
            operations: {
                get: {result = "one"}
            }
        }
    }
    connections = {}
    components = {fixture_overlay = {connections = ["postgres"]}}
    routes = {auth_modes = ["pat"], path_prefix = "/overlay"}
}

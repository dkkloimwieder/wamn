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
            digest = "sha256:aeb1bc5a8a4c39ff4217db95236260cae5f8e15ed8f921005eca9e83ad1038df"
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

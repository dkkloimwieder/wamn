# The minimal overlay above the platform fixture. The generator compiles this
# file to generated/wamn.json.
import manifest

manifest.Package {
    package = {id = "platform_fixture_overlay", version = "2.1.1"}
    # The pin is the contract with the base: its version and the digest of its
    # built component. Nothing here imports the base's wamn.k.
    base_dependencies = {
        base_fixture = {
            package = "platform_fixture"
            version = "2.1.1"
            digest = "sha256:34ebfc0268c2e3fd88736bdf2f7298c578471ac5666b50c961aeb42851cd6ed2"
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
    components = {fixture_overlay = {}}
    routes = {auth_modes = ["pat"], path_prefix = "/overlay"}
}

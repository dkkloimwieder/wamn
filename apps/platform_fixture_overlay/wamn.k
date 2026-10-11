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
            digest = "sha256:9d97f1a5b8f91a0cdfaf93a4fdb39d21429b99c0f3c54bcfc9da31009e51355c"
            operations = ["widget.archive"]
        }
    }
    required_platform_policy_contract = {id = "platform_fixture_data_access", state = "satisfied"}
    models = {
        # The base's widget, with one field the overlay owns.
        widget = manifest.Model("widget", "inventory") {
            table = "widget"
            owner = "platform_fixture"
            field_owners = {overlay_note = "platform_fixture_overlay"}
            server_owned_fields = ["id", "edit_version"]
            operations: {
                get: {result = "one"}
            }
        }
    }
    connections = ["postgres"]
    components = {fixture_overlay = {connections = ["postgres"]}}
    routes = {auth_modes = ["pat"], path_prefix = "/overlay"}
}

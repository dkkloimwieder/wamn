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
            digest = "sha256:7ed4955d0d23eb8ebd9752347755c77fdd02a48a682fd36f55df9385487fd6cc"
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
            server_owned_fields = ["id", "edit_version", "created_at"]
            enum_fields = {code = ["priority", "standard"]}
            operations: {
                get: {result = "one"}
            }
        }
    }
    connections = ["postgres"]
    components = {fixture_overlay = {connections = ["postgres"]}}
    routes = {auth_modes = ["pat"], path_prefix = "/overlay"}
}

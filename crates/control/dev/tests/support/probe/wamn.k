import manifest

manifest.Package {
    package = {id = "probe", version = "2.1.1"}
    required_platform_policy_contract = {id = "probe_data_access", state = "satisfied"}
    models = {
        counter = manifest.Model("counter", "probe_data") {
            table = "counter"
            owner = "probe"
            server_owned_fields = ["id", "edit_version"]
            audit_log = {columns = ["created_at"], retention = "P30D"}
            delete_mode = "hard"
            operations: {
                get: {revision_field = "edit_version", result = "one"}
                create: {writable_fields = ["label"], result = "one"}
            }
        }
    }
    components = {probe = {connections = ["postgres"]}}
    routes = {path_prefix = "/probe", auth_modes = ["pat"]}
}

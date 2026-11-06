import environment

environment.Environment {
    org = "acme"
    project = "orders"
    env = "files"
    target = "cloud"
    policy = "dev"
    integrations = {
        "file-store" = {version = 1}
        "files-no-cdn" = {version = 1}
    }
    applications = {
        files = {
            current = "__RELEASE_DIGEST__", hostname = "files.orders.example", packages = ["file_fixture"]
            connections = {
                files = "file-store"
                "files-read" = "file-store"
                "files-no-cdn" = "files-no-cdn"
            }
        }
    }
}

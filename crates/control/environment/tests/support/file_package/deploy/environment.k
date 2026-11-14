import environment

environment.Environment {
    org = "acme"
    project = "orders"
    env = "files"
    target = "cloud"
    policy = "dev"
    revocation_seconds = 60
    integrations = {
        "file-store" = {version = 1}
        "files-no-cdn" = {version = 1}
    }
    applications = {
        files = {
            revocation_seconds = 5
            hostname = "files.orders.example"
            connections = {
                files = "file-store"
                "files-read" = "file-store"
                "files-no-cdn" = "files-no-cdn"
            }
        }
    }
}

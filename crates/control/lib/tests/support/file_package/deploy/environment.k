import environment

environment.Environment {
    org = "acme"
    project = "orders"
    env = "files"
    target = "cloud"
    release = "__RELEASE_DIGEST__"
    route_host = "files.orders.example"
    policy = "dev"
    applications = {
        files = {
            packages = ["file_fixture"]
            connectors = {s3 = "host"}
            connections = {
                files = "file-store"
                files_read = "file-store"
                files_no_cdn = "files-no-cdn"
            }
        }
    }
}

import environment

environment.Environment {
    target = "cloud"
    org = "acme"
    project = "platform"
    env = "dev"
    policy = "dev"
    revocation_seconds = 60
    applications = {orders = {current = "none", revocation_seconds = 5, hostname = "platform.localhost", packages = ["platform_fixture"]}}
}

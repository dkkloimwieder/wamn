import environment

environment.Environment {
    target = "cloud"
    org = "acme"
    project = "platform"
    env = "dev"
    policy = "dev"
    applications = {orders = {current = "none", hostname = "platform.localhost", packages = ["platform_fixture"]}}
}
